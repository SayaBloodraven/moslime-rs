//! Headless mode for a Raspberry Pi (or any machine without a screen):
//! reads config_six.json, connects every configured tracker in parallel,
//! logs to the terminal, serves a small control web page (see web.rs) and
//! shuts down cleanly on Ctrl+C or SIGTERM.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use btleplug::platform::Adapter;
use futures::future::join_all;
use tokio::runtime::Runtime;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::tracker::{self, ConnectGate, Status, TrackerCtx, TrackerView};
use crate::{ble, config, protocol, web};

pub fn list_adapters(rt: &Runtime) -> Result<()> {
    let names = rt.block_on(ble::adapter_names())?;
    if names.is_empty() {
        println!("No Bluetooth adapters found.");
    }
    for (i, n) in names.iter().enumerate() {
        println!("{i}: {n}");
    }
    println!();
    println!("To pick one, put its address (or part of its description) in config_six.json, e.g.");
    println!("  \"adapter\": \"58:D8:12:11:EA:79\"   (the address stays the same; hci0/hci1 can swap between boots)");
    Ok(())
}

struct Running {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

struct Slot {
    sensor_id: u8,
    mac: String,
    mac_bytes: [u8; 6],
    view: Arc<Mutex<TrackerView>>,
    run: AsyncMutex<Option<Running>>,
}

/// All trackers plus what is needed to start/stop each one. Shared by the
/// headless loop and the web control page.
pub struct Bridge {
    slots: Vec<Slot>,
    adapter: Adapter,
    gate: Arc<ConnectGate>,
    slime_addr: Mutex<SocketAddr>,
    compat: bool,
}

impl Bridge {
    fn addr(&self) -> SocketAddr {
        *self.slime_addr.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn count(&self) -> usize {
        self.slots.len()
    }

    /// Start tracker `i` (index into the configured list). No-op if running.
    pub async fn start(&self, i: usize) -> Result<()> {
        let slot = self.slots.get(i).context("no such tracker")?;
        let mut guard = slot.run.lock().await;

        if let Some(r) = guard.as_ref() {
            if !r.cancel.is_cancelled() && !r.handle.is_finished() {
                return Ok(()); // already running
            }
        }
        // A previous run may still be shutting down: wait for it, so two
        // connection attempts to the same puck never overlap.
        if let Some(old) = guard.take() {
            old.cancel.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(10), old.handle).await;
        }

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(tracker::run_tracker(TrackerCtx {
            sensor_id: slot.sensor_id,
            mac: slot.mac.clone(),
            mac_bytes: slot.mac_bytes,
            adapter: self.adapter.clone(),
            gate: self.gate.clone(),
            view: slot.view.clone(),
            cancel: cancel.clone(),
            slime_addr: self.addr(),
        }));
        *guard = Some(Running { cancel, handle });
        Ok(())
    }

    /// Ask tracker `i` to stop. Returns immediately; the link is released in
    /// the background.
    pub async fn stop(&self, i: usize) -> Result<()> {
        let slot = self.slots.get(i).context("no such tracker")?;
        let guard = slot.run.lock().await;
        if let Some(r) = guard.as_ref() {
            r.cancel.cancel();
        }
        Ok(())
    }

    pub async fn start_all(&self) {
        for i in 0..self.slots.len() {
            let _ = self.start(i).await;
        }
    }

    pub async fn stop_all(&self) {
        for i in 0..self.slots.len() {
            let _ = self.stop(i).await;
        }
    }

    /// Change where packets are sent. The new address is saved to the config
    /// file; trackers that are running briefly reconnect to use it.
    pub async fn set_server(&self, text: &str) -> Result<()> {
        let addr = config::parse_server(text)?;
        config::save_server(&addr.to_string())?;

        let mut running = Vec::new();
        for (i, s) in self.slots.iter().enumerate() {
            let g = s.run.lock().await;
            if let Some(r) = g.as_ref() {
                if !r.cancel.is_cancelled() && !r.handle.is_finished() {
                    running.push(i);
                }
            }
        }
        for &i in &running {
            self.stop(i).await?;
        }
        if let Ok(mut g) = self.slime_addr.lock() {
            *g = addr;
        }
        for &i in &running {
            self.start(i).await?;
        }
        Ok(())
    }

    /// Stop everything and wait (up to 10 s) for the tasks to finish.
    pub async fn shutdown(&self) {
        let mut handles = Vec::new();
        for s in &self.slots {
            let mut g = s.run.lock().await;
            if let Some(r) = g.take() {
                r.cancel.cancel();
                handles.push(r.handle);
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(10), join_all(handles)).await;
    }

    /// Status of every tracker as JSON (hand-built; no extra dependency).
    pub async fn status_json(&self) -> String {
        let mut items = Vec::new();
        for (i, s) in self.slots.iter().enumerate() {
            let running = {
                let g = s.run.lock().await;
                g.as_ref()
                    .map(|r| !r.cancel.is_cancelled() && !r.handle.is_finished())
                    .unwrap_or(false)
            };
            let (status, packets, message, sid, battery) = {
                match s.view.lock() {
                    Ok(v) => (
                        format!("{:?}", v.status),
                        v.packets,
                        v.message.clone(),
                        v.id.clone(),
                        v.battery.map_or(-1, i32::from),
                    ),
                    Err(_) => ("Disconnected".to_string(), 0, String::new(), String::new(), -1),
                }
            };
            items.push(format!(
                "{{\"id\":{i},\"number\":{},\"mac\":\"{}\",\"sid\":\"{}\",\"status\":\"{}\",\"running\":{running},\"packets\":{packets},\"battery\":{battery},\"message\":\"{}\"}}",
                s.sensor_id as usize + 1,
                json_escape(&s.mac),
                json_escape(&sid),
                status,
                json_escape(&message)
            ));
        }
        format!(
            "{{\"compat\":{},\"server\":\"{}\",\"trackers\":[{}]}}",
            self.compat,
            json_escape(&self.addr().to_string()),
            items.join(",")
        )
    }

    /// Printed IDs learned so far, keyed by MAC.
    fn known_ids(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for s in &self.slots {
            if let Ok(v) = s.view.lock() {
                if !v.id.is_empty() {
                    out.insert(s.mac.to_uppercase(), v.id.clone());
                }
            }
        }
        out
    }

    fn summary(&self) -> String {
        let mut up = Vec::new();
        let mut down = Vec::new();
        for s in &self.slots {
            let status = s.view.lock().map(|v| v.status).unwrap_or(Status::Disconnected);
            let n = s.sensor_id as usize + 1;
            if status == Status::Connected {
                up.push(n);
            } else {
                down.push(n);
            }
        }
        format!(
            "[status] {}/{} streaming | streaming: {:?} | not connected: {:?}",
            up.len(),
            self.slots.len(),
            up,
            down
        )
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' | '\r' | '\t' => o.push(' '),
            c if (c as u32) < 0x20 => {}
            c => o.push(c),
        }
    }
    o
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub fn run(rt: &Runtime) -> Result<()> {
    if !config::exists() {
        bail!(
            "{} not found in the current folder. Copy it from your PC, or create one \
             (see config_six.example.json).",
            config::CONFIG_FILE
        );
    }
    let settings = config::load();
    ble::set_adapter_hint(settings.adapter.clone());
    let slime_addr = config::parse_server(&settings.server)?;

    // Only trackers actually listed in the file; blank entries are skipped.
    let mut wanted = Vec::new();
    for (i, raw) in settings.configured.iter().enumerate().take(config::TRACKERS) {
        if raw.trim().is_empty() {
            continue;
        }
        let mac = protocol::normalize_mac(raw).with_context(|| format!("tracker #{}", i + 1))?;
        let mac_bytes = protocol::parse_mac(&mac)?;
        wanted.push((i, mac, mac_bytes));
    }
    if wanted.is_empty() {
        bail!("no tracker addresses in {}", config::CONFIG_FILE);
    }

    rt.block_on(async move {
        let cell: Arc<OnceCell<Adapter>> = Arc::new(OnceCell::new());
        let adapter = ble::shared_adapter(&cell)
            .await
            .context("could not open the Bluetooth adapter")?;
        eprintln!("Using adapter: {}", ble::adapter_label(&adapter).await);
        eprintln!("SlimeVR server: {slime_addr}");
        eprintln!(
            "Trackers: {} (compatibility mode: {})",
            wanted.len(),
            settings.compat
        );

        let gate = Arc::new(ConnectGate::new(config::MAX_PARALLEL_CONNECTS));
        gate.compat.store(settings.compat, Ordering::Relaxed);

        let slots = wanted
            .into_iter()
            .map(|(i, mac, mac_bytes)| {
                let view = TrackerView {
                    id: settings.ids.get(&mac).cloned().unwrap_or_default(),
                    ..TrackerView::default()
                };
                Slot {
                    sensor_id: i as u8,
                    mac,
                    mac_bytes,
                    view: Arc::new(Mutex::new(view)),
                    run: AsyncMutex::new(None),
                }
            })
            .collect();

        let bridge = Arc::new(Bridge {
            slots,
            adapter,
            gate,
            slime_addr: Mutex::new(slime_addr),
            compat: settings.compat,
        });

        let web_cancel = CancellationToken::new();
        if settings.web_port != 0 {
            let b = bridge.clone();
            let c = web_cancel.clone();
            let port = settings.web_port;
            tokio::spawn(async move {
                if let Err(e) = web::serve(b, port, c).await {
                    eprintln!("[web] could not start the control page on port {port}: {e:#}");
                }
            });
            eprintln!(
                "Control page: http://<this machine's IP>:{}  (on this machine: http://localhost:{})",
                settings.web_port, settings.web_port
            );
        }

        bridge.start_all().await;

        let shutdown = shutdown_signal();
        tokio::pin!(shutdown);
        let mut last_saved: BTreeMap<String, String> = BTreeMap::new();
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.tick().await; // the first tick fires immediately; skip it

        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = tick.tick() => {
                    eprintln!("{}", bridge.summary());
                    // Remember newly seen puck IDs so they show immediately next time.
                    let ids = bridge.known_ids();
                    if !ids.is_empty() && ids != last_saved && config::save_ids(&ids).is_ok() {
                        last_saved = ids;
                    }
                }
            }
        }

        eprintln!("Stopping...");
        web_cancel.cancel();
        bridge.shutdown().await;
        Ok::<(), anyhow::Error>(())
    })
}

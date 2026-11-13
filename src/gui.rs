//! Desktop window (egui). Built by default; leave it out with
//! `--no-default-features` for a headless machine such as a Raspberry Pi.

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use btleplug::platform::Adapter;
use eframe::egui;
use tokio::runtime::Runtime;
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::config::{MAX_PARALLEL_CONNECTS, TRACKERS};
use crate::tracker::{self, ConnectGate, Status, TrackerCtx, TrackerView};
use crate::{ble, config, protocol};

const SCAN_SECONDS: u64 = 10;

#[derive(Default)]
struct ScanState {
    scanning: bool,
    message: String,
    results: Vec<(String, String)>, // (name, address)
}

struct App {
    rt: Arc<Runtime>,
    adapter: Arc<OnceCell<Adapter>>,
    gate: Arc<ConnectGate>,
    compat: bool,
    server: String,
    macs: Vec<String>,
    views: Vec<Arc<Mutex<TrackerView>>>,
    tokens: Vec<Option<CancellationToken>>,
    handles: Vec<Option<JoinHandle<()>>>,
    scan: Arc<Mutex<ScanState>>,
    info: String,
}

impl App {
    fn new(rt: Arc<Runtime>) -> Self {
        let settings = config::load();
        let gate = Arc::new(ConnectGate::new(MAX_PARALLEL_CONNECTS));
        gate.compat.store(settings.compat, Ordering::Relaxed);
        Self {
            rt,
            adapter: Arc::new(OnceCell::new()),
            gate,
            compat: settings.compat,
            server: settings.server,
            macs: settings.macs,
            views: (0..TRACKERS)
                .map(|_| Arc::new(Mutex::new(TrackerView::default())))
                .collect(),
            tokens: (0..TRACKERS).map(|_| None).collect(),
            handles: (0..TRACKERS).map(|_| None).collect(),
            scan: Arc::new(Mutex::new(ScanState {
                message: "No scan results".into(),
                ..Default::default()
            })),
            info: String::new(),
        }
    }

    fn start(&mut self, i: usize) {
        if self.tokens[i].is_some() {
            return;
        }
        let mac = match protocol::normalize_mac(&self.macs[i]) {
            Ok(m) => m,
            Err(e) => {
                self.info = format!("Tracker #{}: {e}", i + 1);
                return;
            }
        };
        let mac_bytes = protocol::parse_mac(&mac).expect("already validated");
        let slime_addr: SocketAddr = match config::parse_server(&self.server) {
            Ok(a) => a,
            Err(e) => {
                self.info = format!("{e:#}");
                return;
            }
        };

        let token = CancellationToken::new();
        self.tokens[i] = Some(token.clone());

        let cell = self.adapter.clone();
        let gate = self.gate.clone();
        let view = self.views[i].clone();

        // If this tracker was stopped a moment ago, its old task may still be
        // disconnecting. Wait for it so the two never overlap.
        let prev = self.handles[i].take();
        self.handles[i] = Some(self.rt.spawn(async move {
            if let Some(h) = prev {
                let _ = h.await;
            }
            match ble::shared_adapter(&cell).await {
                Ok(adapter) => {
                    tracker::run_tracker(TrackerCtx {
                        sensor_id: i as u8,
                        mac,
                        mac_bytes,
                        adapter,
                        gate,
                        view,
                        cancel: token,
                        slime_addr,
                    })
                    .await;
                }
                Err(e) => {
                    if let Ok(mut v) = view.lock() {
                        v.message = format!("Bluetooth error: {e:#}");
                    }
                }
            }
        }));
    }

    fn stop(&mut self, i: usize) {
        if let Some(t) = self.tokens[i].take() {
            t.cancel();
        }
    }

    fn start_all(&mut self) {
        for i in 0..TRACKERS {
            self.start(i);
        }
    }

    fn stop_all(&mut self) {
        for i in 0..TRACKERS {
            self.stop(i);
        }
    }

    fn start_scan(&mut self) {
        {
            let mut s = self.scan.lock().unwrap();
            if s.scanning {
                return;
            }
            s.scanning = true;
            s.message = "Scanning...".into();
        }
        let state = self.scan.clone();
        let cell = self.adapter.clone();
        self.rt.spawn(async move {
            let res = ble::scan_trackers(&cell, SCAN_SECONDS).await;
            let mut s = state.lock().unwrap();
            s.scanning = false;
            match res {
                Ok(list) if list.is_empty() => {
                    s.results.clear();
                    s.message = "No trackers found".into();
                }
                Ok(list) => {
                    s.message = format!("{} tracker(s) found", list.len());
                    s.results = list.into_iter().take(10).collect();
                }
                Err(e) => {
                    s.results.clear();
                    s.message = format!("Scan error: {e:#}");
                }
            }
        });
    }

    /// Fill MAC fields, in order, from scan results whose address starts with "3C:".
    fn copy_scan_to_fields(&mut self) {
        let found: Vec<String> = self
            .scan
            .lock()
            .unwrap()
            .results
            .iter()
            .map(|(_, addr)| addr.clone())
            .filter(|a| a.to_uppercase().starts_with("3C:"))
            .collect();
        if found.is_empty() {
            self.info = "No MAC addresses starting with '3C:' in scan results.".into();
            return;
        }
        for (i, mac) in found.iter().enumerate().take(TRACKERS) {
            self.macs[i] = mac.clone();
        }
        self.info = format!("Populated {} MAC address(es).", found.len().min(TRACKERS));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop_all();
        // Give the tasks a moment to cleanly disconnect the BLE links.
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn status_color(s: Status) -> egui::Color32 {
    match s {
        Status::Connected => egui::Color32::from_rgb(0, 200, 0),
        Status::Connecting => egui::Color32::from_rgb(60, 120, 255),
        Status::Disconnected => egui::Color32::from_rgb(220, 50, 50),
    }
}

fn status_text(s: Status) -> &'static str {
    match s {
        Status::Connected => "Connected",
        Status::Connecting => "Connecting",
        Status::Disconnected => "Disconnected",
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll the shared views ~10x/second; tasks never touch the GUI.
        ctx.request_repaint_after(Duration::from_millis(100));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("SlimeVR Multi Bridge (6 Trackers)");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label("SlimeVR PC address:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.server)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("127.0.0.1:6969")
                        .desired_width(220.0),
                );
                ui.label("(applies to trackers started after you change it)");
            });
            ui.add_space(8.0);

            egui::Grid::new("trackers")
                .num_columns(5)
                .spacing([14.0, 10.0])
                .show(ui, |ui| {
                    for i in 0..TRACKERS {
                        ui.label(format!("Tracker #{}", i + 1));
                        // add_sized forces the width, so the grid can't squeeze it.
                        ui.add_sized(
                            [220.0, 22.0],
                            egui::TextEdit::singleline(&mut self.macs[i])
                                .font(egui::TextStyle::Monospace),
                        );

                        let v = self.views[i].lock().unwrap().clone();
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                            ui.painter()
                                .circle_filled(rect.center(), 6.0, status_color(v.status));
                            ui.label(status_text(v.status))
                                .on_hover_text(if v.message.is_empty() {
                                    "-"
                                } else {
                                    v.message.as_str()
                                });
                            if let Some(b) = v.battery {
                                let color = if b < 10 {
                                    egui::Color32::from_rgb(224, 80, 80)
                                } else if b < 25 {
                                    egui::Color32::from_rgb(224, 160, 48)
                                } else {
                                    egui::Color32::GRAY
                                };
                                ui.colored_label(color, format!("{b}%"))
                                    .on_hover_text("Battery");
                            }
                        });

                        ui.monospace(format!(
                            "W:{:.2} X:{:.2} Y:{:.2} Z:{:.2}",
                            v.quat[0], v.quat[1], v.quat[2], v.quat[3]
                        ));

                        let running = self.tokens[i].is_some();
                        if ui.button(if running { "Stop" } else { "Start" }).clicked() {
                            if running {
                                self.stop(i);
                            } else {
                                self.start(i);
                            }
                        }
                        ui.end_row();
                    }
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let scanning = self.scan.lock().unwrap().scanning;
                if ui
                    .add_enabled(!scanning, egui::Button::new("Scan MACs"))
                    .clicked()
                {
                    self.start_scan();
                }
                if ui.button("Copy").clicked() {
                    self.copy_scan_to_fields();
                }
                if ui.button("Start Bridge (All)").clicked() {
                    self.start_all();
                }
                if ui.button("Stop All").clicked() {
                    self.stop_all();
                }
                if ui.button("Save Settings").clicked() {
                    self.info = match config::parse_server(&self.server)
                        .and_then(|a| config::save_server(&a.to_string()))
                        .and_then(|()| config::save(&self.macs, self.compat))
                    {
                        Ok(()) => "Settings saved.".into(),
                        Err(e) => format!("Save failed: {e:#}"),
                    };
                }
                if ui.button("Clear Log").clicked() {
                    {
                        let mut s = self.scan.lock().unwrap();
                        s.results.clear();
                        s.message = "No scan results".into();
                    }
                    self.info.clear();
                }
                if ui.button("Load Settings").clicked() {
                    let settings = config::load();
                    self.macs = settings.macs;
                    self.server = settings.server;
                    self.compat = settings.compat;
                    self.gate.compat.store(self.compat, Ordering::Relaxed);
                    self.info = "Settings loaded.".into();
                }
            });

            ui.add_space(6.0);
            if ui
                .checkbox(
                    &mut self.compat,
                    "MediaTek Compatibility Mode (connect trackers one at a time)",
                )
                .changed()
            {
                self.gate.compat.store(self.compat, Ordering::Relaxed);
            }

            ui.add_space(8.0);
            ui.label("Scan Results:");
            let (msg, results) = {
                let s = self.scan.lock().unwrap();
                (s.message.clone(), s.results.clone())
            };
            ui.label(msg);
            for (name, addr) in results {
                ui.monospace(format!("{name}: {addr}"));
            }

            if !self.info.is_empty() {
                ui.add_space(8.0);
                ui.label(&self.info);
            }
        });
    }
}

pub fn run() -> eframe::Result<()> {
    ble::set_adapter_hint(config::load().adapter);

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start tokio runtime"),
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([860.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SlimeVR Multi Bridge",
        options,
        Box::new(move |_cc| Ok(Box::new(App::new(rt)))),
    )
}

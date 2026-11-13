//! One tokio task per tracker. Each task owns its own UDP socket and packet
//! counter, reconnects on its own with backoff, and never touches the GUI
//! directly (it only updates a small shared view the GUI polls).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use btleplug::api::{Central, CharPropFlags, Peripheral as _, WriteType};
#[cfg(not(target_os = "linux"))]
use btleplug::api::ScanFilter;
use btleplug::platform::{Adapter, Peripheral};
use futures::StreamExt;
use tokio::net::UdpSocket;
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::ble;
use crate::protocol::{self, Quat};
#[cfg(windows)]
use crate::winbt;

const DATA_CHAR_UUID: Uuid = Uuid::from_u128(0x25047e64_657c_4856_afcf_e315048a965b);
const CMD_CHAR_UUID: Uuid = Uuid::from_u128(0x0000ff01_0000_1000_8000_00805f9b34fb);
/// Standard Bluetooth Battery Level (0-100 %).
const BATTERY_CHAR_UUID: Uuid = Uuid::from_u128(0x00002a19_0000_1000_8000_00805f9b34fb);
/// How often the battery level is re-read (one tiny read; kept rare on purpose).
const BATTERY_EVERY: Duration = Duration::from_secs(300);
const START_STREAM_CMD: [u8; 7] = [0x7e, 0x03, 0x18, 0xd6, 0x01, 0x00, 0x00];

const FIND_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// No data for this long on a "connected" tracker => treat as dead and reconnect.
#[cfg(windows)]
const STALE_AFTER: Duration = Duration::from_secs(5);
/// Linux: BlueZ reports a really dead link itself, and a puck can go quiet for
/// several seconds while another puck connects, so be more patient.
#[cfg(not(windows))]
const STALE_AFTER: Duration = Duration::from_secs(10);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(15);
/// Cap for "puck not advertising / BlueZ busy" failures.
const SOFT_BACKOFF_MAX: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Clone, Debug)]
pub struct TrackerView {
    pub status: Status,
    pub quat: Quat,
    pub packets: u64,
    pub message: String,
    /// Printed ID of the puck ("QM-SS1 170E9" -> "170E9"); empty until known.
    pub id: String,
    /// Battery level in percent, when the puck has reported it.
    pub battery: Option<u8>,
}

impl Default for TrackerView {
    fn default() -> Self {
        Self {
            status: Status::Disconnected,
            quat: [1.0, 0.0, 0.0, 0.0],
            packets: 0,
            message: String::new(),
            id: String::new(),
            battery: None,
        }
    }
}

/// Limits how many trackers are in the BLE connect phase at once.
/// Normal mode: up to `max_parallel` at a time. Compatibility mode (cheap
/// onboard adapters, e.g. many MediaTek chips): strictly one at a time, with
/// a short pause before the next tracker starts connecting. Trackers always
/// *stream* in parallel once connected; only the connect phase is metered.
pub struct ConnectGate {
    parallel: Semaphore,
    serial: Semaphore,
    pub compat: AtomicBool,
    #[allow(dead_code)] // only used where the scan stays on (not Linux)
    scan_refreshed: Mutex<Instant>,
    /// When trackers recently failed to connect/stream (for the retry brake).
    failures: Mutex<Vec<Instant>>,
}

pub struct ConnectPermit<'a> {
    _serial: Option<SemaphorePermit<'a>>,
    _parallel: SemaphorePermit<'a>,
}

impl ConnectGate {
    pub fn new(max_parallel: usize) -> Self {
        Self {
            parallel: Semaphore::new(max_parallel),
            serial: Semaphore::new(1),
            compat: AtomicBool::new(false),
            scan_refreshed: Mutex::new(Instant::now()),
            failures: Mutex::new(Vec::new()),
        }
    }

    /// Records a failure and returns how many trackers failed in the last 20 s.
    /// Used to slow retries down when several pucks are down at once: every
    /// connect attempt costs radio time that the still-connected pucks need.
    #[allow(dead_code)]
    fn note_failure(&self) -> usize {
        match self.failures.lock() {
            Ok(mut f) => {
                let now = Instant::now();
                f.retain(|t| now.duration_since(*t) < Duration::from_secs(20));
                f.push(now);
                f.len()
            }
            Err(_) => 1,
        }
    }

    /// True for at most one caller per 10 seconds, so six trackers that all
    /// can't find their puck don't each restart the shared scan.
    #[allow(dead_code)]
    fn claim_scan_refresh(&self) -> bool {
        match self.scan_refreshed.lock() {
            Ok(mut t) if t.elapsed() > Duration::from_secs(10) => {
                *t = Instant::now();
                true
            }
            _ => false,
        }
    }

    fn is_compat(&self) -> bool {
        self.compat.load(Ordering::Relaxed)
    }

    /// Permits are always taken in the same order so waiters cannot deadlock.
    async fn acquire(&self) -> Result<ConnectPermit<'_>> {
        let serial = if self.is_compat() {
            Some(
                self.serial
                    .acquire()
                    .await
                    .map_err(|_| anyhow!("connect gate closed"))?,
            )
        } else {
            None
        };
        let parallel = self
            .parallel
            .acquire()
            .await
            .map_err(|_| anyhow!("connect gate closed"))?;
        Ok(ConnectPermit {
            _serial: serial,
            _parallel: parallel,
        })
    }
}

pub struct TrackerCtx {
    pub sensor_id: u8,
    /// Canonical "AA:BB:CC:DD:EE:FF"
    pub mac: String,
    pub mac_bytes: [u8; 6],
    pub adapter: Adapter,
    /// Limits how many trackers are in the connect phase at once (Windows BLE
    /// stacks tend to fail when many connects are issued simultaneously).
    pub gate: Arc<ConnectGate>,
    pub view: Arc<Mutex<TrackerView>>,
    pub cancel: CancellationToken,
    pub slime_addr: SocketAddr,
}

impl TrackerCtx {
    fn set_status(&self, s: Status) {
        if let Ok(mut v) = self.view.lock() {
            v.status = s;
        }
    }

    fn note(&self, msg: impl Into<String>) {
        let msg = msg.into();
        eprintln!("[tracker #{}] {}", self.sensor_id as usize + 1, msg);
        if let Ok(mut v) = self.view.lock() {
            v.message = msg;
        }
    }
}

/// Supervisor loop: connect, stream, and on any failure back off and retry
/// until cancelled.
pub async fn run_tracker(ctx: TrackerCtx) {
    let sock = match bind_socket(ctx.slime_addr).await {
        Ok(s) => s,
        Err(e) => {
            ctx.note(format!("UDP socket error: {e:#}"));
            return;
        }
    };

    let mut pcounter: u64 = 0;
    let mut backoff = BACKOFF_MIN;

    while !ctx.cancel.is_cancelled() {
        ctx.set_status(Status::Connecting);
        let started = Instant::now();

        let result = session(&ctx, &sock, &mut pcounter).await;
        // The status is still Connected here only if the session really got to
        // the streaming stage (a failed scan or connect never sets it).
        let was_streaming = ctx
            .view
            .lock()
            .map(|v| v.status == Status::Connected)
            .unwrap_or(false);
        ctx.set_status(Status::Disconnected);

        if ctx.cancel.is_cancelled() {
            break;
        }
        let reason = match result {
            Ok(()) => "stream ended".to_string(),
            Err(e) => format!("{e:#}"),
        };

        // A session that lasted a while resets the backoff; quick failures grow it.
        // A puck that simply isn't advertising right now (timeout / not seen /
        // BlueZ busy) is worth retrying soon; real errors back off further.
        let soft = reason.contains("timed out")
            || reason.contains("In Progress")
            || reason.contains("not seen");
        let cap = if soft { SOFT_BACKOFF_MAX } else { BACKOFF_MAX };
        backoff = if was_streaming && started.elapsed() > Duration::from_secs(10) {
            BACKOFF_MIN
        } else {
            (backoff * 2).min(cap)
        };
        // Retry brake (Linux): with several pucks down, retrying them all at
        // once keeps the radio too busy for the connected ones, so back off
        // further and stagger the pucks so they don't retry in lockstep.
        #[cfg(target_os = "linux")]
        let wait = {
            let failing = ctx.gate.note_failure();
            let mut w = backoff + Duration::from_millis(700 * u64::from(ctx.sensor_id));
            if failing >= 3 {
                w += Duration::from_secs(((failing as u64 - 2) * 3).min(12));
                ctx.note(format!(
                    "{reason} - {failing} pucks down, waiting {}s so the radio can settle",
                    w.as_secs()
                ));
            } else {
                ctx.note(format!("{reason} - retrying in {}s", backoff.as_secs()));
            }
            w
        };
        #[cfg(not(target_os = "linux"))]
        let wait = {
            ctx.note(format!("{reason} - retrying in {}s", backoff.as_secs()));
            backoff
        };

        tokio::select! {
            _ = ctx.cancel.cancelled() => break,
            _ = tokio::time::sleep(wait) => {}
        }
    }

    ctx.set_status(Status::Disconnected);
    ctx.note("stopped");
}

/// The socket is deliberately NOT connected to the server: a connected UDP
/// socket keeps the source IP it picked at the start, so if this machine's IP
/// changes (DHCP, Wi-Fi roaming) it would silently stop reaching SlimeVR until
/// restart. Sending with `send_to` lets the OS choose the source per packet.
async fn bind_socket(_addr: SocketAddr) -> Result<UdpSocket> {
    Ok(UdpSocket::bind("0.0.0.0:0").await?)
}

/// UDP errors are ignored on purpose: on Windows, a connected UDP socket
/// reports ICMP "port unreachable" (SlimeVR server not running yet) as an
/// error on the next call, which must not kill the BLE session.
async fn send(sock: &UdpSocket, to: SocketAddr, pkt: &[u8]) {
    let _ = sock.send_to(pkt, to).await;
}

async fn find_peripheral(
    adapter: &Adapter,
    mac: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    gate: &ConnectGate,
) -> Result<Peripheral> {
    let mut scanning = false;
    let result = find_loop(adapter, mac, timeout, cancel, gate, &mut scanning).await;
    if scanning {
        ble::scan_release(adapter).await;
    }
    result
}

async fn find_loop(
    adapter: &Adapter,
    mac: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    gate: &ConnectGate,
    scanning: &mut bool,
) -> Result<Peripheral> {
    let started = Instant::now();
    let deadline = started + timeout;
    loop {
        for p in adapter.peripherals().await? {
            if p.address().to_string().eq_ignore_ascii_case(mac) {
                return Ok(p);
            }
        }
        // Linux: a paired puck is normally known to BlueZ already. Only if it is
        // missing do we switch the scan on (and off again when we are done).
        #[cfg(target_os = "linux")]
        if !*scanning {
            ble::scan_acquire(adapter).await;
            *scanning = true;
        }
        // Elsewhere: not found for a few seconds means the OS scan may have gone
        // stale, so restart it (rate-limited across all trackers).
        #[cfg(not(target_os = "linux"))]
        if started.elapsed() > Duration::from_secs(4) && gate.claim_scan_refresh() {
            let _ = adapter.stop_scan().await;
            let _ = adapter.start_scan(ScanFilter::default()).await;
        }
        #[cfg(target_os = "linux")]
        let _ = gate; // only used for the non-Linux scan refresh
        let _ = &scanning;
        if Instant::now() > deadline {
            return Err(anyhow!("tracker {mac} not seen in scan"));
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(anyhow!("cancelled")),
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

/// One connect -> stream -> disconnect cycle.
async fn session(ctx: &TrackerCtx, sock: &UdpSocket, pcounter: &mut u64) -> Result<()> {
    let permit = tokio::select! {
        p = ctx.gate.acquire() => p?,
        _ = ctx.cancel.cancelled() => return Ok(()),
    };

    let p = find_peripheral(&ctx.adapter, &ctx.mac, FIND_TIMEOUT, &ctx.cancel, &ctx.gate).await?;

    // Learn the printed ID from the name the puck advertises ("QM-SS1 170E9").
    if let Ok(Some(props)) = p.properties().await {
        if let Some(name) = props.local_name {
            if let Some(id) = name.strip_prefix("QM-SS1") {
                let id = id.trim();
                if !id.is_empty() {
                    if let Ok(mut v) = ctx.view.lock() {
                        v.id = id.to_string();
                    }
                }
            }
        }
    }

    let connected = match tokio::time::timeout(CONNECT_TIMEOUT, p.connect()).await {
        Ok(r) => r,
        Err(_) => {
            // Cancel the attempt that BlueZ is still working on; otherwise the
            // next try is answered with "In Progress" until BlueZ gives up itself.
            let _ = p.disconnect().await;
            return Err(anyhow!("connect timed out"));
        }
    };
    if let Err(e) = connected {
        let _ = p.disconnect().await;
        // Ask Windows directly why, so the log says more than "Not connected".
        #[cfg(windows)]
        {
            let detail = winbt::probe(ctx.mac_bytes).await;
            return Err(anyhow!("connect failed: {e} [{detail}]"));
        }
        #[cfg(not(windows))]
        return Err(anyhow!("connect failed: {e}"));
    }

    let result = stream(ctx, sock, pcounter, &p, permit).await;
    let _ = p.disconnect().await; // always release the BLE link
    // Give Windows and the puck a moment to fully drop the link and start
    // advertising again before anyone tries to reconnect.
    tokio::time::sleep(Duration::from_millis(800)).await;
    result
}

async fn stream(
    ctx: &TrackerCtx,
    sock: &UdpSocket,
    pcounter: &mut u64,
    p: &Peripheral,
    permit: ConnectPermit<'_>,
) -> Result<()> {
    // Windows sometimes hands back an incomplete service list right after
    // connecting (the symptom was "data characteristic not found" on one
    // puck while the others were fine). Ask again a few times before giving up.
    let mut found = None;
    let mut seen = 0usize;
    for attempt in 1..=4 {
        p.discover_services()
            .await
            .context("service discovery failed")?;
        let chars = p.characteristics();
        seen = chars.len();
        let data = chars.iter().find(|c| c.uuid == DATA_CHAR_UUID).cloned();
        let cmd = chars.iter().find(|c| c.uuid == CMD_CHAR_UUID).cloned();
        if let (Some(d), Some(c)) = (data, cmd) {
            found = Some((d, c));
            break;
        }
        ctx.note(&format!(
            "attempt {attempt}/4: tracker reported {seen} characteristic(s), expected ones missing; asking again"
        ));
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    let (data_char, cmd_char) = found.ok_or_else(|| {
        anyhow!("data/command characteristic not found after 4 tries ({seen} characteristics seen)")
    })?;

    let mut notifs = p.notifications().await?;

    // Handshake is re-sent on every (re)connect so the server re-registers us.
    *pcounter += 1;
    send(
        sock,
        ctx.slime_addr,
        &protocol::build_handshake(&ctx.mac_bytes, protocol::FW_VERSION, *pcounter),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    *pcounter += 1;
    send(sock, ctx.slime_addr, &protocol::build_sensor_info(*pcounter, ctx.sensor_id)).await;

    p.subscribe(&data_char)
        .await
        .context("subscribe failed")?;
    let write_type = if cmd_char.properties.contains(CharPropFlags::WRITE) {
        WriteType::WithResponse
    } else {
        WriteType::WithoutResponse
    };
    p.write(&cmd_char, &START_STREAM_CMD, write_type)
        .await
        .context("stream start command failed")?;

    // Connect phase done: let the next tracker in. In compatibility mode,
    // pause first so cheap adapters get a moment to settle.
    if ctx.gate.is_compat() {
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
    drop(permit);
    ctx.set_status(Status::Connected);
    ctx.note("streaming");

    // Battery: read once now, then rarely. Skipped when the puck isn't
    // delivering data, so it never adds traffic to a link that is struggling.
    let battery_char = p
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == BATTERY_CHAR_UUID && c.properties.contains(CharPropFlags::READ));
    let mut last_battery: Option<Instant> = None;

    let mut last_rx = Instant::now();
    let mut watchdog = tokio::time::interval(Duration::from_secs(1));

    // If SlimeVR wasn't running (or its first reply got lost) when we sent the
    // handshake, the server never learns about this tracker. Keep announcing
    // every few seconds until the server answers with anything.
    let mut registered = false;
    let mut last_announce = Instant::now();
    let mut warned = false;
    let mut rxbuf = [0u8; 512];

    loop {
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Ok(()),

            n = notifs.next() => {
                let Some(n) = n else {
                    return Err(anyhow!("notification stream closed (tracker disconnected)"));
                };
                if n.uuid != DATA_CHAR_UUID {
                    continue;
                }
                last_rx = Instant::now();

                if let Some((q, a)) = protocol::decode(&n.value) {
                    *pcounter += 1;
                    send(sock, ctx.slime_addr, &protocol::build_rotation_packet(q, *pcounter, ctx.sensor_id)).await;
                    *pcounter += 1;
                    send(sock, ctx.slime_addr, &protocol::build_accel_packet(a, *pcounter, ctx.sensor_id)).await;

                    if let Ok(mut v) = ctx.view.lock() {
                        v.quat = q;
                        v.packets += 2;
                    }
                }
            }

            r = sock.recv(&mut rxbuf) => {
                match r {
                    Ok(n) if n > 0 => {
                        if !registered {
                            registered = true;
                            ctx.note("SlimeVR server answered");
                        }
                    }
                    Ok(_) => {}
                    // Windows reports an earlier ICMP "port unreachable" here
                    // when the server isn't up; not an error for us.
                    Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                }
            }

            _ = watchdog.tick() => {
                if last_rx.elapsed() > STALE_AFTER {
                    return Err(anyhow!("no data for {}s", STALE_AFTER.as_secs()));
                }
                if let Some(bc) = &battery_char {
                    let due = last_battery.map_or(true, |t| t.elapsed() >= BATTERY_EVERY);
                    if due && last_rx.elapsed() < Duration::from_secs(2) {
                        last_battery = Some(Instant::now());
                        let read = tokio::time::timeout(Duration::from_secs(3), p.read(bc)).await;
                        if let Ok(Ok(v)) = read {
                            if let Some(&pct) = v.first() {
                                if let Ok(mut view) = ctx.view.lock() {
                                    view.battery = Some(pct.min(100));
                                }
                                eprintln!("[tracker #{}] battery {}%", ctx.sensor_id as usize + 1, pct);
                            }
                        }
                    }
                }
                if !registered && last_announce.elapsed() >= Duration::from_secs(3) {
                    if !warned {
                        warned = true;
                        ctx.note("no answer from SlimeVR yet - is it running? re-sending handshake every 3s");
                    }
                    last_announce = Instant::now();
                    *pcounter += 1;
                    send(
                        sock,
                        ctx.slime_addr,
                        &protocol::build_handshake(&ctx.mac_bytes, protocol::FW_VERSION, *pcounter),
                    )
                    .await;
                    *pcounter += 1;
                    send(sock, ctx.slime_addr, &protocol::build_sensor_info(*pcounter, ctx.sensor_id)).await;
                }
            }
        }
    }
}

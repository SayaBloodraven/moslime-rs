use std::net::SocketAddr;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Same file name as the Python version. The "trackers" list keeps its shape,
/// so existing settings load as-is; "compat", "server" and "adapter" are extra
/// keys the Python version simply ignores.
pub const CONFIG_FILE: &str = "config_six.json";

pub const TRACKERS: usize = 6;
/// How many trackers may be in the BLE connect phase at once (normal mode).
/// "Compatibility Mode" drops this to 1 and adds a pause.
pub const MAX_PARALLEL_CONNECTS: usize = 2;
pub const DEFAULT_SERVER: &str = "127.0.0.1:6969";
pub const DEFAULT_WEB_PORT: u16 = 8080;

pub const DEFAULT_MACS: [&str; 6] = [
    "3C:38:F4:B4:95:01",
    "3C:38:F4:B4:95:02",
    "3C:38:F4:B4:95:03",
    "3C:38:F4:B4:95:04",
    "3C:38:F4:B4:95:05",
    "3C:38:F4:B4:95:06",
];

#[derive(Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    trackers: Vec<String>,
    #[serde(default)]
    compat: bool,
    /// Where the SlimeVR server listens, "IP:PORT" (port defaults to 6969).
    #[serde(default = "default_server")]
    server: String,
    /// Optional text that must appear in the Bluetooth adapter's description,
    /// e.g. "hci1". Empty = use the first adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter: Option<String>,
    /// Headless mode: port of the touch/browser control page. 0 = off.
    #[serde(default = "default_web_port")]
    web_port: u16,
}

fn default_web_port() -> u16 {
    DEFAULT_WEB_PORT
}

fn default_server() -> String {
    DEFAULT_SERVER.to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            trackers: Vec::new(),
            compat: false,
            server: default_server(),
            adapter: None,
            web_port: default_web_port(),
        }
    }
}

pub struct Settings {
    /// Always TRACKERS long; missing entries are filled with placeholders (GUI).
    pub macs: Vec<String>,
    /// Exactly what the file contains (headless mode uses only these).
    pub configured: Vec<String>,
    pub compat: bool,
    pub server: String,
    pub adapter: Option<String>,
    pub web_port: u16,
}

pub fn exists() -> bool {
    Path::new(CONFIG_FILE).exists()
}

fn read() -> Config {
    std::fs::read_to_string(CONFIG_FILE)
        .ok()
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .unwrap_or_default()
}

pub fn load() -> Settings {
    let cfg = read();

    let macs = DEFAULT_MACS
        .iter()
        .enumerate()
        .map(|(i, d)| cfg.trackers.get(i).cloned().unwrap_or_else(|| d.to_string()))
        .collect();

    Settings {
        macs,
        configured: cfg.trackers,
        compat: cfg.compat,
        server: cfg.server,
        adapter: cfg.adapter.filter(|a| !a.trim().is_empty()),
        web_port: cfg.web_port,
    }
}

/// Saves the tracker list and compat flag, keeping the server and adapter
/// settings that are already in the file.
pub fn save(macs: &[String], compat: bool) -> Result<()> {
    let mut cfg = read();
    cfg.trackers = macs.to_vec();
    cfg.compat = compat;
    std::fs::write(CONFIG_FILE, serde_json::to_string_pretty(&cfg)?)?;
    Ok(())
}

/// Saves only the adapter hint (e.g. "hci1"); empty clears it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn save_adapter(adapter: &str) -> Result<()> {
    let mut cfg = read();
    cfg.adapter = if adapter.trim().is_empty() {
        None
    } else {
        Some(adapter.trim().to_string())
    };
    std::fs::write(CONFIG_FILE, serde_json::to_string_pretty(&cfg)?)?;
    Ok(())
}

/// Saves only the SlimeVR server address, keeping everything else.
pub fn save_server(server: &str) -> Result<()> {
    let mut cfg = read();
    cfg.server = server.to_string();
    std::fs::write(CONFIG_FILE, serde_json::to_string_pretty(&cfg)?)?;
    Ok(())
}

/// "192.168.1.20:6969" or just "192.168.1.20" (port 6969 assumed).
pub fn parse_server(s: &str) -> Result<SocketAddr> {
    let s = s.trim();
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    format!("{s}:6969")
        .parse::<SocketAddr>()
        .with_context(|| format!("bad server address {s:?} (use IP:PORT, e.g. 192.168.1.20:6969)"))
}

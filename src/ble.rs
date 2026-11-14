use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Result};
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager};
use tokio::sync::OnceCell;

/// Optional text the adapter's description must contain (set once at startup).
static ADAPTER_HINT: OnceLock<Option<String>> = OnceLock::new();

pub fn set_adapter_hint(hint: Option<String>) {
    let _ = ADAPTER_HINT.set(hint.filter(|h| !h.trim().is_empty()));
}

/// True for text shaped like a Bluetooth address (aa:bb:cc:dd:ee:ff).
fn looks_like_address(h: &str) -> bool {
    let parts: Vec<&str> = h.trim().split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Self-repair, step 1: switch the adapter's radio off and on again. Clears most
/// stuck states without touching the Bluetooth service. Linux only.
#[cfg(target_os = "linux")]
pub async fn power_cycle(adapter: &Adapter) -> Result<()> {
    let info = adapter.adapter_info().await.unwrap_or_default();
    let name = info.split_whitespace().next().unwrap_or("").to_string();
    let session = bluer::Session::new().await?;
    let a = session.adapter(&name)?;
    a.set_powered(false).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    a.set_powered(true).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub async fn power_cycle(_adapter: &Adapter) -> Result<()> {
    Err(anyhow!("not supported on this system"))
}

/// Self-repair, step 2: restart the Bluetooth service. This is what fixed the
/// "every puck times out" state by hand. Needs passwordless sudo (the default
/// for the `pi` user on Raspberry Pi OS); returns an error if that is missing.
#[cfg(target_os = "linux")]
pub async fn restart_bluetooth_service() -> Result<()> {
    let status = tokio::process::Command::new("sudo")
        .args(["-n", "systemctl", "restart", "bluetooth"])
        .status()
        .await?;
    if status.success() {
        tokio::time::sleep(Duration::from_secs(3)).await;
        Ok(())
    } else {
        Err(anyhow!("could not restart the bluetooth service (needs passwordless sudo)"))
    }
}

#[cfg(not(target_os = "linux"))]
pub async fn restart_bluetooth_service() -> Result<()> {
    Err(anyhow!("not supported on this system"))
}

/// Linux numbers adapters (hci0, hci1) in whatever order they came up, and
/// the order can change between boots. An address in the config is stable, so
/// turn it into the adapter's current name ("hci0") before matching.
#[cfg(target_os = "linux")]
async fn resolve_hint(h: &str) -> Result<String> {
    if !looks_like_address(h) {
        return Ok(h.to_string());
    }
    let session = bluer::Session::new().await?;
    let mut seen = Vec::new();
    for name in session.adapter_names().await? {
        if let Ok(a) = session.adapter(&name) {
            if let Ok(addr) = a.address().await {
                if addr.to_string().eq_ignore_ascii_case(h.trim()) {
                    // Trailing space so "hci1" can never match "hci10".
                    return Ok(format!("{name} "));
                }
                seen.push(format!("{name} {addr}"));
            }
        }
    }
    Err(anyhow!(
        "no Bluetooth adapter with address {h}; available: {}",
        seen.join(", ")
    ))
}

#[cfg(not(target_os = "linux"))]
async fn resolve_hint(h: &str) -> Result<String> {
    Ok(h.to_string())
}

/// Descriptions of every Bluetooth adapter, e.g. "hci1 (usb:v0BDApB85Bd0200)".
pub async fn adapter_names() -> Result<Vec<String>> {
    let manager = Manager::new().await?;
    let mut out = Vec::new();
    for a in manager.adapters().await? {
        let info = a.adapter_info().await.unwrap_or_else(|_| "(unknown)".to_string());
        out.push(with_address(info).await);
    }
    Ok(out)
}

pub async fn adapter_label(a: &Adapter) -> String {
    let info = a.adapter_info().await.unwrap_or_else(|_| "unknown".to_string());
    with_address(info).await
}

/// Linux: add the adapter's Bluetooth address to its description, so the
/// address (which stays the same across boots) can be copied into the config.
#[cfg(target_os = "linux")]
async fn with_address(info: String) -> String {
    let name = info.split_whitespace().next().unwrap_or("").to_string();
    if let Ok(session) = bluer::Session::new().await {
        if let Ok(a) = session.adapter(&name) {
            if let Ok(addr) = a.address().await {
                return format!("{info} [{addr}]");
            }
        }
    }
    info
}

#[cfg(not(target_os = "linux"))]
async fn with_address(info: String) -> String {
    info
}

/// One adapter shared by every tracker task. The scan is started once and
/// left running, so reconnecting trackers can always find their peripheral
/// in the adapter's cache without each task fighting over start/stop scan.
/// With an adapter hint set, the first adapter whose description contains it
/// is used (useful when a machine has a built-in adapter and a USB dongle).
pub async fn shared_adapter(cell: &OnceCell<Adapter>) -> Result<Adapter> {
    let adapter = cell
        .get_or_try_init(|| async {
            let manager = Manager::new().await?;
            let adapters = manager.adapters().await?;
            if adapters.is_empty() {
                return Err(anyhow!("no Bluetooth adapter found"));
            }
            let hint = ADAPTER_HINT.get().cloned().flatten();
            let adapter = match hint {
                None => adapters.into_iter().next().expect("checked non-empty"),
                Some(h) => {
                    let wanted = resolve_hint(&h).await?.to_lowercase();
                    let mut found = None;
                    let mut seen = Vec::new();
                    for a in adapters {
                        let info = a.adapter_info().await.unwrap_or_default();
                        if info.to_lowercase().contains(&wanted) {
                            found = Some(a);
                            break;
                        }
                        seen.push(info);
                    }
                    found.ok_or_else(|| {
                        anyhow!(
                            "no Bluetooth adapter matching {h:?}; available: {}",
                            seen.join(", ")
                        )
                    })?
                }
            };
            // Linux: scanning is switched on only while somebody needs it (see
            // scan_acquire), because a permanent scan in a busy neighbourhood
            // steals radio time from the connected pucks. Elsewhere it stays on.
            #[cfg(not(target_os = "linux"))]
            adapter.start_scan(ScanFilter::default()).await?;
            Ok::<Adapter, anyhow::Error>(adapter)
        })
        .await?;
    Ok(adapter.clone())
}

/// Number of users that currently want the Bluetooth scan on (Linux only).
#[cfg(target_os = "linux")]
static SCAN_USERS: tokio::sync::Mutex<usize> = tokio::sync::Mutex::const_new(0);

/// Linux: start scanning if nobody else is. Pair every call with `scan_release`.
#[cfg(target_os = "linux")]
pub async fn scan_acquire(adapter: &Adapter) {
    let mut users = SCAN_USERS.lock().await;
    if *users == 0 {
        let _ = adapter.start_scan(ScanFilter::default()).await;
    }
    *users += 1;
}

/// Linux: stop scanning when the last user lets go.
#[cfg(target_os = "linux")]
pub async fn scan_release(adapter: &Adapter) {
    let mut users = SCAN_USERS.lock().await;
    *users = users.saturating_sub(1);
    if *users == 0 {
        let _ = adapter.stop_scan().await;
    }
}

/// Other systems keep the scan running all the time, so these do nothing.
#[cfg(not(target_os = "linux"))]
pub async fn scan_acquire(_adapter: &Adapter) {}
#[cfg(not(target_os = "linux"))]
pub async fn scan_release(_adapter: &Adapter) {}

/// Returns (name, address) for every device whose name starts with "QM-SS1".
#[allow(dead_code)] // used by the GUI build only
pub async fn scan_trackers(cell: &OnceCell<Adapter>, secs: u64) -> Result<Vec<(String, String)>> {
    let adapter = shared_adapter(cell).await?;
    scan_acquire(&adapter).await;
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let result = collect_trackers(&adapter).await;
    scan_release(&adapter).await;
    result
}

#[allow(dead_code)]
async fn collect_trackers(adapter: &Adapter) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for p in adapter.peripherals().await? {
        let Some(props) = p.properties().await? else {
            continue;
        };
        // A puck can be seen before its name has arrived (the name often comes
        // in a separate packet), so also accept the address prefix the pucks use.
        let addr = p.address().to_string();
        let name = props.local_name.unwrap_or_default();
        if name.starts_with("QM-SS1") || addr.to_uppercase().starts_with("3C:38:F4") {
            let label = if name.is_empty() {
                "(name not seen yet)".to_string()
            } else {
                name
            };
            out.push((label, addr));
        }
    }
    Ok(out)
}

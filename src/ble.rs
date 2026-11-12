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

/// Descriptions of every Bluetooth adapter, e.g. "hci1 (usb:v0BDApB85Bd0200)".
pub async fn adapter_names() -> Result<Vec<String>> {
    let manager = Manager::new().await?;
    let mut out = Vec::new();
    for a in manager.adapters().await? {
        out.push(a.adapter_info().await.unwrap_or_else(|_| "(unknown)".to_string()));
    }
    Ok(out)
}

pub async fn adapter_label(a: &Adapter) -> String {
    a.adapter_info().await.unwrap_or_else(|_| "unknown".to_string())
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
                    let wanted = h.to_lowercase();
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
            adapter.start_scan(ScanFilter::default()).await?;
            Ok::<Adapter, anyhow::Error>(adapter)
        })
        .await?;
    Ok(adapter.clone())
}

/// Returns (name, address) for every device whose name starts with "QM-SS1".
pub async fn scan_trackers(cell: &OnceCell<Adapter>, secs: u64) -> Result<Vec<(String, String)>> {
    let adapter = shared_adapter(cell).await?;
    tokio::time::sleep(Duration::from_secs(secs)).await;

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

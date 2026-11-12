//! Windows only: ask Windows directly why a connection failed.
//!
//! btleplug reports every failure as "Not connected". This asks the Windows
//! Bluetooth API itself and returns a short description such as
//! "windows link: disconnected, GATT status: Unreachable". On other
//! platforms it returns a placeholder.

#[cfg(windows)]
fn probe_blocking(mac: [u8; 6]) -> String {
    use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothLEDevice};
    use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

    // Ignore the result: it fails harmlessly if COM is already initialised.
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };

    // A Bluetooth address as a 48-bit number, first byte most significant.
    let addr = mac.iter().fold(0u64, |a, b| (a << 8) | *b as u64);

    let run = || -> windows::core::Result<String> {
        let device = BluetoothLEDevice::FromBluetoothAddressAsync(addr)?.get()?;
        let link = match device.ConnectionStatus()?.0 {
            0 => "disconnected",
            1 => "connected",
            _ => "unknown",
        };
        let result = device
            .GetGattServicesWithCacheModeAsync(BluetoothCacheMode::Uncached)?
            .get()?;
        let gatt = match result.Status()?.0 {
            0 => "Success",
            1 => "Unreachable",
            2 => "ProtocolError",
            3 => "AccessDenied",
            _ => "unknown",
        };
        // Release our handle so this probe never keeps a link open.
        let _ = device.Close();
        Ok(format!("windows link: {link}, GATT status: {gatt}"))
    };

    match run() {
        Ok(s) => s,
        Err(e) => format!("windows probe failed: {e}"),
    }
}

pub async fn probe(mac: [u8; 6]) -> String {
    #[cfg(windows)]
    {
        match tokio::task::spawn_blocking(move || probe_blocking(mac)).await {
            Ok(s) => s,
            Err(e) => format!("windows probe failed: {e}"),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = mac;
        "no Windows probe on this platform".to_string()
    }
}

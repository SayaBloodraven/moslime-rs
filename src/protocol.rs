//! SlimeVR packet building and Mocopi (QM-SS1) data decoding.
//! Pure functions only, so everything here is unit-testable without Bluetooth.

use anyhow::{bail, Result};
use half::f16;

/// Quaternion as [w, x, y, z].
pub type Quat = [f32; 4];

pub const FW_VERSION: &str = "1.0.0";

// ------------------------------------------------
// MAC helpers
// ------------------------------------------------

/// Parse "3C:38:F4:B4:95:01" (or with '-' / no separators) into 6 bytes.
pub fn parse_mac(s: &str) -> Result<[u8; 6]> {
    let hex: String = s
        .chars()
        .filter(|c| *c != ':' && *c != '-' && !c.is_whitespace())
        .collect();
    if !hex.is_ascii() || hex.len() != 12 {
        bail!("invalid MAC address: {s:?}");
    }
    let mut out = [0u8; 6];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow::anyhow!("invalid MAC address: {s:?}"))?;
    }
    Ok(out)
}

/// Canonical "AA:BB:CC:DD:EE:FF" form, used to match scanned peripherals.
pub fn normalize_mac(s: &str) -> Result<String> {
    let b = parse_mac(s)?;
    Ok(format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        b[0], b[1], b[2], b[3], b[4], b[5]
    ))
}

// ------------------------------------------------
// SlimeVR packets (all big-endian)
// ------------------------------------------------

fn fw_string(fw: &str) -> String {
    format!("MoSlime/btleplug - Puck Version:{fw}")
}

pub fn build_handshake(mac: &[u8; 6], fw: &str, counter: u64) -> Vec<u8> {
    let fw_str = fw_string(fw);
    // "1.0.0" -> 100, same mapping as the Python version
    let fw_num: u32 = fw.replace('.', "").parse().unwrap_or(0);

    let mut b = Vec::with_capacity(64 + fw_str.len());
    b.extend_from_slice(&3u32.to_be_bytes()); // packet type = 3
    b.extend_from_slice(&counter.to_be_bytes());
    b.extend_from_slice(&15u32.to_be_bytes()); // board type
    b.extend_from_slice(&8u32.to_be_bytes()); // IMU type
    b.extend_from_slice(&7u32.to_be_bytes()); // MCU type
    b.extend_from_slice(&[0u8; 12]); // IMU info
    b.extend_from_slice(&fw_num.to_be_bytes());
    b.push(fw_str.len() as u8);
    b.extend_from_slice(fw_str.as_bytes());
    b.extend_from_slice(mac);
    b.push(255); // padding
    b
}

pub fn build_sensor_info(counter: u64, sensor_id: u8) -> Vec<u8> {
    let mut b = Vec::with_capacity(15);
    b.extend_from_slice(&15u32.to_be_bytes()); // packet type = 15
    b.extend_from_slice(&counter.to_be_bytes());
    b.push(sensor_id);
    b.push(0); // sensor status
    b.push(8); // sensor type
    b
}

/// `q` is already in SlimeVR-ready (w, x, y, z) naming as produced by [`decode`];
/// this packs it as (-x, z, y, w) exactly like the Python version.
pub fn build_rotation_packet(q: Quat, counter: u64, sensor_id: u8) -> Vec<u8> {
    let [qw, qx, qy, qz] = q;
    let mut b = Vec::with_capacity(31);
    b.extend_from_slice(&17u32.to_be_bytes()); // packet type = 17
    b.extend_from_slice(&counter.to_be_bytes());
    b.push(sensor_id);
    b.push(1); // data type
    for v in [-qx, qz, qy, qw] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.push(0); // calibration info
    b
}

pub fn build_accel_packet(a: [f32; 3], counter: u64, sensor_id: u8) -> Vec<u8> {
    let mut b = Vec::with_capacity(25);
    b.extend_from_slice(&4u32.to_be_bytes()); // packet type = 4
    b.extend_from_slice(&counter.to_be_bytes());
    for v in a {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.push(sensor_id);
    b
}

// ------------------------------------------------
// Mocopi decoding
// ------------------------------------------------

/// Hamilton product (original * rotation) around `axis` by `deg` degrees.
pub fn rotate_quaternion(q: Quat, axis: [f32; 3], deg: f32) -> Quat {
    let half = deg.to_radians() / 2.0;
    let (s, c) = half.sin_cos();
    let (w2, x2, y2, z2) = (c, axis[0] * s, axis[1] * s, axis[2] * s);
    let [w1, x1, y1, z1] = q;
    [
        w1 * w2 - x1 * x2 - y1 * y2 - z1 * z2,
        w1 * x2 + x1 * w2 + y1 * z2 - z1 * y2,
        w1 * y2 - x1 * z2 + y1 * w2 + z1 * x2,
        w1 * z2 + x1 * y2 - y1 * x2 + z1 * w2,
    ]
}

/// data[8..16]  = quaternion, 4 x i16 LE, scaled by 1/8192
/// data[24..30] = acceleration, 3 x f16 LE
pub fn decode(data: &[u8]) -> Option<(Quat, [f32; 3])> {
    if data.len() < 30 {
        return None;
    }
    let q = |o: usize| i16::from_le_bytes([data[o], data[o + 1]]) as f32 / 8192.0;
    let (qw, qx, qy, qz) = (q(8), q(10), q(12), q(14));

    // negate Y, rotate +90 deg about X, then swap Y/Z
    let r = rotate_quaternion([qw, qx, -qy, qz], [1.0, 0.0, 0.0], 90.0);
    let final_q = [r[0], r[1], r[3], r[2]];

    let a = |o: usize| f16::from_le_bytes([data[o], data[o + 1]]).to_f32();
    Some((final_q, [a(24), a(26), a(28)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn mac_roundtrip() {
        assert_eq!(normalize_mac("3c-38-f4-b4-95-01").unwrap(), "3C:38:F4:B4:95:01");
        assert!(parse_mac("nope").is_err());
        assert!(parse_mac("3C:38:F4:B4:95").is_err());
    }

    #[test]
    fn handshake_layout() {
        let mac = parse_mac("3C:38:F4:B4:95:01").unwrap();
        let p = build_handshake(&mac, "1.0.0", 1);
        let n = fw_string("1.0.0").len();
        assert_eq!(&p[0..4], &[0, 0, 0, 3]);
        assert_eq!(&p[4..12], &1u64.to_be_bytes());
        assert_eq!(p.len(), 4 + 8 + 12 + 12 + 4 + 1 + n + 6 + 1);
        assert_eq!(&p[p.len() - 7..p.len() - 1], &mac);
        assert_eq!(*p.last().unwrap(), 255);
    }

    #[test]
    fn packet_sizes() {
        assert_eq!(build_sensor_info(1, 0).len(), 15);
        assert_eq!(build_rotation_packet([1.0, 0.0, 0.0, 0.0], 1, 0).len(), 31);
        assert_eq!(build_accel_packet([0.0; 3], 1, 0).len(), 25);
    }

    #[test]
    fn decode_identity_becomes_90_about_x() {
        let mut d = vec![0u8; 30];
        d[8..10].copy_from_slice(&8192i16.to_le_bytes()); // w = 1.0
        d[24..26].copy_from_slice(&[0x00, 0x3C]); // f16 1.0
        let (q, a) = decode(&d).unwrap();
        let c = std::f32::consts::FRAC_1_SQRT_2;
        assert!(approx(q[0], c) && approx(q[1], c) && approx(q[2], 0.0) && approx(q[3], 0.0));
        assert!(approx(a[0], 1.0));
    }

    #[test]
    fn short_frames_are_ignored() {
        assert!(decode(&[0u8; 29]).is_none());
    }
}

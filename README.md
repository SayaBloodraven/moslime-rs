# moslime-rs

A Rust bridge that sends Sony mocopi (QM-SS1) tracker data to a SlimeVR server,
with parallel connections, automatic reconnection, a desktop window (Windows)
and a headless mode with a web control page (Raspberry Pi / Linux).

This is a Rust port and extension of [moslime](https://github.com/moslime/moslime)
(MIT). The Bluetooth protocol work and the original code are by the moslime
authors (lmore377, PlatinumVsReality, itstait and contributors); thank you.
This project is unofficial and not affiliated with Sony or SlimeVR. mocopi is a
trademark of Sony.

- Windows: see [WINDOWS-README.md](WINDOWS-README.md) (read the Bluetooth notes)
- Raspberry Pi / Linux: see [PI-README.md](PI-README.md) (recommended)

Build from source: `cargo build --release` (window) or
`cargo build --release --no-default-features` (headless, no window).
Prebuilt files come from the GitHub Actions build.

## Status / known limitations
- **Windows:** all six pucks connect from a fresh boot. After a puck is stopped
  (or the app is closed), Windows may refuse to reconnect it until the PC is
  restarted ("Not connected ... GATT status: Unreachable"). The cause is not
  understood; the original Python moslime behaves the same way. Details in
  [WINDOWS-README.md](WINDOWS-README.md).
- **Raspberry Pi / Linux headless build, the web control page and the touch-screen
  kiosk steps have not been tested on real hardware yet.** Treat them as a
  starting point and please report what you find.
- Tested with a TP-Link UB500 Plus adapter. Other adapters (especially some
  built-in ones) may struggle with six trackers; try Compatibility Mode.
- The web control page has no password; anyone on your network can use it.
  Set `web_port` to 0 to turn it off.

Bug reports with the log output are very welcome.

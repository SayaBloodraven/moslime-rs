# moslime-rs

A Rust bridge that sends Sony mocopi (QM-SS1) tracker data to a SlimeVR server,
with parallel connections, automatic reconnection, a desktop window (Windows)
and a headless mode with a web control page (Raspberry Pi / Linux).

This is a Rust port and extension of [moslime](https://github.com/moslime/moslime)
(MIT). The Bluetooth protocol work and the original code are by the moslime
authors (lmore377, PlatinumVsReality, itstait and contributors); thank you.
This port was written by SayaBloodraven with the help of Claude (an AI assistant
made by Anthropic), which wrote much of the Rust code and documentation. The
direction, hardware testing on Windows and project decisions are the owner's.
Parts of it, notably the Raspberry Pi/Linux build, have not been tested on real
hardware yet (see below).

This project is unofficial and not affiliated with Sony or SlimeVR. mocopi is a
trademark of Sony.

- Windows: see [WINDOWS-README.md](WINDOWS-README.md) (read the Bluetooth notes)
- Raspberry Pi / Linux: see [PI-README.md](PI-README.md) (recommended)

Build from source: `cargo build --release` (window) or
`cargo build --release --no-default-features` (headless, no window).
Prebuilt files come from the GitHub Actions build.

## Status / known limitations
- **Raspberry Pi 4B + TP-Link UB500 Plus (Linux, headless):** tested with six
  mocopi pucks. The pairing helper (`--setup`), streaming to SlimeVR, the web
  control page, Stop/Start of single pucks and of all pucks, the touch-screen
  kiosk, and auto-start after a reboot all work. A puck occasionally takes a
  while to connect after a start (they advertise in bursts); the bridge keeps
  retrying.
- **Each card shows** the puck's printed ID, a name you give it (tap the name
  line; e.g. "Left ankle"), its battery level and its updates per second. The
  battery level is live: the bridge asks each puck once a minute, the way
  Sony's app does. A healthy puck sends 50 updates per second.
- **2M radio mode (Linux):** after a puck connects, the bridge asks the adapter
  to move that link to Bluetooth's 2M mode. With six pucks on one dongle in
  the default 1M mode, some pucks were held at 22 or 33 updates per second;
  on 2M all six hold 50 (five hours without a drop in testing). This uses
  `hcitool` (part of the `bluez` package) through passwordless `sudo`, which
  the `pi` user has by default. If it can't be sent, the puck simply stays on
  1M. To turn it off, create an empty file named `no_2m` next to the program.
- **Self-repair (Linux):** if two or more pucks are started and none has
  streamed for 90 seconds, the bridge switches the adapter off and on, and if
  that doesn't help it restarts the Bluetooth service. It also resets the
  adapter if pucks keep dropping right after connecting. When nothing comes
  back (pucks off or flat) it tries less and less often.
- **Known issue (Linux): stopping or reconnecting one puck can silence one or
  two others.** They reconnect by themselves, usually within a minute. Pucks
  with low batteries make this worse, so keep them charged. The cause is not
  understood yet; splitting the pucks over two adapters may help and is
  planned.
- **Adapter choice:** `"adapter"` in the config can be the adapter's Bluetooth
  address (`moslime --list-adapters` shows them). Use the address: Linux can
  swap the `hci0`/`hci1` names between boots.
- **Windows:** all six pucks connect from a fresh boot. After a puck is stopped
  (or the app is closed), Windows may refuse to reconnect it until the PC is
  restarted ("Not connected ... GATT status: Unreachable"). The cause is not
  understood; the original Python moslime behaves the same way. Details in
  [WINDOWS-README.md](WINDOWS-README.md). Linux does not have this problem.
- **Not tested yet:** other Linux distributions, and more than six pucks (a
  second adapter).
- Tested with a TP-Link UB500 Plus adapter. Other adapters (especially some
  built-in ones) may struggle with six trackers; try Compatibility Mode.
- The web control page has no password; anyone on your network can use it.
  Set `web_port` to 0 to turn it off.

## Help test it
This has been tested on one setup: a Raspberry Pi 4B with a TP-Link UB500 Plus
adapter and six pucks. Reports from other machines are the most useful help
right now, whether it works or not: other Pi models, other Linux systems,
other Bluetooth adapters, Windows PCs. Please open an issue (there is a short
form) and include your computer, adapter and the log output.

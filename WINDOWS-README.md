# SlimeVR Multi Bridge for Windows (Sony mocopi QM-SS1)

Run `moslime.exe`. Put your six puck addresses in the boxes (or use
Scan MACs), set the "SlimeVR PC address" if SlimeVR runs on another machine,
press Save Settings, then Start Bridge (All). SlimeVR can be started before or
after; trackers keep announcing themselves until it answers.

## What to know about Windows Bluetooth (please read)
Windows handles Bluetooth LE differently from Linux, and the pucks can get
"stuck" there:
- A first start after a fresh boot works well, including all six pucks.
- After you **Stop** a puck (or close the app) Windows may refuse to
  reconnect it ("Not connected ... GATT status: Unreachable") until the PC is
  restarted. This is a Windows behaviour, not specific to this program; the
  original Python moslime does the same.
- So: avoid Stop/Start on a single puck during a session. If it happens, restart
  the PC, then start the bridge before doing anything else with the pucks.
- Do not use phone Bluetooth near the pucks while connecting; a puck only
  talks to one device at a time.

## Adapters
- Many built-in laptop/motherboard adapters (for example MediaTek) struggle with
  six connections. Tick "MediaTek Compatibility Mode" to connect them one at a
  time.
- A TP-Link UB500 (Plus) USB dongle worked well for connecting all six.
  Plug it into a USB 2.0 port or a short extension cable.

## A more reliable option
A Raspberry Pi (4B) with a UB500 running the headless build avoids the Windows
stuck state and can be controlled from a web page. See PI-README.md.

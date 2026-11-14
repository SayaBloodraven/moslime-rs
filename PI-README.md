# Running the bridge on a Raspberry Pi (headless)

## Quick install (script)
On the Pi (64-bit Raspberry Pi OS or Debian/Ubuntu), as your normal user:

    bash <(wget -qO- https://moslime.bloodraven.net) --service

That address redirects to `install.sh` in this repository. If you'd rather read
the script before running it (a good habit), download it first:

    curl -fsSL https://raw.githubusercontent.com/SayaBloodraven/moslime-rs/main/install.sh -o install.sh
    less install.sh
    bash install.sh --service

It installs the Bluetooth packages, downloads the newest release into
`~/moslime-rs`, creates `config_six.json` (asks for your PC's IP) and, with
`--service`, sets up auto-start. It then prints the pairing steps below.
Use `--build` to compile from source instead.

If the Pi has its own touch screen (no desktop needed), add `--kiosk`:

    bash <(wget -qO- https://moslime.bloodraven.net) --service --kiosk

It installs a minimal X server and a browser, turns on console auto-login and
opens the control page full-screen after a reboot, with the mouse pointer
hidden. Tested on a Pi 4B with a 5-inch DSI touch screen; screen rotation or
touch alignment may need adjusting for other screens. The manual steps follow.

## 1. Prepare the Pi
    sudo apt update
    sudo apt install -y build-essential pkg-config libdbus-1-dev bluez
    sudo apt install -y firmware-realtek     # only if the UB500 isn't detected
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

Plug the UB500 into a USB 2.0 port (black), ideally on a short extension cable
away from the Pi's USB 3.0 ports (they cause 2.4 GHz noise).
Check it: `bluetoothctl list` should show the controller(s).

Optional: turn off the Pi's onboard Bluetooth so only the dongle exists.
Add `dtoverlay=disable-bt` to /boot/firmware/config.txt and reboot.

## 2. Build (no window)
    cargo build --release --no-default-features

## 3. Pair the pucks (required on Linux)
The easy way: with all pucks switched on, run

    ./moslime --setup

It finds your Bluetooth adapter (preferring a USB dongle over the Pi's built-in
one), scans for the pucks, shows their printed IDs, pairs and trusts them,
asks for your PC's IP and writes `config_six.json`. Running it again keeps the
pucks it already saved and adds new ones. The manual steps below are there in
case you prefer them or something goes wrong.

The original moslime wiki says the pucks must be *paired* with the Pi first
(this is why it works on Linux but not Windows). With all six pucks on
(double blue blink):

    bluetoothctl
    [bluetooth]# scan on          # wait until all six QM-SS1 show up, MACs start 3C:38:F4
    [bluetooth]# pair 3C:38:F4:XX:XX:01
    [bluetooth]# trust 3C:38:F4:XX:XX:01
    ... repeat for each puck ...
    [bluetooth]# exit

If you have several adapters, run `select <controller MAC>` (use `list`) inside
bluetoothctl first so you pair against the dongle. The moslime repo also has an
experimental `scripts/autopair.py` that does this for you.
Then turn the pucks off and toggle Bluetooth once: `sudo systemctl restart bluetooth`.

If a puck refuses to connect later: stop the bridge, turn all pucks off,
restart Bluetooth, start again. Still failing: remove the pairing
(`bluetoothctl remove <MAC>`), factory-reset the puck (press its button 10-15
times until it blinks red/blue) and pair again.

## 4. Configure
Copy `config_six.example.json` to `config_six.json` (or copy the one from your PC) and set:
- `trackers`: your six puck MACs (blank entries are skipped)
- `server`: the LAN IP of the PC running SlimeVR, e.g. "192.168.1.50:6969"
- `adapter`: leave "" for the first adapter, or part of the name shown by
  `./target/release/moslime --list-adapters` (e.g. "hci1") to pick the dongle
- `compat`: true connects one puck at a time (slower, gentler)

On the PC: allow inbound UDP 6969 in Windows Firewall (SlimeVR server) and make
sure the Pi and PC are on the same network.

## 5. Run
    ./target/release/moslime --headless
Ctrl+C stops it. A status line prints every 30 s.

## 6. Auto-start (optional)
Edit and install `moslime.service` (instructions at its top).
View logs with `journalctl -u moslime -f`.

## Control page (Start/Stop buttons)
In headless mode the bridge serves a page at `http://<Pi IP>:8080` with a
Start/Stop button for each puck plus Start all / Stop all. Open it from your
phone or PC on the same network. Change the port with `web_port` in
`config_six.json`, or set it to 0 to turn it off. There is no password, so
anyone on your network can use it.

The page also lets you change the PC's address: tap the "SlimeVR PC: ..." line
at the top, type the new IP with the on-page keypad, and Save. It is written to
`config_six.json` and any running pucks reconnect briefly to use it.

## Showing the page on the Pi's touch screen (kiosk, no desktop)
Install a minimal X server and a browser:

    sudo apt install -y xserver-xorg xinit chromium

Turn on console auto-login (`sudo raspi-config` -> System Options -> Boot / Auto
Login -> Console Autologin), then add this to the end of `~/.bash_profile`:

    if [ -z "${DISPLAY:-}" ] && [ "$(tty)" = /dev/tty1 ]; then
      exec startx /usr/bin/chromium --kiosk --noerrdialogs --disable-infobars --no-first-run "http://localhost:8080/?kiosk=1" -- -nocursor
    fi

`-nocursor` is what hides the mouse pointer. (A Wayland kiosk with `cage` also
works but kept showing a stuck pointer in the middle of a touch-only screen.)

The bridge itself still runs from systemd (`moslime.service`). Don't also enable
the `TTYPath=/dev/tty1` lines in the service, since the kiosk uses that screen.
If the screen is rotated or touches land in the wrong place, that is a display/
touch setting for the screen, not the bridge.

## Pi with a small touch screen and no desktop
Use the headless build; the screen isn't needed. To see the status on the Pi's
own screen, uncomment the three `StandardOutput/StandardError/TTYPath` lines in
`moslime.service`. Use a fresh Raspberry Pi OS Lite (64-bit) with SSH enabled.
Building on a Pi 4 takes about 10-20 minutes the first time.

## Notes
- Source of the pairing steps: the moslime wiki (First Time Setup / Troubleshooting).
- moslime lists the Pi 4B and TP-Link UB500 as tested adapters.

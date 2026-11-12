#!/usr/bin/env bash
# moslime-rs installer for Raspberry Pi OS / Debian / Ubuntu (headless build).
#
#   curl -fsSL https://raw.githubusercontent.com/SayaBloodraven/moslime-rs/main/install.sh -o install.sh
#   less install.sh            # have a look before running it
#   bash install.sh
#
# Options:
#   --tag vX.Y.Z    install a specific release (default: the newest, pre-releases included)
#   --build         build from source instead of downloading the release (takes 10-20 min on a Pi 4)
#   --service       also install a systemd service that starts at boot (you start it after pairing)
#   --dir PATH      install folder (default: ~/moslime-rs)
#   --help
set -euo pipefail

REPO="SayaBloodraven/moslime-rs"
DIR="${HOME}/moslime-rs"
TAG=""
BUILD=0
SERVICE=0

usage() {
  cat <<'USAGE'
moslime-rs installer for Raspberry Pi OS / Debian / Ubuntu (headless build).

  bash <(wget -qO- https://raw.githubusercontent.com/SayaBloodraven/moslime-rs/main/install.sh) [options]

Options:
  --tag vX.Y.Z    install a specific release (default: the newest, pre-releases included)
  --build         build from source instead of downloading the release (10-20 min on a Pi 4)
  --service       also install a systemd service that starts at boot (you start it after pairing)
  --dir PATH      install folder (default: ~/moslime-rs)
  --help
USAGE
}

say() { printf '\n==> %s\n' "$*"; }
die() { printf 'Error: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --tag)     [ $# -ge 2 ] || die "--tag needs a value"; TAG="$2"; shift 2 ;;
    --build)   BUILD=1; shift ;;
    --service) SERVICE=1; shift ;;
    --dir)     [ $# -ge 2 ] || die "--dir needs a value"; DIR="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *)         die "unknown option: $1 (try --help)" ;;
  esac
done

[ "$(id -u)" -ne 0 ] || die "run this as your normal user, not root (it uses sudo where needed)"
command -v apt-get >/dev/null || die "this installer needs an apt-based system (Raspberry Pi OS, Debian, Ubuntu)"

case "$(uname -m)" in
  aarch64|arm64) ASSET="moslime-linux-arm64" ;;
  x86_64)        ASSET="moslime-linux-x64" ;;
  *) die "unsupported CPU '$(uname -m)'. Use a 64-bit OS (the Pi needs 64-bit Raspberry Pi OS)." ;;
esac

say "Installing system packages"
PKGS="bluez curl unzip jq ca-certificates"
if [ "$BUILD" -eq 1 ]; then
  PKGS="$PKGS build-essential pkg-config libdbus-1-dev git"
fi
sudo apt-get update
# shellcheck disable=SC2086
sudo apt-get install -y $PKGS

say "Making sure Bluetooth is running"
sudo systemctl enable --now bluetooth || true
getent group bluetooth >/dev/null && sudo usermod -aG bluetooth "$USER" || true

mkdir -p "$DIR"

if [ "$BUILD" -eq 0 ]; then
  if [ -z "$TAG" ]; then
    say "Looking up the newest release"
    TAG="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases" | jq -r '.[0].tag_name // empty')"
    [ -n "$TAG" ] || die "no release found yet. Try again later, or use --build."
  fi
  say "Downloading ${ASSET}.zip (${TAG})"
  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT
  curl -fL "https://github.com/${REPO}/releases/download/${TAG}/${ASSET}.zip" -o "$TMP/pkg.zip" \
    || die "download failed. Does release ${TAG} contain ${ASSET}.zip?"
  # Keep an existing config if there is one.
  unzip -o -q "$TMP/pkg.zip" -d "$DIR"
else
  say "Building from source"
  if ! command -v cargo >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  fi
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
  SRC="$DIR/source"
  if [ -d "$SRC/.git" ]; then
    git -C "$SRC" pull --ff-only
  else
    git clone --depth 1 ${TAG:+--branch "$TAG"} "https://github.com/${REPO}" "$SRC"
  fi
  (cd "$SRC" && cargo build --release --no-default-features)
  cp "$SRC/target/release/moslime" "$DIR/moslime"
  cp "$SRC/config_six.example.json" "$SRC/moslime.service" "$SRC/PI-README.md" "$DIR/"
fi
chmod +x "$DIR/moslime"

CONFIG="$DIR/config_six.json"
if [ ! -f "$CONFIG" ]; then
  say "Creating $CONFIG"
  cp "$DIR/config_six.example.json" "$CONFIG"
  if [ -r /dev/tty ]; then
    printf 'IP address of the PC running SlimeVR (e.g. 192.168.1.50), or press Enter to set it later: ' > /dev/tty
    read -r IP < /dev/tty || IP=""
    if [ -n "$IP" ]; then
      jq --arg s "$IP" '.server = $s' "$CONFIG" > "$CONFIG.tmp" && mv "$CONFIG.tmp" "$CONFIG"
    fi
  fi
else
  say "Keeping your existing $CONFIG"
fi

if [ "$SERVICE" -eq 1 ]; then
  say "Installing the systemd service"
  sudo tee /etc/systemd/system/moslime.service >/dev/null <<UNIT
[Unit]
Description=Mocopi to SlimeVR bridge
After=bluetooth.target network-online.target
Wants=bluetooth.target network-online.target

[Service]
User=$USER
WorkingDirectory=$DIR
ExecStart=$DIR/moslime --headless
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
  sudo systemctl daemon-reload
  sudo systemctl enable moslime
  echo "Service installed (not started yet: pair the pucks and fill in the MACs first)."
fi

cat <<DONE

Installed in: $DIR

Next steps
  1. Pair each puck (they must be on, double blue blink):
       bluetoothctl
       scan on                      # wait for the QM-SS1 devices
       pair  3C:38:F4:XX:XX:XX
       trust 3C:38:F4:XX:XX:XX      # repeat for all six, then: exit
  2. Put the six MAC addresses (and the PC's IP) in:  $CONFIG
  3. Run it:     cd "$DIR" && ./moslime --headless
$( [ "$SERVICE" -eq 1 ] && echo "     or start the service:  sudo systemctl start moslime   (logs: journalctl -u moslime -f)" )
  4. The control page is at  http://<this Pi's IP>:8080

If it says the Bluetooth adapter can't be used, log out and back in once
(you were added to the 'bluetooth' group), or check:  ./moslime --list-adapters
DONE

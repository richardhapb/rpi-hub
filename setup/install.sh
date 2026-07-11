#!/bin/bash
#
# Configure the Pi to act as a Bluetooth HID keyboard, and install the service.
# Run as root on the Pi.
#
# Two BlueZ changes are non-obvious and both are required:
#
#   1. bluetoothd normally loads its `input` plugin, which claims the HID *host*
#      role -- it wants to talk TO keyboards, and it takes L2CAP PSMs 17 and 19
#      to do it. We need to BE a keyboard, so the plugin has to go, or our
#      listeners cannot bind.
#
#   2. Class-of-Device tells the host what kind of thing we are. Left as an audio
#      device, macOS files us under speakers and never asks for HID.
#
# This is destructive to any existing Bluetooth *audio* role on this Pi.
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "run as root" >&2
    exit 1
fi

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BACKUP=/root/rpi-hub-backup

echo "==> backing up current BlueZ config to $BACKUP"
mkdir -p "$BACKUP"
[[ -f "$BACKUP/main.conf.orig" ]] || cp /etc/bluetooth/main.conf "$BACKUP/main.conf.orig"

echo "==> stopping bluetoothd from claiming the HID host role"
mkdir -p /etc/systemd/system/bluetooth.service.d
cat > /etc/systemd/system/bluetooth.service.d/rpi-hub.conf <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/libexec/bluetooth/bluetoothd --noplugin=input
EOF

echo "==> advertising as a keyboard (Class-of-Device 0x002540)"
sed -i 's/^#\?Class = .*/Class = 0x002540/' /etc/bluetooth/main.conf
grep -q '^Class =' /etc/bluetooth/main.conf || sed -i '/^\[General\]/a Class = 0x002540' /etc/bluetooth/main.conf

echo "==> building"
# Build as whoever owns the checkout, not as root: git refuses to operate on a
# repo it does not own, and root has no reason to be writing into ~/.cargo.
REPO_USER="$(stat -c %U "$REPO")"
sudo -u "$REPO_USER" bash -lc "cd '$REPO' && cargo build --release"
install -m 755 "$REPO/target/release/rpi-hub" /usr/local/bin/rpi-hub

echo "==> installing service"
install -m 644 setup/rpi-hub.service /etc/systemd/system/rpi-hub.service

systemctl daemon-reload
systemctl restart bluetooth
sleep 2
systemctl enable --now rpi-hub

echo
echo "==> done. Class of device:"
hciconfig hci0 class | grep -i "device class" || true
echo
echo "Pair 'rpi-hub Keyboard' from the Mac, then type."
echo "Logs:    journalctl -u rpi-hub -f"
echo "Restore: $REPO/setup/uninstall.sh"

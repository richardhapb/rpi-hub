#!/bin/bash
#
# Configure the Pi to act as a Bluetooth HID keyboard, and install the service.
# Run as root on the Pi.
#
# Three BlueZ changes are non-obvious and all are required:
#
#   1. bluetoothd normally loads its `input` plugin, which claims the HID *host*
#      role -- it wants to talk TO keyboards, and it takes L2CAP PSMs 17 and 19
#      to do it. We need to BE a keyboard, so the plugin has to go, or our
#      listeners cannot bind.
#
#   2. Class-of-Device tells the host what kind of thing we are. Left as an audio
#      device, macOS files us under speakers and never asks for HID.
#
#      Setting `Class` in main.conf is NOT sufficient, and believing it was cost a
#      whole evening. BlueZ takes only the major/minor bits from main.conf and
#      RECOMPUTES the *service* bits from whatever profile UUIDs are registered on
#      the adapter. So every plugin that registers an audio profile silently puts
#      the Audio/Telephony bits back:
#
#          a2dp  -> 110a/110b   AudioSource / AudioSink
#          avrcp -> 110c/110e   A/V Remote Control
#          sap   -> 112d        SIM Access      (sets the Telephony bit)
#          midi  -> 03b80e5a    BLE MIDI
#
#      With those loaded the adapter advertises 0x006c0540, not the 0x002540 below.
#      macOS then files the Pi as a *speaker*, adds it as an audio output device,
#      and tries to stream A2DP to a keyboard -- forever, at ~400 failures/min with
#      76% packet retransmission, starving the radio that every other Bluetooth
#      device on the Mac shares. Verify with `busctl get-property ... Adapter1 UUIDs`,
#      not with main.conf.
#
#   3. PipeWire/WirePlumber registers HFP (111e/111f) with BlueZ over D-Bus as an
#      external org.bluez.Profile1. That is NOT a bluetoothd plugin, so --noplugin
#      cannot reach it, and it sets the Audio+Telephony bits all on its own.
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

echo "==> dropping the HID-host and audio plugins (see the header)"
mkdir -p /etc/systemd/system/bluetooth.service.d
cat > /etc/systemd/system/bluetooth.service.d/rpi-hub.conf <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/libexec/bluetooth/bluetoothd --noplugin=input,a2dp,avrcp,sap,midi
EOF

echo "==> stopping WirePlumber registering HFP with BlueZ"
mkdir -p /etc/wireplumber/wireplumber.conf.d
cat > /etc/wireplumber/wireplumber.conf.d/50-rpi-hub-no-bluez.conf <<'EOF'
# The Pi is a Bluetooth HID keyboard, not an audio device. WirePlumber otherwise
# registers HFP (UUIDs 111e/111f) with BlueZ as an external org.bluez.Profile1 --
# which --noplugin cannot suppress, because it is not a bluetoothd plugin -- and
# that alone puts the Audio+Telephony bits back into the Class-of-Device.
wireplumber.profiles = {
  main = {
    monitor.bluez = disabled
    monitor.bluez.seat-monitoring = disabled
  }
}
EOF
# WirePlumber runs per-user, so a system-wide config change needs each user's
# instance restarted. Missing users are fine -- this is best-effort.
for u in $(loginctl list-users --no-legend | awk '$1 >= 1000 {print $2}'); do
    systemctl --machine="$u@.host" --user restart wireplumber 2>/dev/null \
        && echo "    restarted wireplumber for $u" || true
done

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
install -m 644 "$REPO/setup/rpi-hub.service" /etc/systemd/system/rpi-hub.service

systemctl daemon-reload
systemctl restart bluetooth
sleep 2
systemctl enable --now rpi-hub

echo
echo "==> done. Verifying what the adapter actually advertises:"
# The UUID list is the check that matters -- main.conf's Class only sets the
# major/minor bits, and BlueZ derives the service bits from these. Anything
# audio-flavoured here (110a 110b 110c 110e 111e 111f 112d 03b8...) means a
# plugin or an external Profile1 slipped through, and macOS will treat the Pi
# as a speaker.
uuids="$(busctl get-property org.bluez /org/bluez/hci0 org.bluez.Adapter1 UUIDs)"
if grep -qE '0000(110a|110b|110c|110e|111e|111f|112d)|03b80e5a' <<<"$uuids"; then
    echo "    WARNING: the adapter still advertises an audio profile:" >&2
    grep -oE '"[0-9a-f]{8}-' <<<"$uuids" | tr -d '"-' | sort -u | tr '\n' ' ' >&2
    echo >&2
    echo "    macOS will file this Pi as an audio device. See the header." >&2
else
    echo "    no audio profiles advertised -- good"
fi
echo -n "    class: "; bluetoothctl show | awk '/Class/ {print $2}'
echo "    (want 0x002540; a different service byte means an audio UUID is back)"
echo
echo "Pair 'rpi-hub Keyboard' from the Mac, then type."
echo "Logs:    journalctl -u rpi-hub -f"
echo "Restore: $REPO/setup/uninstall.sh"

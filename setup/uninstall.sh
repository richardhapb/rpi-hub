#!/bin/bash
#
# Put the Pi's Bluetooth back the way it was before rpi-hub took it over.
# Run as root on the Pi.
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo "run as root" >&2
    exit 1
fi

BACKUP=/root/rpi-hub-backup

echo "==> stopping service"
systemctl disable --now rpi-hub 2>/dev/null || true
rm -f /etc/systemd/system/rpi-hub.service /usr/local/bin/rpi-hub

echo "==> restoring BlueZ"
rm -f /etc/systemd/system/bluetooth.service.d/rpi-hub.conf
rmdir --ignore-fail-on-non-empty /etc/systemd/system/bluetooth.service.d 2>/dev/null || true

if [[ -f "$BACKUP/main.conf.orig" ]]; then
    cp "$BACKUP/main.conf.orig" /etc/bluetooth/main.conf
    echo "    main.conf restored from $BACKUP"
else
    echo "    WARNING: no backup at $BACKUP -- main.conf left as-is" >&2
fi

systemctl daemon-reload
systemctl restart bluetooth
sleep 2

echo "==> done. Class of device:"
hciconfig hci0 class | grep -i "device class" || true

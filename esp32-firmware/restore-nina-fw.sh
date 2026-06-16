#!/bin/bash
# Restore the onboard ESP32 to its original nina-fw (the SPI WiFi firmware), from
# the backup taken before flashing esp-at. This brings back the SPI-based WiFi
# path (src/nina.rs + the camera web server at commit 3c8a278).
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/nina-fw-backup-4MB.bin"
[ -f "$BIN" ] || gunzip -k "$BIN.gz"
echo "Restoring nina-fw (4MB) to the ESP32..."
exec "$DIR/flash-esp32.sh" "$BIN" 0x0

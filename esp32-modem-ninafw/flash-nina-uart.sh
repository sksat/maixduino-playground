#!/bin/bash
# Flash the UART-modem nina-fw to the Maixduino's onboard ESP32.
#
# This is nina-fw (ESP-IDF v3.3) with its SPI transport replaced by a UART0 command
# protocol (see sketch.ino.cpp). It is the ONLY firmware that associates with this
# u-blox NINA-W102 module on the home AP -- generic arduino-esp32 (idf 4.4/5.5) fails
# at 802.11 assoc even with an identical minimal connect (see ../esp32-modem/README.md).
#
# The merged image (bootloader@0x1000 + partitions@0x8000 + app@0x30000) flashes at
# 0x0. Reuses the CH552 reset dance from ../esp32-firmware/flash-esp32.sh.
#
# Restore SPI nina-fw instead:  ../esp32-firmware/restore-nina-fw.sh
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
GZ="$DIR/nina-modem-merged.bin.gz"
BIN="/tmp/nina-modem-merged.bin"
gunzip -kc "$GZ" > "$BIN"
echo "Flashing UART-modem nina-fw (merged) to the ESP32..."
exec "$DIR/../esp32-firmware/flash-esp32.sh" "$BIN" 0x0

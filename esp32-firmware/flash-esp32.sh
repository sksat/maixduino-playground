#!/bin/bash
# Flash an image to the Maixduino's onboard ESP32 (over ttyUSB1).
#
# The board has no plain esptool path: the CH552 USB bridge uses an inverted
# DTR/RTS auto-reset, and the ESP32's EN must first be enabled by the K210. So:
#   1) boot_k210_enable.py  -- K210 drives EN high, then halts out of the way
#   2) enter_dl_robust.py   -- bang the lines until the ROM enters download mode
#   3) esptool with the custom (inverted) reset sequence
#
# Usage:  flash-esp32.sh <image.bin> [offset]      (offset default 0x0)
# Restore SPI/nina-fw:  restore-nina-fw.sh
set -euo pipefail
IMG="${1:?usage: flash-esp32.sh <image.bin> [offset]}"
OFFSET="${2:-0x0}"
DIR="$(cd "$(dirname "$0")" && pwd)"
export PATH="$HOME/.local/bin:$PATH"
SEQ="D1|R1|W0.1|R0|W0.1|D0|W0.05|R1|W0.05|D1"

sg uucp -c "
uv run python '$DIR/boot_k210_enable.py' && \
uv run python '$DIR/enter_dl_robust.py' && \
ESPTOOL_CUSTOM_RESET_SEQUENCE='$SEQ' uv run --with esptool esptool \
  --port /dev/ttyUSB1 --before default-reset --connect-attempts 8 \
  write-flash '$OFFSET' '$IMG'
"

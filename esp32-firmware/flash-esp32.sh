#!/bin/bash
# Flash an image to the Maixduino's onboard ESP32 (over ttyUSB1).
#
# The board has no plain esptool path: the CH552 USB bridge uses an inverted
# DTR/RTS auto-reset, and the ESP32's EN must first be enabled by the K210. So:
#   0) -hupcl on both ports -- CRITICAL: without it, every serial open/close toggles
#      the CH552's DTR/RTS->EN/GPIO0 lines and latches the bridge into a state where
#      download mode becomes unreachable (the recurring "FAILED to reach download
#      mode"). -hupcl + fuser -k makes flashing reliable on the first try.
#   1) boot_k210_enable.py  -- K210 drives EN high, then halts out of the way
#   2) enter_dl_robust.py   -- bang the lines until the ROM enters download mode
#   3) esptool with the custom (inverted) reset sequence, at 115200 (the CH552 bridge
#      is unreliable above that)
# AFTER flashing, the ESP32 may be stuck with GPIO0 held low (download); run
# hold_gpio0_high.py (or power-cycle) so the next K210 EN pulse boots the app.
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
fuser -k /dev/ttyUSB0 /dev/ttyUSB1 2>/dev/null; sleep 0.5
stty -F /dev/ttyUSB0 -hupcl; stty -F /dev/ttyUSB1 -hupcl
uv run python '$DIR/boot_k210_enable.py' && \
uv run python '$DIR/enter_dl_robust.py' && \
ESPTOOL_CUSTOM_RESET_SEQUENCE='$SEQ' uv run --with esptool esptool \
  --chip esp32 --port /dev/ttyUSB1 --baud 115200 --before default-reset --connect-attempts 8 \
  write-flash '$OFFSET' '$IMG'
"
sg uucp -c "stty -F /dev/ttyUSB0 hupcl 2>/dev/null" || true

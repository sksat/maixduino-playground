#!/usr/bin/env bash
# cargo runner for the Maixduino (K210): ELF -> raw .bin -> flash via kflash.
#
# kflash is pinned in-repo through uv (see pyproject.toml / uv.lock), so this
# uses `uv run kflash` rather than any globally-installed copy. Override the
# serial port / baud / board preset via env vars if your setup differs:
#
#   K210_PORT=/dev/ttyUSB0  K210_BAUD=1500000  K210_BOARD=maixduino  cargo run
#
set -euo pipefail

# Make sure uv (~/.local/bin) and rust-objcopy (~/.cargo/bin) are reachable no
# matter how cargo invoked us.
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

ELF="${1:?usage: flash.sh <path-to-elf>}"
BIN="${ELF}.bin"
PORT="${K210_PORT:-/dev/ttyUSB0}"
BAUD="${K210_BAUD:-1500000}"
BOARD="${K210_BOARD:-maixduino}"

# Flatten the ELF into the raw image the K210 boot ROM expects.
rust-objcopy -O binary "$ELF" "$BIN"

echo ">> flashing $BIN -> $PORT @ ${BAUD}bd (board=$BOARD)"
exec uv run kflash -p "$PORT" -b "$BAUD" -B "$BOARD" "$BIN"

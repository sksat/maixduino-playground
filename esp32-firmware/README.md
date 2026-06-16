# Onboard ESP32 firmware (Maixduino)

The Maixduino's WiFi is an **onboard ESP32-D0WDQ6 (4MB flash, MAC a4:cf:12:74:2d:8c)**.
By default it runs **nina-fw**, which talks to the K210 over **SPI0** — the same pins
the camera DVP uses. A camera capture wedges that SPI/network path, so the SPI design
needs a ~5s ESP32 reboot per frame (≈0.1 fps; see the main README, step 7).

To avoid that, we can run the ESP32 on **esp-at** and talk to it over **UART (IO6/IO7)**,
which is independent of the camera pins — no wedge, much higher frame rate.

## Preserving the SPI path (nina-fw)

`nina-fw-backup-4MB.bin.gz` is the **exact original nina-fw image** read back from this
board before any reflash. To return to the SPI WiFi path at any time:

```sh
esp32-firmware/restore-nina-fw.sh
```

The K210-side SPI driver (`src/nina.rs`) and the working nina-SPI camera web server
(commit `3c8a278`) remain in the repo, so SPI stays fully usable.

## Flashing the ESP32 (the board's quirks)

There is **no plain esptool path** on this board:
- The **CH552** USB bridge exposes the K210 on `ttyUSB0` and the **ESP32 on `ttyUSB1`**.
- Its auto-reset maps DTR/RTS to the ESP32 GPIO0/EN with **inverted polarity**, so
  esptool's built-in reset fails ("No serial data received").
- The ESP32's **EN is driven by K210 IO8**; with the K210 halted, EN drifts and the
  ESP32 goes silent. It must be enabled by briefly running the K210 first.
- The CH552 reset is **stateful/flaky** — entering download mode needs retries.
- Baud > 115200 over the CH552 bridge is unreliable; flash at **115200**.

`flash-esp32.sh <image.bin> [offset]` encapsulates the working recipe:
1. `boot_k210_enable.py` — boot the K210 so its `nina::init` drives EN high, then halt it.
2. `enter_dl_robust.py` — drive the raw lines until the ROM prints "waiting for download".
3. `esptool` with `ESPTOOL_CUSTOM_RESET_SEQUENCE='D1|R1|W0.1|R0|W0.1|D0|W0.05|R1|W0.05|D1'`.

Everything runs via `uv` (esptool is pulled ephemerally with `--with esptool`).

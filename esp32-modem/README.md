# ESP32 "WiFi modem" firmware (UART WiFi path)

A small Arduino-ESP32 firmware that replaces nina-fw(SPI) on the Maixduino's onboard
ESP32, so the K210 can do WiFi over **UART0 (GPIO1/3 ↔ K210 IO6/IO7)** — a path that
is independent of the camera's SPI0/DVP pads, so a camera capture no longer wedges
WiFi. The goal: kill the ~10s/frame recovery the SPI path needs (see the top README).

The K210 side is `src/uart_wifi.rs` + the bring-up test in `src/main.rs`.

## Protocol (K210 ↔ ESP32, over UART0 @ 921600)

Command (K210→ESP32): `<tag:1><len:2 LE><payload>`
Reply (ESP32→K210): `AA 55 <tag:1><len:2 LE><payload>` — the `AA55` sync prefix lets
the K210 resync past line noise (e.g. the UART restart around WiFi connect).

| cmd | payload | reply |
|-----|---------|-------|
| `P` ping | — | `O` |
| `C` connect | `ssid \0 pass` | `I`+ip[4], or `E`+diagnostics |
| `L` listen | port[2 LE] | `O` |
| `A` accept | — | `A`+connected(1) |
| `R` recv | — | `R`+bytes |
| `S` send | bytes | `S`+sent[2 LE] |
| `X` close | — | `O` |

Per-reply acks rate-limit the K210 to the ESP32's TCP write speed (UART flow control);
`WiFiClient.write()` blocks on the ESP32's own lwip, so no nina-style quiet-gap dance.

## Build & flash

System python 3.14 has no pip, so build via a seeded venv:
```sh
uv venv --seed --python 3.12 /tmp/piovenv && uv pip install --python /tmp/piovenv platformio
(cd esp32-modem && /tmp/piovenv/bin/pio run)
```
Flash the app with `../esp32-firmware/flash-esp32.sh .pio/build/esp32dev/firmware.bin 0x10000`
(it handles the `-hupcl` + CH552 download-mode dance). After flashing, run
`../esp32-firmware/hold_gpio0_high.py` while the K210 boots so the EN pulse boots the
app (the flash can leave GPIO0 latched low = download mode).

## STATUS: ping works, WiFi connect is BLOCKED

Proven: K210↔ESP32 UART link (ping 6/6 @ 921600), camera-independent. The UART link
and the whole modem protocol work.

**Open blocker — `WiFi.begin` never associates.** Connecting to the home WPA2-PSK AP
fails: `WiFi.status()` stays `WL_DISCONNECTED`, STA disconnect reason is consistently
**2 (WIFI_REASON_AUTH_EXPIRE)**, and the `STA_CONNECTED` (association) event NEVER
fires (`assoc=0`) — i.e. it fails at 802.11 auth/assoc, before the password matters.
Instrumented + verified: the AP is found in a scan (channel 6, RSSI −38 dBm, enc=3 =
plain WPA2-PSK), and the ssid/pass arrive byte-exact (checksum + lengths verified end
to end). The SAME ssid/pass + AP connect fine on this same ESP32 running nina-fw
(Adafruit NINA-W102, ESP-IDF v3.3); this firmware is arduino-esp32 2.0.17 / ESP-IDF
v4.4.

Tried and still reason-2 / assoc=0: forced channel+BSSID; 4 retries; country "JP";
`esp_wifi_set_max_tx_power(34)` (~8.5 dBm); `WiFi.setSleep(false)`; full chip erase +
fresh NVS; PMF disabled (`pmf_cfg.capable/required=false` + `threshold.authmode=
WPA2_PSK`); `esp_wifi_set_protocol(11b|11g)` (no 11n); `Serial.end()` during connect
(the "UART interferes with WiFi" theory).

RULED OUT: arduino-esp32 3.3.9 / ESP-IDF 5.5.4 fails IDENTICALLY (wl=6, reason 2,
assoc=0). So it is NOT an idf-version bug -- both 4.4 and 5.5 fail; only the
NINA-W102-specific nina-fw (idf 3.3) associates. The split is nina-fw vs GENERIC
arduino-esp32, which points at module-specific PHY/RF calibration data (u-blox
NINA-W102): generic PHY init data -> impaired management-frame TX -> the AP never
acks the auth frame (fits scan-works/RX but assoc-fails/TX). Codex also ruled out
the UART (GPIO1/3 are plain UART pins, not RF/strap).

Open paths (all deep): (a) extract/inject the NINA-W102 PHY init data; (b) add a
UART command layer to nina-fw itself (idf 3.3, proven WiFi) -- guaranteed assoc but
needs the idf-3.3 build; (c) a monitor-mode pcap on ch6 to confirm ESP-sends-auth /
AP-doesn't-ack; (d) accept the UART path can't do WiFi with generic fw and keep the
SPI camera server (~10s/frame, tag nina-spi-camera-webserver).

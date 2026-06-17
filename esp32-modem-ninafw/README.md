# UART-modem nina-fw (the WiFi-over-UART firmware that actually associates)

nina-fw (ESP-IDF v3.3) with its SPI (SPIS) transport swapped for a tiny **UART0**
command protocol, so the K210 drives WiFi over `GPIO1 TX / GPIO3 RX ↔ K210 IO6/IO7` —
independent of the camera's SPI0/DVP pads, so a capture no longer wedges WiFi.

## Why nina-fw and not a generic arduino-esp32 build

The whole reason this exists: **only nina-fw associates with this module.** The
onboard ESP32 is a u-blox NINA-W102. A generic arduino-esp32 modem (tried both
ESP-IDF 4.4 and 5.5) fails at 802.11 auth/assoc — `WiFi.status()=WL_DISCONNECTED`,
disconnect reason 2 (`AUTH_EXPIRE`), the `STA_CONNECTED` association event never
fires — and it fails *identically* even with a byte-for-byte nina-fw-equivalent
minimal connect (no forced channel/BSSID, no threshold, PMF off, default
protocol/country/TX-power). The scan finds the AP fine; only management-frame
TX/assoc fails. PHY init data is the same default 128-byte blob in both, so the
deciding factor is the idf-version WiFi/PHY binary blobs (libpp/libnet80211). nina-fw
(idf 3.3) is the only thing that connects, so we reuse its WiFi stack and only change
the transport. (The failed generic build is kept in `../esp32-modem/` as the negative
result.)

## What changed vs stock nina-fw

Only `main/sketch.ino.cpp` (this dir's copy) — the SPIS transport + Arduino-NINA
`CommandHandler` are replaced by `setup()`/`loop()` that speak the modem protocol over
UART0 using nina-fw's own WiFi library (`WiFi`/`WiFiServer`/`WiFiClient`).
`main/CommandHandler.cpp` is dropped from the build.

Protocol (matches the K210's unchanged `src/uart_wifi.rs`), framing
`<tag:1><len:2 LE><payload>`, replies prefixed `AA 55` for resync:

| cmd | payload | reply |
|-----|---------|-------|
| `P` ping | — | `O` |
| `C` connect | `ssid \0 pass` | `I`+ip[4], or `E`+status[1] |
| `L` listen | port[2 LE] | `O` |
| `A` accept/poll | — | `A`+connected[1] |
| `R` recv | — | `R`+bytes |
| `S` send | bytes | `S`+sent[2 LE] |
| `X` close | — | `O` |

Status: **works** — associates over UART, pulls DHCP (`IP 192.168.0.7`), ping 6/6.

## Build

ESP-IDF **v3.3** + its matching toolchain **xtensa-esp32-elf gcc 5.2.0**
(`1.22.0-80-g6c4433a`). The gcc-8.2.0/esp-2019r2 toolchain does NOT work with the
v3.3 *base* tag: v3.3 ships newlib-2.x headers (`components/newlib/include`) and the
8.2.0 toolchain is newlib-3.x, so C++ components (`cxx`, `asio`) fail with
`__result_use_check`/`_EXFUN`/`_PTR` header-skew errors. Use 5.2.0 (what v3.3's
get-started docs pin) — or move to idf v3.3.1+, which bumped both together.

```sh
export IDF_PATH=~/esp/esp-idf-v3.3
export PATH="$HOME/esp/xtensa-esp32-elf-520/bin:$HOME/esp/idf38/bin:$HOME/.local/bin:$PATH"
# copy this dir's sketch.ino.cpp over nina-fw's main/sketch.ino.cpp,
# and move main/CommandHandler.cpp out of the build, then:
make -j"$(nproc)"
```

## Flash

```sh
./flash-nina-uart.sh           # merged image (bootloader+partitions+app) at 0x0
```
Then `../esp32-firmware/hold_gpio0_high.py` while the K210 boots (the flash can leave
GPIO0 latched low = download mode). `nina-modem-merged.bin.gz` is the prebuilt image
so you can flash without rebuilding the whole idf tree.

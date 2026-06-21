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

Two changes vs stock nina-fw:
1. `main/sketch.ino.cpp` (this dir's copy) — the SPIS transport + Arduino-NINA
   `CommandHandler` are replaced by `setup()`/`loop()` that speak the modem protocol
   over UART0 (3 Mbaud) using nina-fw's own WiFi library. `main/CommandHandler.cpp` is
   dropped from the build.
2. `wificlient-blocking-write.patch` (apply to `arduino/libraries/WiFi/src/WiFiClient.cpp`)
   — stock `WiFiClient::write` does one `lwip_send_r(MSG_DONTWAIT)` and treats any
   `result<0` as fatal, so a full lwip send buffer (`EWOULDBLOCK`) **closes the socket
   mid-transfer**. Harmless for tiny SPI replies, but a fast producer streaming a 230 KB
   QVGA frame trips it -> truncated response. The patch makes `write` send all bytes,
   waiting on `select` (yields until writable) on backpressure and only closing on a
   real error. Required for QVGA-and-larger frames.

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

Status: **works** — associates over UART, pulls DHCP (`IP 192.168.0.7`), ping 6/6, and
serves a live QVGA (320×240) camera stream at ~1.0 s/frame (full 230 KB frames, 3 Mbaud).

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
# move main/CommandHandler.cpp out of the build,
# and apply the WiFiClient fix, then build:
git -C "$IDF_NINAFW" apply /path/to/esp32-modem-ninafw/wificlient-blocking-write.patch
make -j"$(nproc)"
```

## Flash

```sh
./flash-nina-uart.sh           # merged image (bootloader+partitions+app) at 0x0
```
Then `../esp32-firmware/hold_gpio0_high.py` while the K210 boots (the flash can leave
GPIO0 latched low = download mode). `nina-modem-merged.bin.gz` is the prebuilt image
so you can flash without rebuilding the whole idf tree.

## Reproducing the build (confirmed base: nina-fw 1.4.8)

The original build tree was cleaned up; to rebuild, the confirmed-good base is
**arduino/nina-fw tag `1.4.8`** (the last of the idf-3.3.1 line — tags 1.3.0–1.4.8 use
idf v3.3.x; 1.5.0+ moved to idf 4.4 which does NOT associate on this module). App
partition is `0x30000` (matches the prebuilt). Integration:

```sh
git clone https://github.com/arduino/nina-fw && cd nina-fw && git checkout 1.4.8
cp <repo>/esp32-modem-ninafw/sketch.ino.cpp main/sketch.ino.cpp
mv main/CommandHandler.cpp main/CommandHandler.cpp.disabled   # main/ auto-globs *.cpp
# 1.4.8's WiFiClient::write is a single MSG_PEEK send (the blocking-write.patch's MSG_DONTWAIT
# base does NOT apply) -> replace write() with a send-all loop that blocks on EWOULDBLOCK
# via lwip_select (same logic as the patch). Required for frames > the lwip send buffer.
export IDF_PATH=~/esp/esp-idf-v3.3
export PATH="$HOME/esp/xtensa-esp32-elf-520/bin:$HOME/esp/idf38/bin:$HOME/.local/bin:$PATH"
make -j$(nproc) && python combine.py /tmp/nina.bin
<repo>/esp32-firmware/flash-esp32.sh /tmp/nina.bin 0x0
```

## Throughput tuning — negative result

The stream/serve tops out at **~100 KB/s** (VGA ~2.5 fps, QVGA ~5.7 fps). Hypothesis: the
idf-3.3 lwip TCP send buffer/window (`CONFIG_TCP_SND_BUF_DEFAULT`=`TCP_WND_DEFAULT`=5744=4·MSS)
caps it. Tested by rebuilding with **TCP_SND_BUF/WND=28720 (20·MSS) + `TCP_NODELAY` on accept**.
**It did not help — it made throughput *worse* and unstable** (VGA 20–95 KB/s, QVGA single
fetches stalled ~5 s). So the bottleneck is the **ESP32's real WiFi TX rate** (PHY/signal/idf3.3
stack), not the TCP window; a bigger buffer just bufferbloats. Rolled back to the prebuilt. The
fps ceiling on this board is WiFi TX and is not breakable from the K210 side or via TCP config.

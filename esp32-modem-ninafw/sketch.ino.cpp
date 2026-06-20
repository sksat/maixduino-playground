// nina-fw, repurposed as a UART "WiFi modem" for the Maixduino's onboard ESP32.
//
// Stock nina-fw talks to the host over SPI (SPIS) and runs the Arduino NINA command
// set (CommandHandler). On the Maixduino the K210's SPI0/DVP pads are shared with the
// camera, so a camera capture wedges the SPI WiFi link. This build instead speaks a
// tiny command protocol over UART0 (GPIO1 TX / GPIO3 RX -> K210 IO6/IO7), which is
// independent of the camera -> no wedge.
//
// Crucially this reuses nina-fw's idf-3.3 WiFi stack, which is the ONLY firmware that
// associates with this u-blox NINA-W102 module + the home AP (generic arduino-esp32
// idf 4.4/5.5 fail at 802.11 auth/assoc -- reason 2, association never completes,
// even with a byte-for-byte identical minimal connect). So the host side
// (src/uart_wifi.rs) is unchanged; only the ESP32 transport moved from SPI to UART.
//
// Wire framing (both directions): <tag:1><len:2 LE><payload:len>
//   K210 -> ESP32:  'P' ping | 'C' connect ssid\0pass | 'L' listen port[2] |
//                   'A' accept/poll | 'R' recv | 'S' send bytes | 'X' close
//   ESP32 -> K210:  AA 55 <tag> <len:2 LE> <payload>   (AA55 = resync prefix)
//     'O' ok | 'I'+ip[4] | 'E'+status[1] | 'A'+connected[1] | 'R'+bytes | 'S'+sent[2]

#include <WiFi.h>
#include <WiFiClient.h>
#include <WiFiServer.h>
#include "Arduino.h"

extern "C" {
  #include "driver/uart.h"
  #include "esp_bt.h"
  #include "esp_log.h"
}

static const int      LINK_UART = UART_NUM_0;       // GPIO1/3 -> K210 IO6/IO7
static const uint32_t LINK_BAUD = 3000000; // 3 Mbaud (exact K210 divisor 4.0625). UART
                                           // is the image-transfer bottleneck, so this
                                           // ~halves QVGA frame time vs 1.5M. (The earlier
                                           // 3M truncation was the WiFiClient EWOULDBLOCK
                                           // bug, since fixed -- not a baud problem.)
static const uint16_t MAXPL     = 1600;

static WiFiServer server(80);
static WiFiClient client;

static void uartInit() {
  uart_config_t cfg;
  memset(&cfg, 0, sizeof(cfg));
  cfg.baud_rate = LINK_BAUD;
  cfg.data_bits = UART_DATA_8_BITS;
  cfg.parity    = UART_PARITY_DISABLE;
  cfg.stop_bits = UART_STOP_BITS_1;
  cfg.flow_ctrl = UART_HW_FLOWCTRL_DISABLE;
  uart_param_config((uart_port_t)LINK_UART, &cfg);
  // keep UART0's default pins (GPIO1 TX / GPIO3 RX)
  uart_driver_install((uart_port_t)LINK_UART, 8192, 0, 0, NULL, 0);
}

static inline void uartWrite(const uint8_t *b, size_t n) {
  uart_write_bytes((uart_port_t)LINK_UART, (const char *)b, n);
}

// Read exactly n bytes within timeout_ms total; false on timeout.
static bool uartReadN(uint8_t *buf, size_t n, int timeout_ms) {
  size_t got = 0;
  uint32_t start = millis();
  while (got < n) {
    int r = uart_read_bytes((uart_port_t)LINK_UART, buf + got, n - got,
                            10 / portTICK_PERIOD_MS);
    if (r > 0) got += r;
    else if ((millis() - start) > (uint32_t)timeout_ms) return false;
  }
  return true;
}

// Reply frame with AA55 resync prefix.
static void sendFrame(uint8_t tag, const uint8_t *payload, uint16_t len) {
  uint8_t hdr[5] = {0xAA, 0x55, tag, (uint8_t)(len & 0xff), (uint8_t)(len >> 8)};
  uartWrite(hdr, 5);
  if (len) uartWrite(payload, len);
}

void setup() {
  esp_log_level_set("*", ESP_LOG_NONE);     // keep IDF logs off UART0 (our link)
  esp_bt_controller_mem_release(ESP_BT_MODE_BTDM); // reclaim BT heap (WiFi-only)
  WiFi.status();                            // force WiFi-stack lazy init
  uartInit();
  delay(50);
  uartWrite((const uint8_t *)"\xAA\x55MDM1\n", 7); // ready marker (host resyncs on it)
}

void loop() {
  uint8_t cmd;
  if (uart_read_bytes((uart_port_t)LINK_UART, &cmd, 1, 20 / portTICK_PERIOD_MS) != 1)
    return;

  uint8_t lh[2];
  if (!uartReadN(lh, 2, 1000)) return;
  uint16_t len = lh[0] | (lh[1] << 8);

  static uint8_t pl[MAXPL];
  if (len > MAXPL) {                        // oversized: drain + reject
    uint8_t junk[64];
    uint16_t rem = len;
    while (rem) { uint16_t c = rem > 64 ? 64 : rem; if (!uartReadN(junk, c, 1000)) break; rem -= c; }
    sendFrame('E', NULL, 0);
    return;
  }
  if (len && !uartReadN(pl, len, 2000)) return;

  switch (cmd) {
    case 'P':
      sendFrame('O', NULL, 0);
      break;

    case 'C': {                             // ssid '\0' pass
      int z = -1;
      for (uint16_t i = 0; i < len; i++) if (pl[i] == 0) { z = i; break; }
      if (z < 0) { sendFrame('E', NULL, 0); break; }
      char ssid[33], pass[64];
      uint16_t sl = z < 32 ? z : 32;
      memcpy(ssid, pl, sl); ssid[sl] = 0;
      uint16_t off = z + 1;
      uint16_t pn = len - off; if (pn > 63) pn = 63;
      memcpy(pass, pl + off, pn); pass[pn] = 0;

      WiFi.disconnect();
      delay(50);
      WiFi.begin(ssid, pass);
      uint32_t t = millis();
      while (WiFi.status() != WL_CONNECTED && (millis() - t) < 20000) delay(100);

      if (WiFi.status() == WL_CONNECTED) {
        uint32_t ip = WiFi.localIP();       // octet0 in the low byte (like IPAddress[])
        uint8_t b[4] = {(uint8_t)ip, (uint8_t)(ip >> 8), (uint8_t)(ip >> 16), (uint8_t)(ip >> 24)};
        sendFrame('I', b, 4);
      } else {
        uint8_t st = WiFi.status();
        sendFrame('E', &st, 1);
      }
      break;
    }

    case 'L': {                             // listen on port
      uint16_t port = len >= 2 ? (pl[0] | (pl[1] << 8)) : 80;
      server = WiFiServer(port);
      server.begin();
      sendFrame('O', NULL, 0);
      break;
    }

    case 'A': {                             // accept / poll for a client
      if (!client.connected()) client = server.available();
      uint8_t c = client.connected() ? 1 : 0;
      sendFrame('A', &c, 1);
      break;
    }

    case 'R': {                             // drain available request bytes
      static uint8_t rb[1024];
      int n = 0;
      while (client.connected() && client.available() && n < (int)sizeof(rb)) {
        int ch = client.read();
        if (ch < 0) break;
        rb[n++] = (uint8_t)ch;
      }
      sendFrame('R', rb, (uint16_t)n);
      break;
    }

    case 'S': {                             // send payload to the client
      size_t sent = 0;
      if (client.connected()) sent = client.write(pl, len);
      uint8_t b[2] = {(uint8_t)(sent & 0xff), (uint8_t)(sent >> 8)};
      sendFrame('S', b, 2);
      break;
    }

    case 'B': {                             // RGB565 payload -> expand to BGR24, send
      // The K210 sends 2 bytes/px (RGB565 LE) instead of inflating to 3 -- 33% less
      // UART, which is the bottleneck. We expand here (WiFi side has headroom) so the
      // browser still gets a normal 24-bit BMP. Reply = SOURCE (RGB565) bytes consumed.
      static uint8_t ob[MAXPL / 2 * 3];
      uint16_t np = len / 2;                // pixels in this chunk
      uint16_t k = 0;
      for (uint16_t i = 0; i < np; i++) {
        uint16_t p = pl[2 * i] | (pl[2 * i + 1] << 8);
        ob[k++] = (uint8_t)((p & 0x1f) << 3);        // B
        ob[k++] = (uint8_t)(((p >> 5) & 0x3f) << 2); // G
        ob[k++] = (uint8_t)(((p >> 11) & 0x1f) << 3);// R
      }
      size_t w = 0;
      if (client.connected()) w = client.write(ob, k); // blocks until lwip accepts all
      // client.write returns bytes written (k on success); map back to source bytes.
      uint16_t consumed = (w >= k) ? (uint16_t)(np * 2) : (uint16_t)((w / 3) * 2);
      uint8_t b[2] = {(uint8_t)(consumed & 0xff), (uint8_t)(consumed >> 8)};
      sendFrame('S', b, 2);
      break;
    }

    case 'X':                               // close the client
      client.flush();
      client.stop();
      sendFrame('O', NULL, 0);
      break;

    default:
      sendFrame('E', NULL, 0);
      break;
  }
}

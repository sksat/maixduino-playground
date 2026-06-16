// ESP32 "WiFi modem" for the Maixduino onboard ESP32.
//
// The K210 is the brain; this firmware is a thin WiFi/TCP peripheral driven over
// UART0 (GPIO1 TX / GPIO3 RX -> K210 IO6/IO7). That UART is independent of the
// camera's DVP/SPI0 pads, so a camera capture no longer wedges WiFi -> the K210 can
// capture and serve every request live.
//
// Wire framing (both directions): <tag:1><len:2 LE><payload:len>
// Commands (K210 -> ESP32):
//   'P' ping                      -> 'O'
//   'C' connect  ssid '\0' pass   -> 'I' + ip[4]   (or 'E' on failure/timeout)
//   'L' listen   port[2 LE]       -> 'O'
//   'A' accept/poll               -> 'A' + connected(1)
//   'R' recv (drain client RX)    -> 'R' + bytes
//   'S' send     bytes            -> 'S' + sent[2 LE]
//   'X' close client              -> 'O'
//
// Per-frame replies rate-limit the K210 to the ESP32's TCP write speed (UART flow
// control), and WiFiClient.write() blocks until lwip accepts the data -- the ESP32's
// stack runs freely, so no nina-style buffer-abort / quiet-gap dance is needed.

#include <Arduino.h>
#include <WiFi.h>

static const uint32_t LINK_BAUD = 921600;
static const uint16_t MAXPL = 1600;

WiFiServer server(80);
WiFiClient client;

static bool readN(uint8_t *buf, size_t n) {
  return Serial.readBytes(buf, n) == n;
}

static void sendFrame(uint8_t tag, const uint8_t *payload, uint16_t len) {
  uint8_t hdr[3] = {tag, (uint8_t)(len & 0xff), (uint8_t)(len >> 8)};
  Serial.write(hdr, 3);
  if (len) Serial.write(payload, len);
  Serial.flush();
}

void setup() {
  Serial.setRxBufferSize(4096);
  Serial.begin(LINK_BAUD);
  Serial.setTimeout(3000);
  WiFi.persistent(false);
  WiFi.mode(WIFI_STA);
  delay(50);
  // ready marker so the K210 can sync past the ROM boot noise
  Serial.write((const uint8_t *)"\xAA\x55MDM1\n", 7);
  Serial.flush();
}

void loop() {
  if (!Serial.available()) return;
  uint8_t cmd = Serial.read();

  uint8_t lh[2];
  if (!readN(lh, 2)) return;
  uint16_t len = lh[0] | (lh[1] << 8);

  static uint8_t pl[MAXPL];
  if (len > MAXPL) { // oversized: drain and reject
    uint8_t junk[64];
    while (len) { uint16_t c = len > 64 ? 64 : len; if (Serial.readBytes(junk, c) != c) break; len -= c; }
    sendFrame('E', nullptr, 0);
    return;
  }
  if (len && !readN(pl, len)) return;

  switch (cmd) {
    case 'P':
      sendFrame('O', nullptr, 0);
      break;

    case 'C': { // ssid '\0' pass
      int z = -1;
      for (uint16_t i = 0; i < len; i++) if (pl[i] == 0) { z = i; break; }
      if (z < 0) { sendFrame('E', nullptr, 0); break; }
      char ssid[64], pass[80];
      uint16_t sl = z < 63 ? z : 63;
      memcpy(ssid, pl, sl); ssid[sl] = 0;
      uint16_t pl_off = z + 1;
      uint16_t pn = len - pl_off; if (pn > 79) pn = 79;
      memcpy(pass, pl + pl_off, pn); pass[pn] = 0;
      WiFi.disconnect();
      WiFi.begin(ssid, pass);
      uint32_t t = millis();
      while (WiFi.status() != WL_CONNECTED && millis() - t < 15000) delay(100);
      if (WiFi.status() == WL_CONNECTED) {
        IPAddress ip = WiFi.localIP();
        uint8_t b[4] = {ip[0], ip[1], ip[2], ip[3]};
        sendFrame('I', b, 4);
      } else {
        sendFrame('E', nullptr, 0);
      }
      break;
    }

    case 'L': { // port
      uint16_t port = len >= 2 ? (pl[0] | (pl[1] << 8)) : 80;
      server.begin(port);
      server.setNoDelay(true);
      sendFrame('O', nullptr, 0);
      break;
    }

    case 'A': { // accept / poll for a client
      if (!client || !client.connected()) {
        client = server.available();
      }
      uint8_t c = (client && client.connected()) ? 1 : 0;
      sendFrame('A', &c, 1);
      break;
    }

    case 'R': { // drain available request bytes from the client
      static uint8_t rb[1024];
      uint16_t n = 0;
      while (client && client.available() && n < sizeof(rb)) rb[n++] = client.read();
      sendFrame('R', rb, n);
      break;
    }

    case 'S': { // send payload to the client (blocking until lwip accepts it)
      uint16_t sent = 0;
      if (client && client.connected()) sent = client.write(pl, len);
      uint8_t b[2] = {(uint8_t)(sent & 0xff), (uint8_t)(sent >> 8)};
      sendFrame('S', b, 2);
      break;
    }

    case 'X': // close the client
      if (client) client.stop();
      sendFrame('O', nullptr, 0);
      break;

    default:
      sendFrame('E', nullptr, 0);
      break;
  }
}

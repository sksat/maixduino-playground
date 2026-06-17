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
#include "esp_wifi.h"

static const uint32_t LINK_BAUD = 921600;
static const uint16_t MAXPL = 1600;

WiFiServer server(80);
WiFiClient client;

static volatile uint8_t lastDiscReason = 0; // last STA disconnect reason code
static volatile uint8_t gotAssoc = 0;       // did STA_CONNECTED (association) fire?

static void onWiFiEvent(WiFiEvent_t event, WiFiEventInfo_t info) {
  if (event == ARDUINO_EVENT_WIFI_STA_CONNECTED) gotAssoc = 1;
  if (event == ARDUINO_EVENT_WIFI_STA_DISCONNECTED)
    lastDiscReason = info.wifi_sta_disconnected.reason;
}

static bool readN(uint8_t *buf, size_t n) {
  return Serial.readBytes(buf, n) == n;
}

// Reply frame: AA 55 <tag> <len:2 LE> <payload>. The AA55 sync prefix lets the K210
// resync past line noise (e.g. the UART restart around WiFi connect).
static void sendFrame(uint8_t tag, const uint8_t *payload, uint16_t len) {
  uint8_t hdr[5] = {0xAA, 0x55, tag, (uint8_t)(len & 0xff), (uint8_t)(len >> 8)};
  Serial.write(hdr, 5);
  if (len) Serial.write(payload, len);
  Serial.flush();
}

void setup() {
  Serial.setRxBufferSize(4096);
  Serial.begin(LINK_BAUD);
  Serial.setTimeout(3000);
  Serial.setDebugOutput(false); // keep IDF logs off UART0 (it's our protocol link)
  WiFi.persistent(false);
  WiFi.onEvent(onWiFiEvent);
  WiFi.mode(WIFI_STA);
  WiFi.setSleep(false);
  esp_wifi_set_ps(WIFI_PS_NONE);
  // NOTE: deliberately NO country/protocol/tx-power overrides here. The earlier
  // build forced ch1-13 country, 11b/g-only (no 11n) and a low TX cap trying to fix
  // a reason-2 AUTH_EXPIRE -- but those are exactly the deltas vs nina-fw (idf 3.3),
  // which associates with NONE of them. This build tests the minimal nina-equivalent
  // path: stock defaults (11bgn, world country, default TX) + a plain WiFi.begin.
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
      lastDiscReason = 0; gotAssoc = 0;
      // Quiet UART0 (the K210 link) during connect; the K210 just waits for the reply.
      Serial.flush();
      Serial.end();
      delay(20);
      // RAW esp_wifi connect, byte-for-byte nina-fw's config (idf 3.3) -- but with PMF
      // EXPLICITLY disabled. arduino-3.x's WiFi.begin defaults pmf_cfg.capable=true and
      // threshold.authmode=WPA2_PSK; nina-fw (idf 3.3) has NO PMF at all. This is the
      // last behavioral delta left after the minimal WiFi.begin test still gave
      // reason-2 AUTH_EXPIRE. WiFi.mode(STA) in setup() already brought up
      // netif+event+esp_wifi_start, so we drive esp_wifi directly here.
      esp_wifi_disconnect();
      delay(100);
      {
        wifi_config_t cfg;
        memset(&cfg, 0, sizeof(cfg));
        strncpy((char *)cfg.sta.ssid, ssid, sizeof(cfg.sta.ssid));
        strncpy((char *)cfg.sta.password, pass, sizeof(cfg.sta.password));
        cfg.sta.scan_method = WIFI_ALL_CHANNEL_SCAN;
        cfg.sta.threshold.authmode = WIFI_AUTH_OPEN; // 0 = no minimum (like nina-fw)
        cfg.sta.pmf_cfg.capable = false;             // <-- the delta vs arduino default
        cfg.sta.pmf_cfg.required = false;
        esp_wifi_set_config(WIFI_IF_STA, &cfg);
      }
      esp_wifi_set_ps(WIFI_PS_NONE);
      esp_wifi_connect();
      uint32_t t = millis();
      while (WiFi.status() != WL_CONNECTED && millis() - t < 20000) delay(100);
      // Only if it FAILED, scan to fill the diagnostic fields (post-connect, so the
      // scan can't perturb the connect attempt itself).
      bool seen = false; int32_t ch = 0, rssi = 0; uint8_t enc = 255;
      if (WiFi.status() != WL_CONNECTED) {
        int found = WiFi.scanNetworks(false, true);
        for (int i = 0; i < found; i++) {
          if (WiFi.SSID(i) == String(ssid)) {
            seen = true; ch = WiFi.channel(i); rssi = WiFi.RSSI(i);
            enc = (uint8_t)WiFi.encryptionType(i);
            break;
          }
        }
        WiFi.scanDelete();
      }
      // bring UART0 back up to talk to the K210
      Serial.setRxBufferSize(4096);
      Serial.begin(LINK_BAUD);
      Serial.setDebugOutput(false);
      delay(30);
      if (WiFi.status() == WL_CONNECTED) {
        IPAddress ip = WiFi.localIP();
        uint8_t b[4] = {ip[0], ip[1], ip[2], ip[3]};
        sendFrame('I', b, 4);
      } else {
        // status + payload checksum + parsed lengths + the disconnect REASON code
        // (15=4way-handshake-timeout=wrong pass, 201=no-AP, 2/15/204=auth, ...).
        uint16_t cs = 0;
        for (uint16_t i = 0; i < len; i++) cs += pl[i];
        uint8_t r[11] = {(uint8_t)WiFi.status(), (uint8_t)(cs & 0xff),
                         (uint8_t)(cs >> 8), (uint8_t)sl, (uint8_t)pn, lastDiscReason,
                         (uint8_t)(seen ? 1 : 0), (uint8_t)ch, (uint8_t)(int8_t)rssi, enc,
                         gotAssoc};
        sendFrame('E', r, 11);
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

    case 'X': // flush + close the client (the S ack only means lwip accepted it)
      if (client) {
        client.flush();
        client.stop();
      }
      sendFrame('O', nullptr, 0);
      break;

    default:
      sendFrame('E', nullptr, 0);
      break;
  }
}

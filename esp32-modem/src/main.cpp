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
  // allow ch 1-13 (Japan/world); the default US region (ch 1-11) can't associate to
  // an AP on ch 12/13 even though the scan finds it.
  esp_wifi_set_country_code("JP", true);
  // Force legacy 11b/g (no 11n/HT). idf 4.4 advertises HT caps that some routers
  // reject during association -> reason-2 AUTH_EXPIRE with assoc never completing,
  // while nina-fw (idf 3.3) associates fine.
  esp_wifi_set_protocol(WIFI_IF_STA, WIFI_PROTOCOL_11B | WIFI_PROTOCOL_11G);
  esp_wifi_set_max_tx_power(34); // low TX -- avoid RF saturation at the very close AP
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
      WiFi.setAutoReconnect(true);
      // The live UART0 peripheral interferes with WiFi association on the ESP32 (a
      // documented cause of reason-2 AUTH_EXPIRE: "ESP32 + hardware UART = auth
      // expired"). Our UART0 is the K210 link -- so QUIET it during scan+connect, then
      // bring it back up to reply. The K210 is just waiting for the reply meanwhile.
      Serial.flush();
      Serial.end();
      delay(20);
      // Scan for the target AP to learn its channel/BSSID/RSSI, then connect forcing
      // that exact channel+BSSID.
      bool seen = false; int32_t ch = 0, rssi = 0; uint8_t bssid[6] = {0}; uint8_t enc = 255;
      int found = WiFi.scanNetworks(false, true);
      for (int i = 0; i < found; i++) {
        if (WiFi.SSID(i) == String(ssid)) {
          seen = true; ch = WiFi.channel(i); rssi = WiFi.RSSI(i);
          enc = (uint8_t)WiFi.encryptionType(i); // 3=WPA2_PSK 6=WPA3 7=WPA2/WPA3-mixed
          memcpy(bssid, WiFi.BSSID(i), 6);
          break;
        }
      }
      WiFi.scanDelete();
      for (int tryn = 0; tryn < 4 && WiFi.status() != WL_CONNECTED; tryn++) {
        WiFi.disconnect(true, true);
        delay(200);
        esp_wifi_set_max_tx_power(34); // low TX -- avoid RF saturation at -37 dBm
        // Populate Arduino's STA config without connecting, then DISABLE PMF: arduino-
        // esp32 2.0.17 sets pmf_cfg.capable=true, and a router with broken optional-PMF
        // handling never completes auth (reason-2 AUTH_EXPIRE) -- nina-fw (idf 3.3, no
        // PMF) connects fine. Force plain WPA2-PSK to the exact AP, then connect.
        if (seen) WiFi.begin(ssid, pass, ch, bssid, false);
        else WiFi.begin(ssid, pass, 0, nullptr, false);
        wifi_config_t cfg = {};
        esp_wifi_get_config(WIFI_IF_STA, &cfg);
        cfg.sta.pmf_cfg.capable = false;
        cfg.sta.pmf_cfg.required = false;
        cfg.sta.threshold.authmode = WIFI_AUTH_WPA2_PSK;
        cfg.sta.scan_method = WIFI_FAST_SCAN;
        if (seen) {
          cfg.sta.channel = ch;
          cfg.sta.bssid_set = true;
          memcpy(cfg.sta.bssid, bssid, 6);
        }
        esp_wifi_set_config(WIFI_IF_STA, &cfg);
        esp_wifi_connect();
        uint32_t t = millis();
        while (WiFi.status() != WL_CONNECTED && millis() - t < 5000) delay(100);
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

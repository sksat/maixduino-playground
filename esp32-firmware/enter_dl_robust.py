"""Put the onboard ESP32 into UART download mode over ttyUSB1.

The Maixduino's CH552 USB bridge maps DTR/RTS to the ESP32's GPIO0/EN with
NON-STANDARD (inverted) polarity, so esptool's built-in reset doesn't work. This
drives the raw lines in the sequence that latches download mode, and -- because
the CH552 reset is stateful/flaky -- retries until the ROM prints the
"waiting for download" banner. After this, run esptool with --before no-reset, or
with ESPTOOL_CUSTOM_RESET_SEQUENCE='D1|R1|W0.1|R0|W0.1|D0|W0.05|R1|W0.05|D1'
--before default-reset (which re-enters download once the chip is already there).
"""
import serial, time, sys
PORT = "/dev/ttyUSB1"

def attempt():
    s = serial.Serial(PORT, 115200, timeout=0.2)
    # (DTR,RTS): (T,T) -> (T,F) -> (F,F) -> (F,T) -> (T,T)
    s.dtr = True;  s.rts = True;  time.sleep(0.1)
    s.rts = False;                time.sleep(0.1)
    s.dtr = False;                time.sleep(0.05)
    s.rts = True;                 time.sleep(0.05)
    s.dtr = True
    buf = b""; t = time.monotonic()
    while time.monotonic() - t < 1.0:
        d = s.read(256)
        if d: buf += d
    s.close()
    return (b"DOWNLOAD" in buf or b"waiting for download" in buf)

for i in range(30):
    if attempt():
        print("download mode reached on attempt %d" % i)
        sys.exit(0)
    time.sleep(0.25)
print("FAILED to reach download mode")
sys.exit(1)

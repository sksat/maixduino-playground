"""Enable the onboard ESP32 for flashing, then get out of the way.

The ESP32's EN is driven by K210 IO8; with the K210 halted, EN drifts and the
ESP32 won't respond on ttyUSB1. So briefly boot the K210 (its nina::init drives
EN high + resets the ESP32 into a known state), then halt the K210 (closing the
port deasserts RTS -> K210 held in reset -> IO8 hi-Z -> EN held high by the board
pull-up) so the CH552 can drive the download-mode reset.

Run this right before entering download mode / flashing the ESP32.
"""
import serial, time

s = serial.Serial("/dev/ttyUSB0", 115200, timeout=0.1)
s.dtr = True; s.rts = False; time.sleep(0.3); s.close(); time.sleep(0.2)
s = serial.Serial("/dev/ttyUSB0", 115200, timeout=0.1)
s.dtr = False; s.rts = True   # K210 runs
buf = b""; t = time.monotonic()
while time.monotonic() - t < 11:
    d = s.read(256)
    if d: buf += d
print("K210 ran, ESP32 enabled:", any(k in buf for k in (b"connecting", b"server up", b"camera")))
s.close()  # halt K210 -> EN floats high, ESP32 stays enabled
time.sleep(0.3)

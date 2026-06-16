"""Put the onboard ESP32 into UART download mode over ttyUSB1.

The Maixduino's CH552 USB bridge maps DTR/RTS to the ESP32's GPIO0/EN with a
non-standard polarity, AND the reset is stateful/flaky -- the combination that
works drifts. So this tries every (which-line-is-GPIO0 x polarity) variant of the
classic ESP reset, repeatedly, until the ROM prints "waiting for download".

After this, esptool connects with ESPTOOL_CUSTOM_RESET_SEQUENCE=
'D1|R1|W0.1|R0|W0.1|D0|W0.05|R1|W0.05|D1' --before default-reset (re-enters
download once the chip is already there).
"""
import serial, time, sys
PORT = "/dev/ttyUSB1"

def seq(io0_is_dtr, inv):
    s = serial.Serial(PORT, 115200, timeout=0.2)
    def setline(which, level):
        line = ('dtr' if io0_is_dtr else 'rts') if which == 'io0' else ('rts' if io0_is_dtr else 'dtr')
        setattr(s, line, (not level) if inv else level)
    setline('io0', False); setline('en', False); time.sleep(0.1)
    setline('en', True);  time.sleep(0.1)
    setline('io0', True); time.sleep(0.05)
    setline('en', False); time.sleep(0.05)
    setline('io0', False)
    buf = b""; t = time.monotonic()
    while time.monotonic() - t < 0.9:
        d = s.read(256)
        if d: buf += d
    s.close()
    return (b"DOWNLOAD" in buf or b"waiting for download" in buf)

variants = [(True, True), (True, False), (False, True), (False, False)]
for rnd in range(12):
    for io0_is_dtr, inv in variants:
        if seq(io0_is_dtr, inv):
            print("download mode reached (round %d, io0_is_dtr=%s inv=%s)" % (rnd, io0_is_dtr, inv))
            sys.exit(0)
        time.sleep(0.1)
print("FAILED to reach download mode")
sys.exit(1)

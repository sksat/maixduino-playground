import serial, time, sys
# Hold ESP32 GPIO0 HIGH (dtr=False) and release EN to the K210 (rts=True) so the
# K210's EN pulse boots the app (not download mode). Clears the CH552 latch.
s = serial.Serial("/dev/ttyUSB1", 115200, timeout=0.1)
s.dtr = False; s.rts = True
time.sleep(int(sys.argv[1]) if len(sys.argv) > 1 else 30)
s.close()

#!/usr/bin/env python3
"""Record the Maixduino camera serial stream to an mp4 (via ffmpeg).

Reads frames for a few seconds, measures the real frame rate, and encodes an
mp4 that plays back at true speed. Small resolutions stream fast enough to look
like (choppy) video over the 1.5 Mbaud link:
    QQVGA 160x120 ~3 fps,  QVGA 320x240 ~0.8 fps,  VGA 640x480 ~0.2 fps.

    uv run python tools/stream.py --res qqvga --seconds 8 --out captures/stream.mp4
"""
import argparse
import re
import subprocess
import sys
import time

import numpy as np
import serial

sys.path.insert(0, __import__("os").path.dirname(__file__))
from grab import RES_CMD, unpack_rgb565  # noqa: E402


def stream_frames(s, seconds):
    """Yield (timestamp, rgb_uint8 HxWx3) for `seconds`, syncing on IMGSTART."""
    header = re.compile(rb"IMGSTART (\d+) (\d+)\n")
    buf = b""
    w = h = need = None
    t0 = time.monotonic()
    while time.monotonic() - t0 < seconds:
        if need is None:
            buf += s.read(8192)
            m = header.search(buf)
            if not m:
                buf = buf[-32:]
                continue
            w, h = int(m.group(1)), int(m.group(2))
            need = w * h * 2
            buf = buf[m.end():]
        while len(buf) < need and time.monotonic() - t0 < seconds + 2:
            buf += s.read(need - len(buf))
        if len(buf) >= need:
            yield time.monotonic(), unpack_rgb565(buf[:need], w, h)
            buf = buf[need:]
            need = None


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", default="/dev/ttyUSB0")
    ap.add_argument("--baud", type=int, default=1_500_000)
    ap.add_argument("--res", choices=list(RES_CMD), help="switch resolution first")
    ap.add_argument("--seconds", type=float, default=8.0)
    ap.add_argument("--out", default="captures/stream.mp4")
    ap.add_argument("--scale", type=int, default=3, help="nearest-neighbour upscale for visibility")
    args = ap.parse_args()

    s = serial.Serial(args.port, args.baud, timeout=5)
    if args.res:
        s.write(RES_CMD[args.res])
        s.flush()
        time.sleep(0.5)
    s.reset_input_buffer()

    frames, ts = [], []
    for t, rgb in stream_frames(s, args.seconds):
        frames.append(rgb)
        ts.append(t)
        print("frame %d (%dx%d)" % (len(frames), rgb.shape[1], rgb.shape[0]), end="\r", flush=True)
    s.close()
    print()
    if len(frames) < 2:
        print("not enough frames captured (%d)" % len(frames))
        sys.exit(1)

    fps = (len(ts) - 1) / (ts[-1] - ts[0])
    h, w = frames[0].shape[:2]
    print("got %d frames %dx%d over %.1fs -> %.2f fps" % (len(frames), w, h, ts[-1] - ts[0], fps))

    ow, oh = w * args.scale, h * args.scale
    cmd = [
        "ffmpeg", "-y", "-loglevel", "error",
        "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", "%dx%d" % (w, h), "-r", "%.4f" % fps,
        "-i", "-",
        "-vf", "scale=%d:%d:flags=neighbor" % (ow, oh),
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-an", args.out,
    ]
    p = subprocess.Popen(cmd, stdin=subprocess.PIPE)
    for rgb in frames:
        # frames may differ in size only if the resolution changed mid-capture;
        # keep just the ones matching the first frame's geometry.
        if rgb.shape[:2] == (h, w):
            p.stdin.write(np.ascontiguousarray(rgb).tobytes())
    p.stdin.close()
    if p.wait() != 0:
        print("ffmpeg failed")
        sys.exit(1)
    print("wrote", args.out, "(%.2f fps, %dx%d upscaled to %dx%d)" % (fps, w, h, ow, oh))


if __name__ == "__main__":
    main()

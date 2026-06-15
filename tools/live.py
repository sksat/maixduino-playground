#!/usr/bin/env python3
"""Live-view the Maixduino camera serial stream in an ffplay window.

Parses the IMGSTART framing off the serial stream and pipes clean RGB24 frames
to ffplay (which needs a display, so run this on your desktop, not headless).

    uv run python tools/live.py --res qqvga
    uv run python tools/live.py --res qvga --scale 2

Use --sink null to test the data path without a display (encodes to /dev/null).
"""
import argparse
import subprocess
import sys
import time

import numpy as np
import serial

sys.path.insert(0, __import__("os").path.dirname(__file__))
from grab import RES_CMD, unpack_rgb565  # noqa: E402
from stream import stream_frames  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", default="/dev/ttyUSB0")
    ap.add_argument("--baud", type=int, default=1_500_000)
    ap.add_argument("--res", choices=list(RES_CMD), help="switch resolution first")
    ap.add_argument("--scale", type=int, default=3, help="nearest-neighbour upscale")
    ap.add_argument("--sink", choices=["ffplay", "null"], default="ffplay")
    ap.add_argument("--seconds", type=float, default=1e9, help="stop after N seconds")
    args = ap.parse_args()

    s = serial.Serial(args.port, args.baud, timeout=5)
    if args.res:
        s.write(RES_CMD[args.res])
        s.flush()
        time.sleep(0.5)
    s.reset_input_buffer()

    # We don't know the size until the first frame; peek one to size the sink.
    gen = stream_frames(s, args.seconds)
    try:
        t0, first = next(gen)
    except StopIteration:
        print("no frames")
        sys.exit(1)
    h, w = first.shape[:2]
    ow, oh = w * args.scale, h * args.scale
    print("streaming %dx%d -> %dx%d (sink=%s, Ctrl-C to stop)" % (w, h, ow, oh, args.sink))

    if args.sink == "ffplay":
        cmd = ["ffplay", "-loglevel", "error", "-f", "rawvideo", "-pixel_format", "rgb24",
               "-video_size", "%dx%d" % (w, h), "-vf", "scale=%d:%d:flags=neighbor" % (ow, oh),
               "-i", "-"]
    else:  # null: prove the pipe/data path with no display
        cmd = ["ffmpeg", "-loglevel", "error", "-f", "rawvideo", "-pixel_format", "rgb24",
               "-video_size", "%dx%d" % (w, h), "-i", "-", "-f", "null", "-"]
    p = subprocess.Popen(cmd, stdin=subprocess.PIPE)

    n = 0
    try:
        for _, rgb in [(t0, first)] + list(gen):
            if rgb.shape[:2] != (h, w):
                continue  # skip a frame from a different resolution
            p.stdin.write(np.ascontiguousarray(rgb).tobytes())
            n += 1
            print("frame %d" % n, end="\r", flush=True)
    except (KeyboardInterrupt, BrokenPipeError):
        pass
    finally:
        s.close()
        try:
            p.stdin.close()
        except BrokenPipeError:
            pass
        p.wait()
    print("\nstopped after %d frames" % n)


if __name__ == "__main__":
    main()

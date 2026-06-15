#!/usr/bin/env python3
"""Grab RGB565 frames the Maixduino camera demo dumps over serial, save a PNG.

Wire format (see src/main.rs), repeated every frame:
    "IMGSTART <w> <h>\n"  then  w*h*2 raw little-endian RGB565 bytes

With --frames N>1 the N frames are combined per pixel by temporal median
(kills the stray salt-pepper byte errors of the DVP path), then a per-row
destripe removes the OV2640's fixed per-line colour bias. All of the heavy
lifting (RGB565 unpack, median, destripe, PNG scanline assembly) is vectorised
with numpy, so a 640x480 clean shot processes in well under a second.

    uv run python tools/grab.py --out captures/cam.png
    uv run python tools/grab.py --frames 5 --out captures/clean.png
"""
import argparse
import sys
import time
import zlib

import numpy as np
import serial


def read_frames(port, baud, n, deadline_s):
    """Sync on the IMGSTART header and read n full frames. Returns (w, h, list)."""
    s = serial.Serial(port, baud, timeout=5)
    s.reset_input_buffer()  # drop the partial frame we opened in the middle of
    frames, buf = [], b""
    w = h = need = None
    t0 = time.monotonic()
    header = __import__("re").compile(rb"IMGSTART (\d+) (\d+)\n")
    while len(frames) < n and time.monotonic() - t0 < deadline_s:
        if need is None:
            buf += s.read(8192)
            m = header.search(buf)
            if not m:
                buf = buf[-32:]  # keep only enough to catch a header spanning reads
                continue
            w, h, need = int(m.group(1)), int(m.group(2)), 0
            need = int(m.group(1)) * int(m.group(2)) * 2
            buf = buf[m.end():]
        while len(buf) < need and time.monotonic() - t0 < deadline_s:
            buf += s.read(need - len(buf))
        if len(buf) >= need:
            frames.append(buf[:need])
            buf = buf[need:]
            need = None
            print("frame", len(frames), flush=True)
    s.close()
    return w, h, frames


def unpack_rgb565(data, w, h):
    """bytes -> (h, w, 3) uint8 RGB888, expanding the 5/6/5 fields to full range."""
    px = np.frombuffer(data, dtype="<u2").astype(np.uint16).reshape(h, w)
    r = (px >> 11) & 0x1F
    g = (px >> 5) & 0x3F
    b = px & 0x1F
    # scale 5/6-bit fields to 8-bit with rounding (x*255/31, x*255/63)
    r = ((r.astype(np.uint32) * 255 + 15) // 31).astype(np.uint8)
    g = ((g.astype(np.uint32) * 255 + 31) // 63).astype(np.uint8)
    b = ((b.astype(np.uint32) * 255 + 15) // 31).astype(np.uint8)
    return np.stack([r, g, b], axis=-1)


def destripe(rgb, radius=4):
    """Remove each row's colour offset (row mean - median of neighbour rows)."""
    out = rgb.astype(np.float32)
    h = out.shape[0]
    for c in range(3):
        rowmean = out[:, :, c].mean(axis=1)
        ref = np.empty(h, np.float32)
        for y in range(h):
            lo, hi = max(0, y - radius), min(h, y + radius + 1)
            ref[y] = np.median(rowmean[lo:hi])
        off = (rowmean - ref)[:, None]
        out[:, :, c] = np.clip(out[:, :, c] - off, 0, 255)
    return out.astype(np.uint8)


def write_png(path, rgb):
    h, w, _ = rgb.shape
    raw = np.zeros((h, 1 + w * 3), np.uint8)  # leading filter byte (0) per scanline
    raw[:, 1:] = rgb.reshape(h, w * 3)
    comp = zlib.compress(raw.tobytes(), 9)

    def chunk(typ, d):
        c = typ + d
        return (
            len(d).to_bytes(4, "big") + c + (zlib.crc32(c) & 0xFFFFFFFF).to_bytes(4, "big")
        )

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", bytes([*w.to_bytes(4, "big"), *h.to_bytes(4, "big"), 8, 2, 0, 0, 0])))
        f.write(chunk(b"IDAT", comp))
        f.write(chunk(b"IEND", b""))


def diagnostics(rgb):
    """Mean luma + adjacent row/col correlation (a real 2D image -> high corr)."""
    lum = (0.299 * rgb[..., 0] + 0.587 * rgb[..., 1] + 0.114 * rgb[..., 2]).astype(np.float32)

    def corr(a, b):
        a = a.ravel() - a.mean()
        b = b.ravel() - b.mean()
        d = np.sqrt((a * a).sum() * (b * b).sum())
        return float((a * b).sum() / d) if d else 0.0

    rc = corr(lum[:-1, :], lum[1:, :])
    cc = corr(lum[:, :-1], lum[:, 1:])
    print("MEAN LUMA = %.1f  row-corr=%.3f  col-corr=%.3f" % (lum.mean(), rc, cc))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", default="/dev/ttyUSB0")
    ap.add_argument("--baud", type=int, default=1_500_000)
    ap.add_argument("--frames", type=int, default=1, help="N>1 -> temporal median")
    ap.add_argument("--out", default="captures/cam.png")
    ap.add_argument("--no-destripe", action="store_true")
    ap.add_argument("--timeout", type=float, default=60.0, help="capture deadline (s)")
    args = ap.parse_args()

    t = time.monotonic()
    w, h, frames = read_frames(args.port, args.baud, args.frames, args.timeout)
    t_cap = time.monotonic() - t
    if not frames:
        print("no frames captured (no IMGSTART header seen)")
        sys.exit(1)
    frames = [f for f in frames if len(f) == w * h * 2]
    print("got %d full frame(s) %dx%d in %.1fs" % (len(frames), w, h, t_cap))

    t = time.monotonic()
    imgs = np.stack([unpack_rgb565(f, w, h) for f in frames])  # (N, h, w, 3)
    rgb = np.median(imgs, axis=0).astype(np.uint8) if len(imgs) > 1 else imgs[0]
    if not args.no_destripe:
        rgb = destripe(rgb)
    write_png(args.out, rgb)
    print("wrote %s in %.2fs (median %d frame(s)%s)"
          % (args.out, time.monotonic() - t, len(frames),
             "" if args.no_destripe else ", destriped"))
    diagnostics(rgb)


if __name__ == "__main__":
    main()

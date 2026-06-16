"""Convert the tiny 24-bit BMP the K210 serves into an upscaled PNG for viewing.
Uses only numpy + stdlib zlib (no Pillow)."""
import struct, sys, zlib
import numpy as np

src = sys.argv[1] if len(sys.argv) > 1 else "captures/cam_capture.bmp"
dst = sys.argv[2] if len(sys.argv) > 2 else "captures/cam_capture.png"
scale = int(sys.argv[3]) if len(sys.argv) > 3 else 12

with open(src, "rb") as f:
    b = f.read()

assert b[:2] == b"BM", "not a BMP"
data_off = struct.unpack_from("<I", b, 10)[0]
w = struct.unpack_from("<i", b, 18)[0]
h = struct.unpack_from("<i", b, 22)[0]
bpp = struct.unpack_from("<H", b, 28)[0]
print(f"BMP {w}x{h} {bpp}bpp data_off={data_off} total={len(b)}")
assert bpp == 24

bottom_up = h > 0
h = abs(h)
row_bytes = (w * 3 + 3) & ~3  # rows padded to 4 bytes
img = np.zeros((h, w, 3), dtype=np.uint8)
for row in range(h):
    off = data_off + row * row_bytes
    line = np.frombuffer(b, dtype=np.uint8, count=w * 3, offset=off).reshape(w, 3)
    rgb = line[:, ::-1]  # BMP BGR -> RGB
    dst_row = (h - 1 - row) if bottom_up else row
    img[dst_row] = rgb

big = np.repeat(np.repeat(img, scale, axis=0), scale, axis=1)
H, W, _ = big.shape


def png(rgb):
    def chunk(tag, data):
        c = tag + data
        return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c) & 0xffffffff)

    raw = bytearray()
    for r in range(rgb.shape[0]):
        raw.append(0)  # filter type 0 (none)
        raw.extend(rgb[r].tobytes())
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", rgb.shape[1], rgb.shape[0], 8, 2, 0, 0, 0)
    return sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")


with open(dst, "wb") as f:
    f.write(png(big))
print(f"wrote {dst} ({W}x{H})")

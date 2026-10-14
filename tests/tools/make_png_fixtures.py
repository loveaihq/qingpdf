"""Write the small PNG files that crates/qingpdf-base/src/image/pngdec.rs is tested against.

Every colour type with every bit depth it may have, not interlaced and interlaced (Adam7), in several sizes (so that some of
the passes of an interlaced image are empty), with the five filters used in turn on the rows, the pixels split over several
IDAT chunks, a transparency chunk (tRNS) of each of the forms it has, a pHYs chunk, and chunks that are not read in between.
The pictures are made here, with Python's zlib and no PNG library, so that the file is written by something other than the
decoder it tests; the test then decodes each file with the decoder and with the `png` crate (a development dependency only)
and requires the same result.

    python tests/tools/make_png_fixtures.py [output_dir]

The default output directory is crates/qingpdf-base/tests/png. The files are the same every time (a fixed seed).
"""

import os
import struct
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_OUT = os.path.normpath(os.path.join(HERE, "..", "..", "crates", "qingpdf-base", "tests", "png"))

SIGNATURE = b"\x89PNG\r\n\x1a\n"
CHANNELS = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}
DEPTHS = {0: (1, 2, 4, 8, 16), 2: (8, 16), 3: (1, 2, 4, 8), 4: (8, 16), 6: (8, 16)}
SIZES = ((1, 1), (2, 3), (5, 4), (13, 11))
ADAM7 = ((0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2))


class Rng:
    """A small linear congruential generator, so that the output does not depend on the Python version."""

    def __init__(self, seed):
        self.state = seed & 0xFFFFFFFFFFFFFFFF

    def next(self, below):
        self.state = (self.state * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        return (self.state >> 33) % below


def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)


def pack_row(pixels, color, depth):
    """The bytes of one row of pixels (each a tuple of samples), without the filter byte."""
    if depth < 8:
        bits = "".join(format(p[0], "0{}b".format(depth)) for p in pixels)
        bits += "0" * (-len(bits) % 8)
        return bytes(int(bits[i : i + 8], 2) for i in range(0, len(bits), 8))
    if depth == 8:
        return bytes(s for p in pixels for s in p)
    return b"".join(struct.pack(">H", s) for p in pixels for s in p)


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def filter_row(kind, row, prev, bpp):
    out = bytearray()
    for i, x in enumerate(row):
        left = row[i - bpp] if i >= bpp else 0
        up = prev[i]
        up_left = prev[i - bpp] if i >= bpp else 0
        if kind == 0:
            v = x
        elif kind == 1:
            v = x - left
        elif kind == 2:
            v = x - up
        elif kind == 3:
            v = x - (left + up) // 2
        else:
            v = x - paeth(left, up, up_left)
        out.append(v & 0xFF)
    return bytes([kind]) + bytes(out)


def filtered(rows, bpp, first_filter):
    """The rows with a filter each: the five in turn, from `first_filter`."""
    out = bytearray()
    prev = bytes(len(rows[0])) if rows else b""
    for n, row in enumerate(rows):
        out += filter_row((first_filter + n) % 5, row, prev, bpp)
        prev = row
    return bytes(out)


def raw_data(image, w, h, color, depth, interlace, first_filter):
    bits_per_pixel = CHANNELS[color] * depth
    bpp = max(1, bits_per_pixel // 8)
    if not interlace:
        return filtered([pack_row(image[y], color, depth) for y in range(h)], bpp, first_filter)
    out = b""
    for x0, y0, dx, dy in ADAM7:
        ys = list(range(y0, h, dy))
        xs = list(range(x0, w, dx))
        if not ys or not xs:
            continue
        rows = [pack_row([image[y][x] for x in xs], color, depth) for y in ys]
        out += filtered(rows, bpp, first_filter)
    return out


def make(color, depth, w, h, interlace, rng, trns=None, phys=False, split=False, extra=False, palette_size=None, level=6, first_filter=0):
    top = (1 << depth) - 1
    channels = CHANNELS[color]
    if color == 3:
        palette_size = palette_size or min(1 << depth, 256)
        # (Some indices are past the end of the palette, in one of the files.)
        limit = palette_size if trns != "out_of_range" else min(top + 1, palette_size + 3)
        image = [[(rng.next(limit),) for _ in range(w)] for _ in range(h)]
    else:
        image = [[tuple(rng.next(top + 1) for _ in range(channels)) for _ in range(w)] for _ in range(h)]
    out = SIGNATURE
    out += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, color, 0, 0, 1 if interlace else 0))
    if extra:
        out += chunk(b"gAMA", struct.pack(">I", 45455))
        out += chunk(b"tEXt", b"Comment\x00made by make_png_fixtures.py")
    if phys:
        out += chunk(b"pHYs", struct.pack(">IIB", 11811, 5906, 1))
    if color == 3:
        out += chunk(b"PLTE", bytes(rng.next(256) for _ in range(3 * palette_size)))
    if trns == "indexed":
        out += chunk(b"tRNS", bytes([0, 128, 255][: max(1, palette_size - 1)] + [77] * max(0, palette_size - 4)))
    elif trns == "indexed_too_long":
        out += chunk(b"tRNS", bytes(rng.next(256) for _ in range(palette_size + 2)))
    elif trns == "key" and color in (0, 2):
        # The colour of the first pixel is the transparent one.
        samples = image[0][0]
        out += chunk(b"tRNS", b"".join(struct.pack(">H", s) for s in samples))
    elif trns == "key_unused" and color in (0, 2):
        out += chunk(b"tRNS", b"".join(struct.pack(">H", top) for _ in range(channels)))
    data = zlib.compress(raw_data(image, w, h, color, depth, interlace, first_filter), level)
    if split:
        step = max(1, len(data) // 3)
        pieces = [data[i : i + step] for i in range(0, len(data), step)]
    else:
        pieces = [data]
    for n, piece in enumerate(pieces):
        out += chunk(b"IDAT", piece)
    if extra:
        out += chunk(b"tIME", b"\x07\xea\x0a\x0e\x0b\x00\x00")
    out += chunk(b"IEND", b"")
    return out


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    os.makedirs(out_dir, exist_ok=True)
    count = 0
    seed = 1
    for color, depths in DEPTHS.items():
        for depth in depths:
            for w, h in SIZES:
                for interlace in (0, 1):
                    seed += 1
                    variants = [("", {})]
                    if (w, h) == (5, 4) or (w, h) == (13, 11):
                        variants.append(("_split", {"split": True, "extra": True, "phys": True, "level": 0, "first_filter": 2}))
                    if color in (0, 2) and depth <= 16 and (w, h) in ((5, 4), (2, 3)):
                        variants.append(("_trns", {"trns": "key"}))
                        variants.append(("_trnsunused", {"trns": "key_unused", "phys": True}))
                    if color == 3 and (w, h) in ((5, 4), (13, 11)):
                        variants.append(("_trns", {"trns": "indexed"}))
                        variants.append(("_trnslong", {"trns": "indexed_too_long"}))
                        variants.append(("_short", {"palette_size": 3 if depth > 1 else 2}))
                        variants.append(("_range", {"trns": "out_of_range", "palette_size": 3 if depth > 1 else 1}))
                    for suffix, options in variants:
                        name = "ct{}_d{}_{}x{}_i{}{}.png".format(color, depth, w, h, interlace, suffix)
                        data = make(color, depth, w, h, interlace, Rng(seed * 7919 + len(suffix)), **options)
                        with open(os.path.join(out_dir, name), "wb") as f:
                            f.write(data)
                        count += 1
    print("wrote", count, "files to", out_dir)


if __name__ == "__main__":
    main()

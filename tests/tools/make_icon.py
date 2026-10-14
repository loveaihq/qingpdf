#!/usr/bin/env python3
"""Draws the qingpdf program icon: a blue document with a folded corner and a white Q on it, flat colours.

    python tests/tools/make_icon.py            # writes crates/qingpdf-view-bin/qingpdf.ico

No libraries are needed (zlib and struct from the standard library). Every size is drawn from the same shapes (so they match), each
pixel is the average of a grid of points (smooth edges, no halo). The sizes are 16, 32 and 48 as plain 32-bit pictures (BMP, the
form every Windows reads) and 256 as a PNG (what Windows Vista and later take for the biggest size, and a lot smaller than a BMP).
`crates/qingpdf-view-bin/build.rs` reads the .ico and puts the pictures into the program as resources.
"""
import math
import os
import struct
import sys
import zlib

BODY = (0x2B, 0x6C, 0xDE)  # the document
FLAP = (0xA9, 0xC9, 0xF7)  # the folded corner
MARK = (0xFF, 0xFF, 0xFF)  # the Q

# The shapes, in a unit square (0..1, y down).
PAGE = (0.17, 0.06, 0.83, 0.94)  # left, top, right, bottom
PAGE_RADIUS = 0.08
FOLD = 0.22  # the corner that is folded over: its size along each side
RING = (0.50, 0.57, 0.225, 0.145)  # centre x, centre y, outer radius, inner radius
TAIL = (0.552, 0.622, 0.708, 0.778, 0.036)  # from, to, half width: a short stroke across the ring, its end flat, so the Q is not a magnifying glass


def in_page(x, y):
    l, t, r, b = PAGE
    if not (l <= x <= r and t <= y <= b):
        return False
    # Round corners (the top right one is cut off by the fold instead).
    k = PAGE_RADIUS
    for cx, cy, sx, sy in ((l + k, t + k, -1, -1), (l + k, b - k, -1, 1), (r - k, b - k, 1, 1)):
        if (x - cx) * sx > 0 and (y - cy) * sy > 0 and math.hypot(x - cx, y - cy) > k:
            return False
    # The fold cuts the top right corner along a diagonal.
    return (x - (r - FOLD)) - (y - t) <= 0.0


def in_flap(x, y):
    l, t, r, b = PAGE
    # The triangle that is the back of the corner: right of the fold's left edge, above its lower edge, below the cut.
    return in_page(x, y) and x >= r - FOLD and y <= t + FOLD


def in_mark(x, y):
    cx, cy, ro, ri = RING
    d = math.hypot(x - cx, y - cy)
    if ri <= d <= ro:
        return True
    x0, y0, x1, y1, hw = TAIL
    dx, dy = x1 - x0, y1 - y0
    length2 = dx * dx + dy * dy
    along = ((x - x0) * dx + (y - y0) * dy) / length2
    across = abs((x - x0) * dy - (y - y0) * dx) / math.sqrt(length2)
    return 0.0 <= along <= 1.0 and across <= hw


def colour_at(x, y):
    if not in_page(x, y):
        return None
    if in_mark(x, y):
        return MARK
    if in_flap(x, y):
        return FLAP
    return BODY


def draw(size, grid):
    """size x size pixels as (r, g, b, a) rows from the top; each pixel is the average of grid x grid points."""
    rows = []
    for py in range(size):
        row = []
        for px in range(size):
            r = g = b = n = 0
            for sy in range(grid):
                for sx in range(grid):
                    c = colour_at((px + (sx + 0.5) / grid) / size, (py + (sy + 0.5) / grid) / size)
                    if c:
                        r, g, b, n = r + c[0], g + c[1], b + c[2], n + 1
            a = round(255 * n / (grid * grid))
            row.append((r // n, g // n, b // n, a) if n else (0, 0, 0, 0))
        rows.append(row)
    return rows


def bmp_entry(rows):
    """The picture of an icon of this size as an .ico holds it: a DIB header (the height doubled for the mask), the 32-bit pixels
    from the bottom row up, and an all-zero mask (the opacity is in the pixels)."""
    size = len(rows)
    head = struct.pack("<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = b"".join(struct.pack("<BBBB", b, g, r, a) for row in reversed(rows) for (r, g, b, a) in row)
    mask = bytes(((size + 31) // 32 * 4) * size)
    return head + pixels + mask


def png_entry(rows):
    size = len(rows)

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + b"".join(bytes(p) for p in row) for row in rows)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "..", "crates", "qingpdf-view-bin", "qingpdf.ico")
    images = [(16, bmp_entry(draw(16, 12))), (32, bmp_entry(draw(32, 8))), (48, bmp_entry(draw(48, 8))), (256, png_entry(draw(256, 4)))]
    data = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for size, blob in images:
        # A width or height of 256 is written as 0.
        data += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    data += b"".join(blob for _, blob in images)
    with open(out, "wb") as f:
        f.write(data)
    print(f"{out}: {len(data)} bytes, sizes {[s for s, _ in images]}")


if __name__ == "__main__":
    main()

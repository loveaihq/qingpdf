#!/usr/bin/env python3
"""Draws the qingpdf program icon: the character 轻 in white on a rounded teal square (2026-10-21, drawn again 2026-10-22).

    python tests/tools/make_icon.py            # writes crates/qingpdf-view-bin/qingpdf.ico

What is drawn is what `crates/qingpdf-view/src/logo.rs` says (read from that file, so the icon, the mark in the title strip and the one on the welcome page are the
same drawing). At 16, 24 and 32 pixels it is the bitmap made by hand there (`P16`, `P24`, `P32`: whole pixels, so that the bars and stems are sharp); at 48 and 256
pixels it is the strokes (`FULL`): a line through some points with round caps and joins, in a square of 100 units. No libraries are needed (zlib and struct from the
standard library). Each pixel of the square's edge (and of the strokes) is the average of a grid of points (smooth edges, no halo). Every size (16, 24, 32, 48 and
256) is a 32-bit PNG: Windows Vista and later read a PNG picture of any size in an icon, and a PNG of these flat colours is a tenth of the plain 32-bit picture (BMP)
it replaces. Each PNG is coded with the filter that is smallest for it, zlib at its best level.
`crates/qingpdf-view-bin/build.rs` reads the .ico and puts the pictures into the program as resources.
"""
import math
import os
import re
import struct
import sys
import zlib

TEAL = (0x1D, 0x9E, 0x75)  # the accent of the program (light)
MARK = (0xFF, 0xFF, 0xFF)  # the character

HERE = os.path.dirname(os.path.abspath(__file__))
LOGO_RS = os.path.join(HERE, "..", "..", "crates", "qingpdf-view", "src", "logo.rs")


def read_logo():
    """(radius, strokes of FULL as [(points, width)], {size: rows of the bitmap}) read from logo.rs."""
    text = open(LOGO_RS, encoding="utf-8").read()
    radius = float(re.search(r"pub const RADIUS: f32 = ([0-9.]+);", text).group(1))
    body = re.search(r"pub const FULL: &\[Stroke\] = &\[(.*?)\n\];", text, re.S).group(1)
    strokes = []
    for m in re.finditer(r"Stroke \{ pts: &\[(.*?)\], width: ([0-9.]+) \}", body):
        pts = [(float(a), float(b)) for a, b in re.findall(r"\(([0-9.]+), ([0-9.]+)\)", m.group(1))]
        strokes.append((pts, float(m.group(2))))
    bitmaps = {}
    for size in (16, 24, 32):
        rows = re.search(r"pub const P%d: &\[&str\] = &\[(.*?)\n\];" % size, text, re.S).group(1)
        bitmaps[size] = re.findall(r'"([.#]+)"', rows)
        assert len(bitmaps[size]) == size and all(len(r) == size for r in bitmaps[size]), size
    return radius, strokes, bitmaps


def segment_distance(px, py, a, b):
    dx, dy = b[0] - a[0], b[1] - a[1]
    l2 = dx * dx + dy * dy
    t = 0.0 if l2 == 0 else max(0.0, min(1.0, ((px - a[0]) * dx + (py - a[1]) * dy) / l2))
    return math.hypot(px - (a[0] + t * dx), py - (a[1] + t * dy))


def in_square(x, y, radius):
    cx, cy = min(max(x, radius), 100 - radius), min(max(y, radius), 100 - radius)
    return math.hypot(x - cx, y - cy) <= radius


def draw(size, grid, radius, strokes):
    """size x size pixels as (r, g, b, a) rows from the top; each pixel is the average of grid x grid points."""
    rows = []
    for py in range(size):
        row = []
        for px in range(size):
            inside = glyph = 0
            for sy in range(grid):
                for sx in range(grid):
                    x, y = (px + (sx + 0.5) / grid) * 100.0 / size, (py + (sy + 0.5) / grid) * 100.0 / size
                    if not in_square(x, y, radius):
                        continue
                    inside += 1
                    if any(segment_distance(x, y, a, b) <= w / 2 for pts, w in strokes for a, b in zip(pts, pts[1:])):
                        glyph += 1
            if inside == 0:
                row.append((0, 0, 0, 0))
                continue
            g = glyph / inside
            colour = tuple(round(TEAL[i] * (1 - g) + MARK[i] * g) for i in range(3))
            row.append(colour + (round(255 * inside / (grid * grid)),))
        rows.append(row)
    return rows


def draw_bitmap(size, grid, radius, bitmap):
    """Like `draw`, with the character from a bitmap (a pixel is all of it or none): the edge of the square is smooth, the character is not."""
    rows = []
    for py in range(size):
        row = []
        for px in range(size):
            inside = 0
            for sy in range(grid):
                for sx in range(grid):
                    x, y = (px + (sx + 0.5) / grid) * 100.0 / size, (py + (sy + 0.5) / grid) * 100.0 / size
                    inside += in_square(x, y, radius)
            if inside == 0:
                row.append((0, 0, 0, 0))
                continue
            on = bitmap[py][px] == "#"
            colour = MARK if on else TEAL
            row.append(colour + (round(255 * inside / (grid * grid)),))
        rows.append(row)
    return rows


def png_entry(rows):
    """The picture as a 32-bit PNG (8 bits per channel with alpha): the smallest of four ways to filter the rows (none, sub, up, and
    for each row the one whose bytes add up to the least, the PNG specification's suggestion), zlib at its best level."""
    size = len(rows)

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    lines = [b"".join(bytes(p) for p in row) for row in rows]

    def filtered(kind, prev, line):
        if kind == 0:
            return line
        return bytes((v - (line[i - 4] if kind == 1 and i >= 4 else (prev[i] if kind == 2 else 0))) & 255 for i, v in enumerate(line))

    def cost(buf):
        return sum(v if v < 128 else 256 - v for v in buf)

    def coded(pick):
        raw = b""
        prev = bytes(len(lines[0]))
        for line in lines:
            kind = pick(prev, line)
            raw += bytes([kind]) + filtered(kind, prev, line)
            prev = line
        return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")

    best = lambda prev, line: min((0, 1, 2), key=lambda k: cost(filtered(k, prev, line)))  # noqa: E731
    return min((coded(lambda prev, line: 0), coded(lambda prev, line: 1), coded(lambda prev, line: 2), coded(best)), key=len)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "..", "..", "crates", "qingpdf-view-bin", "qingpdf.ico")
    radius, strokes, bitmaps = read_logo()
    images = []
    for size, grid in ((16, 12), (24, 10), (32, 8), (48, 8), (256, 4)):
        rows = draw_bitmap(size, grid, radius, bitmaps[size]) if size in bitmaps else draw(size, grid, radius, strokes)
        images.append((size, png_entry(rows)))
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

"""Make a synthetic scanned page for the render speed test: A4 at 300 dpi, black and white,
Group 4 (CCITTFaxDecode) compressed, text-like blocks on white.

    python tests/tools/make_scan_fixture.py

Writes tests/out/perf/scan-ccitt-a4-300dpi.pdf (tests/out is not committed).
Needs numpy and Pillow (Pillow's TIFF writer does the Group 4 coding).
"""

import io
import os
import random

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "out", "perf", "scan-ccitt-a4-300dpi.pdf"))
W, H = 2480, 3508


def page_bits():
    rng = random.Random(7)
    ink = np.zeros((H, W), dtype=np.uint8)  # 1 = ink
    y = 300
    while y < H - 300:
        x = 250
        while x < W - 250:
            w = rng.randint(20, 160)
            if rng.random() < 0.85:
                ink[y:y + 38, x:x + w] = 1
                # a few "letters": gaps inside the word
                for g in range(x + 12, x + w - 6, rng.randint(14, 30)):
                    ink[y:y + 38, g:g + 3] = 0
            x += w + rng.randint(25, 45)
        y += 70
    return ink


def main():
    ink = page_bits()
    # libtiff codes 0 bits as white: ink must be 1.
    im = Image.fromarray(ink * 255).convert("1")
    buf = io.BytesIO()
    im.save(buf, format="TIFF", compression="group4", tiffinfo={278: H})
    tiff = Image.open(io.BytesIO(buf.getvalue()))
    offset, count = tiff.tag_v2[273], tiff.tag_v2[279]
    if not isinstance(offset, int):
        offset, count = offset[0], count[0]
    data = buf.getvalue()[offset:offset + count]
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>",
        None,
        None,
    ]
    content = b"q 595.28 0 0 841.89 0 0 cm /Im Do Q"
    out = bytearray(b"%PDF-1.4\n")
    offsets = []

    def add(num, body):
        offsets.append(len(out))
        out.extend(f"{num} 0 obj\n".encode() + body + b"\nendobj\n")

    for i, body in enumerate(objs[:3], start=1):
        add(i, body)
    add(4, b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream")
    add(5, (b"<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray /BitsPerComponent 1 "
            b"/Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns %d /Rows %d >> /Length %d >>\nstream\n" % (W, H, W, H, len(data)))
        + data + b"\nendstream")
    xref = len(out)
    out.extend(b"xref\n0 6\n0000000000 65535 f \n")
    for off in offsets:
        out.extend(b"%010d 00000 n \n" % off)
    out.extend(b"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % xref)
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "wb") as f:
        f.write(out)
    print(f"wrote {OUT} ({len(out)} bytes, the image is {len(data)} bytes of Group 4)")


if __name__ == "__main__":
    main()

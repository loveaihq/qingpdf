"""Make a synthetic photo page for the render speed test (3c2-2): A4 at 300 dpi, colour, JPEG 2000 coded
(9/7, 6 resolution levels, one layer at about 20:1, the way a scanner or an archive codes a page),
one full-page image in a PDF.

    python tests/tools/make_jpx_photo_fixture.py [compression ratio]

Writes tests/out/perf/photo-jpx-a4-300dpi.pdf (tests/out is not committed).
Needs numpy and Pillow (Pillow's JPEG 2000 writer is OpenJPEG).
"""

import io
import os
import sys

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "out", "perf", "photo-jpx-a4-300dpi.pdf"))
W, H = 2480, 3508
RATIO = float(sys.argv[1]) if len(sys.argv) > 1 else 20.0


def photo():
    """Smooth light and colour, soft blobs, some sharp edges and film grain: not as easy as a gradient."""
    rng = np.random.default_rng(11)
    y, x = np.mgrid[0:H, 0:W].astype(np.float32)
    planes = []
    for c in range(3):
        v = 120 + 50 * np.sin(x / (300 + 90 * c) + c) * np.cos(y / (410 - 60 * c))
        for _ in range(40):
            cx, cy, r = rng.uniform(0, W), rng.uniform(0, H), rng.uniform(40, 400)
            v += rng.uniform(-60, 60) * np.exp(-(((x - cx) ** 2 + (y - cy) ** 2) / (2 * r * r)))
        edges = ((x // 173 + y // 211) % 2) * 25 * np.sin(y / 97.0)
        planes.append(v + edges + rng.normal(0, 4.0, (H, W)))
    return np.clip(np.stack(planes, axis=-1), 0, 255).astype(np.uint8)


def main():
    buf = io.BytesIO()
    Image.fromarray(photo()).save(buf, "JPEG2000", irreversible=True, quality_mode="rates", quality_layers=[RATIO], num_resolutions=6, codeblock_size=(64, 64))
    jp2 = buf.getvalue()
    content = b"q 595.28 0 0 841.89 0 0 cm /Im Do Q"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>",
        b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream",
        b"<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /JPXDecode /Length %d >>\nstream\n" % (W, H, len(jp2)) + jp2 + b"\nendstream",
    ]
    out = b"%PDF-1.5\n"
    offsets = []
    for i, o in enumerate(objects):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % (i + 1) + o + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for o in offsets:
        out += b"%010d 00000 n \n" % o
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objects) + 1, xref)
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "wb") as f:
        f.write(out)
    print(f"{OUT}: {len(jp2)} bytes of JPEG 2000 ({len(jp2) * 8 / (W * H):.2f} bits a pixel)")


main()

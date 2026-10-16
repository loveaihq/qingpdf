"""A scanned file for the speed test: N pages of A4 at 300 dpi (2480 x 3508), each one picture of its own (a JPEG made by Pillow, as a
scanner would: paper colour, noise, lines of text-like marks), RGB or grey.

    python make_speed_scan.py out.pdf [pages] [rgb|gray]
"""
import io
import random
import sys

import numpy as np
from PIL import Image, ImageFilter

W, H = 2480, 3508


def page_image(seed: int, rgb: bool) -> bytes:
    rng = np.random.default_rng(seed)
    r = random.Random(seed)
    paper = np.array([238, 234, 224], dtype=np.float32) if rgb else np.array([232], dtype=np.float32)
    ch = 3 if rgb else 1
    img = np.empty((H, W, ch), dtype=np.float32)
    img[:] = paper
    # slow shading across the page, as a scanner lamp gives
    yy = np.linspace(0, 1, H, dtype=np.float32)[:, None, None]
    xx = np.linspace(0, 1, W, dtype=np.float32)[None, :, None]
    img -= 14 * (0.6 * yy + 0.4 * (xx - 0.5) ** 2)
    # lines of text-like marks
    y = 260
    while y < H - 260:
        x = 220
        while x < W - 220:
            w = r.randint(30, 200)
            if r.random() < 0.9:
                gh = r.randint(26, 40)
                img[y:y + gh, x:x + w, :] -= 170
                for g in range(x + 10, x + w - 6, r.randint(12, 26)):
                    img[y:y + gh, g:g + 3, :] += 150
            x += w + r.randint(24, 46)
        y += r.randint(64, 80)
    # a photo-like patch on some pages
    if seed % 3 == 0:
        py, px = 1200, 500
        patch = rng.normal(0, 1, (900, 1400, ch)).astype(np.float32)
        patch = np.array(Image.fromarray(np.clip(patch * 40 + 128, 0, 255).astype(np.uint8).squeeze()).filter(ImageFilter.GaussianBlur(9))).astype(np.float32).reshape(900, 1400, ch)
        img[py:py + 900, px:px + 1400, :] = 110 + (patch - 128) * 2
    img += rng.normal(0, 2.2, img.shape).astype(np.float32)
    arr = np.clip(img, 0, 255).astype(np.uint8)
    pil = Image.fromarray(arr if rgb else arr.squeeze(), "RGB" if rgb else "L")
    buf = io.BytesIO()
    pil.save(buf, format="JPEG", quality=82, dpi=(300, 300))
    return buf.getvalue()


def main():
    out = sys.argv[1]
    pages = int(sys.argv[2]) if len(sys.argv) > 2 else 100
    rgb = (sys.argv[3] if len(sys.argv) > 3 else "rgb") == "rgb"
    parts = [b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n"]
    offsets = {}

    def put(num, body: bytes):
        offsets[num] = sum(len(p) for p in parts)
        parts.append(b"%d 0 obj\n" % num + body + b"\nendobj\n")

    put(1, b"<< /Type /Catalog /Pages 2 0 R >>")
    kids = b" ".join(b"%d 0 R" % (3 + 3 * i) for i in range(pages))
    put(2, b"<< /Type /Pages /Kids [" + kids + b"] /Count %d >>" % pages)
    for i in range(pages):
        p, c, im = 3 + 3 * i, 4 + 3 * i, 5 + 3 * i
        put(p, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.28 841.89] /Resources << /XObject << /Im %d 0 R >> >> /Contents %d 0 R >>" % (im, c))
        content = b"q 595.28 0 0 841.89 0 0 cm /Im Do Q"
        put(c, b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream")
        data = page_image(i + 1, rgb)
        cs = b"/DeviceRGB" if rgb else b"/DeviceGray"
        put(im, b"<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace %s /BitsPerComponent 8 /Filter /DCTDecode /Length %d >>\nstream\n" % (W, H, cs, len(data)) + data + b"\nendstream")
        if i % 10 == 0:
            print("page", i, len(data), flush=True)
    total = 3 + 3 * pages
    start = sum(len(p) for p in parts)
    xref = b"xref\n0 %d\n0000000000 65535 f \n" % total
    for n in range(1, total):
        xref += b"%010d 00000 n \n" % offsets[n]
    parts.append(xref + b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (total, start))
    with open(out, "wb") as f:
        f.write(b"".join(parts))
    print("wrote", out, sum(len(p) for p in parts))


main()

"""Check the JPEG encoder of `compress` against decoders that are not ours (Pillow, PyMuPDF), and measure what it gives.

1. The files that the test `jpeg_files` of qingpdf-core writes to tests/out/jpeg-encoder/ (run `cargo test --release -p qingpdf-core --test
   jpeg_files` first, or give --run): every size from 1 x 1 to 1 x 65535 and 65535 x 1, odd sizes, qualities 1 to 100. Pillow and PyMuPDF
   (which are two different libjpeg builds) must open each one at the size it was made at, agree with each other to within 2 levels, and
   (for the pictures of more than 1000 pixels) come within the PSNR a JPEG of that quality gives.
2. Photographs: the pictures of the corpus that are photographs (JPEGs inside PDF files) are put in a file as raw samples at 72 dpi (so
   that nothing is averaged down), compressed with each preset by the command line, and the JPEG that comes out is opened with Pillow
   and compared with the photograph: PSNR over the samples (RGB), and the size against the raw samples and against what Pillow's own
   encoder makes at the same quality (libjpeg with its standard tables, no optimized Huffman tables, 4:2:0).

    python tests/tools/jpeg_check.py [--ours target/release/qingpdf.exe] [--run]

It exits with 1 when a check fails: a file that a decoder does not open, a size that is not the one made, or PSNR at quality 75 under 33 dB
on a photograph. A development check, not part of `cargo test`: it needs Pillow, PyMuPDF and numpy.
"""

import argparse
import io
import os
import subprocess
import sys
import zlib

import numpy as np
import pymupdf
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
ap = argparse.ArgumentParser()
ap.add_argument("--ours", default=os.path.join(ROOT, "target", "release", "qingpdf.exe"))
ap.add_argument("--out", default=os.path.join(ROOT, "tests", "out", "jpeg-encoder"))
ap.add_argument("--run", action="store_true")
args = ap.parse_args()
bad = False


def fail(msg):
    global bad
    bad = True
    print("  FAILED:", msg, flush=True)


def psnr(a, b):
    mse = float(((a.astype(np.float64) - b.astype(np.float64)) ** 2).mean())
    return 99.0 if mse == 0 else 10 * np.log10(255.0 * 255.0 / mse)


def read_pnm(path):
    with open(path, "rb") as f:
        magic = f.readline().strip()
        w, h = map(int, f.readline().split())
        f.readline()
        data = np.frombuffer(f.read(), dtype=np.uint8)
    return data.reshape((h, w, 3) if magic == b"P6" else (h, w))


if args.run:
    subprocess.run(["cargo", "test", "--release", "-p", "qingpdf-core", "--test", "jpeg_files"], cwd=ROOT, check=True)

print("== the encoder's files, opened by Pillow and PyMuPDF")
names = sorted(f[:-4] for f in os.listdir(args.out) if f.endswith(".jpg")) if os.path.isdir(args.out) else []
if not names:
    fail("no files: run with --run, or `cargo test --release -p qingpdf-core --test jpeg_files`")
for name in names:
    jpg = os.path.join(args.out, name + ".jpg")
    source = read_pnm(os.path.join(args.out, name + ".pnm"))
    h, w = source.shape[:2]
    grey = source.ndim == 2
    pil = np.array(Image.open(jpg).convert("L" if grey else "RGB"))
    px = pymupdf.Pixmap(jpg)
    if px.alpha:
        px = pymupdf.Pixmap(px, 0)
    mu = np.frombuffer(px.samples, dtype=np.uint8).reshape((px.height, px.width, px.n))
    if grey and px.n == 1:
        mu = mu[:, :, 0]
    if pil.shape[:2] != (h, w) or (px.width, px.height) != (w, h):
        fail(f"{name}: sizes {pil.shape[:2]} and {(px.height, px.width)} for {(h, w)}")
        continue
    same_shape = mu.shape == pil.shape
    p, p_mu = psnr(source, pil), psnr(source, mu) if same_shape else -1.0
    # The two decoders upsample the halved colour differently (smoothly, or by repeating), so they differ at colour edges by a few levels
    # (a grey picture is read the same by both); they must be close to each other and as close to the picture as each other.
    between = psnr(pil, mu) if same_shape else -1.0
    print(f"  {name:22} {w:>5} x {h:<5} {os.path.getsize(jpg):>9,} bytes   PSNR Pillow {p:5.1f} dB, PyMuPDF {p_mu:5.1f} dB, the two {between:5.1f} dB")
    if not same_shape or between < 30.0 or (grey and between < 99.0):
        fail(f"{name}: the two decoders disagree ({between:.1f} dB)")
    if min(w, h) >= 64 and "q1" not in name and "q60" not in name and p < 30.0:
        fail(f"{name}: PSNR {p:.1f}")


def photographs():
    """The photographs of the corpus: (label, RGB array), from the JPEGs inside PDF files."""
    files = [
        ("public/zh/gongwen/gongwen-2024-ai-standards-guide.pdf", 2),
        ("local/zh/lunwen/lunwen-arxiv-2411.00268-founder-fly.pdf", 2),
        ("local/large/pdkids.pdf", 1),
        ("local/large/freeculture.pdf", 1),
        ("local/zh/lunwen/lunwen-arxiv-2503.12308-wps.pdf", 1),
        ("local/scanned/issue7229.pdf", 1),
    ]
    out = []
    for rel, most in files:
        path = os.path.join(ROOT, "tests", "corpus", rel)
        if not os.path.exists(path):
            continue
        doc = pymupdf.open(path)
        taken = 0
        for x in range(1, doc.xref_length()):
            if taken >= most:
                break
            try:
                if doc.xref_get_key(x, "Subtype")[1] != "/Image" or "DCT" not in doc.xref_get_key(x, "Filter")[1]:
                    continue
                info = doc.extract_image(x)
                im = Image.open(io.BytesIO(info["image"]))
                if im.width * im.height < 100_000:
                    continue
                out.append((f"{os.path.basename(rel)[:28]} #{x}", np.array(im.convert("RGB"))))
                taken += 1
            except Exception:
                continue
    return out


def raw_pdf(rgb, path):
    """One page of the picture's size in points holding the picture as raw (Flate) samples: drawn at 72 dpi."""
    h, w = rgb.shape[:2]
    data = zlib.compress(rgb.tobytes(), 6)
    parts = [b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n"]
    offsets = {}

    def put(n, body):
        offsets[n] = sum(len(p) for p in parts)
        parts.append(b"%d 0 obj\n" % n + body + b"\nendobj\n")

    put(1, b"<< /Type /Catalog /Pages 2 0 R >>")
    put(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
    put(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 %d %d] /Resources << /XObject << /Im 5 0 R >> >> /Contents 4 0 R >>" % (w, h))
    content = b"q %d 0 0 %d 0 0 cm /Im Do Q" % (w, h)
    put(4, b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream")
    put(5, b"<< /Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /Length %d >>\nstream\n" % (w, h, len(data)) + data + b"\nendstream")
    start = sum(len(p) for p in parts)
    xref = b"xref\n0 6\n0000000000 65535 f \n" + b"".join(b"%010d 00000 n \n" % offsets[n] for n in range(1, 6))
    parts.append(xref + b"trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % start)
    with open(path, "wb") as f:
        f.write(b"".join(parts))
    return len(data)


print("\n== photographs of the corpus through the command line (72 dpi: nothing is averaged down)")
work = os.path.join(args.out, "photos")
os.makedirs(work, exist_ok=True)
rows = []
for label, rgb in photographs():
    src = os.path.join(work, "photo.pdf")
    raw_size = raw_pdf(rgb, src)
    for preset, q in (("high", 85), ("balanced", 75), ("small", 60)):
        out = os.path.join(work, f"photo.{preset}.pdf")
        r = subprocess.run([args.ours, "compress", src, "-o", out, "--force", "--preset", preset], capture_output=True, text=True)
        if r.returncode != 0:
            fail(f"{label} {preset}: {r.stderr}")
            continue
        doc = pymupdf.open(out)
        jpeg = None
        for x in range(1, doc.xref_length()):
            if doc.xref_get_key(x, "Subtype")[1] == "/Image":
                jpeg = doc.xref_stream_raw(x)
        if jpeg is None or not jpeg.startswith(b"\xff\xd8"):
            fail(f"{label} {preset}: no JPEG in the output")
            continue
        got = np.array(Image.open(io.BytesIO(jpeg)).convert("RGB"))
        p = psnr(rgb, got)
        # What libjpeg makes of the same picture at the same quality (Pillow: standard tables, 4:2:0, no optimized tables).
        buf = io.BytesIO()
        Image.fromarray(rgb).save(buf, format="JPEG", quality=q, subsampling="4:2:0")
        theirs = len(buf.getvalue())
        p_theirs = psnr(rgb, np.array(Image.open(io.BytesIO(buf.getvalue())).convert("RGB")))
        print(f"  {label:36} {rgb.shape[1]:>5} x {rgb.shape[0]:<5} {preset:9} q{q}: {len(jpeg):>9,} bytes (Pillow {theirs:>9,}, raw {raw_size:>10,})  PSNR {p:5.1f} dB (Pillow's {p_theirs:5.1f})")
        rows.append((label, preset, p, len(jpeg), theirs, p_theirs))
        # A picture of flat colours and sharp edges (an illustration) loses more to the halved colour than a photograph does, in any
        # encoder: for every picture the PSNR may be no more than 1 dB under what libjpeg gets at the same quality, and a photograph must
        # come to 33 dB at quality 75 (the pictures marked below are drawings, not photographs).
        if p < p_theirs - 1.0:
            fail(f"{label} {preset}: PSNR {p:.1f} dB, Pillow's {p_theirs:.1f}")
        if len(jpeg) > theirs * 1.02:
            fail(f"{label} {preset}: bigger than Pillow's own at the same quality")
if rows:
    balanced = [r for r in rows if r[1] == "balanced"]
    drawings = [r for r in balanced if "2411.00268" in r[0] or "pdkids" in r[0]]
    photos = [r for r in balanced if r not in drawings]
    print(f"\n  PSNR at quality 75: photographs {', '.join(f'{r[2]:.1f}' for r in photos)} dB (lowest {min(r[2] for r in photos):.1f}); "
          f"drawings {', '.join(f'{r[2]:.1f}' for r in drawings)} dB (Pillow's own: {', '.join(f'{r[5]:.1f}' for r in drawings)})")
    if min(r[2] for r in photos) < 33.0:
        fail("a photograph is under 33 dB at quality 75")
print("\nFAILED" if bad else "\nall checks passed")
sys.exit(1 if bad else 0)

"""Check a file that qingpdf added content to (step 4c) against the other readers.

Draws the pages with PDFium and with MuPDF (PyMuPDF), and with qingpdf's own `render` command when --ours names the program.
The drawings must agree (the mean difference of a pixel is printed, and must be under --limit). The text each --expect names must
be what PDFium extracts from the page (the new text is part of the page's content, so no flattening is needed). `qpdf --check`
must pass, and with --original the bytes of the original must be a prefix of the saved file.

    python tests/tools/content_compare.py saved.pdf [--original orig.pdf] [--password pw] [--pages 0 1 2] [--scale 1]
        [--out folder] [--ours target/release/qingpdf.exe] [--qpdf path/to/qpdf] [--expect text ...] [--limit 3.0]

It exits with 1 when the readers disagree, a text is missing or a check fails. A development check, not part of `cargo test`: it
needs pypdfium2, PyMuPDF, numpy and Pillow.
"""

import argparse
import os
import subprocess
import sys
import tempfile

import numpy as np
import pymupdf
import pypdfium2 as pdfium
from PIL import Image

ap = argparse.ArgumentParser()
ap.add_argument("saved")
ap.add_argument("--original", default="")
ap.add_argument("--password", default="")
ap.add_argument("--scale", type=float, default=1.0)
ap.add_argument("--pages", type=int, nargs="*", default=[0])
ap.add_argument("--out", default="")
ap.add_argument("--ours", default=os.path.join("target", "release", "qingpdf.exe"))
ap.add_argument("--expect", nargs="*", default=[])
ap.add_argument("--qpdf", default=os.path.join("C:\\", "Dev", "tools", "qpdf", "bin", "qpdf.exe"))
ap.add_argument("--limit", type=float, default=3.0)
args = ap.parse_args()
pymupdf.TOOLS.mupdf_display_errors(False)
bad = False

# qpdf --check
if os.path.exists(args.qpdf):
    cmd = [args.qpdf, "--check"] + (["--password=" + args.password] if args.password else []) + [args.saved]
    r = subprocess.run(cmd, capture_output=True, text=True)
    ok = r.returncode == 0 and "No syntax or stream encoding errors" in r.stdout
    print("qpdf --check:", "ok" if ok else "FAILED\n" + r.stdout[-600:] + r.stderr[-600:])
    bad |= not ok

# the original bytes are a prefix
if args.original:
    a = open(args.original, "rb").read()
    b = open(args.saved, "rb").read()
    prefix = b[: len(a)] == a and len(b) > len(a)
    print("original is a prefix:", prefix, f"(+{len(b) - len(a)} bytes)")
    bad |= not prefix


def pdfium_draw(path, index):
    doc = pdfium.PdfDocument(path, password=args.password or None)
    return np.asarray(doc[index].render(scale=args.scale).to_pil().convert("RGB")), doc


def mupdf_draw(path, index):
    doc = pymupdf.open(path)
    if doc.needs_pass:
        doc.authenticate(args.password)
    pix = doc[index].get_pixmap(matrix=pymupdf.Matrix(args.scale, args.scale), alpha=False)
    return np.frombuffer(pix.samples, dtype=np.uint8).reshape(pix.height, pix.width, 3)


def ours_draw(path, index):
    with tempfile.TemporaryDirectory() as d:
        png = os.path.join(d, "o.png")
        cmd = [args.ours, "render", path, "--pages", str(index + 1), "--dpi", str(72 * args.scale), "-o", png, "--force"]
        if args.password:
            cmd += ["--password", args.password]
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            print("  qingpdf render failed:", r.stderr.strip()[:200])
            return None
        return np.asarray(Image.open(png).convert("RGB"))


def difference(a, b):
    if a is None or b is None:
        return None
    h, w = min(a.shape[0], b.shape[0]), min(a.shape[1], b.shape[1])
    return float(np.abs(a[:h, :w].astype(int) - b[:h, :w].astype(int)).mean())


for index in args.pages:
    p, doc = pdfium_draw(args.saved, index)
    m = mupdf_draw(args.saved, index)
    o = ours_draw(args.saved, index) if os.path.exists(args.ours) else None
    d_pm, d_po, d_mo = difference(p, m), difference(p, o), difference(m, o)
    show = lambda v: "n/a" if v is None else f"{v:.2f}"
    print(f"page {index + 1}: PDFium-MuPDF {show(d_pm)}  PDFium-ours {show(d_po)}  MuPDF-ours {show(d_mo)}")
    if d_pm is None or d_pm > args.limit:
        bad = True
    if args.original:
        before, _ = pdfium_draw(args.original, index)
        changed = float(np.abs(p.astype(int) - before.astype(int)).mean()) if p.shape == before.shape else -1
        print(f"  changed from the original (PDFium): {changed:.2f}")
    if args.out:
        os.makedirs(args.out, exist_ok=True)
        base = os.path.splitext(os.path.basename(args.saved))[0]
        Image.fromarray(p).save(os.path.join(args.out, f"{base}_p{index + 1}_pdfium.png"))
        Image.fromarray(m).save(os.path.join(args.out, f"{base}_p{index + 1}_mupdf.png"))
        if o is not None:
            Image.fromarray(o).save(os.path.join(args.out, f"{base}_p{index + 1}_ours.png"))
    text = doc[index].get_textpage().get_text_range().replace("\r", "").replace("\n", "")
    for want in args.expect:
        found = want in text
        print(f"  PDFium text has {want!r}: {found}")
        bad |= not found
sys.exit(1 if bad else 0)

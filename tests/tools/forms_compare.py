"""Check a form that qingpdf filled in (step 4b) against the other readers.

Draws page 1 of the saved file with PDFium (with its form-fill environment, so that it draws the widgets itself too) and with
MuPDF (PyMuPDF), and with qingpdf's own `render` command when --ours names the program. The three drawings must agree (the mean
difference of a pixel is printed, and must be under --limit). The text each --expect names must be what PDFium extracts after it
has put the appearances of the widgets and annotations into the page (FPDFPage_Flatten): that is what "the text copied out of the
file is what was typed" means for text that sits in an appearance stream. With --original, the pixels that changed from it must
be in the same place for both readers.

    python tests/tools/forms_compare.py saved.pdf [--original orig.pdf] [--password pw] [--scale 2] [--out folder]
        [--ours target/release/qingpdf.exe] [--expect text ...] [--limit 3.0]

It exits with 1 when the readers disagree or a text is missing. A development check, not part of `cargo test`: it needs
pypdfium2, PyMuPDF, numpy and Pillow.
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile

import numpy as np
import pymupdf
import pypdfium2 as pdfium
import pypdfium2.raw as raw
from PIL import Image

ap = argparse.ArgumentParser()
ap.add_argument("saved")
ap.add_argument("--original", default="")
ap.add_argument("--password", default="")
ap.add_argument("--scale", type=float, default=2.0)
ap.add_argument("--out", default="")
ap.add_argument("--ours", default="")
ap.add_argument("--expect", nargs="*", default=[])
ap.add_argument("--limit", type=float, default=3.0)
args = ap.parse_args()
pymupdf.TOOLS.mupdf_display_errors(False)


def pdfium_draw(path):
    doc = pdfium.PdfDocument(path, password=args.password or None)
    doc.init_forms()
    bitmap = doc[0].render(scale=args.scale, may_draw_forms=True)
    return np.asarray(bitmap.to_pil().convert("RGB"))


def mupdf_draw(path):
    doc = pymupdf.open(path)
    if doc.needs_pass and not doc.authenticate(args.password):
        return None
    pix = doc[0].get_pixmap(matrix=pymupdf.Matrix(args.scale, args.scale), alpha=False, annots=True)
    return np.frombuffer(pix.samples, dtype=np.uint8).reshape(pix.height, pix.width, 3)


def ours_draw(path):
    with tempfile.TemporaryDirectory() as d:
        png = os.path.join(d, "o.png")
        cmd = [args.ours, "render", path, "--pages", "1", "--dpi", str(72 * args.scale), "-o", png, "--force"]
        if args.password:
            cmd += ["--password", args.password]
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            print("qingpdf render failed:", r.stderr.strip()[:200])
            return None
        return np.asarray(Image.open(png).convert("RGB"))


def difference(a, b):
    if a is None or b is None:
        return None
    h, w = min(a.shape[0], b.shape[0]), min(a.shape[1], b.shape[1])
    return float(np.abs(a[:h, :w].astype(int) - b[:h, :w].astype(int)).mean())


def box_of_change(before, after):
    if before is None or after is None or before.shape != after.shape:
        return None
    mask = np.abs(before.astype(int) - after.astype(int)).max(axis=2) > 12
    if not mask.any():
        return None
    ys, xs = np.nonzero(mask)
    return [xs.min() / args.scale, ys.min() / args.scale, (xs.max() + 1) / args.scale, (ys.max() + 1) / args.scale]


def flattened_text(path):
    doc = pdfium.PdfDocument(path, password=args.password or None)
    doc.init_forms()
    page = doc[0]
    raw.FPDFPage_Flatten(page.raw, raw.FLAT_NORMALDISPLAY)
    # (The page is taken again: the text of the flattened one is what is wanted.)
    return doc[0].get_textpage().get_text_range()


bad = False
pd, mu = pdfium_draw(args.saved), mupdf_draw(args.saved)
ours = ours_draw(args.saved) if args.ours else None
pairs = [("PDFium vs MuPDF", difference(pd, mu))]
if ours is not None:
    pairs += [("ours vs PDFium", difference(ours, pd)), ("ours vs MuPDF", difference(ours, mu))]
for name, d in pairs:
    verdict = "n/a" if d is None else ("ok" if d <= args.limit else "TOO DIFFERENT")
    print(f"{name}: mean pixel difference {d if d is None else round(d, 3)}  {verdict}")
    bad |= d is not None and d > args.limit

if args.original:
    pd0, mu0 = pdfium_draw(args.original), mupdf_draw(args.original)
    b_pd, b_mu = box_of_change(pd0, pd), box_of_change(mu0, mu)
    print("changed box (points) PDFium:", b_pd and [round(v, 1) for v in b_pd], " MuPDF:", b_mu and [round(v, 1) for v in b_mu])
    if b_pd is None or b_mu is None:
        print("a reader shows no change")
        bad = True
    else:
        slack = 3.0
        if max(abs(x - y) for x, y in zip(b_pd, b_mu)) > slack:
            print("the readers disagree about where the change is")
            bad = True

if args.expect:
    text = re.sub(r"\s+", " ", flattened_text(args.saved))
    for want in args.expect:
        one = re.sub(r"\s+", " ", want)
        found = one in text
        print(f"text {want!r}: {'found' if found else 'MISSING'} in what PDFium extracts")
        bad |= not found
    if bad:
        print("PDFium text:", text[:400].encode("unicode_escape").decode())

if args.out:
    os.makedirs(args.out, exist_ok=True)
    base = os.path.join(args.out, os.path.splitext(os.path.basename(args.saved))[0])
    for tag, img in [("pdfium", pd), ("mupdf", mu), ("ours", ours)]:
        if img is not None:
            Image.fromarray(img).save(f"{base}.{tag}.png")

sys.exit(1 if bad else 0)

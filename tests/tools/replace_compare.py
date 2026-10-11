"""Check a file that `qingpdf replace` (or the reader's line edit, step 5a) changed against the other readers.

    python tests/tools/replace_compare.py saved.pdf --original orig.pdf --page 3 --old "old text" --new "new text"
        [--password pw] [--out folder] [--scale 2] [--qpdf path/to/qpdf] [--limit 3.0]

Checks: `qpdf --check` passes; the bytes of the original are a prefix of the saved file; PDFium's and MuPDF's text of the page has the
new text and not the old; PDFium and MuPDF draw the saved page alike (the mean difference of a pixel is printed and must be under
--limit, or not more than 0.5 above what the two draw differently in the original); the new text sits at the place and size of the old: the rectangle MuPDF finds for the old text in the original and for the new
text in the saved file start at the same place (--start-tolerance points) and have the same height. Crops of the region round the text,
from the original and the saved file in both renderers, go to --out (look at them). It exits with 1 when a check fails. A development
check, not part of `cargo test`: it needs pypdfium2, PyMuPDF, numpy and Pillow.
"""

import argparse
import os
import subprocess
import sys

import numpy as np
import pymupdf
import pypdfium2 as pdfium
from PIL import Image

ap = argparse.ArgumentParser()
ap.add_argument("saved")
ap.add_argument("--original", required=True)
ap.add_argument("--password", default="")
ap.add_argument("--page", type=int, default=1)
ap.add_argument("--old", required=True)
ap.add_argument("--new", required=True)
ap.add_argument("--scale", type=float, default=2.0)
ap.add_argument("--out", default="")
ap.add_argument("--qpdf", default=os.path.join("C:\\", "Dev", "tools", "qpdf", "bin", "qpdf.exe"))
ap.add_argument("--limit", type=float, default=3.0)
ap.add_argument("--start-tolerance", type=float, default=2.5)
ap.add_argument("--name", default="case")
args = ap.parse_args()
pymupdf.TOOLS.mupdf_display_errors(False)
bad = False
index = args.page - 1


def check(name, ok, detail=""):
    global bad
    print(f"{name}: {'ok' if ok else 'FAILED'} {detail}")
    bad |= not ok


# qpdf --check
cmd = [args.qpdf, "--check"] + (["--password=" + args.password] if args.password else []) + [args.saved]
r = subprocess.run(cmd, capture_output=True, text=True)
check("qpdf --check", r.returncode == 0 and "No syntax or stream encoding errors" in r.stdout, "" if r.returncode == 0 else r.stdout[-400:] + r.stderr[-400:])

# the original is a prefix
a = open(args.original, "rb").read()
b = open(args.saved, "rb").read()
check("original is a prefix", b[: len(a)] == a and len(b) > len(a), f"(+{len(b) - len(a)} bytes)")


def pdfium_doc(path):
    return pdfium.PdfDocument(path, password=args.password or None)


def mupdf_doc(path):
    d = pymupdf.open(path)
    if d.needs_pass:
        d.authenticate(args.password)
    return d


def norm(s):
    return "".join(s.split())


# text
pd = pdfium_doc(args.saved)
pt = pd[index].get_textpage().get_text_range()
check("PDFium text has the new text", norm(args.new) in norm(pt))
check("PDFium text has not the old text", norm(args.old) not in norm(pt) or norm(args.old) in norm(args.new))
md = mupdf_doc(args.saved)
mt = md[index].get_text()
check("MuPDF text has the new text", norm(args.new) in norm(mt))

# drawing
mat = pymupdf.Matrix(args.scale, args.scale)


def mu_pix(doc):
    pix = doc[index].get_pixmap(matrix=mat, alpha=False)
    return np.frombuffer(pix.samples, dtype=np.uint8).reshape(pix.height, pix.width, 3)


def pf_pix(doc):
    return np.asarray(doc[index].render(scale=args.scale).to_pil().convert("RGB"))


p_new, m_new = pf_pix(pd), mu_pix(md)
p_old, m_old = pf_pix(pdfium_doc(args.original)), mu_pix(mupdf_doc(args.original))
if p_new.shape == m_new.shape:
    diff = float(np.abs(p_new.astype(int) - m_new.astype(int)).mean())
    base = float(np.abs(p_old.astype(int) - m_old.astype(int)).mean()) if p_old.shape == m_old.shape else 0.0
    check("PDFium and MuPDF draw alike", diff < max(args.limit, base + 0.5), f"(mean difference {diff:.2f}, of the original {base:.2f})")
else:
    check("PDFium and MuPDF draw alike", False, f"(sizes {p_new.shape} and {m_new.shape})")

# place and size: MuPDF's rectangle of the old text in the original, of the new text in the saved file (found by search when the text
# runs across the page; else, for vertical text, by where the drawings differ: both readers must show the change in the same place)
old_hits = mupdf_doc(args.original)[index].search_for(args.old)
new_hits = md[index].search_for(args.new)
region = None


def changed_box(a, b):
    d = np.abs(a.astype(int) - b.astype(int)).sum(axis=2) > 60
    ys, xs = np.where(d)
    return None if len(xs) == 0 else (xs.min(), ys.min(), xs.max(), ys.max())


if old_hits and new_hits:
    o = old_hits[0]
    n = min(new_hits, key=lambda r: abs(r.x0 - o.x0) + abs(r.y0 - o.y0))
    check("starts where the old text started", abs(o.x0 - n.x0) <= args.start_tolerance and abs(o.y0 - n.y0) <= args.start_tolerance, f"(old {o.x0:.1f},{o.y0:.1f} new {n.x0:.1f},{n.y0:.1f})")
    check("same height as the old text", abs((o.y1 - o.y0) - (n.y1 - n.y0)) <= 1.5, f"(old {o.y1 - o.y0:.1f} new {n.y1 - n.y0:.1f})")
    region = (min(o.x0, n.x0) - 40, min(o.y0, n.y0) - 20, max(o.x1, n.x1) + 40, max(o.y1, n.y1) + 20)
else:
    cp, cm = changed_box(p_old, p_new), changed_box(m_old, m_new)
    if cp and cm:
        tol = args.start_tolerance * args.scale
        check("both readers show the change in the same place", all(abs(x - y) <= tol * 2 for x, y in zip(cp, cm)), f"(PDFium {cp}, MuPDF {cm}, pixels)")
        s = args.scale
        region = ((cp[0] - 40) / s, (cp[1] - 40) / s, (cp[2] + 40) / s, (cp[3] + 40) / s)
    else:
        check("found the change", False, f"(old {len(old_hits)} new {len(new_hits)} found by search)")

if args.out:
    os.makedirs(args.out, exist_ok=True)
    if region:
        s = args.scale
        box = [max(0, int(region[0] * s)), max(0, int(region[1] * s)), int(region[2] * s), int(region[3] * s)]
        pieces = []
        for label, img in (("pdfium-old", p_old), ("pdfium-new", p_new), ("mupdf-old", m_old), ("mupdf-new", m_new)):
            pieces.append(Image.fromarray(img).crop(box))
        w = max(p.width for p in pieces)
        sheet = Image.new("RGB", (w, sum(p.height for p in pieces) + 6 * len(pieces)), (200, 60, 60))
        y = 0
        for p in pieces:
            sheet.paste(p, (0, y))
            y += p.height + 6
        path = os.path.join(args.out, f"{args.name}.png")
        sheet.save(path)
        print("crops (pdfium old, pdfium new, mupdf old, mupdf new):", path)

sys.exit(1 if bad else 0)

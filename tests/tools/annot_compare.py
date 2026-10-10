"""Check annotations that qingpdf saved against the other readers (step 4a).

For a file and the same file after qingpdf added annotations to it (an incremental update), draw the pages of both with
PDFium (pypdfium2) and MuPDF (PyMuPDF). The pages that differ must differ in the same place and in the same colour for both
readers: that is what "the annotation is visible, in the right place and colour" means. Prints, for each page that changed
and each reader, the box of the changed pixels (points on the page, from the top left) and the colour most of them have,
and the verdict. With --out, saves a picture (before | PDFium | MuPDF) of each changed page.

    python tests/tools/annot_compare.py original.pdf saved.pdf [--password pw] [--pages 8] [--scale 2] [--out folder]
        [--expect-colour 255,235,0] [--expect-box left,top,right,bottom]

It exits with 1 when a reader shows nothing, or the readers disagree about the place or the colour, or the place or colour
differs from what --expect-* says. A development check, not part of `cargo test`: it needs pypdfium2, PyMuPDF, numpy and
Pillow.
"""

import argparse
import os
import sys

import numpy as np
import pymupdf
import pypdfium2 as pdfium
from PIL import Image

ap = argparse.ArgumentParser()
ap.add_argument("original")
ap.add_argument("saved")
ap.add_argument("--password", default="")
ap.add_argument("--pages", type=int, default=8)
ap.add_argument("--scale", type=float, default=2.0)
ap.add_argument("--out", default="")
ap.add_argument("--expect-colour", default="")
ap.add_argument("--expect-box", default="")
args = ap.parse_args()


def pdfium_pages(path, n, scale):
    doc = pdfium.PdfDocument(path, password=args.password or None)
    out = []
    for i in range(min(n, len(doc))):
        page = doc[i]
        bitmap = page.render(scale=scale, may_draw_forms=False)
        out.append(np.asarray(bitmap.to_pil().convert("RGB")))
    return out


def mupdf_pages(path, n, scale):
    doc = pymupdf.open(path)
    if doc.needs_pass:
        if not doc.authenticate(args.password):
            # (MuPDF does not take every password PDFium takes, a Chinese one for revision 4 among them.)
            return None
    out = []
    for i in range(min(n, doc.page_count)):
        pix = doc[i].get_pixmap(matrix=pymupdf.Matrix(scale, scale), alpha=False)
        out.append(np.frombuffer(pix.samples, dtype=np.uint8).reshape(pix.height, pix.width, 3))
    return out


def change(before, after):
    """What differs between two drawings of a page: the box (points, left top right bottom), the mask, and the commonest colours of the
    changed pixels (a list of (colour, pixels)); None if they are the same."""
    if before.shape != after.shape:
        return ("size", before.shape, after.shape)
    mask = np.abs(before.astype(int) - after.astype(int)).max(axis=2) > 12
    if not mask.any():
        return None
    ys, xs = np.nonzero(mask)
    box = [xs.min() / args.scale, ys.min() / args.scale, (xs.max() + 1) / args.scale, (ys.max() + 1) / args.scale]
    pixels = after[mask] // 32 * 32 + 16
    values, counts = np.unique(pixels, axis=0, return_counts=True)
    order = counts.argsort()[::-1][:4]
    return box, mask, [([int(c) for c in values[k]], int(counts[k])) for k in order]


def iou(a, b):
    x0, y0, x1, y1 = max(a[0], b[0]), max(a[1], b[1]), min(a[2], b[2]), min(a[3], b[3])
    inter = max(0, x1 - x0) * max(0, y1 - y0)
    union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter
    return inter / union if union > 0 else 0


def near(a, b, tolerance=48):
    return max(abs(x - y) for x, y in zip(a, b)) <= tolerance


readers = {"PDFium": (pdfium_pages(args.original, args.pages, args.scale), pdfium_pages(args.saved, args.pages, args.scale))}
_mu = (mupdf_pages(args.original, args.pages, args.scale), mupdf_pages(args.saved, args.pages, args.scale))
if _mu[0] is not None and _mu[1] is not None:
    readers["MuPDF"] = _mu
else:
    print("MuPDF cannot open this file with that password: only PDFium is checked")
failures = []
count = min(len(r[0]) for r in readers.values())
seen_any = False
for i in range(count):
    found = {}
    for name, (before, after) in readers.items():
        found[name] = change(before[i], after[i])
    if all(v is None for v in found.values()):
        continue
    seen_any = True
    line = f"page {i + 1}:"
    for name, v in found.items():
        if v is None:
            line += f"  {name}: nothing changed"
            failures.append(f"page {i + 1}: {name} shows no change")
        elif v[0] == "size":
            line += f"  {name}: size changed {v[1]} -> {v[2]}"
            failures.append(f"page {i + 1}: {name} page size changed")
        else:
            box, mask, colours = v
            line += f"\n    {name}: box [{box[0]:.0f} {box[1]:.0f} {box[2]:.0f} {box[3]:.0f}], {int(mask.sum())} px, colours {[c for c, _ in colours]}"
    print(line)
    a, b = found["PDFium"], found.get("MuPDF")
    if a and b and a[0] != "size" and b[0] != "size":
        agree = iou(a[0], b[0])
        both = int((a[1] & b[1]).sum())
        shape = both / max(1, int((a[1] | b[1]).sum()))
        # The commonest colour of each reader is among the commonest of the other (they may count the edge pixels differently, and
        # PDFium smooths the text a little differently on a page that has a blend mode).
        same_colours = any(near(a[2][0][0], d) for d, _ in b[2]) and any(near(b[2][0][0], d) for d, _ in a[2])
        print(f"    the boxes agree {agree:.2f}, the changed pixels agree {shape:.2f}, same colours: {same_colours}")
        if agree < 0.8:
            failures.append(f"page {i + 1}: the readers put it in different places ({agree:.2f})")
        if shape < 0.6:
            failures.append(f"page {i + 1}: the readers do not draw the same shapes ({shape:.2f})")
        if not same_colours:
            failures.append(f"page {i + 1}: the readers colour it differently ({[c for c, _ in a[2]]} vs {[c for c, _ in b[2]]})")
        if args.expect_colour:
            want = [int(x) for x in args.expect_colour.split(",")]
            for name, v in found.items():
                if not any(near(c, want, 40) for c, _ in v[2]):
                    failures.append(f"page {i + 1}: {name} colours {[c for c, _ in v[2]]} do not include {want}")
        if args.expect_box:
            want = [float(x) for x in args.expect_box.split(",")]
            for name, v in found.items():
                if max(abs(x - y) for x, y in zip(v[0], want)) > 4:
                    failures.append(f"page {i + 1}: {name} box {[round(x) for x in v[0]]} is not {want}")
    if args.out:
        os.makedirs(args.out, exist_ok=True)
        pieces = [readers["PDFium"][0][i], readers["PDFium"][1][i]] + ([readers["MuPDF"][1][i]] if "MuPDF" in readers else [])
        picture = np.concatenate([p for p in pieces], axis=1)
        Image.fromarray(picture).save(os.path.join(args.out, f"{os.path.basename(args.saved)}-p{i + 1}.png"))
if not seen_any:
    failures.append("no page differs: neither reader shows the annotations")
for f in failures:
    print("FAIL:", f)
if not failures:
    print("OK: both readers show the annotations, in the same place and colour")
sys.exit(1 if failures else 0)

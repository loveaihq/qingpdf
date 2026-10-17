"""Check shapes and changed annotations that qingpdf saved against the other readers (the annotation work of 2026-10-17).

`cargo test -p qingpdf-core --release --test annot_edit` leaves a `*.json` record for each file it saved in `tests/out/annot_edit/`: the file
before the annotation was added (or with it taken out), the file saved, the page, and the boxes (points, from the top left of the page as
shown) where the two must differ, with the colour the difference should have there. This draws both files with PDFium (pypdfium2) and
MuPDF (PyMuPDF) and checks, for each reader:

- in every box there is a difference (the annotation is drawn, at that place);
- a `tight` box is where the difference ends (a shape fills its rectangle: IoU of the two boxes at least 0.7);
- the difference lies in the boxes: at most 0.1 % of the page's pixels differ outside them;
- the colour: at least 12 % of the pixels that differ in a box are near the colour expected (a few are anti-aliased, a fill is lighter).

    python tests/tools/annot_edit_compare.py [folder] [--scale 2] [--out folder]

It exits with 1 when a reader fails a check. A development check, not part of `cargo test`: it needs pypdfium2, PyMuPDF, numpy and Pillow.
"""

import argparse
import glob
import json
import os
import sys

import numpy as np
import pymupdf
import pypdfium2 as pdfium

ap = argparse.ArgumentParser()
ap.add_argument("folder", nargs="?", default=os.path.join(os.path.dirname(__file__), "..", "out", "annot_edit"))
ap.add_argument("--scale", type=float, default=2.0)
ap.add_argument("--out", default="")
args = ap.parse_args()


def pdfium_page(path, page, password):
    doc = pdfium.PdfDocument(path, password=password or None)
    return np.asarray(doc[page].render(scale=args.scale, may_draw_forms=False).to_pil().convert("RGB"))


def mupdf_page(path, page, password):
    doc = pymupdf.open(path)
    if doc.needs_pass and not doc.authenticate(password):
        return None
    pix = doc[page].get_pixmap(matrix=pymupdf.Matrix(args.scale, args.scale), alpha=False)
    return np.frombuffer(pix.samples, dtype=np.uint8).reshape(pix.height, pix.width, 3)


def iou(a, b):
    x0, y0, x1, y1 = max(a[0], b[0]), max(a[1], b[1]), min(a[2], b[2]), min(a[3], b[3])
    inter = max(0, x1 - x0) * max(0, y1 - y0)
    union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter
    return inter / union if union > 0 else 0.0


def check(name, reader, before, after, boxes):
    """Returns a list of complaints."""
    bad = []
    if before.shape != after.shape:
        return [f"{reader}: the two drawings are not the same size"]
    s = args.scale
    h, w, _ = before.shape
    mask = np.abs(before.astype(int) - after.astype(int)).max(axis=2) > 12
    inside = np.zeros_like(mask)
    for k, b in enumerate(boxes):
        x0, y0, x1, y1 = b["box"]
        pad = 4
        px0, py0, px1, py1 = max(0, int((x0 - pad) * s)), max(0, int((y0 - pad) * s)), min(w, int((x1 + pad) * s) + 1), min(h, int((y1 + pad) * s) + 1)
        inside[py0:py1, px0:px1] = True
        region = mask[py0:py1, px0:px1]
        n = int(region.sum())
        if n < 10:
            bad.append(f"{reader}: box {k} {b['box']}: only {n} pixels differ there")
            continue
        ys, xs = np.nonzero(region)
        got = [(xs.min() + px0) / s, (ys.min() + py0) / s, (xs.max() + 1 + px0) / s, (ys.max() + 1 + py0) / s]
        if b.get("tight") and iou(got, b["box"]) < 0.7:
            bad.append(f"{reader}: box {k}: the difference ends at {[round(v, 1) for v in got]}, expected near {[round(v, 1) for v in b['box']]}")
        if b.get("colour"):
            pixels = after[py0:py1, px0:px1][region].astype(int)
            near = (np.abs(pixels - np.array(b["colour"])).max(axis=1) <= 90).mean()
            if near < 0.12:
                values, counts = np.unique(pixels // 32 * 32 + 16, axis=0, return_counts=True)
                top = values[counts.argsort()[::-1][0]].tolist()
                bad.append(f"{reader}: box {k}: only {near:.0%} of the pixels are near {b['colour']} (the commonest is {top})")
    outside = int((mask & ~inside).sum())
    if outside > 0.001 * w * h:
        bad.append(f"{reader}: {outside} pixels differ outside the boxes")
    return bad


failed = 0
records = sorted(glob.glob(os.path.join(args.folder, "*.json")))
if not records:
    print("no records: run the cargo test first")
    sys.exit(1)
checked = 0
for rec in records:
    m = json.load(open(rec, encoding="utf-8"))
    results = []
    for reader, draw in (("PDFium", pdfium_page), ("MuPDF", mupdf_page)):
        try:
            before = draw(m["base"], m["page"], m["password"])
            after = draw(m["saved"], m["page"], m["password"])
        except Exception as e:  # a reader that cannot open the file at all
            results.append(f"{reader}: could not draw it: {e}")
            continue
        if before is None or after is None:
            continue  # (MuPDF does not take every password PDFium takes.)
        results += check(m["name"], reader, before, after, m["boxes"])
        if args.out:
            os.makedirs(args.out, exist_ok=True)
            from PIL import Image
            Image.fromarray(np.concatenate([before, after], axis=1)).save(os.path.join(args.out, f"{m['name']}-{reader}.png"))
    checked += 1
    if results:
        failed += 1
        print(f"FAIL {m['name']}")
        for r in results:
            print("   ", r)
    else:
        print(f"ok   {m['name']}  ({len(m['boxes'])} box{'es' if len(m['boxes']) != 1 else ''})")
print(f"{checked} records, {failed} failed")
sys.exit(1 if failed else 0)

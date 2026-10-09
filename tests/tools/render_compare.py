"""Check qingpdf's output against Chrome's PDF engine (PDFium).

For every PDF in tests/corpus, rewrite it with `qingpdf split --pages 1-`
and render the first pages of the original and the copy with PDFium.
They must look the same. This is a development check, not part of
`cargo test`: it needs Python with pypdfium2, numpy and Pillow.

    python tests/tools/render_compare.py [path/to/qingpdf.exe]

Second mode (layer 3): `--engine-compare` renders the pages of every corpus file
with `qingpdf render` and with PDFium at the same dpi, and reports the mean pixel difference
page by page. Text pages are compared too (step 3b). Pages on which qingpdf could not get the
glyph of some characters and drew an outline box instead are counted apart, and so are the
pages that use something of step 3c; the boxes are tallied by cause. The box count comes from
the warning `page N: K characters are drawn as outline boxes (...)` that `qingpdf render` prints.

    python tests/tools/render_compare.py --engine-compare [path/to/qingpdf.exe]
        [--dpi 72] [--max-pages 8] [--threshold 3.0] [--only substring] [--no-annots]

Annotations are drawn on both sides (step 3c2-3: `qingpdf render` by default; PDFium with FPDF_ANNOT, and with its form
fill environment so that it draws form fields too). `--no-annots` leaves them out on both sides, which is the comparison
made before that step.
"""

import argparse
import glob
import os
import re
import subprocess
import sys
import tempfile
import time

import numpy as np
import pypdfium2 as pdfium

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", "corpus"))

_ap = argparse.ArgumentParser()
_ap.add_argument("qingpdf", nargs="?", default=os.path.normpath(os.path.join(HERE, "..", "..", "target", "release", "qingpdf.exe")))
_ap.add_argument("--engine-compare", action="store_true")
_ap.add_argument("--dpi", type=float, default=72.0)
_ap.add_argument("--max-pages", type=int, default=8)
_ap.add_argument("--threshold", type=float, default=3.0)
_ap.add_argument("--only", default="")
_ap.add_argument("--no-annots", action="store_true", help="draw no annotations, on either side")
_ap.add_argument("--all", action="store_true", help="list every page at or over the threshold, not just the worst 10")
_ap.add_argument("--save", default="", help="folder to put side by side pictures (ours | PDFium | difference) of the pages over the threshold")
ARGS = _ap.parse_args()
QINGPDF = ARGS.qingpdf
MAX_PAGES = 8
SCALE = 0.5          # 36 dpi is enough to see a missing glyph or image
THRESHOLD = 0.5      # mean absolute difference per pixel, 0-255 grey levels

# Differences already investigated (2026-10-06): PDFium is the odd one out.
KNOWN = {
    os.path.join("public", "page-tree", "Pages-tree-refs.pdf"):
        "PDFium trusts /Count and lists a page inside a loop that does not exist; it cannot load it",
    os.path.join("public", "page-tree", "cropped_no_overlap.pdf"):
        "CropBox lies outside MediaBox; pypdfium2 refuses to render the original and the copy alike",
    os.path.join("local", "linearized", "linearized_bug_1055.pdf"):
        "PDFium does not draw the annotations of this damaged original; MuPDF does, and our copy matches MuPDF",
}


def render(path, pages):
    doc = pdfium.PdfDocument(path)
    images = [np.asarray(doc[i].render(scale=SCALE).to_pil().convert("L"), dtype=np.int16) for i in pages]
    count = len(doc)
    doc.close()
    return count, images


def main():
    results = {"same": [], "differs": [], "known PDFium differences": [], "refused by qingpdf": [],
               "PDFium cannot open original": []}
    with tempfile.TemporaryDirectory() as tmp:
        copy = os.path.join(tmp, "copy.pdf")
        for path in sorted(glob.glob(os.path.join(ROOT, "**", "*.pdf"), recursive=True)):
            rel = os.path.relpath(path, ROOT)
            try:
                doc = pdfium.PdfDocument(path)
                count = len(doc)
                doc.close()
            except Exception:
                results["PDFium cannot open original"].append(rel)
                continue
            run = subprocess.run([QINGPDF, "split", path, "--pages", "1-", "-o", copy, "--force"],
                                 capture_output=True, text=True, encoding="utf-8", errors="replace")
            if run.returncode != 0 or count == 0:
                results["refused by qingpdf"].append(f"{rel}: {(run.stderr or run.stdout).strip()[:120] or 'no pages'}")
                continue
            if rel in KNOWN:
                results["known PDFium differences"].append(f"{rel}: {KNOWN[rel]}")
                continue
            pages = list(range(min(count, MAX_PAGES)))
            try:
                _, original = render(path, pages)
                copy_count, copied = render(copy, pages)
            except Exception as e:
                results["differs"].append(f"{rel}: PDFium render error: {e}")
                continue
            worst = max(float(np.abs(a - b).mean()) if a.shape == b.shape else 999.0
                        for a, b in zip(original, copied))
            line = f"{rel}: pages {count} -> {copy_count}, worst mean pixel difference {worst:.2f}"
            results["same" if copy_count == count and worst < THRESHOLD else "differs"].append(line)
    for name, items in results.items():
        print(f"--- {name}: {len(items)}")
        if name != "same":
            for item in items:
                print("   ", item)
    return 1 if results["differs"] else 0


def engine_compare():
    """Our pixels against PDFium's, page by page, for the pages that have no characters drawn as boxes.

    Pages on which qingpdf says something is "not drawn yet" (JBIG2 and JPEG 2000 images, patterns,
    shadings, blend modes and soft masks: step 3c) are compared too, but counted apart: they are
    expected to differ."""
    from PIL import Image
    rows = []          # (diff, rel, page, note, uses 3c features, boxes)
    causes = [0, 0, 0] # boxes by cause: no font, no character, not in the font
    box_files = {}     # file -> boxes
    failed = []        # files qingpdf did not render, or PDFium did not open
    started = time.time()
    with tempfile.TemporaryDirectory() as tmp:
        for path in sorted(glob.glob(os.path.join(ROOT, "**", "*.pdf"), recursive=True)):
            rel = os.path.relpath(path, ROOT)
            if ARGS.only and ARGS.only not in rel:
                continue
            try:
                doc = pdfium.PdfDocument(path)
                if not ARGS.no_annots:
                    # The form fill environment is what makes PDFium draw form fields (widgets); with the
                    # annotation flag alone it leaves them out.
                    doc.init_forms()
                count = len(doc)
            except Exception:
                failed.append(f"{rel}: PDFium cannot open it")
                continue
            for i in range(min(count, ARGS.max_pages)):
                n = i + 1
                out = os.path.join(tmp, "page.png")
                if os.path.exists(out):
                    os.remove(out)
                run = subprocess.run([QINGPDF, "render", path, "--pages", str(n), "--dpi", str(ARGS.dpi), "-o", out, "--force"] + (["--no-annots"] if ARGS.no_annots else []),
                                     capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=300)
                if run.returncode != 0:
                    message = (run.stderr or run.stdout).strip().splitlines()
                    failed.append(f"{rel} page {n}: qingpdf: {message[-1][:140] if message else 'failed'}")
                    break
                boxes = 0
                m = re.search(r"page \d+: (\d+) characters are drawn as outline boxes \(no font for them: (\d+); code with no character: (\d+); character not in the font: (\d+)\)", run.stderr)
                if m:
                    boxes = int(m.group(1))
                    for k in range(3):
                        causes[k] += int(m.group(2 + k))
                    box_files[rel] = box_files.get(rel, 0) + boxes
                later = "not drawn yet" in run.stderr or "grey block" in run.stderr
                try:
                    ours = np.asarray(Image.open(out).convert("RGB"), dtype=np.int16)
                    ref = np.asarray(doc[i].render(scale=ARGS.dpi / 72.0, may_draw_forms=not ARGS.no_annots, draw_annots=not ARGS.no_annots).to_pil().convert("RGB"), dtype=np.int16)
                except Exception as e:
                    failed.append(f"{rel} page {n}: {e}")
                    continue
                if abs(ours.shape[0] - ref.shape[0]) > 1 or abs(ours.shape[1] - ref.shape[1]) > 1:
                    rows.append((999.0, rel, n, f"size {ours.shape[1]}x{ours.shape[0]} against PDFium's {ref.shape[1]}x{ref.shape[0]}", later, boxes))
                    continue
                h, w = min(ours.shape[0], ref.shape[0]), min(ours.shape[1], ref.shape[1])
                diff = float(np.abs(ours[:h, :w] - ref[:h, :w]).mean())
                rows.append((diff, rel, n, "uses 3c features: " + "; ".join(sorted({l.split(": ", 1)[-1][:60] for l in run.stderr.splitlines() if "not drawn yet" in l or "grey block" in l})) if later else (f"{boxes} boxes" if boxes else ""), later, boxes))
                if ARGS.save and diff >= ARGS.threshold:
                    os.makedirs(ARGS.save, exist_ok=True)
                    delta = np.clip(255 - np.abs(ours[:h, :w] - ref[:h, :w]) * 2, 0, 255).astype(np.uint8)
                    side = np.concatenate([ours[:h, :w].astype(np.uint8), ref[:h, :w].astype(np.uint8), delta], axis=1)
                    Image.fromarray(side).save(os.path.join(ARGS.save, rel.replace(os.sep, "_") + f"_p{n}.png"))
            doc.close()
    print(f"engine compare at {ARGS.dpi:g} dpi, {time.time() - started:.0f} s")
    print(f"pages compared: {len(rows)}   files with a problem: {len(failed)}")
    print(f"characters drawn as boxes on these pages: {sum(causes)} (no font for them: {causes[0]}; code with no character: {causes[1]}; character not in the font: {causes[2]})")
    for rel, n in sorted(box_files.items(), key=lambda kv: -kv[1])[:12]:
        print(f"    {n:6d} boxes  {rel}")
    allrows = rows
    mean_all = sum(r[0] for r in rows if not r[4]) / max(1, len([r for r in rows if not r[4]]))
    print(f"mean pixel difference over all pages without 3c features (boxes included): {mean_all:.3f}")
    for title, group in (("pages drawn completely (no boxes, no 3c features)", [r for r in rows if not r[4] and not r[5]]), ("pages with some characters drawn as boxes (no 3c features)", [r for r in rows if not r[4] and r[5]]), ("pages that use 3c features (expected to differ)", [r for r in rows if r[4]])):
        group.sort(key=lambda r: -r[0])
        print(f"--- {title}: {len(group)}")
        if not group:
            continue
        mean = sum(r[0] for r in group) / len(group)
        over = [r for r in group if r[0] >= ARGS.threshold]
        print(f"mean pixel difference {mean:.3f} (0-255); pages at or over {ARGS.threshold:g}: {len(over)}")
        print("pages over the threshold:" if ARGS.all else "worst 10:")
        for diff, rel, n, note, _, _ in (over if ARGS.all else group[:10]):
            print(f"  {diff:7.2f}  {rel} page {n} {note}")
        if ARGS.all and over:
            # The same pages by file: to explain them a file at a time.
            by_file = {}
            for diff, rel, n, note, _, _ in over:
                by_file.setdefault(rel, []).append(diff)
            print("over the threshold, by file (pages over / pages compared, worst, mean of those over):")
            totals = {}
            for r in group:
                totals[r[1]] = totals.get(r[1], 0) + 1
            for rel, diffs in sorted(by_file.items(), key=lambda kv: -max(kv[1])):
                print(f"  {len(diffs):2d}/{totals.get(rel, 0):2d}  worst {max(diffs):6.2f}  mean {sum(diffs) / len(diffs):5.2f}  {rel}")
    for line in failed:
        print("  problem:", line)
    return 0


if __name__ == "__main__":
    sys.exit(engine_compare() if ARGS.engine_compare else main())

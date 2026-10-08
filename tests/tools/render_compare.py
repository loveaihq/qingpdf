"""Check qingpdf's output against Chrome's PDF engine (PDFium).

For every PDF in tests/corpus, rewrite it with `qingpdf split --pages 1-`
and render the first pages of the original and the copy with PDFium.
They must look the same. This is a development check, not part of
`cargo test`: it needs Python with pypdfium2, numpy and Pillow.

    python tests/tools/render_compare.py [path/to/qingpdf.exe]

Second mode (layer 3, step 3a): `--engine-compare` renders the pages of every corpus file
with `qingpdf render` and with PDFium at the same dpi, and reports the mean pixel difference
page by page. Pages on which qingpdf drew characters as outline boxes (every font but
Type 3, until step 3b) are left out: PDFium draws real letters there. The box count comes from
the warning `page N: K characters are drawn as outline boxes` that `qingpdf render` prints.

    python tests/tools/render_compare.py --engine-compare [path/to/qingpdf.exe]
        [--dpi 72] [--max-pages 8] [--threshold 3.0] [--only substring]
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
    rows = []          # (diff, rel, page, note, uses 3c features)
    skipped_text = 0   # pages left out because they have letters drawn as boxes
    failed = []        # files qingpdf did not render, or PDFium did not open
    started = time.time()
    with tempfile.TemporaryDirectory() as tmp:
        for path in sorted(glob.glob(os.path.join(ROOT, "**", "*.pdf"), recursive=True)):
            rel = os.path.relpath(path, ROOT)
            if ARGS.only and ARGS.only not in rel:
                continue
            try:
                doc = pdfium.PdfDocument(path)
                count = len(doc)
            except Exception:
                failed.append(f"{rel}: PDFium cannot open it")
                continue
            for i in range(min(count, ARGS.max_pages)):
                n = i + 1
                out = os.path.join(tmp, "page.png")
                if os.path.exists(out):
                    os.remove(out)
                run = subprocess.run([QINGPDF, "render", path, "--pages", str(n), "--dpi", str(ARGS.dpi), "-o", out, "--force"],
                                     capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=300)
                if run.returncode != 0:
                    message = (run.stderr or run.stdout).strip().splitlines()
                    failed.append(f"{rel} page {n}: qingpdf: {message[-1][:140] if message else 'failed'}")
                    break
                if re.search(r"characters are drawn as outline boxes", run.stderr):
                    skipped_text += 1
                    continue
                later = "not drawn yet" in run.stderr or "grey block" in run.stderr
                try:
                    ours = np.asarray(Image.open(out).convert("RGB"), dtype=np.int16)
                    ref = np.asarray(doc[i].render(scale=ARGS.dpi / 72.0, may_draw_forms=False, draw_annots=False).to_pil().convert("RGB"), dtype=np.int16)
                except Exception as e:
                    failed.append(f"{rel} page {n}: {e}")
                    continue
                if abs(ours.shape[0] - ref.shape[0]) > 1 or abs(ours.shape[1] - ref.shape[1]) > 1:
                    rows.append((999.0, rel, n, f"size {ours.shape[1]}x{ours.shape[0]} against PDFium's {ref.shape[1]}x{ref.shape[0]}", later))
                    continue
                h, w = min(ours.shape[0], ref.shape[0]), min(ours.shape[1], ref.shape[1])
                diff = float(np.abs(ours[:h, :w] - ref[:h, :w]).mean())
                rows.append((diff, rel, n, "uses 3c features: " + "; ".join(sorted({l.split(": ", 1)[-1][:60] for l in run.stderr.splitlines() if "not drawn yet" in l or "grey block" in l})) if later else "", later))
                if ARGS.save and diff >= ARGS.threshold:
                    os.makedirs(ARGS.save, exist_ok=True)
                    delta = np.clip(255 - np.abs(ours[:h, :w] - ref[:h, :w]) * 2, 0, 255).astype(np.uint8)
                    side = np.concatenate([ours[:h, :w].astype(np.uint8), ref[:h, :w].astype(np.uint8), delta], axis=1)
                    Image.fromarray(side).save(os.path.join(ARGS.save, rel.replace(os.sep, "_") + f"_p{n}.png"))
            doc.close()
    print(f"engine compare at {ARGS.dpi:g} dpi, {time.time() - started:.0f} s")
    print(f"pages compared: {len(rows)}   left out because letters are boxes: {skipped_text}   files with a problem: {len(failed)}")
    for title, group in (("pages with 3a features only", [r for r in rows if not r[4]]), ("pages that use 3c features (expected to differ)", [r for r in rows if r[4]])):
        group.sort(key=lambda r: -r[0])
        print(f"--- {title}: {len(group)}")
        if not group:
            continue
        mean = sum(r[0] for r in group) / len(group)
        over = [r for r in group if r[0] >= ARGS.threshold]
        print(f"mean pixel difference {mean:.3f} (0-255); pages at or over {ARGS.threshold:g}: {len(over)}")
        print("pages over the threshold:" if ARGS.all else "worst 10:")
        for diff, rel, n, note, _ in (over if ARGS.all else group[:10]):
            print(f"  {diff:7.2f}  {rel} page {n} {note}")
    for line in failed:
        print("  problem:", line)
    return 0


if __name__ == "__main__":
    sys.exit(engine_compare() if ARGS.engine_compare else main())

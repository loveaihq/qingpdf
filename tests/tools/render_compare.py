"""Check qingpdf's output against Chrome's PDF engine (PDFium).

For every PDF in tests/corpus, rewrite it with `qingpdf split --pages 1-`
and render the first pages of the original and the copy with PDFium.
They must look the same. This is a development check, not part of
`cargo test`: it needs Python with pypdfium2, numpy and Pillow.

    python tests/tools/render_compare.py [path/to/qingpdf.exe]
"""

import glob
import os
import subprocess
import sys
import tempfile

import numpy as np
import pypdfium2 as pdfium

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", "corpus"))
QINGPDF = sys.argv[1] if len(sys.argv) > 1 else os.path.normpath(
    os.path.join(HERE, "..", "..", "target", "release", "qingpdf.exe"))
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


if __name__ == "__main__":
    sys.exit(main())

"""Check the files the bookmark tests saved (2026-10-20) against PDFium and PyMuPDF: the same titles, levels and pages.

`cargo test -p qingpdf-core --test bookmarks` leaves its files and a `manifest.json` in tests/out/bookmarks. For each case of the manifest
this reads the saved file's outline with PDFium (pypdfium2 `get_toc`) and with PyMuPDF (`get_toc`) and checks it against what the test says
it must be: for each bookmark its level (from 1), its title and its page (from 1; 0 for a bookmark that leads nowhere, which PDFium and
PyMuPDF do not agree about, so only the title and the level are compared for it).

    python tests/tools/bookmarks_check.py [tests/out/bookmarks/manifest.json]

It exits with 1 after printing all the results if a case failed. A development check, not part of `cargo test`: it needs pypdfium2 and
PyMuPDF.
"""

import json
import os
import sys

import pymupdf
import pypdfium2 as pdfium

sys.stdout.reconfigure(encoding="utf-8", errors="replace")


def pdfium_toc(path, password):
    doc = pdfium.PdfDocument(path, password=password or None)
    out = []
    for item in doc.get_toc():
        # PDFium counts levels from 0 and pages from 0 (no destination, or none it can follow: None).
        dest = item.get_dest()
        index = dest.get_index() if dest is not None else None
        out.append([item.level + 1, item.get_title(), (index + 1) if index is not None else 0])
    doc.close()
    return out


def mupdf_toc(path, password):
    doc = pymupdf.open(path)
    if doc.needs_pass:
        doc.authenticate(password)
    # PyMuPDF counts pages from 1, -1 when the bookmark leads nowhere.
    out = [[lvl, title, page if page > 0 else 0] for lvl, title, page in doc.get_toc(simple=True)]
    doc.close()
    return out


def same(got, expect):
    if len(got) != len(expect):
        return f"{len(got)} bookmarks, expected {len(expect)}"
    for k, (g, e) in enumerate(zip(got, expect)):
        if g[0] != e[0] or g[1] != e[1] or (e[2] and g[2] != e[2]):
            return f"bookmark {k}: got {g}, expected {e}"
    return ""


def main():
    manifest = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "out", "bookmarks", "manifest.json")
    with open(manifest, encoding="utf-8") as f:
        cases = json.load(f)
    failed = 0
    for case in cases:
        problems = []
        for name, reader in (("PDFium", pdfium_toc), ("PyMuPDF", mupdf_toc)):
            try:
                problem = same(reader(case["output"], case["password"]), case["expect"])
            except Exception as e:  # a reader could not open the file: that is a failure
                problem = f"{type(e).__name__}: {e}"
            if problem:
                problems.append(f"{name}: {problem}")
        print(f"{'FAIL' if problems else 'ok  '} {case['label']} ({len(case['expect'])} bookmarks){': ' + '; '.join(problems) if problems else ''}")
        failed += bool(problems)
    print(f"{len(cases) - failed} of {len(cases)} cases agree with PDFium and PyMuPDF")
    sys.exit(1 if failed else 0)


main()

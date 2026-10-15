"""Check the files the page tests saved (2026-10-15) against PDFium: pages turned, taken out, moved and put in.

`cargo test -p qingpdf-core --test pages` leaves its files and a `manifest.json` in tests/out/pages. For each case of the manifest this
opens the file the test started from (and the file pages were put in from), and the file the test saved, with PDFium (pypdfium2), and
checks that the saved file has the pages the test says, in the order it says: the same number of pages; each page the page of the
original (or of the inserted file) it should be, told by what is on it (its text) and by its size; and each page turned as far as it
was turned (PDFium's own rotation of the page: the page's /Rotate added to the turn the test made).

    python tests/tools/pages_check.py [tests/out/pages/manifest.json]

It exits with 1 on the first failed case after printing all the results. A development check, not part of `cargo test`: it needs
pypdfium2.
"""

import hashlib
import json
import os
import sys

import pypdfium2 as pdfium

sys.stdout.reconfigure(encoding="utf-8", errors="replace")


def fingerprints(path, password):
    """For each page: (the characters of its text, sorted (the order PDFium reads a turned page in is another one) and boiled down to a hash;
    its size sorted so that a turn does not matter; its rotation)."""
    doc = pdfium.PdfDocument(path, password=password or None)
    out = []
    for i in range(len(doc)):
        page = doc[i]
        w, h = page.get_size()
        text = "".join(sorted("".join(page.get_textpage().get_text_range().split())))
        out.append((hashlib.md5(text.encode("utf-8")).hexdigest()[:12], tuple(sorted((round(w), round(h)))), page.get_rotation()))
        page.close()
    doc.close()
    return out


def check(case):
    label = case["label"]
    original = fingerprints(case["original"], case["original_password"])
    inserted = fingerprints(case["insert"], case["insert_password"]) if case["insert"] else []
    output = fingerprints(case["output"], case["output_password"])
    expect = case["expect"]
    if len(output) != len(expect):
        return f"{len(output)} pages, expected {len(expect)}"
    for k, ((kind, index, turn), got) in enumerate(zip(expect, output)):
        source = (original if kind == "o" else inserted)[index]
        want = (source[0], source[1], (source[2] + turn) % 360)
        if got != want:
            return f"page {k}: PDFium sees {got}, expected {want} (page {index} of the {'original' if kind == 'o' else 'inserted file'}, turned {turn})"
    return ""


def main():
    manifest = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "..", "out", "pages", "manifest.json")
    with open(manifest, encoding="utf-8") as f:
        cases = json.load(f)
    failed = 0
    for case in cases:
        try:
            problem = check(case)
        except Exception as e:  # PDFium could not open a file: that is a failure
            problem = f"{type(e).__name__}: {e}"
        pages = len(case["expect"])
        print(f"{'FAIL' if problem else 'ok  '} {case['label']} ({pages} pages){': ' + problem if problem else ''}")
        failed += bool(problem)
    print(f"{len(cases) - failed} of {len(cases)} cases agree with PDFium")
    sys.exit(1 if failed else 0)


main()

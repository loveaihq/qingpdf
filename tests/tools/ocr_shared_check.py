"""Check with PDFium that pages which share their resources each have the text layer of their own.

`cargo test --release -p qingpdf-core --test ocr` leaves `tests/out/ocr/sharing-ocr.pdf` (six pages: two share a /Resources object, two
inherit one, two share the /XObject dictionary of their own /Resources, each given a different recognised word). PDFium must read each
page's own word from it.

    python tests/tools/ocr_shared_check.py [tests/out/ocr/sharing-ocr.pdf]

Exits with 1 when a page has another page's word. A development check, not part of `cargo test`: it needs pypdfium2.
"""

import os
import sys

import pypdfium2 as pdfium

path = sys.argv[1] if len(sys.argv) > 1 else os.path.join("tests", "out", "ocr", "sharing-ocr.pdf")
names = ["ALPHA", "BRAVO", "CHARLIE", "DELTA", "ECHO", "FOXTROT"]
doc = pdfium.PdfDocument(path)
bad = False
for i in range(len(doc)):
    text = doc[i].get_textpage().get_text_range().strip()
    ok = text == names[i]
    bad |= not ok
    print(f"page {i + 1}: PDFium reads {text!r}, wanted {names[i]!r}: {'ok' if ok else 'WRONG'}")
sys.exit(1 if bad else 0)

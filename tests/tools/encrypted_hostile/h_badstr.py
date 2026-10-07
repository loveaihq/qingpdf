import sys
sys.path.insert(0, ".")
from encpdf import Enc, build
for kind in ["aes-r4", "aes-r6"]:
    enc = Enc(kind)
    objs = {1: b"<< /Type /Catalog /Pages 2 0 R /Outlines 8 0 R >>", 2: b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Contents 4 0 R /PieceInfo << /X << /Private (plain) >> >> >>",
            4: (b"<< >>", b"BT /F1 12 Tf 10 10 Td (page one) Tj ET", None),
            5: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Contents 9 0 R >>",
            9: (b"<< >>", b"BT /F1 12 Tf 10 10 Td (page two) Tj ET", None),
            8: b"<< /Type /Outlines /First 10 0 R /Last 10 0 R /Count 1 >>",
            10: b"<< /Title (plain title) /Parent 8 0 R /Dest [3 0 R /Fit] >>",
            6: enc.dict_text()}
    open(f"h/badstr-{kind}.pdf", "wb").write(build(objs, None, enc, encrypt_num=6, xref_stream=False))

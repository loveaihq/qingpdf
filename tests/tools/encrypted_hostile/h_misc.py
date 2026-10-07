import sys
sys.path.insert(0, ".")
from encpdf import Enc, build, aes_cbc
import os
CONTENT = b"BT /F1 12 Tf 10 10 Td (no padding here) Tj ET\n" * 3
CONTENT = CONTENT + b" " * (-len(CONTENT) % 16)
for kind in ["aes-r4", "aes-r6"]:
    enc = Enc(kind)
    # C: content stream encrypted without the PKCS#7 padding block
    key = enc.objkey(4, 0, enc.method)
    nopad = aes_cbc(key, os.urandom(16), CONTENT, pad=False)
    objs = {1: b"<< /Type /Catalog /Pages 2 0 R >>", 2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Contents 4 0 R >>",
            4: (b"<< >>", nopad, "none"), 6: enc.dict_text(),
            7: b"<< /Title <<S:title>> >>"}
    open(f"h/nopad-{kind}.pdf", "wb").write(build(objs, None, enc, encrypt_num=6, trailer_extra=b"/Info 7 0 R", xref_stream=False))
    # D: signature dictionary whose /ByteRange is an indirect object; /Contents is raw (not encrypted)
    objs = {1: b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [8 0 R] /SigFlags 3 >> >>",
            2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Annots [8 0 R] >>",
            6: enc.dict_text(),
            8: b"<< /Type /Annot /Subtype /Widget /FT /Sig /Rect [0 0 0 0] /P 3 0 R /T <<S:Sig1>> /V 9 0 R >>",
            9: b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange 10 0 R /Contents <3082DEADBEEF00000000> /M <<S:D:20240101>> >>",
            10: b"[0 100 200 300]"}
    open(f"h/sigref-{kind}.pdf", "wb").write(build(objs, None, enc, encrypt_num=6, xref_stream=False))

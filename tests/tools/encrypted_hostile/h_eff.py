import sys
sys.path.insert(0, ".")
from encpdf import Enc, build
for user in [b"", b"u"]:
    enc = Enc("aes-r4", user=user)
    d = enc.dict_text().replace(b"/StmF /StdCF /StrF /StdCF", b"/StmF /Identity /StrF /Identity /EFF /StdCF").replace(b"/AuthEvent /DocOpen", b"/AuthEvent /EFOpen")
    ATT = b"ATTACHMENT BODY " * 10
    objs = {1: b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(a.txt) 13 0 R] >> >> >>",
            2: b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>",
            3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Contents 4 0 R >>",
            4: (b"<< >>", b"BT /F1 12 Tf 10 10 Td (page one plain) Tj ET", "none"),
            5: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 100] /Contents 12 0 R >>",
            12: (b"<< /Filter [/Crypt] /DecodeParms [<< /Type /CryptFilterDecodeParms /Name /StdCF >>] >>", b"BT /F1 12 Tf 10 10 Td (page two crypt filter) Tj ET", "aesv2"),
            11: (b"<< /Type /EmbeddedFile >>", ATT, "aesv2"),
            13: b"<< /Type /Filespec /F (a.txt) /UF (a.txt) /EF << /F 11 0 R >> >>",
            6: d}
    # strings are Identity: write them plain -> no <<S:>> used
    tag = "empty" if not user else "user"
    open(f"h/eff-{tag}.pdf", "wb").write(build(objs, None, enc, encrypt_num=6, xref_stream=False))
    # build() with enc=None encrypts nothing: encrypt by hand the two streams
    import re
    data = open(f"h/eff-{tag}.pdf", "rb").read()
    for num, plain in [(11, ATT), (12, b"BT /F1 12 Tf 10 10 Td (page two crypt filter) Tj ET")]:
        pass

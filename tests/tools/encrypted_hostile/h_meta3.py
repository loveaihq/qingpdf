import sys, re
sys.path.insert(0, ".")
from encpdf import Enc, build
XMP = b'<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/">CLEAR METADATA TEXT</x:xmpmeta><?xpacket end="w"?>'
for kind in ["rc4-v4", "aes-r4", "aes-r6"]:
    enc = Enc(kind, encrypt_metadata=False)
    objs = {
        4: (b"<< >>", b"BT /F1 12 Tf 10 10 Td (hi) Tj ET", None),
        5: (b"<< /Type /Metadata /Subtype /XML >>", XMP, "none"),
        6: enc.dict_text(),
        7: b"<< /Title <<S:secret title>> >>",
    }
    objs[1] = b"<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>"
    objstms = {10: [
                    (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
                    (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>")]}
    data = build(objs, objstms, enc, encrypt_num=6, trailer_extra=b"/Info 7 0 R")
    open(f"h/meta3-{kind}.pdf", "wb").write(data)
    i = data.rfind(b"startxref")
    m = re.match(rb"startxref\s+(\d+)", data[i:])
    open(f"h/meta3-{kind}-broken.pdf", "wb").write(data[:i] + b"startxref\n7" + data[i + m.end():])

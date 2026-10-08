"""A tiny PDF writer for the synthetic test pages (nothing but the standard library).

    pdf = Pdf()
    page = pdf.page(300, 300, content_bytes, resources="<< ... >>")
    pdf.obj("<< ... >>")                       # -> object number
    pdf.stream("/Type /XObject ...", data)      # -> object number
    pdf.save("out.pdf")

Objects are numbered as they are added; `page` writes the page and its content stream. Written by the qingpdf
project (MIT OR Apache-2.0) for its own tests; there is no third-party content in the files it makes.
"""

import zlib


class Pdf:
    def __init__(self):
        self.objects = {}   # number -> bytes (the body after "N 0 obj")
        self.next = 3       # 1 is the catalog, 2 the page tree
        self.pages = []
        self.catalog_extra = ""

    def reserve(self):
        n = self.next
        self.next += 1
        return n

    def obj(self, body, number=None):
        n = number or self.reserve()
        self.objects[n] = body.encode("latin-1") if isinstance(body, str) else body
        return n

    def stream(self, entries, data, number=None, compress=True):
        n = number or self.reserve()
        if isinstance(data, str):
            data = data.encode("latin-1")
        if compress:
            data = zlib.compress(data, 6)
            entries = entries + " /Filter /FlateDecode"
        head = f"<< {entries} /Length {len(data)} >>".encode("latin-1")
        self.objects[n] = head + b"\nstream\n" + data + b"\nendstream"
        return n

    def page(self, width, height, content, resources="<< >>", extra=""):
        c = self.stream("", content)
        p = self.obj(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Resources {resources} /Contents {c} 0 R {extra} >>")
        self.pages.append(p)
        return p

    def save(self, path):
        kids = " ".join(f"{p} 0 R" for p in self.pages)
        self.objects[1] = f"<< /Type /Catalog /Pages 2 0 R {self.catalog_extra} >>".encode("latin-1")
        self.objects[2] = f"<< /Type /Pages /Kids [{kids}] /Count {len(self.pages)} >>".encode("latin-1")
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets = {}
        for n in sorted(self.objects):
            offsets[n] = len(out)
            out += f"{n} 0 obj\n".encode() + self.objects[n] + b"\nendobj\n"
        top = max(self.objects) + 1
        xref = len(out)
        out += f"xref\n0 {top}\n".encode() + b"0000000000 65535 f \n"
        for n in range(1, top):
            out += (f"{offsets[n]:010d} 00000 n \n" if n in offsets else "0000000000 65535 f \n").encode()
        out += f"trailer\n<< /Size {top} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
        with open(path, "wb") as f:
            f.write(out)


def circle(cx, cy, r):
    """Path operators for a circle of four Bezier curves."""
    k = 0.5522847498 * r
    return (f"{cx + r:.3f} {cy:.3f} m "
            f"{cx + r:.3f} {cy + k:.3f} {cx + k:.3f} {cy + r:.3f} {cx:.3f} {cy + r:.3f} c "
            f"{cx - k:.3f} {cy + r:.3f} {cx - r:.3f} {cy + k:.3f} {cx - r:.3f} {cy:.3f} c "
            f"{cx - r:.3f} {cy - k:.3f} {cx - k:.3f} {cy - r:.3f} {cx:.3f} {cy - r:.3f} c "
            f"{cx + k:.3f} {cy - r:.3f} {cx + r:.3f} {cy - k:.3f} {cx + r:.3f} {cy:.3f} c h ")

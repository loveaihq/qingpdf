"""Check `qingpdf redact` (item 8: redaction that really takes the content out) against other readers: six kinds of file, each redacted, then looked at
the way a person who wanted the content back would look.

    python tests/tools/redact_check.py [--qingpdf target/release/qingpdf.exe] [--qpdf path/to/qpdf.exe] [--out folder]

The six files: a Chinese official document (the phrase is text), an English paper (text), a scanned page whose picture and whose invisible text layer
(text rendering mode 3, in a form XObject) both say the phrase, a page whose text is in a form XObject that another page draws too, an AES-256 file
(opened with the owner password), and a page with a colour photograph (JPEG). For each, after the redaction:

  (a) the text of the page is without the phrase, as `qingpdf text`, PDFium (pypdfium2) and PyMuPDF read it;
  (b) a scan of everything in the output file (every object's dictionary, every stream decoded, decrypted when the file is) finds the phrase in no
      shape: not as UTF-8, Latin-1, UTF-16BE, as hexadecimal of those, nor as the codes the fonts use for it (found through their /ToUnicode); the
      same scan finds it in the original (so that the scan is known to work); for the page whose form another page draws too, only what the
      redacted page draws is scanned;
  (c) under a mark on a picture the pixels PDFium draws are the colour of the box;
  (d) the rest of the page looks as it did: the mean difference of the pixels outside the marks (as PDFium draws before and after) is small, and the
      other page that shares the form still has its text;
  (e) `qpdf --check` passes;
  (f) the picture as it was is in no stream of the new file (a piece of its bytes, raw and decoded);
  (g) the redaction warns that the fonts' /ToUnicode tables still list the characters that were taken out (they are not cut down).

Then the cases of the review of 2026-10-21 (`review_cases`): small files made for each way the content could be left in the file (marked content
words inline and through /Properties and in the structure tree, a page's own metadata, a reply to a note that goes, what notes and fields say, text in a
font that is not there, a page of several streams, text in a pattern that nothing paints now, attachments and scripts, a font without widths), each
redacted and looked at the same way: the needle is out of every stream and string of the new file, or the redaction is refused with a reason.

It exits with 1 when a check fails. A development check, not part of `cargo test`: it needs pypdfium2, PyMuPDF, numpy and Pillow, and qpdf (the
encrypted file is made with it).
"""

import argparse
import io
import os
import random
import re
import subprocess
import sys

import numpy as np
import pymupdf
import pypdfium2 as pdfium
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)
from pdfmaker import Pdf  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--qingpdf", default=os.path.join(ROOT, "target", "release", "qingpdf.exe"))
ap.add_argument("--qpdf", default=os.path.join("C:\\", "Dev", "tools", "qpdf", "bin", "qpdf.exe"))
ap.add_argument("--out", default=os.path.join(ROOT, "tests", "out", "redact"))
ap.add_argument("--scale", type=float, default=1.5)
args = ap.parse_args()
os.makedirs(args.out, exist_ok=True)
pymupdf.TOOLS.mupdf_display_errors(False)
FAILED = []


def check(case, name, ok, detail=""):
    print(f"  {name}: {'ok' if ok else 'FAILED'} {detail}")
    if not ok:
        FAILED.append(f"{case}: {name}")


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


# --- reading a file the way someone who wants the content back would ------------------------------------------------------------------------------

ESCAPES = {ord("n"): 10, ord("r"): 13, ord("t"): 9, ord("b"): 8, ord("f"): 12, ord("("): 40, ord(")"): 41, 92: 92}


def unescape_literal(body):
    out = bytearray()
    i = 0
    while i < len(body):
        c = body[i]
        if c == 92 and i + 1 < len(body):
            n = body[i + 1]
            if n in ESCAPES:
                out.append(ESCAPES[n])
                i += 2
            elif 0x30 <= n <= 0x37:
                j, v = i + 1, 0
                while j < len(body) and j < i + 4 and 0x30 <= body[j] <= 0x37:
                    v = v * 8 + body[j] - 0x30
                    j += 1
                out.append(v & 255)
                i = j
            elif n in (10, 13):
                i += 2
            else:
                out.append(n)
                i += 2
        else:
            out.append(c)
            i += 1
    return bytes(out)


STRING = re.compile(rb"\((?:\\.|[^\\()]|\((?:\\.|[^\\()])*\))*\)|<([0-9A-Fa-f\s]*)>", re.S)


def strings_of(data):
    """The bytes of every string (literal or hexadecimal) in a stream of content or in the text of an object, in order."""
    out = []
    for m in STRING.finditer(data):
        s = m.group(0)
        if s.startswith(b"("):
            out.append(unescape_literal(s[1:-1]))
        else:
            hexa = re.sub(rb"\s", b"", m.group(1))
            if len(hexa) % 2:
                hexa += b"0"
            try:
                out.append(bytes.fromhex(hexa.decode()))
            except ValueError:
                pass
    return out


def to_unicode_maps(doc):
    """For each /ToUnicode stream of the file: character -> the codes its font shows it with."""
    maps = []
    for x in range(1, doc.xref_length()):
        try:
            if "/ToUnicode" not in doc.xref_object(x, compressed=False):
                continue
            ref = doc.xref_get_key(x, "ToUnicode")
        except Exception:
            continue
        if ref[0] != "xref":
            continue
        try:
            cmap = doc.xref_stream(int(ref[1].split()[0])).decode("latin-1")
        except Exception:
            continue
        codes = {}
        for blk in re.findall(r"beginbfchar(.*?)endbfchar", cmap, re.S):
            for src, dst in re.findall(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>", blk):
                try:
                    ch = bytes.fromhex(dst).decode("utf-16-be")
                except Exception:
                    continue
                codes.setdefault(ch, set()).add(bytes.fromhex(src))
        for blk in re.findall(r"beginbfrange(.*?)endbfrange", cmap, re.S):
            for lo, hi, dst in re.findall(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>", blk):
                lo_i, hi_i, base = int(lo, 16), int(hi, 16), int(dst, 16)
                if hi_i - lo_i > 70000:
                    continue
                for k in range(hi_i - lo_i + 1):
                    try:
                        ch = chr(base + k)
                    except ValueError:
                        continue
                    codes.setdefault(ch, set()).add((lo_i + k).to_bytes(len(lo) // 2, "big"))
        maps.append(codes)
    return maps


def patterns_of(phrase, maps):
    """Every shape the phrase may be in: name -> bytes to look for. (Typesetting programs leave the spaces to the spacing of the letters, so the phrase
    is also looked for without them.)"""
    pats = {}
    for tag, text in (("", phrase), ("nospace:", re.sub(r"\s+", "", phrase))):
        pats[tag + "utf8"] = text.encode("utf-8")
        pats[tag + "utf16be"] = text.encode("utf-16-be")
        pats[tag + "utf16be-bom"] = b"\xfe\xff" + text.encode("utf-16-be")
        try:
            pats[tag + "latin1"] = text.encode("latin-1")
        except UnicodeEncodeError:
            pass
        # The codes of a font: through its /ToUnicode, each character's codes (the first and the last of them), one font at a time.
        for k, codes in enumerate(maps):
            seq = [sorted(codes.get(ch, [])) for ch in text]
            if text and all(seq):
                for pick in (0, -1):
                    pats[f"{tag}font{k}-code{pick}"] = b"".join(q[pick] for q in seq)
    for name, p in list(pats.items()):
        pats["hex:" + name] = p.hex().encode()
    return pats


def reachable(doc, index):
    """The objects of one page: its contents and the forms it draws."""
    seen = set(doc[index].get_contents())
    seen |= {x[0] for x in doc[index].get_xobjects()}
    return seen


def scan_file(path, phrase, password="", only_page=None):
    """Names of the shapes the phrase was found in anywhere in the file (empty: nowhere); with `only_page`, only in what that page draws."""
    found = []
    raw = open(path, "rb").read()
    doc = pymupdf.open(path)
    if doc.needs_pass:
        doc.authenticate(password)
    pats = patterns_of(phrase, to_unicode_maps(doc))
    limit = reachable(doc, only_page) if only_page is not None else None

    def look(blob, where, joined):
        flat = re.sub(rb"\s", b"", blob).lower()
        for name, p in pats.items():
            if p in blob or (name.startswith("hex:") and p.lower() in flat) or p in joined:
                found.append(f"{name} in {where}")

    if limit is None:
        look(raw, "the file's bytes", b"")
    for x in range(1, doc.xref_length()):
        if limit is not None and x not in limit:
            continue
        try:
            text = doc.xref_object(x, compressed=False).encode("latin-1", "replace")
        except Exception:
            text = b""
        stream = b""
        try:
            if doc.xref_is_stream(x):
                stream = doc.xref_stream(x) or b""
        except Exception:
            pass
        for label, blob in (("object %d" % x, text), ("stream of %d" % x, stream)):
            if blob:
                look(blob, label, b"".join(strings_of(blob)))
    return found


def page_texts(path, index, password=""):
    """The text of a page as qingpdf, PDFium and PyMuPDF read it."""
    tmp = os.path.join(args.out, "_t.txt")
    cmd = [args.qingpdf, "text", path, "--pages", str(index + 1), "--force", "-o", tmp] + (["--password", password] if password else [])
    r = run(cmd)
    ours = open(tmp, encoding="utf-8").read() if r.returncode == 0 else ""
    theirs = pdfium.PdfDocument(path, password=password or None)[index].get_textpage().get_text_range()
    md = pymupdf.open(path)
    if md.needs_pass:
        md.authenticate(password)
    return ours, theirs, md[index].get_text()


def squash(t):
    return re.sub(r"\s+", "", t)


def render(path, index, password="", scale=1.5):
    pd = pdfium.PdfDocument(path, password=password or None)
    return np.asarray(pd[index].render(scale=scale).to_pil().convert("RGB")).astype(np.int16)


def qpdf_check(path, password=""):
    cmd = [args.qpdf, "--check"] + (["--password=" + password] if password else []) + [path]
    r = run(cmd)
    return r.returncode in (0, 3) and "No syntax or stream encoding errors" in r.stdout, (r.stdout + r.stderr)[-300:]


def outside(mask_boxes, shape, scale, page_h, pad=3):
    """A boolean image that is true outside the boxes (points from the lower left corner: x0 y0 x1 y1), grown by `pad` pixels."""
    m = np.ones(shape[:2], dtype=bool)
    for x0, y0, x1, y1 in mask_boxes:
        a, b = int(x0 * scale) - pad, int((page_h - y1) * scale) - pad
        c, d = int(x1 * scale) + pad, int((page_h - y0) * scale) + pad
        m[max(b, 0):max(d, 0), max(a, 0):max(c, 0)] = False
    return m


def found_boxes(path, index, phrase, password=""):
    """Where MuPDF finds the phrase on the page, as boxes (points from the lower left corner)."""
    md = pymupdf.open(path)
    if md.needs_pass:
        md.authenticate(password)
    page = md[index]
    h = page.rect.height
    return [(r.x0, h - r.y1, r.x1, h - r.y0) for r in page.search_for(phrase)]


def do_redact(src, out, extra, password=""):
    return run([args.qingpdf, "redact", src, "-o", out, "--force"] + extra + (["--password", password] if password else []))


# --- the files -------------------------------------------------------------------------------------------------------------------------------

def make_scan(path):
    """A scanned page: a grey JPEG (150 dpi A4) with the phrase written on it, and the invisible text layer a recogniser would add (in a form)."""
    w, h = 1240, 1754
    img = Image.new("L", (w, h), 245)
    d = ImageDraw.Draw(img)
    try:
        f = ImageFont.truetype("arial.ttf", 44)
    except OSError:
        f = ImageFont.load_default()
    rnd = random.Random(3)
    for y in range(160, 1500, 90):
        d.text((100, y), " ".join(rnd.choice(["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]) for _ in range(7)), fill=40, font=f)
    d.text((100, 700), "SECRET-ID 440105199001011234 end", fill=0, font=f)
    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=85)
    pdf = Pdf()
    im = pdf.stream(f"/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode", buf.getvalue(), compress=False)
    k = 595.28 / w
    # The text layer: Helvetica 21 points high at the place of the phrase (the picture's row 752 is, from the bottom, 841.89 - 752 * k).
    base = 841.89 - 752 * k
    layer = pdf.stream("/Type /XObject /Subtype /Form /BBox [0 0 595.28 841.89] /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
                       f"BT 3 Tr /F1 21 Tf 48 {base:.2f} Td (SECRET-ID 440105199001011234 end) Tj ET")
    pdf.page(595.28, 841.89, "q 595.28 0 0 841.89 0 0 cm /Im Do Q /Ocr Do", resources=f"<< /XObject << /Im {im} 0 R /Ocr {layer} 0 R >> >>")
    pdf.save(path)
    # The mark: the whole line, from a little left of the phrase to a little right of the layer's text, and a bit above and below.
    return [(44, base - 8, 400, base + 24)]


def make_photo(path):
    w, h = 800, 600
    rnd = np.random.default_rng(5)
    y, x = np.mgrid[0:h, 0:w]
    arr = np.stack([(x * 255 / w), (y * 255 / h), ((x + y) * 127 / (w + h) * 2)], axis=-1) + rnd.normal(0, 6, (h, w, 3))
    img = Image.fromarray(np.clip(arr, 0, 255).astype(np.uint8), "RGB")
    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=90)
    pdf = Pdf()
    im = pdf.stream(f"/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode", buf.getvalue(), compress=False)
    pdf.page(420, 330, "q 400 0 0 300 10 15 cm /Im Do Q", resources=f"<< /XObject << /Im {im} 0 R >> >>")
    pdf.save(path)
    return [(110, 100, 260, 200)]


def make_shared_form(path):
    pdf = Pdf()
    # Helvetica with its widths: a standard font without them has widths that every program has to guess.
    widths = [278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
              1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
              222, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584]
    font = pdf.obj("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 32 /LastChar 126 /Widths [" + " ".join(map(str, widths)) + "] >>")
    form = pdf.stream(f"/Type /XObject /Subtype /Form /BBox [0 0 300 80] /Resources << /Font << /F1 {font} 0 R >> >>", "BT /F1 18 Tf 10 50 Td (FORMSECRET alpha beta) Tj 0 -24 Td (and a line that stays) Tj ET")
    res = f"<< /XObject << /Fm {form} 0 R >> /Font << /F1 {font} 0 R >> >>"
    pdf.page(400, 300, "BT /F1 12 Tf 20 270 Td (page one) Tj ET q 1 0 0 1 40 150 cm /Fm Do Q", resources=res)
    pdf.page(400, 300, "BT /F1 12 Tf 20 270 Td (page two) Tj ET q 1 0 0 1 40 150 cm /Fm Do Q", resources=res)
    pdf.save(path)
    # The mark: "FORMSECRET alpha" on page one (Helvetica 18 points: 175.03 points wide from x = 40 + 10), a little above and below.
    return [(46, 194, 225.2, 220)]



# --- what the review of 2026-10-21 found: small files made for each, redacted, and looked at the same way ---------------------------------------------

NEEDLE = "SECRETNEEDLE"
TEXT = f"BT /F1 12 Tf 50 200 Td ({NEEDLE}) Tj ET\n"


def courier(pdf):
    """Courier with its widths (every letter 600): where the letters are is known to every program."""
    return pdf.obj("<< /Type /Font /Subtype /Type1 /BaseFont /Courier /FirstChar 32 /LastChar 126 /Widths [" + " ".join(["600"] * 95) + "] /Encoding /WinAnsiEncoding >>")


def expand(path):
    """The file with every stream decoded, as qpdf shows it."""
    r = subprocess.run([args.qpdf, "--qdf", "--object-streams=disable", "--stream-data=uncompress", "--no-original-object-ids", path, "-"], capture_output=True)
    return r.stdout or open(path, "rb").read()


def needle_in(path, needle=NEEDLE):
    """The shapes of the needle (plain, UTF-16BE, and the hexadecimal of both) that are in the file, case ignored."""
    data = expand(path).lower()
    forms = [needle.lower().encode(), needle.encode("utf-16-be").lower(), needle.encode().hex().encode(), needle.encode("utf-16-be").hex().encode()]
    return [i for i, f in enumerate(forms) if f in data]


def review(name, build, area="1:40,190,130,25", find=None, extra=(), refused=None, in_source=True):
    """Make the file, redact it (`area`, or `find`), and say whether the needle is out of the new file. `refused`: the words the error has to hold."""
    case = "review: " + name
    pdf = Pdf()
    build(pdf)
    src = os.path.join(args.out, "rv-" + name + ".pdf")
    out = os.path.join(args.out, "rv-" + name + ".out.pdf")
    pdf.save(src)
    if os.path.exists(out):
        os.remove(out)
    r = run([args.qingpdf, "redact", src] + (["--find", find] if find else ["--area", area]) + ["-o", out, "--force"] + list(extra))
    if refused:
        check(case, "is refused, with a reason", r.returncode != 0 and refused in r.stderr and not os.path.exists(out), r.stderr.strip()[:200])
        return None
    ok = r.returncode == 0 and os.path.exists(out)
    check(case, "redacted", ok, r.stderr.strip()[:200])
    if not ok:
        return None
    if in_source:
        check(case, "the needle is in the file before (so the scan works)", bool(needle_in(src)))
    found = needle_in(out)
    check(case, "the needle is in no shape in the new file", not found, str(found))
    q_ok, why = qpdf_check(out)
    check(case, "qpdf --check", q_ok, "" if q_ok else why)
    return r, out


def review_cases():
    print("== the review's cases")

    def page(pdf, content=TEXT, res="", extra="", objs=()):
        f = courier(pdf)
        for number, body in objs:
            pdf.obj(body, number=number)
        return pdf.page(300, 300, content, resources=f"<< /Font << /F1 {f} 0 R >> {res} >>", extra=extra)

    # Marked content: /ActualText, /Alt and /E, in the statement and through /Properties (a direct dictionary, and an object).
    review("props-actualtext", lambda p: page(p, "/Span /P1 BDC " + TEXT + "EMC\n", res=f"/Properties << /P1 << /ActualText ({NEEDLE}) >> >>"))
    review("props-object", lambda p: page(p, "/Span /P1 BDC " + TEXT + "EMC\n", res="/Properties << /P1 70 0 R >>", objs=[(70, f"<< /Alt ({NEEDLE}) >>")]))
    review("inline-alt", lambda p: page(p, f"/Span <</Alt ({NEEDLE})>> BDC " + TEXT + "EMC\n"))
    review("inline-e", lambda p: page(p, f"/Span <</E ({NEEDLE})>> BDC " + TEXT + "EMC\n"))

    def with_image(tag):
        def build(pdf):
            n = pdf.stream("/Type /XObject /Subtype /Image /Width 4 /Height 4 /ColorSpace /DeviceGray /BitsPerComponent 8", bytes(range(16)), number=71)
            page(pdf, f"/Figure <</{tag} ({NEEDLE})>> BDC q 100 0 0 30 50 190 cm /Im1 Do Q EMC\n", res=f"/XObject << /Im1 {n} 0 R >>")
        return build

    review("figure-alt-on-a-picture", with_image("Alt"))
    review("actualtext-on-a-picture", with_image("ActualText"))

    def struct(pdf):
        pg = page(pdf)
        root, el = pdf.reserve(), pdf.reserve()
        pdf.obj(f"<< /Type /StructTreeRoot /K {el} 0 R >>", number=root)
        pdf.obj(f"<< /Type /StructElem /S /P /P {root} 0 R /Pg {pg} 0 R /K 0 /Alt ({NEEDLE}) /ActualText ({NEEDLE}) >>", number=el)
        pdf.catalog_extra += f"/StructTreeRoot {root} 0 R"

    review("structure-tree", struct)

    def page_meta(pdf):
        m = pdf.stream("/Type /Metadata /Subtype /XML", f"<x>{NEEDLE}</x>", compress=False)
        page(pdf, extra=f"/Metadata {m} 0 R")

    review("page-metadata", page_meta)

    # Notes: a reply to a note that goes; a find that looks at what annotations and fields say (and what their appearances draw).
    def reply(pdf):
        pdf.obj("<< /Type /Annot /Subtype /Text /Rect [60 197 70 207] /Contents (a note) >>", number=80)
        pdf.obj(f"<< /Type /Annot /Subtype /Text /Rect [250 50 262 62] /Contents (reply: {NEEDLE}) /IRT 80 0 R /RT /R >>", number=81)
        page(pdf, extra="/Annots [80 0 R 81 0 R]")

    review("reply", reply)

    def annots(pdf):
        f = courier(pdf)
        pdf.stream(f"/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 {f} 0 R >> >>", f"BT /F1 12 Tf 2 5 Td ({NEEDLE}) Tj ET", number=80)
        pdf.obj(f"<< /Type /Annot /Subtype /Widget /FT /Tx /T (f1) /V ({NEEDLE}) /Rect [50 100 150 120] /AP << /N 80 0 R >> /P 3 0 R /F 4 >>", number=81)
        pdf.obj(f"<< /Type /Annot /Subtype /FreeText /Rect [50 50 150 70] /Contents ({NEEDLE}) /DA (/F1 12 Tf 0 g) /AP << /N 80 0 R >> /F 4 >>", number=82)
        pdf.obj(f"<< /Type /Annot /Subtype /Highlight /Rect [50 20 150 40] /QuadPoints [50 40 150 40 50 20 150 20] /Contents ({NEEDLE}) /AP << /N 80 0 R >> /F 4 >>", number=83)
        pdf.page(300, 300, TEXT, resources=f"<< /Font << /F1 {f} 0 R >> >>", extra="/Annots [81 0 R 82 0 R 83 0 R]")
        pdf.catalog_extra += "/AcroForm << /Fields [81 0 R] >>"

    review("find-in-notes-and-fields", annots, find="secretneedle")

    # Text that cannot be read is not passed over: no font, a font that the page does not have.
    review("font-missing", lambda p: page(p, "BT /F9 12 Tf 50 200 Td (" + NEEDLE + ") Tj ET\n"), refused="not in the resources")
    review("font-none", lambda p: page(p, "BT 50 200 Td (" + NEEDLE + ") Tj ET\n"), refused="not in the resources")

    # A page whose content is in several streams, a statement from one into the next.
    def split(pdf):
        f = courier(pdf)
        a = pdf.stream("", f"BT /F1 12 Tf 50 200 Td ({NEEDLE})")
        b = pdf.stream("", " Tj ET")
        pdf.pages.append(pdf.obj(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 {f} 0 R >> >> /Contents [{a} 0 R {b} 0 R] >>"))

    review("split-streams", split)

    # Text in a pattern that nothing is painted with any more.
    def pattern(pdf):
        f = courier(pdf)
        pdf.stream(f"/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 150 30] /XStep 150 /YStep 30 /Resources << /Font << /F1 {f} 0 R >> >>", f"BT /F1 12 Tf 5 10 Td ({NEEDLE}) Tj ET", number=50)
        pdf.page(300, 300, "/Pattern cs /P1 scn 40 190 130 25 re f\n", resources="<< /Pattern << /P1 50 0 R >> >>")

    review("pattern-text", pattern)

    # What is left in the file: counted and said; taken out when asked.
    def extras(pdf):
        pdf.obj(f"<< /Type /Outlines /First 61 0 R /Last 61 0 R /Count 1 >>", number=60)
        pdf.obj(f"<< /Title ({NEEDLE}) /Parent 60 0 R >>", number=61)
        pdf.stream("/Type /EmbeddedFile", f"the file says {NEEDLE}", number=65)
        pdf.obj("<< /Type /Filespec /F (a.txt) /EF << /F 65 0 R >> >>", number=66)
        pdf.obj("<< /Names [(a.txt) 66 0 R] >>", number=67)
        pdf.obj(f"<< /S /JavaScript /JS (app.alert\\('{NEEDLE}'\\)) >>", number=68)
        pdf.obj("<< /Names [(js) 68 0 R] >>", number=69)
        page(pdf)
        pdf.catalog_extra += "/Outlines 60 0 R /Names << /EmbeddedFiles 67 0 R /JavaScript 69 0 R >>"

    pdf = Pdf()
    extras(pdf)
    src = os.path.join(args.out, "rv-extras.pdf")
    pdf.save(src)
    out = os.path.join(args.out, "rv-extras.out.pdf")
    r = run([args.qingpdf, "redact", src, "--area", "1:40,190,130,25", "-o", out, "--force"])
    check("review: extras", "the file's attachments and scripts are counted, and where the text is left is said", r.returncode == 0 and "1 attachment(s) and 1 script(s)" in r.stderr and "bookmarks" in r.stderr and "attachments" in r.stderr, r.stderr.strip()[:300])
    r = run([args.qingpdf, "redact", src, "--area", "1:40,190,130,25", "-o", out, "--force", "--remove-attachments"])
    data = expand(out)
    check("review: extras", "--remove-attachments takes the file and the script out", r.returncode == 0 and b"the file says" not in data and b"app.alert" not in data and "Took out" in r.stderr, r.stderr.strip()[:300])

    # A font without widths: every line that the mark crosses goes (where its letters are is a guess); the digits are where MuPDF puts them.
    def helvetica_without_widths(pdf):
        f = pdf.obj("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        pdf.page(400, 200, "BT /F1 12 Tf 20 100 Td (iiiiiiiiiiiiiiiiiiii SECRET NEEDLE ID 110105194912310021 llllllllll) Tj ET\n", resources=f"<< /Font << /F1 {f} 0 R >> >>")

    pdf = Pdf()
    helvetica_without_widths(pdf)
    src = os.path.join(args.out, "rv-helvetica.pdf")
    pdf.save(src)
    hit = pymupdf.open(src)[0].search_for("110105194912310021")[0]
    area = f"1:{hit.x0:.2f},{200 - hit.y1:.2f},{hit.x1 - hit.x0:.2f},{hit.y1 - hit.y0:.2f}"
    out = os.path.join(args.out, "rv-helvetica.out.pdf")
    r = run([args.qingpdf, "redact", src, "--area", area, "-o", out, "--force"])
    text = pymupdf.open(out)[0].get_text() if r.returncode == 0 else "?"
    check("review: font without widths", "no digit of the number is left (and the line is said to be taken whole)", r.returncode == 0 and not re.search(r"\d{2,}", text) and "no widths" in r.stderr, repr(text) + r.stderr.strip()[:120])


def main():
    corpus = os.path.join(ROOT, "tests", "corpus")
    gongwen = os.path.join(corpus, "public", "zh", "gongwen", "gongwen-2024-nicheng-work-report.pdf")
    paper = os.path.join(corpus, "local", "xref-classic", "tracemonkey.pdf")
    area = lambda b: ["--area", "1:%.2f,%.2f,%.2f,%.2f" % (b[0], b[1], b[2] - b[0], b[3] - b[1])]
    cases = []
    cases.append(dict(name="gongwen (Chinese text)", src=gongwen, page=2, phrase="全年税收收入预计完成", opts=["--find", "全年税收收入预计完成"], boxes=None, label="已涂黑"))
    cases.append(dict(name="English paper", src=paper, page=0, phrase="dynamic languages", opts=["--find", "dynamic languages"], boxes=None, label=""))
    scan = os.path.join(args.out, "case-scan.pdf")
    box = make_scan(scan)[0]
    cases.append(dict(name="scan with an invisible text layer", src=scan, page=0, phrase="SECRET-ID 440105199001011234", opts=area(box), boxes=[box], image_box=box, label=""))
    shared = os.path.join(args.out, "case-form.pdf")
    box = make_shared_form(shared)[0]
    cases.append(dict(name="text in a form that two pages draw", src=shared, page=0, phrase="FORMSECRET alpha", opts=area(box), boxes=[box], other_page=(1, "FORMSECRET"), label=""))
    enc = os.path.join(args.out, "case-aes256.pdf")
    r = run([args.qpdf, "--encrypt", "userpw", "ownerpw", "256", "--", gongwen, enc])
    assert r.returncode in (0, 3), r.stderr
    cases.append(dict(name="AES-256 file (owner password)", src=enc, page=2, phrase="全年税收收入预计完成", opts=["--find", "全年税收收入预计完成"], boxes=None, password="ownerpw", label=""))
    photo = os.path.join(args.out, "case-photo.pdf")
    box = make_photo(photo)[0]
    cases.append(dict(name="page with a photograph", src=photo, page=0, phrase="", opts=area(box), boxes=[box], image_box=box, label=""))

    # A Word paper whose pages have each their content in eight streams (statements run from one into the next): a page of several streams is written as one.
    word_paper = os.path.join(corpus, "public", "zh", "lunwen", "lunwen-arxiv-2401.01717-word.pdf")
    if os.path.exists(word_paper):
        shown = pymupdf.open(word_paper)
        page = next((i for i in range(shown.page_count) if "虚假新闻" in shown[i].get_text().replace("\n", "")), 0)
        cases.append(dict(name="Word paper with eight content streams to a page", src=word_paper, page=page, phrase="虚假新闻", opts=["--find", "虚假新闻"], boxes=None, label=""))

    for case in cases:
        name = case["name"]
        pw = case.get("password", "")
        print(f"== {name}")
        out = os.path.join(args.out, "out-" + re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-") + ".pdf")
        extra = list(case["opts"])
        if case.get("label"):
            extra += ["--label", case["label"]]
        r = do_redact(case["src"], out, extra, pw)
        print("  " + (r.stdout.strip().splitlines() or [""])[-1])
        if r.returncode != 0:
            check(name, "redacted", False, r.stderr.strip()[-300:])
            continue
        check(name, "redacted", True)
        phrase = case["phrase"]
        index = case["page"]
        if phrase:
            before = page_texts(case["src"], index, pw)
            after = page_texts(out, index, pw)
            had = [squash(phrase) in squash(t) for t in before]
            has = [squash(phrase) in squash(t) for t in after]
            check(name, "the phrase was there before", all(had), str(had))
            check(name, "(a) no reader finds the phrase (qingpdf, PDFium, PyMuPDF)", not any(has), f"after: {has}")
            only = index if "other_page" in case else None
            found = scan_file(out, phrase, pw, only_page=only)
            check(name, "(b) the scan finds the phrase in no shape" + (" (in what the page draws)" if only is not None else ""), not found, "; ".join(found[:4]))
            was = scan_file(case["src"], phrase, pw, only_page=only)
            check(name, "(b') the same scan does find it in the original (so the scan works)", bool(was), "found in: " + ("; ".join(was[:3]) or "nothing"))
        ok, why = qpdf_check(out, pw)
        check(name, "(e) qpdf --check", ok, "" if ok else why)
        if "全年税收" in phrase:
            # (g) the fonts keep the tables that list their characters (they are not cut down): the redaction says so.
            check(name, "(g) the redaction says that the fonts' /ToUnicode tables still list the characters taken out", "/ToUnicode" in r.stderr, r.stderr.strip()[-200:])
        # (c), (d): what PDFium draws.
        a = render(case["src"], index, pw, args.scale)
        b = render(out, index, pw, args.scale)
        page_h = a.shape[0] / args.scale
        boxes = case.get("boxes")
        if boxes is None:
            # The places of the phrase on the original page, as MuPDF finds them.
            boxes = found_boxes(case["src"], index, phrase, pw)
            check(name, "MuPDF finds the places of the phrase on the original page", bool(boxes), f"({len(boxes)} place(s))")
        mask = outside(boxes, a.shape, args.scale, page_h)
        mean = float(np.abs(a - b)[mask].mean()) if mask.any() else 0.0
        worst = int(np.abs(a - b)[mask].max()) if mask.any() else 0
        check(name, "(d) the rest of the page looks the same", mean < 0.8, f"(mean difference {mean:.3f} outside the marks, the biggest {worst}; {int(mask.sum())} pixels compared)")
        if "image_box" in case:
            x0, y0, x1, y1 = case["image_box"]
            r0, r1 = int((page_h - y1) * args.scale) + 3, int((page_h - y0) * args.scale) - 3
            c0, c1 = int(x0 * args.scale) + 3, int(x1 * args.scale) - 3
            inner = b[r0:r1, c0:c1].reshape(-1, 3)
            dark = (inner.max(axis=1) <= 4).mean()
            check(name, "(c) the pixels under the mark are the colour of the box", dark > 0.97, f"({dark:.3%} of {len(inner)} pixels are black)")
            # (f) the picture as it was is nowhere in the file: not by a name in the resources, not as an object nothing names. A piece of its
            # bytes (all of it: a piece of a page of flat grey is in the picture painted over too) is looked for in the file and in every stream (raw and
            # decoded) of the output.
            src_doc = pymupdf.open(case["src"])
            if src_doc.needs_pass:
                src_doc.authenticate(pw)
            pieces = []
            for im in src_doc[index].get_images(full=True):
                for raw in (src_doc.xref_stream_raw(im[0]) or b"", src_doc.xref_stream(im[0]) or b""):
                    if len(raw) > 600:
                        pieces.append(raw)
            out_doc = pymupdf.open(out)
            if out_doc.needs_pass:
                out_doc.authenticate(pw)
            blobs = [open(out, "rb").read()]
            for x in range(1, out_doc.xref_length()):
                try:
                    if out_doc.xref_is_stream(x):
                        blobs.append(out_doc.xref_stream(x) or b"")
                        blobs.append(out_doc.xref_stream_raw(x) or b"")
                except Exception:
                    pass
            leaked = [i for i, p in enumerate(pieces) if any(p in blob for blob in blobs)]
            check(name, "(f) the picture as it was is not in the output file", bool(pieces) and not leaked,
                  f"({len(pieces)} picture(s) looked for, {len(leaked)} found; the page names {len(out_doc[index].get_images(full=True))} picture(s))")
        if "other_page" in case:
            p2, word = case["other_page"]
            t2 = page_texts(out, p2, pw)
            check(name, "(d') the other page that draws the form still has its text", all(word in t for t in t2), str([word in t for t in t2]))
        Image.fromarray(b.astype(np.uint8)).save(os.path.join(args.out, "look-" + os.path.basename(out)[4:-4] + ".png"))
    review_cases()
    print()
    if FAILED:
        print("FAILED:\n  " + "\n  ".join(FAILED))
        sys.exit(1)
    print("all checks passed")


if __name__ == "__main__":
    main()

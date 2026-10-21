"""Check cropping, the pictures of pages, and the properties (item 9) against other readers.

    python tests/tools/page_tools_check.py [--qingpdf target/release/qingpdf.exe] [--qpdf path/to/qpdf.exe] [--out folder]

  crop      a Chinese official document (margins, and the white margins), a page turned a quarter, and an AES-256 file: the original bytes are the start
            of the new file, `qpdf --check` passes, PDFium (pypdfium2) reports the crop box that is expected and draws just the part of the page that
            is kept (the same pixels as the same part of the uncropped page), and PyMuPDF reads the same crop box. The white margins: what PDFium draws
            of a cropped page has its ink within a few millimetres of every edge.
  render    `qingpdf render` at 150 and 600 dpi, as PNG and as JPEG: the sizes are the page's, the picture is the page as PDFium draws it within the
            differences of two renderers, and the 600 dpi picture of an A4 page (more pixels than one drawing allows) is whole.
  images    a scan whose picture is one JPEG: `qingpdf images` writes the bytes of the stream, the same bytes PyMuPDF extracts; the other pictures are PNGs
            of the size PDFium and PyMuPDF know.
  meta      the title, author, subject and keywords are read back by PyMuPDF and PDFium, the dates and the other keys are as they were, the XMP parses as
            XML and says the same, the original bytes are the start of the file, `qpdf --check` passes.

It exits with 1 when a check fails. A development check, not part of `cargo test`: it needs pypdfium2, PyMuPDF, numpy and Pillow, and qpdf.
"""

import argparse
import io
import os
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

import numpy as np
import pymupdf
import pypdfium2 as pdfium
from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)
from pdfmaker import Pdf  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--qingpdf", default=os.path.join(ROOT, "target", "release", "qingpdf.exe"))
ap.add_argument("--qpdf", default=os.path.join("C:\\", "Dev", "tools", "qpdf", "bin", "qpdf.exe"))
ap.add_argument("--out", default=os.path.join(ROOT, "tests", "out", "page-tools"))
args = ap.parse_args()
os.makedirs(args.out, exist_ok=True)
pymupdf.TOOLS.mupdf_display_errors(False)
FAILED = []
MM = 72.0 / 25.4
CORPUS = os.path.join(ROOT, "tests", "corpus", "public")
GONGWEN = os.path.join(CORPUS, "zh", "gongwen", "gongwen-2024-ai-standards-guide.pdf")
SCAN = os.path.join(CORPUS, "zh", "gongwen", "gongwen-1954-gazette02-scan.pdf")
AES = os.path.join(CORPUS, "encrypted", "encrypted_hello_world_r6.pdf")
AES_PASSWORD = "h\u00f4tel"


def check(case, name, ok, detail=""):
    print(f"  {name}: {'ok' if ok else 'FAILED'} {detail}")
    if not ok:
        FAILED.append(f"{case}: {name}")


def run(cmd):
    return subprocess.run(cmd, capture_output=True)


def qingpdf(*a):
    r = run([args.qingpdf, *a])
    return r.returncode, r.stdout.decode("utf-8", "replace"), r.stderr.decode("utf-8", "replace")


def qpdf_check(path, password=""):
    cmd = [args.qpdf, "--check"]
    if password:
        cmd.append(f"--password={password}")
    r = run([*cmd, path])
    return r.returncode, (r.stdout + r.stderr).decode("utf-8", "replace")


def out(name):
    return os.path.join(args.out, name)


def prefix_ok(original, new):
    a, b = open(original, "rb").read(), open(new, "rb").read()
    return b.startswith(a) and len(b) > len(a)


def near(got, want, tol=0.05):
    return len(got) == len(want) and all(abs(g - w) <= tol for g, w in zip(got, want))


def render_pdfium(path, page, password=None, scale=1.0):
    pdf = pdfium.PdfDocument(path, password=password)
    return np.asarray(pdf[page].render(scale=scale).to_pil().convert("L"), dtype=np.int16)


def ink_box(img, threshold=235):
    ys, xs = np.where(img < threshold)
    return (xs.min(), ys.min(), xs.max() + 1, ys.max() + 1) if len(xs) else None


# --- crop -----------------------------------------------------------------------------------------------------------------------------------

print("crop: the gongwen, by margins")
margins = out("gongwen-margins.pdf")
code, text, err = qingpdf("crop", GONGWEN, "--margins", "10,15,20,25", "--pages", "1-3", "-o", margins, "--force")
check("crop", "exit code", code == 0, err.strip())
check("crop", "the original bytes are the start of the file", prefix_ok(GONGWEN, margins))
code, text = qpdf_check(margins)
check("crop", "qpdf --check", code == 0, text.strip()[:200])
orig, new = pdfium.PdfDocument(GONGWEN), pdfium.PdfDocument(margins)
mb = orig[0].get_mediabox()
want = (mb[0] + 25 * MM, mb[1] + 20 * MM, mb[2] - 15 * MM, mb[3] - 10 * MM)
got = new[0].get_cropbox()
check("crop", "PDFium reports the crop box", near(got, want, 0.1), f"{got} against {want}")
check("crop", "the size PDFium gives", near(new[0].get_size(), (want[2] - want[0], want[3] - want[1]), 0.1), str(new[0].get_size()))
check("crop", "page 4 is as it was", near(new[3].get_cropbox(), orig[3].get_cropbox()))
doc = pymupdf.open(margins)
r = doc[0].cropbox
check("crop", "PyMuPDF reads the same box", near((r.x0, r.y0, r.x1, r.y1), (want[0], mb[3] - want[3], want[2], mb[3] - want[1]), 0.1) or near((r.x0, r.y0, r.x1, r.y1), want, 0.1), str(r))
# What is drawn is the kept part of the page: the pixels of the cropped page are those of the same part of the whole page.
a, b = render_pdfium(GONGWEN, 0, scale=2.0), render_pdfium(margins, 0, scale=2.0)
x0, y0 = round((want[0] - mb[0]) * 2.0), round((mb[3] - want[3]) * 2.0)
# (The kept part starts between two pixels, so the same text is drawn at another phase: the two are compared at a quarter of the size, at the best of
# the nine offsets round the place, and the same text shifted a whole line would still be far off.)
def shrunk(x, k=4):
    h, w = x.shape[0] // k * k, x.shape[1] // k * k
    return x[:h, :w].reshape(h // k, k, w // k, k, *x.shape[2:]).mean(axis=(1, 3))


best = 1e9
for dx in (-1, 0, 1):
    for dy in (-1, 0, 1):
        part = a[y0 + dy:y0 + dy + b.shape[0], x0 + dx:x0 + dx + b.shape[1]]
        if part.shape == b.shape:
            best = min(best, float(np.abs(shrunk(part) - shrunk(b)).mean()))
check("crop", "PDFium draws the kept part of the page", best < 2.5, f"{b.shape} of {a.shape}, difference {best:.3f}")

print("crop: the gongwen, the white margins")
auto = out("gongwen-auto.pdf")
code, text, err = qingpdf("crop", GONGWEN, "--auto", "--pages", "2-6", "-o", auto, "--force")
check("crop", "exit code", code == 0, err.strip())
check("crop", "the original bytes are the start of the file", prefix_ok(GONGWEN, auto))
code, text = qpdf_check(auto)
check("crop", "qpdf --check", code == 0, text.strip()[:200])
for page in range(1, 6):
    img = render_pdfium(auto, page, scale=2.0)
    box = ink_box(img)
    if box is None:
        check("crop", f"page {page + 1} has ink", False)
        continue
    left, top, right, bottom = box[0], box[1], img.shape[1] - box[2], img.shape[0] - box[3]
    slack = (3 + 1.5) * MM * 2.0 + 6
    check("crop", f"page {page + 1}: the ink is within 3 mm and a little of every edge", max(left, top, right, bottom) <= slack, f"{(left, top, right, bottom)} pixels at 2x")
check("crop", "page 1 was not asked for", near(pdfium.PdfDocument(auto)[0].get_cropbox(), orig[0].get_cropbox()))

print("crop: a page turned a quarter")
pdf = Pdf()
pdf.page(300, 200, "1 0 0 rg 20 20 100 60 re f 0 0 1 rg 200 120 60 40 re f", extra="/Rotate 90")
turned = out("turned.pdf")
pdf.save(turned)
cropped = out("turned-cropped.pdf")
code, text, err = qingpdf("crop", turned, "--margins", "10,20,30,40", "-o", cropped, "--force")
check("turned", "exit code", code == 0, err.strip())
check("turned", "the original bytes are the start of the file", prefix_ok(turned, cropped))
code, text = qpdf_check(cropped)
check("turned", "qpdf --check", code == 0, text.strip()[:200])
# Shown 200 wide and 300 high; the top is user x from 0, the left is user y from 0 (a quarter turn): the crop box is [t, l, 300 - b, 200 - r].
t, rt, b, l = 10 * MM, 20 * MM, 30 * MM, 40 * MM
want = (t, l, 300 - b, 200 - rt)
c = pdfium.PdfDocument(cropped)
check("turned", "PDFium reports the crop box", near(c[0].get_cropbox(), want, 0.1), f"{c[0].get_cropbox()} against {want}")
size = c[0].get_size()
check("turned", "the size as shown (turned)", near(size, (200 - l - rt, 300 - t - b), 0.1), str(size))
whole, kept = render_pdfium(turned, 0, scale=2.0), render_pdfium(cropped, 0, scale=2.0)
x0, y0 = round(l * 2.0), round(t * 2.0)
part = whole[y0:y0 + kept.shape[0], x0:x0 + kept.shape[1]]
check("turned", "PDFium draws the kept part of the turned page", part.shape == kept.shape and float(np.abs(part - kept).mean()) < 1.0)

print("crop: an AES-256 file")
aes_out = out("aes-cropped.pdf")
code, text, err = qingpdf("crop", AES, "--margins", "5,5,5,5", "--password", AES_PASSWORD, "-o", aes_out, "--force")
check("aes", "exit code", code == 0, err.strip())
check("aes", "the original bytes are the start of the file", prefix_ok(AES, aes_out))
code, text = qpdf_check(aes_out, AES_PASSWORD)
check("aes", "qpdf --check", code == 0, text.strip()[:200])
a0, a1 = pdfium.PdfDocument(AES, password=AES_PASSWORD), pdfium.PdfDocument(aes_out, password=AES_PASSWORD)
# (PDFium reports a default 612 x 792 for this file's own page box, so the page box is read by PyMuPDF.)
pm = pymupdf.open(AES)
pm.authenticate(AES_PASSWORD)
mr = pm[0].mediabox
mb = (mr.x0, min(mr.y0, mr.y1), mr.x1, max(mr.y0, mr.y1))
check("aes", "PDFium (with the password) reports the crop box", near(a1[0].get_cropbox(), (mb[0] + 5 * MM, mb[1] + 5 * MM, mb[2] - 5 * MM, mb[3] - 5 * MM), 0.1), str(a1[0].get_cropbox()))
check("aes", "it is still encrypted", pymupdf.open(aes_out).is_encrypted)

# --- render -------------------------------------------------------------------------------------------------------------------------------

print("render: PNG and JPEG at 150 and 600 dpi")
mb = orig[0].get_mediabox()
w_pt, h_pt = mb[2] - mb[0], mb[3] - mb[1]
for dpi in (150, 600):
    for ext, extra in (("png", []), ("jpg", ["--quality", "90"])):
        path = out(f"gongwen-{dpi}.{ext}")
        code, text, err = qingpdf("render", GONGWEN, "--pages", "1", "--dpi", str(dpi), *extra, "-o", path, "--force")
        check("render", f"{dpi} dpi {ext}: exit code", code == 0, err.strip())
        img = Image.open(path)
        check("render", f"{dpi} dpi {ext}: the page's size", abs(img.width - round(w_pt * dpi / 72)) <= 1 and abs(img.height - round(h_pt * dpi / 72)) <= 1, f"{img.size}")
        if dpi == 150:
            ref = render_pdfium(GONGWEN, 0, scale=dpi / 72)
            mine = np.asarray(img.convert("L"), dtype=np.int16)
            h, w = min(ref.shape[0], mine.shape[0]), min(ref.shape[1], mine.shape[1])
            mean = float(np.abs(ref[:h, :w] - mine[:h, :w]).mean())
            check("render", f"{dpi} dpi {ext}: it is the page (mean difference from PDFium's picture)", mean < 12.0, f"{mean:.2f}")
        else:
            check("render", f"{dpi} dpi {ext}: more pixels than one drawing allows", img.width * img.height > 16_000_000, f"{img.width * img.height}")
            small = img.resize((img.width // 4, img.height // 4), Image.BOX).convert("L")
            ref = render_pdfium(GONGWEN, 0, scale=150 / 72)
            mine = np.asarray(small, dtype=np.int16)
            h, w = min(ref.shape[0], mine.shape[0]), min(ref.shape[1], mine.shape[1])
            mean = float(np.abs(ref[:h, :w] - mine[:h, :w]).mean())
            check("render", f"{dpi} dpi {ext}: it is the page (from end to end, shrunk)", mean < 12.0, f"{mean:.2f}")

# --- images -------------------------------------------------------------------------------------------------------------------------------

print("images: the pictures of a scan")
# A scan whose pictures are JPEG (the corpus has only CCITT scans), the same picture on two pages, and a picture with a soft mask.
from PIL import ImageDraw  # noqa: E402

scan_path = out("jpeg-scan.pdf")
rng = np.random.default_rng(7)
photo = Image.fromarray((rng.integers(0, 255, (160, 240, 3)) * 0.3 + np.linspace(0, 180, 240)[None, :, None]).clip(0, 255).astype(np.uint8))
ImageDraw.Draw(photo).ellipse((60, 40, 180, 120), outline=(255, 0, 0), width=4)
jbuf = io.BytesIO()
photo.save(jbuf, "JPEG", quality=80)
rgba = Image.new("RGBA", (64, 48), (0, 128, 255, 255))
ImageDraw.Draw(rgba).ellipse((8, 4, 56, 44), fill=(255, 255, 0, 90))
pbuf = io.BytesIO()
rgba.save(pbuf, "PNG")
sd = pymupdf.open()
for k in range(2):
    pg = sd.new_page(width=400, height=300)
    pg.insert_image(pymupdf.Rect(20, 20, 380, 260), stream=jbuf.getvalue())
    if k == 0:
        pg.insert_image(pymupdf.Rect(300, 230, 380, 290), stream=pbuf.getvalue())
sd.save(scan_path)
folder = out("scan-pictures")
shutil.rmtree(folder, ignore_errors=True)
code, text, err = qingpdf("images", scan_path, "--pages", "1-2", "-o", folder, "--force")
check("images", "exit code", code == 0, err.strip())
files = sorted(os.listdir(folder)) if os.path.isdir(folder) else []
check("images", "files were written", len(files) > 0, str(files))
doc = pymupdf.open(scan_path)
seen = {}
for pno in (0, 1):
    for item in doc[pno].get_images(full=True):
        seen.setdefault(item[0], pno + 1)
jpegs = 0
for xref, pno in seen.items():
    info = doc.extract_image(xref)
    if info["ext"] in ("jpeg", "jpg"):
        jpegs += 1
        names = [f for f in files if f.startswith(f"jpeg-scan-p{pno}-") and f.endswith(".jpg")]
        same = any(open(os.path.join(folder, f), "rb").read() == info["image"] for f in names)
        check("images", f"the JPEG of page {pno} (object {xref}) is the stream, byte for byte", same, str(names))
check("images", "the scan has a JPEG picture", jpegs > 0)
check("images", "the picture that is on both pages is saved once", len([f for f in files if f.endswith(".jpg")]) == 1, str(files))
pngs = [f for f in files if f.endswith(".png")]
check("images", "the picture with a soft mask is a PNG with a transparent part", len(pngs) == 1 and Image.open(os.path.join(folder, pngs[0])).mode == "RGBA" and Image.open(os.path.join(folder, pngs[0])).getchannel("A").getextrema()[0] < 255, str(pngs))
# The CCITT scan of the corpus: the pictures are decoded and written as PNG of the size PyMuPDF knows.
shutil.rmtree(out("ccitt-pictures"), ignore_errors=True)
code, text, err = qingpdf("images", SCAN, "--pages", "1", "-o", out("ccitt-pictures"), "--force")
check("images", "a CCITT scan: exit code", code == 0, err.strip())
ccitt = os.listdir(out("ccitt-pictures")) if os.path.isdir(out("ccitt-pictures")) else []
size_ok = bool(ccitt) and Image.open(os.path.join(out("ccitt-pictures"), ccitt[0])).size == (1968, 2818)
check("images", "a CCITT scan: one PNG of the picture's size", len(ccitt) == 1 and ccitt[0].endswith(".png") and size_ok, str(ccitt))

# A JPEG with a soft mask (a logo on a transparent ground) is a PNG with the transparency, not the opaque JPEG it would be as it is.
logo = pymupdf.open()
alpha = Image.new("L", (240, 160), 0)
ImageDraw.Draw(alpha).ellipse((20, 20, 220, 140), fill=255)
abuf = io.BytesIO()
alpha.save(abuf, "PNG")
logo.new_page(width=200, height=100).insert_image(pymupdf.Rect(10, 10, 190, 90), stream=jbuf.getvalue(), mask=abuf.getvalue())
logo_path = out("logo.pdf")
logo.save(logo_path)
shutil.rmtree(out("logo-pictures"), ignore_errors=True)
code, text, err = qingpdf("images", logo_path, "-o", out("logo-pictures"), "--force")
names = sorted(os.listdir(out("logo-pictures"))) if os.path.isdir(out("logo-pictures")) else []
im = Image.open(os.path.join(out("logo-pictures"), names[0])) if names else None
check("images", "a JPEG with a soft mask is a PNG with the transparency", code == 0 and len(names) == 1 and names[0].endswith(".png") and im.mode == "RGBA" and im.getchannel("A").getextrema() == (0, 255), f"{names} {err.strip()}")
# Files that are there are checked before any is written.
shutil.rmtree(out("scan-again"), ignore_errors=True)
os.makedirs(out("scan-again"))
open(os.path.join(out("scan-again"), "jpeg-scan-p1-2.png"), "wb").write(b"mine")
code, text, err = qingpdf("images", scan_path, "-o", out("scan-again"))
check("images", "an existing name stops the run before any file is written", code == 1 and sorted(os.listdir(out("scan-again"))) == ["jpeg-scan-p1-2.png"] and "--force" in err, f"{code} {os.listdir(out('scan-again'))} {err.strip()}")

# --- crop: paper that is grey -------------------------------------------------------------------------------------------------------------

print("crop: a scan on grey paper")
grey = pymupdf.open()
gp = grey.new_page(width=300, height=200)
gp.draw_rect(pymupdf.Rect(0, 0, 300, 200), color=None, fill=(0.91, 0.91, 0.91))
gp.draw_rect(pymupdf.Rect(100, 60, 200, 140), color=None, fill=(0, 0, 0))
grey_path = out("grey-paper.pdf")
grey.save(grey_path)
grey_out = out("grey-paper-cropped.pdf")
code, text, err = qingpdf("crop", grey_path, "--auto", "-o", grey_out, "--force")
check("crop", "grey paper: exit code", code == 0, err.strip())
got = pdfium.PdfDocument(grey_out)[0].get_cropbox()
# The block is x 100..200 and (up from the bottom) y 60..140; 3 mm (8.5 points) round it.
check("crop", "grey paper: the crop box is the block with its pad", near(got, (91.5, 51.5, 208.5, 148.5), 4.0), str(got))
# A page that is all picture has no margin: the run says so and writes nothing.
quarters = pymupdf.open()
qp = quarters.new_page(width=300, height=200)
for (x, y, c) in [(0, 0, (1, 0, 0)), (150, 0, (0, 1, 0)), (0, 100, (0, 0, 1)), (150, 100, (1, 1, 0))]:
    qp.draw_rect(pymupdf.Rect(x, y, x + 150, y + 100), color=None, fill=c)
quarters_path = out("quarters.pdf")
quarters.save(quarters_path)
code, text, err = qingpdf("crop", quarters_path, "--auto", "-o", out("quarters-cropped.pdf"), "--force")
check("crop", "a page that is all picture: no white margin, nothing written", code == 1 and "no white margin" in err and not os.path.exists(out("quarters-cropped.pdf")), f"{code} {err.strip()}")

# --- meta ---------------------------------------------------------------------------------------------------------------------------------

print("meta: title, author, subject, keywords")
xmp = (
    '<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
    '<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:pdf="http://ns.adobe.com/pdf/1.3/" pdf:Producer="Maker">'
    '<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Old title</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>'
)
pdf = Pdf()
pdf.page(200, 100, "0 g 10 10 50 20 re f")
xmp_obj = pdf.stream("/Type /Metadata /Subtype /XML", xmp, compress=False)
info_obj = pdf.obj("<< /Title (Old title) /Author (Old author) /Creator (Tester) /Producer (Maker) /CreationDate (D:20260101080000+08'00') >>")
pdf.catalog_extra = f"/Metadata {xmp_obj} 0 R"
meta_in = out("meta-in.pdf")
pdf.save(meta_in)
# (pdfmaker has no entry for the trailer: the reference to the information dictionary is added by hand; the table is made after the objects.)
data = open(meta_in, "rb").read().replace(b"/Root 1 0 R", f"/Root 1 0 R /Info {info_obj} 0 R".encode(), 1)
open(meta_in, "wb").write(data)
meta_out = out("meta-out.pdf")
title, author, subject, keywords = "\u65b0\u6807\u9898 New", "", "A subject", "k1, k2"
code, text, err = qingpdf("meta", meta_in, "--title", title, "--author", author, "--subject", subject, "--keywords", keywords, "-o", meta_out, "--force")
check("meta", "exit code", code == 0, err.strip())
check("meta", "the original bytes are the start of the file", prefix_ok(meta_in, meta_out))
code, text = qpdf_check(meta_out)
check("meta", "qpdf --check", code == 0, text.strip()[:200])
m = pymupdf.open(meta_out).metadata
check("meta", "PyMuPDF reads the title, subject and keywords", (m["title"], m["subject"], m["keywords"]) == (title, subject, keywords), str(m))
check("meta", "PyMuPDF: the author is gone, the creator and the date stay", not m["author"] and m["creator"] == "Tester" and m["creationDate"].startswith("D:20260101080000"), str(m))
pm = pdfium.PdfDocument(meta_out).get_metadata_dict()
check("meta", "PDFium reads the title, subject and keywords", (pm.get("Title"), pm.get("Subject"), pm.get("Keywords")) == (title, subject, keywords), str(pm))
xml = pymupdf.open(meta_out).get_xml_metadata()
try:
    root = ET.fromstring(xml[xml.index("<x:xmpmeta"):xml.index("</x:xmpmeta>") + len("</x:xmpmeta>")])
    ns = {"dc": "http://purl.org/dc/elements/1.1/", "pdf": "http://ns.adobe.com/pdf/1.3/", "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#"}
    desc = root.find(".//rdf:Description", ns)
    t = desc.find("dc:title/rdf:Alt/rdf:li", ns).text
    s = desc.find("dc:description/rdf:Alt/rdf:li", ns).text
    k = desc.find("pdf:Keywords", ns).text
    check("meta", "the XMP parses and says the same", (t, s, k) == (title, subject, keywords), f"{t!r} {s!r} {k!r}")
    check("meta", "the XMP keeps its other parts", desc.get("{http://ns.adobe.com/pdf/1.3/}Producer") == "Maker" and desc.find("dc:creator", ns) is None)
except Exception as e:  # noqa: BLE001
    check("meta", "the XMP parses", False, f"{e}: {xml[:200]}")

# A packet the way Acrobat and Word write one, a description for each namespace: two changes in a row (the second on the file the first wrote) leave
# an XMP that an XML parser accepts (every prefix declared where it is used) and that says what was asked.
acrobat = (
    '<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'
    '<rdf:Description rdf:about="" xmlns:pdf="http://ns.adobe.com/pdf/1.3/"><pdf:Producer>Acrobat</pdf:Producer></rdf:Description>'
    '<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><xmp:CreatorTool>Word</xmp:CreatorTool></rdf:Description>'
    '<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:format>application/pdf</dc:format>'
    '<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Old</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>'
)
pdf = Pdf()
pdf.page(200, 100, "0 g 10 10 50 20 re f")
xmp_obj = pdf.stream("/Type /Metadata /Subtype /XML", acrobat, compress=False)
info_obj = pdf.obj("<< /Title (Old) /Author (Old author) >>")
pdf.catalog_extra = f"/Metadata {xmp_obj} 0 R"
acro_in = out("acrobat-in.pdf")
pdf.save(acro_in)
acro_data = open(acro_in, "rb").read().replace(b"/Root 1 0 R", f"/Root 1 0 R /Info {info_obj} 0 R".encode(), 1)
open(acro_in, "wb").write(acro_data)


def xmp_values(path):
    """The title, the creators, the description and the keywords of the XMP of a file, parsed strictly (an unbound prefix is an error)."""
    xml = pymupdf.open(path).get_xml_metadata()
    root = ET.fromstring(xml[xml.index("<x:xmpmeta"):xml.index("</x:xmpmeta>") + len("</x:xmpmeta>")])
    ns = {"dc": "http://purl.org/dc/elements/1.1/", "pdf": "http://ns.adobe.com/pdf/1.3/", "rdf": "http://www.w3.org/1999/02/22-rdf-syntax-ns#", "xmp": "http://ns.adobe.com/xap/1.0/"}
    one = lambda path_: [e.text for e in root.findall(path_, ns)]  # noqa: E731
    return one(".//dc:title/rdf:Alt/rdf:li"), one(".//dc:creator/rdf:Seq/rdf:li"), one(".//dc:description/rdf:Alt/rdf:li"), one(".//pdf:Keywords"), one(".//pdf:Producer"), one(".//xmp:CreatorTool"), one(".//dc:format")


print("meta: a packet with a description for each namespace, changed twice")
step1, step2 = out("acrobat-1.pdf"), out("acrobat-2.pdf")
code, text, err = qingpdf("meta", acro_in, "--author", "Me", "--subject", "About", "--keywords", "k", "-o", step1, "--force")
check("meta", "acrobat: the first change", code == 0, err.strip())
try:
    check("meta", "acrobat: the XMP parses after the first change", xmp_values(step1) == (["Old"], ["Me"], ["About"], ["k"], ["Acrobat"], ["Word"], ["application/pdf"]), str(xmp_values(step1)))
except Exception as e:  # noqa: BLE001
    check("meta", "acrobat: the XMP parses after the first change", False, str(e))
code, text, err = qingpdf("meta", step1, "--title", "New title", "--author", "You", "--subject", "Other", "--keywords", "k2", "-o", step2, "--force")
check("meta", "acrobat: the second change", code == 0, err.strip())
try:
    check("meta", "acrobat: the XMP parses after the second change and says what was asked", xmp_values(step2) == (["New title"], ["You"], ["Other"], ["k2"], ["Acrobat"], ["Word"], ["application/pdf"]), str(xmp_values(step2)))
except Exception as e:  # noqa: BLE001
    check("meta", "acrobat: the XMP parses after the second change and says what was asked", False, str(e))
code, text = qpdf_check(step2)
check("meta", "acrobat: qpdf --check", code == 0, text.strip()[:200])

print()
if FAILED:
    print("FAILED:", *FAILED, sep="\n  ")
    sys.exit(1)
print("all checks passed")

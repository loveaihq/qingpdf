"""Check `qingpdf pagenum` (page numbers, headers and footers) against PDFium, qpdf and qingpdf's own `text`.

For each file it runs the command line, then checks: `qpdf --check` passes, the original bytes are a prefix of the result, `qingpdf text`
gives the same text as before (the numbers are artifacts of the page), and on every page PDFium finds the text of each place where it must
be: the boxes of its characters are mapped to the device (the page as it is drawn, with its /Rotate and crop box) and must read left to
right on one line (upright), with the line `margin` from the top (head) or the bottom (foot) and the side places at the margin from the
left or the right edge (or the middle of the page); with --mirror the left and right places change sides on the even pages.

    python tests/tools/pagenum_check.py [--ours target/release/qingpdf.exe] [--qpdf path/to/qpdf.exe] [--out folder] [--long book-500.pdf]

It exits with 1 when a check fails. A development check, not part of `cargo test`: it needs pypdfium2 (and qpdf for the first check).
"""

import argparse
import ctypes
import os
import subprocess
import sys
import time

import pypdfium2 as pdfium
import pypdfium2.raw as raw

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pdfmaker import Pdf  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--ours", default=os.path.join("target", "release", "qingpdf.exe"))
ap.add_argument("--qpdf", default=os.path.join("C:\\", "Dev", "tools", "qpdf", "bin", "qpdf.exe"))
ap.add_argument("--out", default=os.path.join("tests", "out", "pagenum-tools"))
ap.add_argument("--long", default="", help="a 500-page file for the speed check")
args = ap.parse_args()
os.makedirs(args.out, exist_ok=True)
MM = 72.0 / 25.4
SCALE = 4.0
bad = False


def fail(msg):
    global bad
    bad = True
    print("  FAILED:", msg)


def ours(*a):
    return subprocess.run([args.ours, *a], capture_output=True, text=True, encoding="utf-8")


def device_box(page, tp, first, count, w, h):
    """The box of characters first..first+count in device pixels (the page drawn at SCALE), and the centres of the characters."""
    cx, cy = ctypes.c_int(), ctypes.c_int()
    dw, dh = int(round(w * SCALE)), int(round(h * SCALE))
    pts, centres = [], []
    for i in range(first, first + count):
        l, b, r, t = tp.get_charbox(i)
        xs, ys = [], []
        for (x, y) in ((l, b), (r, t)):
            raw.FPDF_PageToDevice(page.raw, 0, 0, dw, dh, 0, x, y, ctypes.byref(cx), ctypes.byref(cy))
            xs.append(cx.value)
            ys.append(cy.value)
        pts.append((min(xs), min(ys), max(xs), max(ys)))
        centres.append(((min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2))
    return (min(p[0] for p in pts), min(p[1] for p in pts), max(p[2] for p in pts), max(p[3] for p in pts)), centres


def expect_text(template, number, total, name, date):
    return template.replace("{page}", str(number)).replace("{pages}", str(total)).replace("{date}", date).replace("{file}", name)


def check_case(name, source, slots, password="", extra=(), mirror=False, size=12.0, margin=15.0, date=None):
    """`slots`: {option: template}. Returns after printing a line for the case."""
    print(f"{name}:")
    out = os.path.join(args.out, name + ".pdf")
    cmd = ["pagenum", source, "-o", out, "--force", "--size", str(size), "--margin", str(margin), "--color", "ff0000", *extra]
    for option, template in slots.items():
        cmd += [option, template]
    if mirror:
        cmd.append("--mirror")
    if password:
        cmd += ["--password", password]
    r = ours(*cmd)
    if r.returncode != 0:
        fail(f"the command failed: {r.stderr.strip()}")
        return
    # qpdf --check
    if os.path.exists(args.qpdf):
        check = subprocess.run([args.qpdf, "--check"] + (["--password=" + password] if password else []) + [out], capture_output=True, text=True)
        before = subprocess.run([args.qpdf, "--check"] + (["--password=" + password] if password else []) + [source], capture_output=True, text=True)
        if check.returncode > before.returncode or "No syntax or stream encoding errors" not in check.stdout:
            fail("qpdf --check: " + check.stdout[-300:])
        else:
            print("  qpdf --check ok (original:", before.returncode, ", result:", check.returncode, ")")
    a, b = open(source, "rb").read(), open(out, "rb").read()
    if not (b.startswith(a) and len(b) > len(a)):
        fail("the bytes of the original are not a prefix")
    else:
        print(f"  original bytes are a prefix (+{len(b) - len(a)} bytes)")
    # `qingpdf text` is unchanged
    t0 = ours("text", source, *(["--password", password] if password else []))
    t1 = ours("text", out, *(["--password", password] if password else []))
    if t0.returncode != 0 or t0.stdout != t1.stdout:
        fail(f"qingpdf text changed ({len(t0.stdout)} -> {len(t1.stdout)} characters)")
    else:
        print(f"  qingpdf text unchanged ({len(t0.stdout)} characters)")
    # PDFium: every place, every page
    doc = pdfium.PdfDocument(out, password=password or None)
    total = len(doc)
    today = date or time.strftime("%Y-%m-%d", time.gmtime())
    fname = os.path.basename(source)
    errors, worst = 0, 0.0
    places = {"--header-left": (0, "head"), "--header-center": (1, "head"), "--header-right": (2, "head"), "--footer-left": (0, "foot"), "--footer-center": (1, "foot"), "--footer-right": (2, "foot")}
    for index in range(total):
        page = doc[index]
        w, h = page.get_size()
        tp = page.get_textpage()
        text = tp.get_text_range()
        for option, template in slots.items():
            side, row = places[option]
            if mirror and index % 2 == 1 and side != 1:
                side = 2 - side
            want = expect_text(template, index + 1, total, fname, today)
            at = text.rfind(want)
            if at < 0:
                errors += 1
                if errors < 4:
                    fail(f"page {index + 1}: {want!r} is not in what PDFium reads")
                continue
            # (PDFium counts characters of the text range, which are the characters of the boxes.)
            # (the indices of PDFium are UTF-16 code units: a page with letters outside the first plane, like the maths of a paper, counts more)
            at16 = len(text[:at].encode("utf-16-le")) // 2
            box, centres = device_box(page, tp, at16, len(want.encode("utf-16-le")) // 2, w, h)
            l, t, r_, bt = (v / SCALE for v in box)
            ok = True
            # reads left to right on one line
            xs = [c[0] for c in centres]
            ys = [c[1] for c in centres]
            if any(b2 < a2 for a2, b2 in zip(xs, xs[1:])) or max(ys) - min(ys) > 0.4 * size * SCALE:
                ok = False
                why = "not upright (the characters do not run left to right on a line)"
            else:
                why = ""
            mm = margin * MM
            if row == "head":
                ok &= mm - 1.5 <= t <= mm + size
            else:
                ok &= h - mm - 0.45 * size - 1.0 <= bt <= h - mm + 1.5
            if side == 0:
                ok &= mm - 2.0 <= l <= mm + 0.6 * size
            elif side == 1:
                ok &= abs((l + r_) / 2 - w / 2) <= 2.0
            else:
                ok &= w - mm - 0.6 * size <= r_ <= w - mm + 2.0
            if not ok:
                errors += 1
                if errors < 4:
                    fail(f"page {index + 1} {option}: {want!r} is at x {l:.1f}..{r_:.1f}, y {t:.1f}..{bt:.1f} on a page {w:.0f} x {h:.0f} {why}")
            worst = max(worst, abs(bt - (h - mm)) if row == "foot" else abs(t - mm))
    print(f"  PDFium: {total} pages, {len(slots)} places each: {'all where they must be' if not errors else str(errors) + ' wrong'}"
          f" (the line is within {worst:.1f} points of the margin)")
    if errors:
        fail(f"{errors} places are wrong")


# --- the four kinds of file --------------------------------------------------------------------------------------------------------
zh = os.path.join("tests", "corpus", "public", "zh")
check_case("gongwen", os.path.join(zh, "gongwen", "gongwen-2024-ai-standards-guide.pdf"),
           {"--footer-right": "- {page} -", "--footer-center": "第 {page} 页 共 {pages} 页", "--header-left": "{file}"}, mirror=True, size=12.0, margin=20.0)
check_case("english-paper", os.path.join(zh, "lunwen", "lunwen-arxiv-2601.14329-latex.pdf"),
           {"--footer-center": "Page {page} of {pages}", "--header-right": "{date}", "--footer-left": "{file}"}, size=10.5, margin=12.0)

# a file with /Rotate 90 and 270 pages, a crop box that does not start at 0, and a page turned 180
pdf = Pdf()
font = pdf.obj("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
res = f"<< /Font << /F1 {font} 0 R >> >>"
body = "BT /F1 14 Tf 60 200 Td (Body text) Tj ET"
for extra in ("/Rotate 90", "/Rotate 270 /CropBox [40 30 260 380]", "/Rotate 180 /CropBox [10 10 290 390]", "", "/Rotate 90 /CropBox [20 20 280 380]", "/Rotate 270"):
    pdf.page(300, 400, body, resources=res, extra=extra)
rotated = os.path.join(args.out, "rotated-cropped.pdf")
pdf.save(rotated)
check_case("rotated-cropbox", rotated, {"--footer-center": "{page}/{pages}", "--header-left": "L {page}", "--header-right": "R {page}", "--footer-right": "{file}"}, mirror=True, size=12.0, margin=10.0)

# an AES-256 file
if os.path.exists(args.qpdf):
    locked = os.path.join(args.out, "aes256-in.pdf")
    subprocess.run([args.qpdf, "--encrypt", "pw", "pw", "256", "--", os.path.join(zh, "gongwen", "gongwen-2025-service-mfg-plan.pdf"), locked], capture_output=True)
    check_case("aes256", locked, {"--footer-center": "- {page} -", "--header-right": "{file}"}, password="pw", size=11.0, margin=15.0)

# a signed file is refused, and so is a file whose content may not be changed (without the owner password)
print("refused:")
signed = Pdf()
signed.catalog_extra = "/AcroForm << /Fields [10 0 R] /SigFlags 3 >>"
sig = signed.obj("<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>")
widget = signed.obj(f"<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [0 0 0 0] /V {sig} 0 R >>", number=10)
signed.page(300, 400, "0 g", extra=f"/Annots [{widget} 0 R]")
signed_path = os.path.join(args.out, "signed.pdf")
signed.save(signed_path)
r = ours("pagenum", signed_path, "-o", os.path.join(args.out, "signed-out.pdf"), "--force")
print("  signed:", r.returncode, r.stderr.strip())
if r.returncode == 0 or "signed" not in r.stderr:
    fail("a signed file was not refused")
if os.path.exists(args.qpdf):
    plain = os.path.join(args.out, "plain.pdf")
    simple = Pdf()
    simple_font = simple.obj("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    simple.page(300, 400, body, resources=f"<< /Font << /F1 {simple_font} 0 R >> >>")
    simple.save(plain)
    nomod = os.path.join(args.out, "nomodify.pdf")
    subprocess.run([args.qpdf, "--encrypt", "", "owner", "256", "--modify=none", "--", plain, nomod], capture_output=True)
    r = ours("pagenum", nomod, "-o", os.path.join(args.out, "nomodify-out.pdf"), "--force")
    print("  no-modify:", r.returncode, r.stderr.strip()[:120])
    if r.returncode == 0 or "permission" not in r.stderr:
        fail("a file that may not be changed was not refused")

# the speed of 500 pages
if args.long:
    t0 = time.time()
    r = ours("pagenum", args.long, "--footer-center", "第 {page} 页 共 {pages} 页", "--header-left", "{file}", "-o", os.path.join(args.out, "long.pdf"), "--force")
    took = time.time() - t0
    print(f"500 pages: {took:.2f} s ({r.stdout.strip()})")
    if r.returncode != 0 or took > 2.0:
        fail(f"too slow or failed: {r.stderr.strip()}")

sys.exit(1 if bad else 0)

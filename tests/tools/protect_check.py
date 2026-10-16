"""Check `qingpdf encrypt` (passwords and permissions) against PDFium and qpdf, and time it.

For each case it runs the command line on a source file and checks that PDFium (pypdfium2) opens the result with the user password and
with the owner password, refuses a wrong one, finds the security handler revision (6 for AES-256, 4 for AES-128), reports exactly the
permission flags that were set (FPDF_GetDocUserPermissions: the file's own /P, whichever password opened it), and draws every page
pixel for pixel as it draws the source; its text is the source's text; and qpdf --check passes and --show-encryption says the same
revision, flags and method. The sources are a classic file, a file with object streams and a cross-reference stream, a file with
Chinese text, and files that are encrypted already (re-encrypted with another method). Then the speed: a 500-page text file and a 100 MB
file, whole command line, in seconds.

    python tests/tools/protect_check.py [--ours target/release/qingpdf.exe] [--qpdf path/to/qpdf.exe] [--out folder] [--no-speed]

It exits with 1 when a check fails. A development check, not part of `cargo test`: it needs pypdfium2 (and qpdf for some of the checks).
The passwords are made up here; they are written to files for `--password-file` and never printed.
"""

import argparse
import os
import subprocess
import sys
import time

import pypdfium2 as pdfium
import pypdfium2.raw as raw

ap = argparse.ArgumentParser()
ap.add_argument("--ours", default=os.path.join("target", "release", "qingpdf.exe"))
ap.add_argument("--qpdf", default=os.path.join("C:\\", "Dev", "tools", "qpdf", "bin", "qpdf.exe"))
ap.add_argument("--out", default=os.path.join("tests", "out", "protect-tools"))
ap.add_argument("--no-speed", action="store_true")
args = ap.parse_args()
os.makedirs(args.out, exist_ok=True)
corpus = os.path.join("tests", "corpus", "public")
bad = False
checks = 0


def fail(msg):
    global bad
    bad = True
    print("  FAILED:", msg)


def ours(*a):
    return subprocess.run([args.ours, *a], capture_output=True, text=True, encoding="utf-8", errors="replace")


def flags(print_level="high", copy=True, modify=True, annotate=True, forms=True, assemble=True, accessibility=True):
    """The /P of Table 22 for a file of revision 3 or later."""
    p = 0xFFFFF0C0
    for on, bit in ((print_level != "none", 3), (modify, 4), (copy, 5), (annotate, 6), (forms, 9), (accessibility, 10), (assemble, 11),
                    (print_level == "high", 12)):
        if on:
            p |= 1 << (bit - 1)
    return p


def render(page):
    bitmap = page.render(scale=1.0)
    return bitmap.width, bitmap.height, bytes(bitmap.buffer)


def open_pdfium(path, password, latin1=False):
    """PDFium takes the password as bytes it compares as they are. pypdfium2 sends UTF-8; revision 4 keeps passwords in PDFDocEncoding
    (Latin-1 for the letters used here), so for that case the raw call is used with the Latin-1 bytes."""
    if latin1 and password:
        path = os.path.abspath(path)
        handle = raw.FPDF_LoadDocument(path.encode("utf-8") + b"\x00", password.encode("latin-1") + b"\x00")
        if not handle:
            raise pdfium.PdfiumError("PDFium does not open it with that password")
        return pdfium.PdfDocument(handle, autoclose=True)
    return pdfium.PdfDocument(path, password=password or None)


def pdfium_refuses(path, password):
    try:
        pdfium.PdfDocument(path, password=password)
    except pdfium.PdfiumError:
        return True
    return False


def case(name, source, user, owner, *, aes128=False, options=(), perm_flags=None, source_password="", latin1=False):
    """Encrypt `source` and check the result. `owner` is what was given ('' for none); the owner password in effect is the user's then."""
    global checks
    checks += 1
    print(f"{name}:")
    out = os.path.join(args.out, name + ".pdf")
    pwfile = os.path.join(args.out, name + ".pw")
    with open(pwfile, "w", encoding="utf-8", newline="") as f:
        f.write(user + "\n" + owner + "\n")
    cmd = ["encrypt", source, "-o", out, "--force", "--password-file", pwfile, *options]
    if aes128:
        cmd.append("--aes128")
    if source_password:
        cmd += ["--password", source_password]
    r = ours(*cmd)
    os.remove(pwfile)
    if r.returncode != 0:
        fail("the command failed: " + r.stderr.strip())
        return
    effective_owner = owner or user
    want_p = flags() if perm_flags is None else perm_flags
    want_revision = 4 if aes128 else 6
    src = open_pdfium(source, source_password)
    src_pages = len(src)
    want_pages = [render(src[i]) for i in range(src_pages)]
    want_text = [src[i].get_textpage().get_text_range() for i in range(src_pages)]
    for label, password in (("user", user), ("owner", effective_owner)):
        if label == "user" and not user:
            password = ""
        try:
            doc = open_pdfium(out, password, latin1)
        except pdfium.PdfiumError as e:
            fail(f"PDFium does not open it with the {label} password: {e}")
            continue
        revision = raw.FPDF_GetSecurityHandlerRevision(doc.raw)
        if revision != want_revision:
            fail(f"PDFium finds revision {revision}, not {want_revision}")
        got_p = raw.FPDF_GetDocUserPermissions(doc.raw) & 0xFFFFFFFF
        if got_p != want_p:
            fail(f"PDFium finds the flags {got_p:#010x}, not {want_p:#010x} (opened with the {label} password)")
        # What the person who opened it may do: everything for the owner (a password that is the owner's and not the user's), else the flags.
        have_p = raw.FPDF_GetDocPermissions(doc.raw) & 0xFFFFFFFF
        want_have = 0xFFFFFFFC if (label == "owner" and effective_owner != user) else want_p
        if have_p != want_have:
            fail(f"PDFium: opened with the {label} password the person may do {have_p:#010x}, not {want_have:#010x}")
        if len(doc) != src_pages:
            fail(f"PDFium finds {len(doc)} pages, not {src_pages}")
            continue
        wrong_pixels = [i for i in range(src_pages) if render(doc[i]) != want_pages[i]]
        wrong_text = [i for i in range(src_pages) if doc[i].get_textpage().get_text_range() != want_text[i]]
        if wrong_pixels:
            fail(f"PDFium draws pages {[i + 1 for i in wrong_pixels[:5]]} differently from the source ({label} password)")
        if wrong_text:
            fail(f"PDFium reads the text of pages {[i + 1 for i in wrong_text[:5]]} differently ({label} password)")
        print(f"  PDFium opens it with the {label} password: revision {revision}, flags {got_p:#010x}, {src_pages} pages drawn as the source's"
              f" ({'same text' if not wrong_text else 'TEXT DIFFERS'})")
    if user and not latin1:
        if not (pdfium_refuses(out, "wrong-password-x") and pdfium_refuses(out, "")):
            fail("PDFium opens it with a wrong or no password")
        else:
            print("  PDFium refuses a wrong password and none")
    if os.path.exists(args.qpdf):
        for password in {user, effective_owner}:
            pw = [f"--password={password}"] if password else []
            check = subprocess.run([args.qpdf, "--check", *pw, out], capture_output=True, text=True, encoding="utf-8", errors="replace")
            if check.returncode not in (0, 3) or "No syntax or stream encoding errors" not in check.stdout:
                fail("qpdf --check: " + check.stdout[-300:] + check.stderr[-300:])
        show = subprocess.run([args.qpdf, f"--password={effective_owner}", "--show-encryption", out], capture_output=True, text=True,
                              encoding="utf-8", errors="replace").stdout
        method = "AESv2" if aes128 else "AESv3"
        if f"R = {want_revision}" not in show or f"P = {want_p - (1 << 32)}" not in show or show.count(f"encryption method: {method}") != 3:
            fail("qpdf --show-encryption: " + show.replace("\n", " | ")[:300])
        else:
            print(f"  qpdf --check ok, R = {want_revision}, P = {want_p - (1 << 32)}, {method}")
    else:
        print("  (no qpdf here)")


zh = os.path.join(corpus, "zh")
classic = os.path.join(zh, "gongwen", "gongwen-2025-service-mfg-plan.pdf")
objstm = os.path.join(zh, "gongwen", "gongwen-2024-nicheng-work-report.pdf")
paper = os.path.join(zh, "lunwen", "lunwen-arxiv-2401.01717-word.pdf")
gen = os.path.join(corpus, "encrypted", "qpdf-generated")

case("aes256-user-and-owner", classic, "open-me-1", "owner-1", options=["--no-copy", "--print", "low"],
     perm_flags=flags(print_level="low", copy=False))
case("aes256-user-only", classic, "just-the-user", "")
case("aes256-owner-only", classic, "", "just-the-owner", options=["--no-modify", "--no-annotate"], perm_flags=flags(modify=False, annotate=False))
case("aes128-user-and-owner", classic, "open-me-2", "owner-2", aes128=True, options=["--print", "none", "--no-forms", "--no-assemble"],
     perm_flags=flags(print_level="none", forms=False, assemble=False))
case("aes128-user-only", classic, "just-the-user", "", aes128=True)
case("aes128-owner-only", classic, "", "just-the-owner", aes128=True, options=["--no-accessibility", "--no-copy"],
     perm_flags=flags(accessibility=False, copy=False))
case("aes256-chinese", classic, "密码一二三", "主人密码", options=["--no-copy"], perm_flags=flags(copy=False))
case("aes256-chinese-owner-only", classic, "", "管理员密码", options=["--no-modify"], perm_flags=flags(modify=False))
case("aes128-latin1", classic, "hôtel", "âge", aes128=True, options=["--no-copy"], perm_flags=flags(copy=False), latin1=True)
case("objstm-256", objstm, "u-1", "o-1", options=["--no-copy"], perm_flags=flags(copy=False))
case("objstm-128", objstm, "u-1", "o-1", aes128=True, options=["--no-annotate"], perm_flags=flags(annotate=False))
case("objstm-word-256-owner-only", paper, "", "o-1", options=["--print", "low"], perm_flags=flags(print_level="low"))
# Files that are encrypted already, read with their own passwords and written under another method.
case("again-from-aes128-to-256", os.path.join(gen, "bookmarks.r4-aes128-user-assemble-n.pdf"), "new-user", "new-owner", source_password="owner")
case("again-from-aes256-to-128", os.path.join(gen, "pdf20utf8.r6-aes256-user-modify-none.pdf"), "new-user", "new-owner", aes128=True,
     source_password="owner")
case("again-from-rc4-to-256", os.path.join(gen, "bookmarks.r3-128-rc4-user-modify-none.pdf"), "new-user", "new-owner", source_password="owner",
     options=["--no-copy"], perm_flags=flags(copy=False))
case("again-from-chinese-r6", os.path.join(gen, "vertical.r6-aes256-user-chinese.pdf"), "", "new-owner", source_password="主人",
     options=["--no-modify"], perm_flags=flags(modify=False))

# What is refused.
print("refused:")
r = ours("encrypt", classic, "-o", os.path.join(args.out, "never.pdf"), "--force", "--aes128", "--user-password", "密码")
print("  chinese password with AES-128:", r.returncode, r.stderr.strip())
if r.returncode == 0 or "AES-256" not in r.stderr or os.path.exists(os.path.join(args.out, "never.pdf")):
    fail("a Chinese password was not refused for AES-128")
r = ours("encrypt", os.path.join(corpus, "incremental", "signature_reason.pdf"), "-o", os.path.join(args.out, "never.pdf"), "--force",
         "--user-password", "u")
print("  signed:", r.returncode, r.stderr.strip())
if r.returncode == 0 or "signed" not in r.stderr:
    fail("a signed file was not refused")
r = ours("encrypt", os.path.join(gen, "two.r6-aes256-user-everything-denied.pdf"), "-o", os.path.join(args.out, "never.pdf"), "--force",
         "--user-password", "n", "--password", "user")
print("  restricted file, user password:", r.returncode, r.stderr.strip()[:110])
if r.returncode == 0:
    fail("a restricted file was changed with the user password")

# Speed: the whole command line, read, rewrite, encrypt, write.
if not args.no_speed:
    print("speed:")
    book = os.path.join(zh, "ebook", "ebook-wikisource-minfadian.pdf")           # 128 pages of text
    five = os.path.join(args.out, "book-512.pdf")
    big = os.path.join(args.out, "paper-26.pdf")
    if not os.path.exists(five):
        r = ours("merge", book, book, book, book, "-o", five, "--force")
        assert r.returncode == 0, r.stderr
    if not os.path.exists(big):
        latex = os.path.join(zh, "lunwen", "lunwen-arxiv-2601.14329-latex.pdf")
        r = ours("merge", *([latex] * 26), "-o", big, "--force")
        assert r.returncode == 0, r.stderr
    for label, src, limit in (("512-page text file", five, 1.0), ("100 MB file", big, 2.0)):
        size = os.path.getsize(src) / 1e6
        times = []
        for n in range(3):
            t0 = time.perf_counter()
            r = ours("encrypt", src, "-o", os.path.join(args.out, "speed-out.pdf"), "--force", "--user-password", "speed-pw", "--owner-password", "speed-owner")
            times.append(time.perf_counter() - t0)
            assert r.returncode == 0, r.stderr
        best = min(times)
        pages = ours("info", src).stdout.split("Pages:")[1].split()[0]
        print(f"  {label} ({size:.1f} MB, {pages} pages): " + ", ".join(f"{t:.2f}" for t in times) + f" s (limit {limit} s)")
        if best > limit:
            fail(f"{label}: {best:.2f} s is over {limit} s")

print(f"{checks} cases, " + ("FAILED" if bad else "all passed"))
sys.exit(1 if bad else 0)

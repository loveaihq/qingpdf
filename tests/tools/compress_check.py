"""Check `qingpdf compress` against PDFium and qpdf, and time it.

For each file of the list it runs the command line three times (--lossless, balanced, small), and for each output checks that qpdf
--check passes (and says so for an encrypted one with its password), that `qingpdf text` of the output is the input's text, that the
pages draw in PDFium (pypdfium2, 100 dpi) as the input's pages do (the lossless output pixel for pixel; the lossy ones within a mean
difference per sample that THRESHOLD says), and for an encrypted file that the output opens in PDFium with the same passwords (the user
password and the owner password) and not with a wrong one. It prints the table of sizes.
Then the speed: a scanned file of 100 pages at 300 dpi (made by make_speed_scan.py, or any file given with --speed-file), whole
command line, balanced.

    python tests/tools/compress_check.py [--ours target/release/qingpdf.exe] [--qpdf path/to/qpdf.exe] [--out folder] [--pages N]
                                         [--speed-file file.pdf] [--only substring]

It exits with 1 when a check fails. A development check, not part of `cargo test`: it needs pypdfium2, numpy and qpdf. The passwords
of the encrypted case are made up here; they are written to a file for `--password-file` and are not printed.
"""

import argparse
import os
import subprocess
import sys
import time

import numpy as np
import pypdfium2 as pdfium

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))

ap = argparse.ArgumentParser()
ap.add_argument("--ours", default=os.path.join(ROOT, "target", "release", "qingpdf.exe"))
ap.add_argument("--qpdf", default=os.path.join("C:\\", "Dev", "tools", "qpdf", "bin", "qpdf.exe"))
ap.add_argument("--out", default=os.path.join(ROOT, "tests", "out", "compress-tools"))
ap.add_argument("--pages", type=int, default=40, help="how many pages of a file are drawn and compared (spread over the file)")
ap.add_argument("--speed-file", default=None)
ap.add_argument("--only", default=None)
args = ap.parse_args()
os.makedirs(args.out, exist_ok=True)
bad = False

# The files: (label, path below tests/corpus, password or None, what it stands for).
FILES = [
    ("scan JPEG", "local/scanned/issue7229.pdf", None),
    ("scan JBIG2", "local/zh/scanned/scan-nlc-jiankang-207p.pdf", None),
    ("photo flyer", "public/zh/gongwen/gongwen-2024-ai-standards-guide.pdf", None),
    ("text-only paper", "local/xref-classic/tracemonkey.pdf", None),
    ("Chinese gongwen", "public/zh/gongwen/gongwen-2024-nicheng-work-report.pdf", None),
    ("ebook", "public/zh/ebook/ebook-wikisource-minfadian.pdf", None),
    ("CMYK file", "local/zh/gongwen/gongwen-wuhan-gazette-2024-05.pdf", None),
    ("encrypted (AES-256)", "public/zh/gongwen/gongwen-2024-ai-standards-guide.pdf", "aes256"),
    ("encrypted (AES-128)", "public/zh/gongwen/gongwen-2024-ai-standards-guide.pdf", "aes128"),
    ("paper with figures", "public/zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf", None),
    ("JPEG 2000 scan", "local/zh/scanned/scan-nlc-hechuan-juan49-jpx.pdf", None),
    ("book with a photo", "local/large/freeculture.pdf", None),
    ("magazine, mostly garbage", "local/large/issue3188.pdf", None),
]
USER_PW, OWNER_PW = "open sesame", "owner pw"

# The mean absolute difference per sample (0 to 255) the drawn pages of a lossy output may have from the input's, at 100 dpi in PDFium.
# The worst page of the worst file of the corpus decides: a scan of small text (issue7229.pdf: 200 dpi, thin strokes), where averaging the
# picture down and coding it again moves the edge of every stroke, which is where the whole difference is (looked at: the pages read the
# same); a photograph or a figure is 0.2 to 2.8. Measured (docs/decisions.md): high 0.79, balanced 3.86, small 5.79 at the most; the
# limits are those with a little room. A lossless output must draw to the same pixels exactly.
THRESHOLD = {"lossless": 0.0, "high": 1.5, "balanced": 4.5, "small": 6.5}


def fail(msg):
    global bad
    bad = True
    print("  FAILED:", msg, flush=True)


def ours(*a):
    return subprocess.run([args.ours, *a], capture_output=True, text=True, encoding="utf-8", errors="replace")


def qpdf_check(path, password=None):
    cmd = [args.qpdf, "--check"]
    pwfile = None
    if password:
        pwfile = os.path.join(args.out, "pw.txt")
        with open(pwfile, "w", encoding="utf-8") as f:
            f.write(password)
        cmd.append("--password-file=" + pwfile)
    r = subprocess.run(cmd + [path], capture_output=True, text=True, encoding="utf-8", errors="replace")
    if pwfile:
        os.remove(pwfile)
    return r.returncode == 0, (r.stdout + r.stderr)


def render_pages(path, password, picks):
    doc = pdfium.PdfDocument(path, password=password or None)
    out = []
    for i in picks:
        bitmap = doc[i].render(scale=100 / 72)
        out.append(np.array(bitmap.to_numpy(), dtype=np.int16))
    return out


def spread(n, k):
    if n <= k:
        return list(range(n))
    return sorted({round(i * (n - 1) / (k - 1)) for i in range(k)})


def sizes_line(a, b):
    return f"{a:>11,} -> {b:>11,} ({(a - b) * 100 / max(a, 1):5.1f}% smaller)"


def run_case(label, rel, mode, table):
    src = os.path.join(ROOT, "tests", "corpus", rel)
    password = None
    if mode:
        # An encrypted copy of the file, made by our encrypt command: the user password opens it, the owner password lifts the limits.
        enc = os.path.join(args.out, f"enc-{mode}.pdf")
        pwfile = os.path.join(args.out, "enc-pw.txt")
        with open(pwfile, "w", encoding="utf-8") as f:
            f.write(USER_PW + "\n" + OWNER_PW + "\n")
        r = ours("encrypt", src, "-o", enc, "--force", "--password-file", pwfile, *(["--aes128"] if mode == "aes128" else []))
        os.remove(pwfile)
        if r.returncode != 0:
            fail(f"{label}: encrypt: {r.stderr}")
            return
        src, password = enc, USER_PW
    size = os.path.getsize(src)
    print(f"== {label}: {rel} ({size:,} bytes)", flush=True)
    base = os.path.splitext(os.path.basename(src))[0]
    want_text = ours("text", src, *(["--password", password] if password else []))
    if want_text.returncode != 0:
        fail(f"{label}: text of the input: {want_text.stderr}")
    n_pages = len(pdfium.PdfDocument(src, password=password or None))
    picks = spread(n_pages, args.pages)
    before = render_pages(src, password, picks)
    row = {"label": label, "original": size}
    for name, extra in (("lossless", ["--lossless"]), ("balanced", []), ("small", ["--preset", "small"]), ("high", ["--preset", "high"])):
        out = os.path.join(args.out, f"{base}.{name}.pdf")
        t = time.time()
        r = ours("compress", src, "-o", out, "--force", *extra, *(["--password", password] if password else []))
        took = time.time() - t
        if r.returncode != 0:
            fail(f"{label} {name}: {r.stderr}")
            continue
        got = os.path.getsize(out)
        kept = "no smaller" in r.stdout
        row[name] = got
        if kept and got != size:
            fail(f"{label} {name}: said it is no smaller but the output differs from the input")
        print(f"  {name:9} {sizes_line(size, got)} {'(kept as is) ' if kept else ''}{took:6.2f} s", flush=True)
        if got > size:
            fail(f"{label} {name}: the output is bigger than the input")
        ok, text = qpdf_check(out, password)
        if not ok:
            fail(f"{label} {name}: qpdf --check: {text.strip()[-300:]}")
        t_out = ours("text", out, *(["--password", password] if password else []))
        if t_out.stdout != want_text.stdout:
            fail(f"{label} {name}: the text differs")
        try:
            after = render_pages(out, password, picks)
        except pdfium.PdfiumError as e:
            fail(f"{label} {name}: PDFium does not open it: {e}")
            continue
        worst = 0.0
        for a, b, i in zip(before, after, picks):
            if a.shape != b.shape:
                fail(f"{label} {name}: page {i + 1} draws at another size")
                continue
            d = float(np.abs(a - b).mean())
            worst = max(worst, d)
        row[name + "_diff"] = worst
        limit = THRESHOLD[name]
        print(f"            drawn pages (100 dpi, {len(picks)} compared): worst mean difference {worst:.3f} (limit {limit})", flush=True)
        if worst > limit:
            fail(f"{label} {name}: the pages draw {worst:.3f} off (limit {limit})")
        if password:
            # The same passwords open it, a wrong one does not.
            for pw in (USER_PW, OWNER_PW):
                try:
                    pdfium.PdfDocument(out, password=pw)
                except pdfium.PdfiumError:
                    fail(f"{label} {name}: PDFium does not open it with the same password")
            try:
                pdfium.PdfDocument(out, password="not the password")
                fail(f"{label} {name}: PDFium opens it with a wrong password")
            except pdfium.PdfiumError:
                pass
            if not ours("text", out, "--password", OWNER_PW).stdout == want_text.stdout:
                fail(f"{label} {name}: the owner password does not give the same text")
    table.append(row)


def main():
    table = []
    for label, rel, mode in FILES:
        if args.only and args.only not in label and args.only not in rel:
            continue
        if not os.path.exists(os.path.join(ROOT, "tests", "corpus", rel)):
            print(f"== {label}: {rel} is not here, skipped")
            continue
        run_case(label, rel, mode, table)
    print("\n| file | original | lossless | balanced | small |")
    print("|---|---:|---:|---:|---:|")
    for r in table:
        cells = [f"{r.get(k, 0):,}" for k in ("original", "lossless", "balanced", "small")]
        print(f"| {r['label']} | " + " | ".join(cells) + " |")
    speed = args.speed_file
    if speed is None:
        speed = os.path.join(args.out, "..", "perf", "scan-300dpi-100p.pdf")
    if os.path.exists(speed) and not args.only:
        out = os.path.join(args.out, "speed.balanced.pdf")
        times = []
        for _ in range(3):
            t = time.time()
            r = ours("compress", speed, "-o", out, "--force")
            times.append(time.time() - t)
            if r.returncode != 0:
                fail("speed: " + r.stderr)
                break
        print(f"\nspeed, {os.path.basename(speed)} ({os.path.getsize(speed):,} bytes) balanced, whole command line: "
              + ", ".join(f"{x:.2f} s" for x in times) + f"; output {os.path.getsize(out):,} bytes")
        if min(times) > 10.0:
            fail("speed: more than 10 seconds")
    print("\nFAILED" if bad else "\nall checks passed")
    sys.exit(1 if bad else 0)


main()

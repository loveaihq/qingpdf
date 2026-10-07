# -*- coding: utf-8 -*-
"""Compare qingpdf's text extraction with PyMuPDF and pdftotext.

    python tests/tools/text_compare.py                 # public corpus + local/zh if present
    python tests/tools/text_compare.py FILE_OR_DIR ... # only these
    python tests/tools/text_compare.py --zh-only       # just the Chinese group
    python tests/tools/text_compare.py --dump DIR      # also write the three texts per file

For every PDF it runs `target/release/qingpdf text` (build first with
`cargo build --release`), PyMuPDF's `page.get_text()` and Git's `pdftotext`
(`C:\\Program Files\\Git\\mingw64\\bin\\pdftotext.exe`, or on PATH), and prints
the character similarity of ours against each:

  ordered  difflib ratio per page on the text with all white space removed
           (2 * matching chars / (chars of both)), summed over the pages: it also
           drops when whole lines or columns come in another order
  bag      the same without order: how much of the characters agree (a mapping
           problem shows here, an ordering difference does not)

Acceptance (layer 2): ordered similarity against PyMuPDF >= 98% on every file of
the Chinese group (`corpus/public/zh`, and `corpus/local/zh`). Files below are
listed at the end, to be looked at one by one. Exit status 1 when one is below.
"""
import argparse
import collections
import difflib
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CORPUS = os.path.join(ROOT, "tests", "corpus")
EXE = os.path.join(ROOT, "target", "release", "qingpdf.exe" if os.name == "nt" else "qingpdf")
PDFTOTEXT = r"C:\Program Files\Git\mingw64\bin\pdftotext.exe"
THRESHOLD = 98.0


def find_pdfs(paths):
    found = []
    for p in paths:
        if os.path.isdir(p):
            for d, _, files in os.walk(p):
                found += [os.path.join(d, f) for f in sorted(files) if f.lower().endswith(".pdf")]
        elif p.lower().endswith(".pdf"):
            found.append(p)
    return sorted(set(found))


def is_zh(path):
    return "/zh/" in path.replace("\\", "/")


def ours(path, exe, timeout=300):
    started = time.time()
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, "o.txt")
        r = subprocess.run([exe, "text", path, "-o", out], capture_output=True, timeout=timeout)
        took = time.time() - started
        if r.returncode != 0:
            return None, took, r.stderr.decode("utf-8", "replace").strip().splitlines()[-1:]
        warnings = [l for l in r.stderr.decode("utf-8", "replace").splitlines() if l.startswith("warning:")]
        with open(out, encoding="utf-8", newline="") as f:
            return f.read(), took, warnings


def pymupdf_text(path):
    import pymupdf
    pymupdf.TOOLS.mupdf_display_errors(False)
    try:
        doc = pymupdf.open(path)
        if doc.needs_pass:
            return None
        return chr(12).join(page.get_text() for page in doc)
    except Exception:  # PyMuPDF cannot read it: there is no reference text
        return None


def pdftotext(path):
    exe = PDFTOTEXT if os.path.exists(PDFTOTEXT) else shutil.which("pdftotext")
    if not exe:
        return None
    try:
        r = subprocess.run([exe, "-enc", "UTF-8", path, "-"], capture_output=True, timeout=300)
    except subprocess.TimeoutExpired:
        return None
    if r.returncode != 0:
        return None
    text = r.stdout.decode("utf-8", "replace")
    return text[:-1] if text.endswith("\f") else text


def squeeze(s):
    return re.sub(r"\s+", "", s)


def similarity(a, b):
    """(ordered %, bag %) of two texts, page by page (pages are split at form feeds)."""
    pa, pb = a.split("\f"), b.split("\f")
    n = max(len(pa), len(pb))
    pa += [""] * (n - len(pa))
    pb += [""] * (n - len(pb))
    matched = total = 0
    for x, y in zip(pa, pb):
        x, y = squeeze(x), squeeze(y)
        total += len(x) + len(y)
        if x == y:
            matched += 2 * len(x)
        elif x and y:
            matched += 2 * sum(m.size for m in difflib.SequenceMatcher(None, x, y, autojunk=False).get_matching_blocks())
    ca, cb = collections.Counter(squeeze(a)), collections.Counter(squeeze(b))
    bag = 2 * sum((ca & cb).values())
    bag_total = sum(ca.values()) + sum(cb.values())
    if total == 0:
        return 100.0, 100.0
    return 100.0 * matched / total, 100.0 * bag / bag_total if bag_total else 100.0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*")
    ap.add_argument("--zh-only", action="store_true")
    ap.add_argument("--dump", metavar="DIR")
    ap.add_argument("--max-mb", type=float, default=40.0, help="skip files bigger than this")
    ap.add_argument("--exe", default=EXE, help="the qingpdf program to test (default: the release build)")
    args = ap.parse_args()
    args.exe = os.path.abspath(args.exe)
    if not os.path.exists(args.exe):
        sys.exit("build first: cargo build --release  (%s is missing)" % args.exe)
    if args.paths:
        files = find_pdfs(args.paths)
    else:
        roots = [os.path.join(CORPUS, "public", "zh"), os.path.join(CORPUS, "local", "zh")]
        if not args.zh_only:
            roots += [os.path.join(CORPUS, "public"), os.path.join(CORPUS, "local")]
        files = find_pdfs([r for r in roots if os.path.isdir(r)])
    if args.dump:
        os.makedirs(args.dump, exist_ok=True)

    rows, below = [], []
    print("%-58s %5s %8s %8s | %7s %7s | %7s %7s | %s" % (
        "file", "pages", "chars", "time", "mu ord", "mu bag", "pt ord", "pt bag", "note"))
    for path in files:
        rel = os.path.relpath(path, CORPUS).replace("\\", "/")
        if os.path.getsize(path) > args.max_mb * 1024 * 1024:
            print("%-58s skipped (over %.0f MB)" % (rel[-58:], args.max_mb))
            continue
        try:
            text, took, notes = ours(path, args.exe)
        except subprocess.TimeoutExpired:
            print("%-58s TIMEOUT" % rel[-58:])
            below.append((rel, "timeout"))
            continue
        if text is None:
            print("%-58s %s" % (rel[-58:], "qingpdf: " + " ".join(notes)))
            continue
        mu = pymupdf_text(path)
        pt = pdftotext(path)
        pages = text.count("\f") + 1
        chars = len(squeeze(text))
        mu_o, mu_b = similarity(text, mu) if mu is not None else (None, None)
        pt_o, pt_b = similarity(text, pt) if pt is not None else (None, None)
        f = lambda v: "%6.2f%%" % v if v is not None else "      -"
        note = ""
        if notes:
            note = "%d warning(s)" % len(notes)
        if chars == 0 and mu is not None and not squeeze(mu):
            note = (note + " no text layer").strip()
        print("%-58s %5d %8d %7.2fs | %s %s | %s %s | %s" % (rel[-58:], pages, chars, took, f(mu_o), f(mu_b), f(pt_o), f(pt_b), note))
        rows.append((rel, mu_o, mu_b, pt_o, pt_b, is_zh(path)))
        if mu_o is not None and is_zh(path) and mu_o < THRESHOLD:
            below.append((rel, "%.2f%%" % mu_o))
        if args.dump:
            base = os.path.join(args.dump, rel.replace("/", "__"))
            for suffix, content in (("ours", text), ("pymupdf", mu), ("pdftotext", pt)):
                if content is not None:
                    with open(base + "." + suffix + ".txt", "w", encoding="utf-8", newline="") as out:
                        out.write(content)

    def summary(name, items, col):
        vals = [r[col] for r in items if r[col] is not None]
        if vals:
            print("  %-34s files %3d  min %6.2f%%  mean %6.2f%%" % (name, len(vals), min(vals), sum(vals) / len(vals)))

    zh = [r for r in rows if r[5]]
    print("\nsummary (ordered similarity)")
    summary("vs PyMuPDF, Chinese group", zh, 1)
    summary("vs PyMuPDF, all files", rows, 1)
    summary("vs pdftotext, Chinese group", zh, 3)
    summary("vs pdftotext, all files", rows, 3)
    worst = sorted([r for r in rows if r[1] is not None], key=lambda r: r[1])[:8]
    print("\nworst vs PyMuPDF:")
    for r in worst:
        print("  %6.2f%% (bag %6.2f%%)  %s" % (r[1], r[2], r[0]))
    if below:
        print("\nBELOW %.0f%% vs PyMuPDF in the Chinese group:" % THRESHOLD)
        for rel, v in below:
            print("  %s  %s" % (rel, v))
        sys.exit(1)
    print("\nChinese group: every file >= %.0f%% vs PyMuPDF" % THRESHOLD)


if __name__ == "__main__":
    main()

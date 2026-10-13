#!/usr/bin/env python3
"""Check a .docx made by `qingpdf docx` (5b): the structure, and the text against `qingpdf text`.

Usage:
    docx_check.py out.docx [--pdf in.pdf [--pages 1-3]] [--qingpdf PATH (default: target/release/qingpdf.exe of this repository)] [--show-missing N] [--json]
                  [--dump paragraphs.txt]

Structure (exit status 1 on any failure):
  * the zip opens and every entry's CRC and sizes are right (zipfile reads each entry to the end);
  * every .xml and .rels part parses;
  * every part has a content type (a Default for its extension or an Override), and every Override names a part that is there;
  * every relationship target is in the package; every r:id / r:embed in the document, headers and footers is defined;
  * word/document.xml has a body that ends with a sectPr, every table row has cells and every cell ends with a paragraph;
    numbering ids and styles used are defined.

Text (with --pdf): the text of document.xml is read in document order (a list item gets the label Word would write from
numbering.xml) and compared with the text of `qingpdf text`, word by word (every other character by itself, so Chinese is
compared character by character), by the longest runs that are the same in both:
  * in_order      tokens in both, in the same order;
  * moved         tokens in both but not in the same place (a table is read cell by cell, not line by line; a heading that
                  the layout put before the text it was drawn after);
  * lost          tokens of the PDF text that are not in the Word file at all, other than the running header and footer (their
                  words are in header1.xml / footer1.xml) and page numbers;
  * only_in_docx  tokens in the Word file that are not in the PDF text.
  * chars_lost    characters of the PDF text (counted, wherever they are) that are in no part of the Word file: this is the
                  "no lost characters" number (list markers are written again from numbering.xml, so they are not lost).
A word split at the end of a line ("compu-" "tation") is one word in the Word file, so it is joined in the PDF text first.
"""
import argparse
import collections
import difflib
import json
import os
import re
import subprocess
import sys
import zipfile
import xml.etree.ElementTree as ET

# The engine, relative to the root of the repository (this file is in tests/tools).
DEFAULT_QINGPDF = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "release", "qingpdf.exe")

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PR = "http://schemas.openxmlformats.org/package/2006/relationships"
CT = "http://schemas.openxmlformats.org/package/2006/content-types"
WP = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"


def q(tag):
    return "{%s}%s" % (W, tag)


def structure(z):
    problems = []
    bad = z.testzip()
    if bad:
        problems.append("zip entry %s fails its CRC" % bad)
    names = z.namelist()
    if len(set(names)) != len(names):
        problems.append("duplicate entry names")
    trees = {}
    for n in names:
        if n.endswith(".xml") or n.endswith(".rels"):
            try:
                trees[n] = ET.fromstring(z.read(n))
            except ET.ParseError as e:
                problems.append("%s does not parse: %s" % (n, e))
    ct = trees.get("[Content_Types].xml")
    if ct is None:
        problems.append("no [Content_Types].xml")
    else:
        defaults = {d.get("Extension").lower() for d in ct.findall("{%s}Default" % CT)}
        overrides = {o.get("PartName") for o in ct.findall("{%s}Override" % CT)}
        for n in names:
            if n == "[Content_Types].xml" or n.endswith("/"):
                continue
            ext = n.rsplit(".", 1)[-1].lower() if "." in n else ""
            if "/" + n not in overrides and ext not in defaults:
                problems.append("%s has no content type" % n)
        for o in overrides:
            if o.lstrip("/") not in names:
                problems.append("content type override for %s, which is not in the package" % o)
    rel_ids = {}
    for n, t in trees.items():
        if not n.endswith(".rels"):
            continue
        base = n.replace("_rels/", "", 1)[:-5] if "_rels/" in n else ""
        folder = base.rsplit("/", 1)[0] + "/" if "/" in base else ""
        ids = set()
        for rel in t.findall("{%s}Relationship" % PR):
            rid = rel.get("Id")
            if rid in ids:
                problems.append("%s: relationship id %s twice" % (n, rid))
            ids.add(rid)
            if rel.get("TargetMode") == "External":
                continue
            target = rel.get("Target")
            path = target.lstrip("/") if target.startswith("/") else (folder + target)
            parts = []
            for seg in path.split("/"):
                if seg == "..":
                    if parts:
                        parts.pop()
                elif seg != ".":
                    parts.append(seg)
            if "/".join(parts) not in names:
                problems.append("%s: target %s (%s) is not in the package" % (n, target, "/".join(parts)))
        rel_ids[base] = ids
    for n, t in trees.items():
        if n.endswith(".rels") or n == "[Content_Types].xml":
            continue
        used = set()
        for el in t.iter():
            for k, v in el.attrib.items():
                if k.startswith("{%s}" % R):
                    used.add(v)
        missing = used - rel_ids.get(n, set())
        if missing:
            problems.append("%s uses relationship ids that are not defined: %s" % (n, sorted(missing)))
    doc = trees.get("word/document.xml")
    if doc is None:
        problems.append("no word/document.xml")
        return problems, trees
    body = doc.find(q("body"))
    if body is None or len(body) == 0 or body[-1].tag != q("sectPr"):
        problems.append("the body does not end with a sectPr")
    for tbl in doc.iter(q("tbl")):
        rows = tbl.findall(q("tr"))
        if not rows:
            problems.append("a table without rows")
        for tr in rows:
            cells = tr.findall(q("tc"))
            if not cells:
                problems.append("a row without cells")
            for tc in cells:
                kids = [k for k in tc if k.tag != q("tcPr")]
                if not kids or kids[-1].tag != q("p"):
                    problems.append("a cell that does not end with a paragraph")
                    break
    num = trees.get("word/numbering.xml")
    if num is not None:
        defined = {n.get(q("numId")) for n in num.iter(q("num"))}
        for nid in doc.iter(q("numId")):
            if nid.get(q("val")) not in defined:
                problems.append("numId %s is not defined" % nid.get(q("val")))
    st = trees.get("word/styles.xml")
    if st is not None:
        known = {s.get(q("styleId")) for s in st.iter(q("style"))}
        for ps in doc.iter(q("pStyle")):
            if ps.get(q("val")) not in known:
                problems.append("style %s is not defined" % ps.get(q("val")))
    for ext in doc.iter("{%s}extent" % WP):
        if int(ext.get("cx")) <= 0 or int(ext.get("cy")) <= 0:
            problems.append("a picture of no size")
    return problems, trees


def numbering_labels(trees):
    """numId -> (format, text, start)."""
    num = trees.get("word/numbering.xml")
    out = {}
    if num is None:
        return out
    abstract = {}
    for a in num.iter(q("abstractNum")):
        lvl = a.find(q("lvl"))
        fmt = lvl.find(q("numFmt")).get(q("val"))
        text = lvl.find(q("lvlText")).get(q("val"))
        start = int(lvl.find(q("start")).get(q("val")))
        abstract[a.get(q("abstractNumId"))] = (fmt, text, start)
    for n in num.iter(q("num")):
        fmt, text, start = abstract[n.find(q("abstractNumId")).get(q("val"))]
        ov = n.find("%s/%s" % (q("lvlOverride"), q("startOverride")))
        if ov is not None:
            start = int(ov.get(q("val")))
        out[n.get(q("numId"))] = (fmt, text, start)
    return out


def paragraph_text(p):
    out = []
    for el in p.iter():
        if el.tag == q("t"):
            out.append(el.text or "")
        elif el.tag == q("tab"):
            out.append("\t")
        elif el.tag in (q("br"), q("cr")):
            out.append("\n")
    return "".join(out)


def document_content(trees):
    doc = trees["word/document.xml"]
    labels = numbering_labels(trees)
    counters = {}
    paras = []
    headings = 0
    for p in doc.iter(q("p")):
        text = paragraph_text(p)
        ppr = p.find(q("pPr"))
        style = None
        if ppr is not None:
            ps = ppr.find(q("pStyle"))
            if ps is not None:
                style = ps.get(q("val"))
            np = ppr.find(q("numPr"))
            if np is not None:
                nid = np.find(q("numId")).get(q("val"))
                fmt, ltext, start = labels[nid]
                n = counters.get(nid, start)
                counters[nid] = n + 1
                label = ltext if fmt == "bullet" else ltext.replace("%1", str(n))
                text = label + " " + text
        if style and style.startswith("Heading"):
            headings += 1
        paras.append(text)
    return {
        "paragraphs": paras,
        "headings": headings,
        "tables": len(list(doc.iter(q("tbl")))),
        "images": len(list(doc.iter(q("drawing")))),
        "sections": len(list(doc.iter(q("sectPr")))),
        "vertical_sections": len(list(doc.iter(q("textDirection")))),
    }


TOKEN = re.compile(r"[A-Za-zÀ-ɏ]+|[0-9]+|[^\s]")
PAGE_NUMBER = re.compile(r"[#\-–—·|./()\[\]]+|[ivxlcdmIVXLCDM]{1,6}")


def tokens(text):
    """Words, and every other character by itself (so Chinese text is compared character by character)."""
    return TOKEN.findall(text)


def pdf_text(path, pages, exe):
    cmd = [os.path.abspath(exe), "text", path]
    if pages:
        cmd += ["--pages", pages]
    out = subprocess.run(cmd, capture_output=True, check=True).stdout.decode("utf-8", "replace")
    return out.replace("\f", "\n")


def compare(text, content, trees, show):
    joined = re.sub(r"(?<=[a-z])-\n(?=[a-z])", "", text)
    lines = joined.split("\n")
    a, a_line = [], []
    for i, line in enumerate(lines):
        for t in tokens(line):
            a.append(t)
            a_line.append(i)
    band = ""
    for part in ("word/header1.xml", "word/footer1.xml"):
        if part in trees:
            band += " ".join(paragraph_text(p) for p in trees[part].iter(q("p"))) + " "
    b = tokens(" ".join(content["paragraphs"]))
    sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
    equal = 0
    deleted, inserted = [], []
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            equal += i2 - i1
        else:
            deleted.extend(range(i1, i2))
            inserted.extend(b[j1:j2])
    ins = collections.Counter(inserted)
    moved = 0
    gone = []
    for i in deleted:
        if ins[a[i]] > 0:
            ins[a[i]] -= 1
            moved += 1
        else:
            gone.append(i)
    in_band = collections.Counter(re.sub(r"\d", "#", t) for t in tokens(band))
    expected, lost = [], []
    for i in gone:
        t = re.sub(r"\d", "#", a[i])
        if in_band[t] > 0 or PAGE_NUMBER.fullmatch(t):
            expected.append(i)
        else:
            lost.append(i)
    samples = []
    for i in lost:
        line = lines[a_line[i]]
        if line not in samples:
            samples.append(line)
        if len(samples) >= show:
            break
    # And by characters, wherever they are: what is in the PDF text and nowhere in the Word file (the header and footer count).
    pc = collections.Counter(c for c in joined if not c.isspace())
    dc = collections.Counter(c for c in " ".join(content["paragraphs"]) + band if not c.isspace())
    # The running header and footer are once in the Word file and on every page of the PDF; a page number is a field there.
    band_lines = set()
    for part in ("word/header1.xml", "word/footer1.xml"):
        if part in trees:
            band_lines.update(re.sub(r"\d", "#", re.sub(r"\s+", "", paragraph_text(p))) for p in trees[part].iter(q("p")))
    expected_chars = collections.Counter()
    for line in lines:
        squeezed = re.sub(r"\s+", "", line)
        norm = re.sub(r"\d", "#", squeezed)
        if squeezed and ((norm in band_lines) or PAGE_NUMBER.fullmatch(norm)):
            expected_chars.update(squeezed)
    lost_chars = pc - dc - expected_chars
    return {
        "chars_in_pdf_text": sum(pc.values()),
        "chars_lost": sum(lost_chars.values()),
        "chars_lost_top": lost_chars.most_common(6),
        "chars_only_in_docx": sum((dc - pc).values()),
        "tokens_in_pdf_text": len(a),
        "tokens_in_docx": len(b),
        "in_order": equal,
        "moved": moved,
        "dropped_as_header_footer_or_number": len(expected),
        "lost": len(lost),
        "lost_samples": samples,
        "only_in_docx": sum(ins.values()),
    }


def main():
    # (The report has Chinese in it; the console's code page may not.)
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser()
    ap.add_argument("docx")
    ap.add_argument("--pdf")
    ap.add_argument("--pages")
    ap.add_argument("--qingpdf", default=DEFAULT_QINGPDF)
    ap.add_argument("--show-missing", type=int, default=5)
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--dump", help="write the paragraphs of the document, one to a line, to this file")
    a = ap.parse_args()
    try:
        z = zipfile.ZipFile(a.docx)
    except zipfile.BadZipFile as e:
        print("FAIL: not a zip file:", e)
        return 1
    problems, trees = structure(z)
    result = {"docx": a.docx, "bytes": os.path.getsize(a.docx), "parts": len(z.namelist()), "problems": problems}
    if "word/document.xml" in trees:
        content = document_content(trees)
        result.update({k: v for k, v in content.items() if k != "paragraphs"})
        if a.dump:
            with open(a.dump, "w", encoding="utf-8") as f:
                f.write(chr(10).join(content["paragraphs"]))
        result["paragraph_count"] = len(content["paragraphs"])
        result["chars"] = sum(len(p) for p in content["paragraphs"])
        if a.pdf:
            result["compare"] = compare(pdf_text(a.pdf, a.pages, a.qingpdf), content, trees, a.show_missing)
    if a.json:
        print(json.dumps(result, ensure_ascii=False, indent=1))
    else:
        print("%s: %s" % (a.docx, "OK" if not problems else "FAIL"))
        for p in problems:
            print("  problem:", p)
        for k, v in result.items():
            if k not in ("docx", "problems", "compare"):
                print("  %s: %s" % (k, v))
        if "compare" in result:
            for k, v in result["compare"].items():
                print("  text.%s: %s" % (k, v))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

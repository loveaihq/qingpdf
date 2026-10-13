#!/usr/bin/env python3
"""Check a .xlsx made by `qingpdf xlsx` (5d): the structure, what the cells are, and the text against `qingpdf text`.

Usage:
    xlsx_check.py out.xlsx [--pdf in.pdf [--pages 1-3]] [--qingpdf PATH (default: target/release/qingpdf.exe of this repository)]
                  [--dump N] [--json]

Structure (exit status 1 on any failure):
  * the zip opens and every entry's CRC is right;
  * every .xml and .rels part parses;
  * every part has a content type (a Default for its extension or an Override), and every Override names a part that is there;
  * the package relationship names the workbook, every relationship target is in the package, every sheet of the workbook has a
    relationship to a worksheet part, sheet ids are different, sheet names are at most 31 characters, none of [ ] : * ? / \\ , no
    name begins or ends with an apostrophe, and no two are the same (case ignored);
  * in the sheets: rows and cells are in order, every cell is inside the dimension, a cell is a shared string (t="s", an index that
    exists), a number (with a style that exists), or an inline/other string; merged ranges are well written and do not overlap;
  * the shared strings part has `uniqueCount` entries and `count` equals the cells that use one; the styles part has the number
    formats the cell formats name.

Cells: each sheet is listed with its rows and columns, and what is in each column (text, whole or decimal numbers, dates, amounts);
`--dump N` lists the first N cells of every sheet: reference, type (text, number, date) and value, with the format. Problems for
cells: text that is surely a number in a format (an amount, a date) stored as text; a number that was an identity number, a code
with a zero at the front or a number of more than 15 digits.

Text (with --pdf): the cells of the sheets of page P are compared with the text of page P of `qingpdf text`, word by word (Chinese
character by character), with numbers and dates shown as their format shows them:
  * extra     tokens in the cells that the page text does not have (must be 0);
  * covered   share of the page text's tokens that are in the cells of its tables (the rest is text outside the tables).
"""
import argparse
import collections
import datetime
import json
import os
import re
import subprocess
import sys
import zipfile
import xml.etree.ElementTree as ET

DEFAULT_QINGPDF = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "release", "qingpdf.exe")

S = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PR = "http://schemas.openxmlformats.org/package/2006/relationships"
CT = "http://schemas.openxmlformats.org/package/2006/content-types"
TOKEN = re.compile(r"[A-Za-zÀ-ɏ]+|[0-9]+|[^\s]")


def q(tag, ns=S):
    return "{%s}%s" % (ns, tag)


def col_index(letters):
    n = 0
    for ch in letters:
        n = n * 26 + ord(ch) - 64
    return n - 1


def split_ref(ref):
    m = re.fullmatch(r"([A-Z]{1,3})([0-9]+)", ref)
    if not m:
        return None
    return col_index(m.group(1)), int(m.group(2)) - 1


def structure(z):
    problems = []
    trees = {}
    bad = z.testzip()
    if bad:
        problems.append("zip entry %s fails its CRC" % bad)
    names = z.namelist()
    if len(set(names)) != len(names):
        problems.append("duplicate zip entries")
    for n in names:
        if n.endswith(".xml") or n.endswith(".rels"):
            try:
                trees[n] = ET.fromstring(z.read(n))
            except ET.ParseError as e:
                problems.append("%s does not parse: %s" % (n, e))
    # content types
    types = trees.get("[Content_Types].xml")
    if types is None:
        problems.append("no [Content_Types].xml")
    else:
        defaults = {d.get("Extension").lower() for d in types.iter(q("Default", CT))}
        overrides = {o.get("PartName") for o in types.iter(q("Override", CT))}
        for n in names:
            if n == "[Content_Types].xml":
                continue
            if "/" + n not in overrides and n.rsplit(".", 1)[-1].lower() not in defaults:
                problems.append("%s has no content type" % n)
        for o in overrides:
            if o.lstrip("/") not in names:
                problems.append("content type override for %s, which is not in the package" % o)
    # relationships
    def rel_targets(rels_name, base):
        out = {}
        tree = trees.get(rels_name)
        if tree is None:
            problems.append("no %s" % rels_name)
            return out
        for r in tree.iter(q("Relationship", PR)):
            target = r.get("Target")
            path = os.path.normpath(os.path.join(base, target)).replace("\\", "/") if not target.startswith("/") else target[1:]
            if path not in names and r.get("TargetMode") != "External":
                problems.append("%s: %s is not in the package" % (rels_name, target))
            out[r.get("Id")] = (r.get("Type").rsplit("/", 1)[-1], path)
        return out
    root = rel_targets("_rels/.rels", "")
    workbook_path = next((p for t, p in root.values() if t == "officeDocument"), None)
    if workbook_path is None:
        problems.append("the package has no officeDocument relationship")
        return problems, trees, {}
    base = os.path.dirname(workbook_path)
    wb_rels = rel_targets("%s/_rels/%s.rels" % (base, os.path.basename(workbook_path)), base)
    wb = trees.get(workbook_path)
    sheets = []
    seen_names, seen_ids = set(), set()
    if wb is not None:
        for sh in wb.iter(q("sheet")):
            name, sid, rid = sh.get("name"), sh.get("sheetId"), sh.get(q("id", R))
            if not name or len(name.encode("utf-16-le")) // 2 > 31 or re.search(r"[\[\]:*?/\\]", name) or name.startswith("'") or name.endswith("'"):
                problems.append("bad sheet name %r" % name)
            if name and name.lower() in seen_names:
                problems.append("sheet name %r is used twice" % name)
            seen_names.add((name or "").lower())
            if sid in seen_ids:
                problems.append("sheet id %s is used twice" % sid)
            seen_ids.add(sid)
            if rid not in wb_rels or wb_rels[rid][0] != "worksheet":
                problems.append("sheet %r has no worksheet relationship (%s)" % (name, rid))
                continue
            sheets.append((name, wb_rels[rid][1]))
        if not sheets:
            problems.append("the workbook has no sheet")
    # shared strings and styles
    strings = []
    sst_path = next((p for t, p in wb_rels.values() if t == "sharedStrings"), None)
    sst = trees.get(sst_path) if sst_path else None
    if sst is not None:
        for si in sst.iter(q("si")):
            strings.append("".join(t.text or "" for t in si.iter(q("t"))))
        if sst.get("uniqueCount") is not None and int(sst.get("uniqueCount")) != len(strings):
            problems.append("sharedStrings uniqueCount %s is not %d" % (sst.get("uniqueCount"), len(strings)))
        if len(set(strings)) != len(strings):
            problems.append("sharedStrings has %d strings that are the same as another" % (len(strings) - len(set(strings))))
    styles_path = next((p for t, p in wb_rels.values() if t == "styles"), None)
    styles = trees.get(styles_path) if styles_path else None
    formats, xfs = {}, []
    if styles is not None:
        for nf in styles.iter(q("numFmt")):
            formats[int(nf.get("numFmtId"))] = nf.get("formatCode")
        cell_xfs = styles.find(q("cellXfs"))
        if cell_xfs is not None:
            xfs = [int(x.get("numFmtId", "0")) for x in cell_xfs.findall(q("xf"))]
            if cell_xfs.get("count") is not None and int(cell_xfs.get("count")) != len(xfs):
                problems.append("cellXfs count %s is not %d" % (cell_xfs.get("count"), len(xfs)))
        for nid in xfs:
            if nid >= 164 and nid not in formats:
                problems.append("cell format uses number format %d, which is not defined" % nid)
    info = {"sheets": sheets, "strings": strings, "formats": formats, "xfs": xfs, "uses": 0, "sst_count": int(sst.get("count")) if sst is not None and sst.get("count") else None}
    return problems, trees, info


BUILTIN = {1: "0", 2: "0.00", 3: "#,##0", 4: "#,##0.00", 9: "0%", 10: "0.00%", 14: "yyyy-mm-dd", 22: "yyyy-mm-dd hh:mm"}


def is_date_code(code):
    return bool(re.search(r"yy|dd|hh", re.sub(r'"[^"]*"', "", code or "")))


def date_text(v, code):
    secs = round(v * 86400)
    d = datetime.datetime(1899, 12, 30) + datetime.timedelta(seconds=secs)
    out, after_h = [], False
    for tok in re.findall(r'yyyy|hh|ss|dd|mm|m|d|"[^"]*"|.', code):
        if tok == "yyyy":
            out.append("%04d" % d.year)
        elif tok == "mm":
            out.append("%02d" % (d.minute if after_h else d.month))
        elif tok == "m":
            out.append(str(d.month))
        elif tok == "dd":
            out.append("%02d" % d.day)
        elif tok == "d":
            out.append(str(d.day))
        elif tok == "hh":
            out.append("%02d" % d.hour)
            after_h = True
        elif tok == "ss":
            out.append("%02d" % d.second)
        elif tok.startswith('"'):
            out.append(tok[1:-1])
        else:
            out.append(tok)
    return "".join(out)


def number_text(v, code):
    if not code or code == "General":
        return "%.15g" % v
    sections = code.split(";")
    sec = sections[1] if v < 0 and len(sections) > 1 else sections[0]
    shown_v = abs(v) if (v < 0 and len(sections) > 1) else v
    m = re.search(r"[#0,]*0(\.0+)?%?", sec)
    if not m:
        return "%.15g" % v
    body = m.group(0)
    decimals = len(m.group(1)) - 1 if m.group(1) else 0
    pct = body.endswith("%")
    x = shown_v * (100 if pct else 1)
    text = ("{:,.%df}" if "," in body else "{:.%df}") % decimals
    text = text.format(x) + ("%" if pct else "")
    lit = lambda s: re.sub(r'_.', "", s).replace('"', "")
    return lit(sec[: m.start()]) + text + lit(sec[m.end():])


def cells_of(tree, info, problems, sheet_name):
    """The cells of a worksheet: (row, col, kind, value, format code, shown text)."""
    cells = []
    dim = tree.find(q("dimension"))
    box = None
    if dim is not None:
        parts = dim.get("ref").split(":")
        a, b = split_ref(parts[0]), split_ref(parts[-1])
        if not a or not b:
            problems.append("%s: dimension %r is not a range" % (sheet_name, dim.get("ref")))
        else:
            box = (a, b)
    last_row = -1
    for row in tree.iter(q("row")):
        r = int(row.get("r")) - 1
        if r <= last_row:
            problems.append("%s: row %d is out of order" % (sheet_name, r + 1))
        last_row, last_col = r, -1
        for c in row.findall(q("c")):
            ref = split_ref(c.get("r"))
            if not ref or ref[1] != r:
                problems.append("%s: cell %s is not in row %d" % (sheet_name, c.get("r"), r + 1))
                continue
            if ref[0] <= last_col:
                problems.append("%s: cell %s is out of order" % (sheet_name, c.get("r")))
            last_col = ref[0]
            if box and not (box[0][0] <= ref[0] <= box[1][0] and box[0][1] <= ref[1] <= box[1][1]):
                problems.append("%s: cell %s is outside the dimension" % (sheet_name, c.get("r")))
            t = c.get("t", "n")
            v = c.find(q("v"))
            style = int(c.get("s", "0"))
            if style >= len(info["xfs"]):
                problems.append("%s: cell %s has style %d, which does not exist" % (sheet_name, c.get("r"), style))
                continue
            nid = info["xfs"][style]
            code = info["formats"].get(nid) or BUILTIN.get(nid) or "General"
            if t == "s":
                info["uses"] += 1
                idx = int(v.text)
                if idx >= len(info["strings"]):
                    problems.append("%s: cell %s uses string %d, which does not exist" % (sheet_name, c.get("r"), idx))
                    continue
                text = info["strings"][idx]
                cells.append((r, ref[0], "text", text, None, text))
            elif t == "inlineStr":
                text = "".join(x.text or "" for x in c.iter(q("t")))
                cells.append((r, ref[0], "text", text, None, text))
            elif t == "n" and v is not None:
                val = float(v.text)
                if is_date_code(code):
                    cells.append((r, ref[0], "date", val, code, date_text(val, code)))
                else:
                    cells.append((r, ref[0], "number", val, code, number_text(val, code)))
            else:
                problems.append("%s: cell %s has an unexpected type %r" % (sheet_name, c.get("r"), t))
    merged = []
    for m in tree.iter(q("mergeCell")):
        parts = m.get("ref").split(":")
        a, b = split_ref(parts[0]), split_ref(parts[-1]) if len(parts) == 2 else None
        if len(parts) != 2 or not a or not b or a[0] > b[0] or a[1] > b[1]:
            problems.append("%s: merged range %r is not well written" % (sheet_name, m.get("ref")))
            continue
        for o in merged:
            if not (b[0] < o[0][0] or a[0] > o[1][0] or b[1] < o[0][1] or a[1] > o[1][1]):
                problems.append("%s: merged ranges overlap at %s" % (sheet_name, m.get("ref")))
        merged.append((a, b))
    mc = tree.find(q("mergeCells"))
    if mc is not None and mc.get("count") is not None and int(mc.get("count")) != len(merged):
        problems.append("%s: mergeCells count %s is not %d" % (sheet_name, mc.get("count"), len(merged)))
    return cells, merged, box


LOOKS_NUMERIC = re.compile(r"^[-+(]?[¥￥$€£]?[-+]?\d{1,3}(,\d{3})+(\.\d+)?%?\)?$|^[-+(]?[¥￥$€£]?[-+]?\d{1,9}(\.\d+)?%?\)?$|^\d{4}[-/.]\d{1,2}[-/.]\d{1,2}$|^\d{4}年\d{1,2}月\d{1,2}日$")


def judge(cells, sheet_name, problems):
    """What each column holds, and the cells a spreadsheet may have spoiled or that should be numbers."""
    cols = collections.defaultdict(collections.Counter)
    notes = collections.Counter()
    for r, c, kind, val, code, shown in cells:
        # (A whole number in brackets or with the minus after it, "(1)" or "3-", is text unless its column is made of amounts.)
        if kind == "text" and LOOKS_NUMERIC.match(val.strip()) and not re.fullmatch(r"0\d+|\(\d+\)|\d+[-−]", val.strip()):
            notes["number_or_date_kept_as_text"] += 1
        if kind == "number":
            digits = len(re.sub(r"\D", "", ("%.15g" % abs(val)).split(".")[0]))
            if digits >= 10 and (code in (None, "General") or code == "0"):
                notes["whole_number_of_ten_digits_or_more"] += 1
            if "e" in ("%r" % val).lower() and abs(val) >= 1e15:
                notes["number_over_15_digits"] += 1
            if re.match(r"^0\d", shown):
                notes["leading_zero_lost"] += 1
        label = kind
        if kind == "number":
            label = "amount" if '"' in (code or "") else ("percent" if "%" in (code or "") else ("decimal" if "." in (code or "") else "whole"))
        cols[c][label] += 1
    return cols, notes


def tokens(text):
    return TOKEN.findall(text)


def pdf_pages(path, pages, exe):
    cmd = [os.path.abspath(exe), "text", path]
    if pages:
        cmd += ["--pages", pages]
    out = subprocess.run(cmd, capture_output=True, check=True).stdout.decode("utf-8", "replace")
    return out.split("\f")


def page_numbers(spec, count):
    if not spec:
        return list(range(1, count + 1))
    out = []
    for part in spec.split(","):
        if "-" in part:
            a, b = part.split("-")
            out += list(range(int(a), int(b) + 1))
        else:
            out.append(int(part))
    return out


def col_letters(c):
    s = ""
    c += 1
    while c:
        c, rem = divmod(c - 1, 26)
        s = chr(65 + rem) + s
    return s


def main():
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser()
    ap.add_argument("xlsx")
    ap.add_argument("--pdf")
    ap.add_argument("--pages")
    ap.add_argument("--qingpdf", default=DEFAULT_QINGPDF)
    ap.add_argument("--dump", type=int, default=0, help="list the first N cells of each sheet")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    try:
        z = zipfile.ZipFile(a.xlsx)
    except zipfile.BadZipFile as e:
        print("FAIL: not a zip file:", e)
        return 1
    problems, trees, info = structure(z)
    report = {"xlsx": a.xlsx, "bytes": os.path.getsize(a.xlsx), "parts": len(z.namelist()), "problems": problems, "sheets": []}
    by_page = collections.defaultdict(list)
    notes_all = collections.Counter()
    for name, path in info.get("sheets", []):
        tree = trees.get(path)
        if tree is None:
            continue
        cells, merged, box = cells_of(tree, info, problems, name)
        cols, notes = judge(cells, name, problems)
        notes_all.update(notes)
        rows = (box[1][1] + 1) if box else 0
        ncols = (box[1][0] + 1) if box else 0
        kinds = {col_letters(c): dict(k) for c, k in sorted(cols.items())}
        report["sheets"].append({"name": name, "rows": rows, "cols": ncols, "cells": len(cells), "merged": len(merged), "columns": kinds, "notes": dict(notes)})
        m = re.match(r"第(\d+)页-表\d+", name)
        if m:
            by_page[int(m.group(1))] += [c[5] for c in cells]
        if a.dump:
            print("--- %s (%d rows x %d columns, %d cells, %d merged)" % (name, rows, ncols, len(cells), len(merged)))
            for r, c, kind, val, code, shown in cells[: a.dump]:
                print("  %s%d  %-6s %s%s" % (col_letters(c), r + 1, kind, repr(val) if kind != "text" else val, "   [%s] -> %s" % (code, shown) if code else ""))
    if info.get("sst_count") is not None and info["uses"] != info["sst_count"]:
        problems.append("sharedStrings count %s is not the %d cells that use a string" % (info["sst_count"], info["uses"]))
    report["notes"] = dict(notes_all)
    for k, n in notes_all.items():
        problems.append("%d cells: %s" % (n, k))
    if a.pdf and by_page:
        pages = pdf_pages(a.pdf, a.pages, a.qingpdf)
        numbers = page_numbers(a.pages, len(pages))
        extra_total, page_total, covered_total = 0, 0, 0
        extras = []
        for number, text in zip(numbers, pages):
            if number not in by_page:
                continue
            page_tokens = collections.Counter(tokens(text))
            cell_tokens = collections.Counter(t for shown in by_page[number] for t in tokens(shown))
            extra = cell_tokens - page_tokens
            covered = sum((cell_tokens & page_tokens).values())
            extra_total += sum(extra.values())
            page_total += sum(page_tokens.values())
            covered_total += covered
            if extra:
                extras.append((number, dict(list(extra.items())[:8])))
        report["text"] = {"tokens_extra": extra_total, "page_tokens": page_total, "covered": covered_total, "extra_examples": extras[:5]}
    if a.json:
        print(json.dumps(report, ensure_ascii=False, indent=1))
    else:
        print("%s: %s" % (a.xlsx, "OK" if not problems else "FAIL"))
        for p in problems:
            print("  problem:", p)
        print("  bytes: %d, parts: %d, sheets: %d" % (report["bytes"], report["parts"], len(report["sheets"])))
        for s in report["sheets"]:
            print("  sheet %-18s rows %-4d cols %-3d cells %-5d merged %-3d columns %s" % (s["name"], s["rows"], s["cols"], s["cells"], s["merged"], json.dumps(s["columns"], ensure_ascii=False)))
        if "text" in report:
            for k, v in report["text"].items():
                print("  text.%s: %s" % (k, v))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

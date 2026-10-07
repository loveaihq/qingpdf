# -*- coding: utf-8 -*-
"""Hand-written minimal Chinese PDFs for qingpdf tests.

Emits raw PDF bytes with plain string concatenation. No PDF library is used,
so every byte in the output is known. Run:  python make_handmade.py
Output goes next to this script (the .pdf files and the .expected.txt sidecars).

All fonts are NON-embedded predefined-CMap CID fonts (ISO 32000-1 9.7).
Text is written as UCS-2BE hex strings, because the UniGB-UCS2-* and
UniCNS-UCS2-* CMaps take 2-byte Unicode code points as character codes.
The poems used (Tang dynasty) are in the public domain.

Object layout (fixed):
  1 Catalog, 2 Pages, 3 Page, 4 Type0 font, 5 CIDFont, 6 FontDescriptor,
  7 Contents, 8 ToUnicode (only when present)
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))


def hexs(text):
    return "<" + text.encode("utf-16-be").hex().upper() + ">"


def build_pdf(objects, header_ver="1.5"):
    """objects: list of bytes bodies for object 1..N."""
    out = bytearray()
    out += ("%PDF-" + header_ver + "\n").encode("ascii")
    out += b"%\xe2\xe3\xcf\xd3\n"
    offsets = []
    for i, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += ("%d 0 obj\n" % i).encode("ascii") + body + b"\nendobj\n"
    xref_pos = len(out)
    n = len(objects) + 1
    out += ("xref\n0 %d\n" % n).encode("ascii")
    out += b"0000000000 65535 f \n"
    for off in offsets:
        out += ("%010d 00000 n \n" % off).encode("ascii")
    out += ("trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (n, xref_pos)).encode("ascii")
    return bytes(out)


def stream(dict_body, data):
    head = "<< %s/Length %d >>\nstream\n" % ((dict_body + " ") if dict_body else "", len(data))
    return head.encode("ascii") + data + b"\nendstream"


def tounicode_cmap(chars, overrides=None):
    """ToUnicode CMap for codes that equal their own Unicode value.
    Runs of 3+ consecutive code points become bfrange, the rest bfchar,
    so both forms are exercised. `overrides` maps a character to the text its
    code must give instead (one or more characters): written as bfchar."""
    overrides = overrides or {}
    cps = sorted(set(ord(c) for c in chars if c not in overrides))
    runs, cur = [], [cps[0]]
    for cp in cps[1:]:
        if cp == cur[-1] + 1:
            cur.append(cp)
        else:
            runs.append(cur)
            cur = [cp]
    runs.append(cur)
    bfchar = ["<%04X> <%s>" % (ord(c), v.encode("utf-16-be").hex().upper()) for c, v in sorted(overrides.items())]
    bfrange = []
    for r in runs:
        if len(r) >= 3:
            bfrange.append("<%04X> <%04X> <%04X>" % (r[0], r[-1], r[0]))
        else:
            for cp in r:
                bfchar.append("<%04X> <%04X>" % (cp, cp))
    s = ["/CIDInit /ProcSet findresource begin", "12 dict begin", "begincmap",
         "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def",
         "/CMapName /Adobe-Identity-UCS def", "/CMapType 2 def",
         "1 begincodespacerange", "<0000> <FFFF>", "endcodespacerange"]
    for i in range(0, len(bfchar), 100):
        chunk = bfchar[i:i + 100]
        s += ["%d beginbfchar" % len(chunk)] + chunk + ["endbfchar"]
    for i in range(0, len(bfrange), 100):
        chunk = bfrange[i:i + 100]
        s += ["%d beginbfrange" % len(chunk)] + chunk + ["endbfrange"]
    s += ["endcmap", "CMapName currentdict /CMap defineresource pop", "end", "end", ""]
    return "\n".join(s).encode("ascii")


def make_pdf(content, type0_basefont, cid_basefont, encoding, ordering, supplement, tounicode=None):
    t0 = ("<< /Type /Font /Subtype /Type0 /BaseFont /%s /Encoding /%s "
          "/DescendantFonts [5 0 R]" % (type0_basefont, encoding))
    if tounicode:
        t0 += " /ToUnicode 8 0 R"
    t0 += " >>"
    cid = ("<< /Type /Font /Subtype /CIDFontType0 /BaseFont /%s "
           "/CIDSystemInfo << /Registry (Adobe) /Ordering (%s) /Supplement %d >> "
           "/FontDescriptor 6 0 R /DW 1000 >>" % (cid_basefont, ordering, supplement))
    fd = ("<< /Type /FontDescriptor /FontName /%s /Flags 6 /FontBBox [-25 -254 1000 880] "
          "/ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 880 /StemV 93 >>" % cid_basefont)
    objs = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] "
        b"/Resources << /Font << /F1 4 0 R >> >> /Contents 7 0 R >>",
        t0.encode("ascii"),
        cid.encode("ascii"),
        fd.encode("ascii"),
        stream("", content),
    ]
    if tounicode:
        objs.append(stream("", tounicode))
    return build_pdf(objs)


def horizontal_content(lines, size=24, x=60, y=780, leading=44):
    s = ["BT", "/F1 %d Tf" % size, "%d TL" % leading, "%d %d Td" % (x, y)]
    for i, line in enumerate(lines):
        if i:
            s.append("T*")
        s.append("%s Tj" % hexs(line))
    s.append("ET")
    return ("\n".join(s) + "\n").encode("ascii")


def vertical_content(columns, size=28, x_right=500, y_top=770, col_gap=60):
    """Each column is its own text object; first column is the rightmost
    (traditional reading order). With a -V CMap the x given to Tm is the
    horizontal centre of the glyph column."""
    s = []
    for i, col in enumerate(columns):
        s += ["BT", "/F1 %d Tf" % size, "1 0 0 1 %d %d Tm" % (x_right - i * col_gap, y_top),
              "%s Tj" % hexs(col), "ET"]
    return ("\n".join(s) + "\n").encode("ascii")


SIMP_LINES = [
    "轻PDF中文测试文件",
    "床前明月光，疑是地上霜。",
    "举头望明月，低头思故乡。",
    "一二三四五六七八九十 0123456789",
    "国家标准字符集：常用汉字、标点。",
]
SIMP_VERT_COLUMNS = [
    "床前明月光，疑是地上霜。",
    "举头望明月，低头思故乡。",
    "轻PDF中文竖排测试",
]
TRAD_LINES = [
    "輕PDF中文測試文件",
    "春眠不覺曉，處處聞啼鳥。",
    "夜來風雨聲，花落知多少。",
    "一二三四五六七八九十 0123456789",
    "繁體字：國家圖書館、臺灣、龍鳳。",
]


def write(name, pdf, expected_lines):
    with open(os.path.join(HERE, name + ".pdf"), "wb") as f:
        f.write(pdf)
    with open(os.path.join(HERE, name + ".expected.txt"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(expected_lines) + "\n")


def main():
    # 1. non-embedded STSong-Light, UniGB-UCS2-H, no ToUnicode
    write("zh-gb1-h-noembed",
          make_pdf(horizontal_content(SIMP_LINES), "STSong-Light", "STSong-Light",
                   "UniGB-UCS2-H", "GB1", 2),
          SIMP_LINES)
    # 2. vertical writing, UniGB-UCS2-V (columns run top to bottom, first column rightmost)
    write("zh-gb1-v-noembed",
          make_pdf(vertical_content(SIMP_VERT_COLUMNS), "STSong-Light", "STSong-Light",
                   "UniGB-UCS2-V", "GB1", 2),
          SIMP_VERT_COLUMNS)
    # 3. same as 1, with a ToUnicode CMap
    write("zh-gb1-h-tounicode",
          make_pdf(horizontal_content(SIMP_LINES), "STSong-Light", "STSong-Light",
                   "UniGB-UCS2-H", "GB1", 2, tounicode=tounicode_cmap("".join(SIMP_LINES))),
          SIMP_LINES)
    # 4. Traditional Chinese, CNS1, MSung-Light, UniCNS-UCS2-H
    write("zh-cns1-h-noembed",
          make_pdf(horizontal_content(TRAD_LINES), "MSung-Light", "MSung-Light",
                   "UniCNS-UCS2-H", "CNS1", 3),
          TRAD_LINES)
    # 6. ToUnicode that DISAGREES with the predefined CMap (9.10.2: ToUnicode wins).
    #    The codes are UniGB-UCS2 Unicode values, so a reader that took the predefined
    #    CMap (or the Adobe-GB1 table) would give the original character. Three
    #    characters are remapped, one of them to two characters (a ligature-like value).
    overrides = {"床": "牀", "明": "朙", "汉": "漢字"}
    differs_expected = ["".join(overrides.get(c, c) for c in line) for line in SIMP_LINES]
    write("zh-gb1-h-tounicode-differs",
          make_pdf(horizontal_content(SIMP_LINES), "STSong-Light", "STSong-Light",
                   "UniGB-UCS2-H", "GB1", 2,
                   tounicode=tounicode_cmap("".join(SIMP_LINES), overrides)),
          differs_expected)
    # 5. (extra) same as 1, but the Type0 BaseFont follows the ISO 32000-1 Table 121 convention
    #    "<CIDFont BaseFont>-<CMap name>", which is what most real producers write
    write("zh-gb1-h-basefont-suffix",
          make_pdf(horizontal_content(SIMP_LINES), "STSong-Light-UniGB-UCS2-H", "STSong-Light",
                   "UniGB-UCS2-H", "GB1", 2),
          SIMP_LINES)


if __name__ == "__main__":
    main()

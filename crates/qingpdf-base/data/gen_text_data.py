# -*- coding: utf-8 -*-
"""Generate the data tables embedded by qingpdf-core's text extraction (layer 2).

    python gen_text_data.py <cmap-resources checkout> <agl-aglfn checkout> <CJKRadicals.txt>

Inputs (not stored in this repository):
  * https://github.com/adobe-type-tools/cmap-resources  (BSD-3-Clause)
        pinned at f5cf3bca7fdfeaceb77aa82847e974f2306c20b4
  * https://github.com/adobe-type-tools/agl-aglfn        (BSD-3-Clause)
        pinned at 4036a9ca80a62f64f9de4f7321a9a045ad0ecfd6
  * https://www.unicode.org/Public/UCD/latest/ucd/CJKRadicals.txt (Unicode 18.0.0, Unicode
    License v3): the radical characters and the ideographs they stand for.
  * data/src/symbol-encoding.txt, data/src/zapfdingbats-encoding.txt: the code to
    glyph-name tables of ISO 32000-1 Annex D.5 and D.6 ("octal-code name").

Outputs (checked in, zlib-compressed, read with miniz_oxide at run time):
  data/text/cid2uni-<ordering>.bin   CID -> Unicode, one table per Adobe character collection
                                     (cid2code.txt's Unicode columns first, then the legacy code columns)
  data/text/cmaps-<ordering>.bin     the legacy (non-Unicode) predefined CMaps of that collection
  data/text/agl.bin                  Adobe Glyph List + ZapfDingbats names
  data/text/cjknorm.bin              Kangxi radicals, CJK radicals and compatibility ideographs to
                                     the unified ideograph they stand for (CJKRadicals.txt; NFKC)
  src/text/encodings.rs              StandardEncoding, WinAnsi, MacRoman, Symbol, ZapfDingbats
                                     (code -> Unicode, 0 = no character)

Formats (all integers are LEB128 varints unless said otherwise):

cid2uni:   count N, then for each CID 0..N-1 one value v. v = 0: no Unicode value; else
           code point = previous mapped code point + 1 + unzigzag(v - 1) (previous starts at 0).
cmaps:     count; per CMap: name (len, bytes), flags (1 byte; bit 0 = has /UseCMap, its name
           follows as name), code space count, per code space (n bytes, lo, hi), then
           per code length n = 1..4 a group: range count, per range
           (lo - previous hi, hi - lo, zigzag(cid - previous end cid - 1)); the first range
           of a group is relative to 0.
agl:       text, one "name;HEX[ HEX...]" per line, sorted by name.
cjknorm:   count, then per entry (code point - previous code point, target code point),
           sorted by code point.
"""
import os
import re
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "text")

# Predefined CMaps of ISO 32000-1 Table 118 that are not code=Unicode (those need no table:
# the code is the Unicode value). The vertical (-V) variants differ only in a few glyph
# substitutions of the same characters, so they are served by the -H table.
COLLECTIONS = {
    "gb1": ("Adobe-GB1-6", "UniGB",
            ["GB-EUC-H", "GBpc-EUC-H", "GBK-EUC-H", "GBKp-EUC-H", "GBK2K-H"]),
    "cns1": ("Adobe-CNS1-7", "UniCNS",
             ["B5pc-H", "HKscs-B5-H", "ETen-B5-H", "ETenms-B5-H", "CNS-EUC-H"]),
    "japan1": ("Adobe-Japan1-7", "UniJIS",
               ["83pv-RKSJ-H", "90ms-RKSJ-H", "90msp-RKSJ-H", "90pv-RKSJ-H", "Add-RKSJ-H",
                "EUC-H", "Ext-RKSJ-H", "H"]),
    "korea1": ("Adobe-Korea1-2", "UniKS",
               ["KSC-EUC-H", "KSCms-UHC-H", "KSCms-UHC-HW-H", "KSCpc-EUC-H"]),
}

# Many CIDs (the half-width and proportional Latin, the vertical forms) have no Unicode value in
# cid2code.txt's Unicode columns, because no Uni* CMap maps to them; Adobe's own CID to Unicode
# files give them the character they show. Their legacy code columns tell which: decoded with
# the matching Python codec ("seven-bit" columns are JIS-style 7-bit codes, |0x8080 to make EUC).
UNICODE_EXTRA = {"japan1": ["UniJIS-UCS2-HW"]}
LEGACY = {
    "gb1": [("GBK-EUC", "gbk", False), ("GBKp-EUC", "gbk", False), ("GBpc-EUC", "gb2312", False),
            ("GB-EUC", "gb2312", False), ("GBK2K", "gb18030", False), ("GBT-EUC", "gb2312", False),
            ("GB", "gb2312", True)],
    "cns1": [("ETen-B5", "cp950", False), ("B5pc", "cp950", False), ("ETenms-B5", "cp950", False),
             ("B5", "cp950", False), ("HKscs-B5", "big5hkscs", False)],
    "japan1": [("90ms-RKSJ", "cp932", False), ("90pv-RKSJ", "cp932", False), ("83pv-RKSJ", "cp932", False),
               ("Add-RKSJ", "cp932", False), ("Ext-RKSJ", "cp932", False), ("RKSJ", "cp932", False),
               ("EUC", "euc_jp", False), ("H", "euc_jp", True)],
    "korea1": [("KSC-EUC", "euc_kr", False), ("KSCms-UHC", "cp949", False), ("KSCpc-EUC", "euc_kr", False),
               ("KSC", "euc_kr", True)],
}


def varint(n):
    assert n >= 0
    out = bytearray()
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def zigzag(n):
    return (n << 1) if n >= 0 else ((-n << 1) - 1)


def write_blob(name, data):
    packed = zlib.compress(data, 9)
    with open(os.path.join(OUT, name), "wb") as f:
        f.write(packed)
    print("%-24s %8d -> %7d bytes" % (name, len(data), len(packed)))
    return len(packed)


# --- CID -> Unicode -------------------------------------------------------------------------

def decode_legacy(cell, codec, seven_bit):
    """The single character a legacy-encoding cell ("21,aaa1") stands for, else None."""
    if cell == "*":
        return None
    for part in cell.split(","):
        part = part.rstrip("v")
        if len(part) % 2 or not part:
            continue
        raw = bytes.fromhex(part)
        if seven_bit and len(raw) == 2:
            raw = bytes(b | 0x80 for b in raw)
        try:
            text = raw.decode(codec)
        except UnicodeDecodeError:
            continue
        if len(text) != 1:
            continue
        cp = ord(text)
        if cp < 0x20 or 0x7F <= cp < 0xA0 or 0xE000 <= cp <= 0xF8FF or 0xD800 <= cp < 0xE000 or cp == 0xFFFD:
            continue
        return cp
    return None


def cid2uni(root, folder, uni, key):
    path = os.path.join(root, folder, "cid2code.txt")
    rows = [l.rstrip("\n").split("\t") for l in open(path, encoding="utf-8") if not l.startswith("#")]
    header, rows = rows[0], rows[1:]
    ucs2 = header.index(uni + "-UCS2")
    utf32 = header.index(uni + "-UTF32")

    def candidates(cell):
        if cell == "*":
            return []
        found = []
        for part in cell.split(","):
            vertical = part.endswith("v")
            if vertical:
                part = part[:-1]
            found.append((vertical, int(part, 16)))
        # horizontal candidates first, then vertical ones; order inside each kept
        return [cp for v, cp in found if not v] + [cp for v, cp in found if v]

    extra = [header.index(c) for c in UNICODE_EXTRA.get(key, [])]
    legacy = [(header.index(c), codec, seven) for c, codec, seven in LEGACY[key]]
    table = []
    recovered = 0
    for row in rows:
        cid = int(row[0])
        assert cid == len(table), (cid, len(table))
        cands = candidates(row[ucs2]) or candidates(row[utf32])
        for col in extra:
            cands = cands or candidates(row[col])
        cp = cands[0] if cands else 0
        if cp == 0 and cid != 0:
            for col, codec, seven in legacy:
                found = decode_legacy(row[col], codec, seven)
                if found:
                    cp = found
                    recovered += 1
                    break
        # a code point that is a control character or not a character is no mapping
        if cp < 0x20 or 0xD800 <= cp < 0xE000 or cp > 0x10FFFF:
            cp = 0
        table.append(cp)
    print("%-8s %6d CIDs, %6d with Unicode (%d of them from legacy codes)"
          % (key, len(table), sum(1 for c in table if c), recovered))
    out = bytearray(varint(len(table)))
    prev = 0
    for cp in table:
        if cp == 0:
            out += varint(0)
        else:
            out += varint(zigzag(cp - prev - 1) + 1)
            prev = cp
    return bytes(out), table


# --- CMaps ----------------------------------------------------------------------------------

HEX = r"<([0-9A-Fa-f]*)>"


def parse_cmap(text):
    """-> (usecmap name or None, codespaces [(n, lo, hi)], ranges [(n, lo, hi, cid)])"""
    use = None
    m = re.search(r"/([\w.-]+)\s+usecmap", text)
    if m:
        use = m.group(1)
    spaces, ranges = [], []
    for block in re.findall(r"begincodespacerange(.*?)endcodespacerange", text, re.S):
        for lo, hi in re.findall(HEX + r"\s*" + HEX, block):
            spaces.append((len(lo) // 2, int(lo, 16), int(hi, 16)))
    for block in re.findall(r"begincidrange(.*?)endcidrange", text, re.S):
        for lo, hi, cid in re.findall(HEX + r"\s*" + HEX + r"\s*(\d+)", block):
            ranges.append((len(lo) // 2, int(lo, 16), int(hi, 16), int(cid)))
    for block in re.findall(r"begincidchar(.*?)endcidchar", text, re.S):
        for code, cid in re.findall(HEX + r"\s*(\d+)", block):
            ranges.append((len(code) // 2, int(code, 16), int(code, 16), int(cid)))
    ranges.sort()
    return use, spaces, ranges


def encode_cmap(name, use, spaces, ranges):
    out = bytearray()
    nb = name.encode("ascii")
    out += varint(len(nb)) + nb
    out.append(1 if use else 0)
    if use:
        ub = use.encode("ascii")
        out += varint(len(ub)) + ub
    out += varint(len(spaces))
    for n, lo, hi in spaces:
        out.append(n)
        out += varint(lo) + varint(hi)
    for n in (1, 2, 3, 4):
        group = [r for r in ranges if r[0] == n]
        out += varint(len(group))
        prev_hi, prev_cid_end = 0, 0
        for _, lo, hi, cid in group:
            assert lo >= prev_hi, (name, lo, prev_hi)
            out += varint(lo - prev_hi) + varint(hi - lo) + varint(zigzag(cid - prev_cid_end - 1))
            prev_hi = hi
            prev_cid_end = cid + (hi - lo)
    return bytes(out)


def cmaps(root, folder, names):
    blob = bytearray(varint(len(names)))
    for name in names:
        text = open(os.path.join(root, folder, "CMap", name), encoding="latin-1").read()
        use, spaces, ranges = parse_cmap(text)
        blob += encode_cmap(name, use, spaces, ranges)
    return bytes(blob)


# --- Adobe Glyph List -----------------------------------------------------------------------

def read_list(path):
    entries = {}
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, value = line.split(";")
        entries[name] = [int(v, 16) for v in value.split()]
    return entries


def agl(agl_root):
    names = read_list(os.path.join(agl_root, "glyphlist.txt"))
    zapf = read_list(os.path.join(agl_root, "zapfdingbats.txt"))
    for k, v in zapf.items():
        names.setdefault(k, v)
    text = "".join("%s;%s\n" % (k, " ".join("%04X" % c for c in names[k])) for k in sorted(names))
    return text.encode("ascii"), names, zapf


# --- CJK characters that only look like a unified ideograph -----------------------------------

def cjk_norm(radicals_path):
    """Kangxi radicals (U+2F00..2FD5) and CJK radicals supplement (U+2E80..2EF3) as the unified
    ideograph of the same radical (CJKRadicals.txt), and the compatibility ideographs
    (U+F900..FAD9, U+2F800..2FA1D) as the character NFKC turns them into. PDF producers often
    map the glyphs of a font to these through ToUnicode (a Wikisource export writes every
    common character that is also a radical this way), so the text would not match a search
    for the ordinary character."""
    import unicodedata
    table = {}
    for line in open(radicals_path, encoding="utf-8"):
        if line.startswith("#") or not line.strip():
            continue
        _, radical, ideograph = [f.strip() for f in line.split(";")]
        if radical:
            table[int(radical, 16)] = int(ideograph, 16)
    for lo, hi in ((0x2E80, 0x2FD5), (0xF900, 0xFAD9), (0x2F800, 0x2FA1D)):
        for cp in range(lo, hi + 1):
            n = unicodedata.normalize("NFKC", chr(cp))
            if len(n) == 1 and n != chr(cp):
                table.setdefault(cp, ord(n))
    # Radicals that stand for the ideograph their name says, in neither source; seen in
    # Wikisource PDF exports (a font maps the glyph of the ideograph to the radical).
    for cp, target in ((0x2EA0, 0x6C11), (0x2ED1, 0x9577), (0x2ED8, 0x9752), (0x2EDD, 0x98DF)):
        table.setdefault(cp, target)
    pairs = sorted((cp, t) for cp, t in table.items() if cp != t)
    out = bytearray(varint(len(pairs)))
    prev = 0
    for cp, target in pairs:
        out += varint(cp - prev) + varint(target)
        prev = cp
    return bytes(out)


# --- simple font encodings ------------------------------------------------------------------

STANDARD_UPPER = {
    0xA1: 0x00A1, 0xA2: 0x00A2, 0xA3: 0x00A3, 0xA4: 0x2044, 0xA5: 0x00A5, 0xA6: 0x0192,
    0xA7: 0x00A7, 0xA8: 0x00A4, 0xA9: 0x0027, 0xAA: 0x201C, 0xAB: 0x00AB, 0xAC: 0x2039,
    0xAD: 0x203A, 0xAE: 0xFB01, 0xAF: 0xFB02, 0xB1: 0x2013, 0xB2: 0x2020, 0xB3: 0x2021,
    0xB4: 0x00B7, 0xB6: 0x00B6, 0xB7: 0x2022, 0xB8: 0x201A, 0xB9: 0x201E, 0xBA: 0x201D,
    0xBB: 0x00BB, 0xBC: 0x2026, 0xBD: 0x2030, 0xBF: 0x00BF, 0xC1: 0x0060, 0xC2: 0x00B4,
    0xC3: 0x02C6, 0xC4: 0x02DC, 0xC5: 0x00AF, 0xC6: 0x02D8, 0xC7: 0x02D9, 0xC8: 0x00A8,
    0xCA: 0x02DA, 0xCB: 0x00B8, 0xCD: 0x02DD, 0xCE: 0x02DB, 0xCF: 0x02C7, 0xD0: 0x2014,
    0xE1: 0x00C6, 0xE3: 0x00AA, 0xE8: 0x0141, 0xE9: 0x00D8, 0xEA: 0x0152, 0xEB: 0x00BA,
    0xF1: 0x00E6, 0xF5: 0x0131, 0xF8: 0x0142, 0xF9: 0x00F8, 0xFA: 0x0153, 0xFB: 0x00DF,
}


def standard():
    t = [0] * 256
    for c in range(0x20, 0x7F):
        t[c] = c
    t[0x27] = 0x2019
    t[0x60] = 0x2018
    for c, u in STANDARD_UPPER.items():
        t[c] = u
    return t


def win_ansi():
    t = [0] * 256
    for c in range(0x20, 0x100):
        try:
            t[c] = ord(bytes([c]).decode("cp1252"))
        except UnicodeDecodeError:
            t[c] = 0x2022  # D.2: unused codes above 040 octal are the bullet
    t[0x7F] = 0x2022
    t[0xA0] = 0x20
    t[0xAD] = 0x2D
    return t


def mac_roman():
    t = [0] * 256
    for c in range(0x20, 0x7F):
        t[c] = c
    for c in range(0x80, 0x100):
        t[c] = ord(bytes([c]).decode("mac_roman"))
    t[0xCA] = 0x20      # no-break space shown as a space
    t[0xDB] = 0x00A4    # D.2: currency (the later Mac OS has the euro here)
    t[0xF0] = 0         # the Apple logo
    return t


def named_encoding(path, names):
    t = [0] * 256
    for line in open(path, encoding="utf-8"):
        code, name = line.split()
        cps = names.get(name)
        if cps and cps[0] <= 0xFFFF:
            t[int(code, 8)] = cps[0]
    return t


def rust_array(name, table):
    assert all(0 <= v <= 0xFFFF for v in table)
    lines = ["pub static %s: [u16; 256] = [" % name]
    for i in range(0, 256, 16):
        lines.append("    " + ", ".join("0x%04X" % v for v in table[i:i + 16]) + ",")
    lines.append("];")
    return "\n".join(lines)


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    cmap_root, agl_root, radicals = sys.argv[1], sys.argv[2], sys.argv[3]
    os.makedirs(OUT, exist_ok=True)
    total = 0
    for key, (folder, uni, names) in COLLECTIONS.items():
        data, _ = cid2uni(cmap_root, folder, uni, key)
        total += write_blob("cid2uni-%s.bin" % key, data)
        total += write_blob("cmaps-%s.bin" % key, cmaps(cmap_root, folder, names))
    text, names, zapf = agl(agl_root)
    total += write_blob("agl.bin", text)
    total += write_blob("cjknorm.bin", cjk_norm(radicals))
    print("total %d bytes" % total)

    here = os.path.join(HERE, "src")
    sym = named_encoding(os.path.join(here, "symbol-encoding.txt"), names)
    zap = named_encoding(os.path.join(here, "zapfdingbats-encoding.txt"), zapf)
    src = ["// Generated by data/gen_text_data.py; do not edit.",
           "// Code -> Unicode of the simple font encodings of ISO 32000-1 Annex D (0 = no character).",
           "", rust_array("STANDARD", standard()), "", rust_array("WIN_ANSI", win_ansi()), "",
           rust_array("MAC_ROMAN", mac_roman()), "", rust_array("SYMBOL", sym), "",
           rust_array("ZAPF_DINGBATS", zap), ""]
    os.makedirs(os.path.join(HERE, "..", "src", "text"), exist_ok=True)
    with open(os.path.join(HERE, "..", "src", "text", "encodings.rs"), "w", newline="\n") as f:
        f.write("\n".join(src))


if __name__ == "__main__":
    main()

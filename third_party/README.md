# Third-party data

The text extraction (layer 2) embeds tables derived from two Adobe open-source
data sets, both under the BSD 3-Clause licence, and one Unicode data file (Unicode
License v3). The licence texts are in this folder; the copyright notices and
conditions must stay with any redistribution.

| Data set | Commit | What is derived from it | Where |
|---|---|---|---|
| [cmap-resources](https://github.com/adobe-type-tools/cmap-resources) | `f5cf3bca7fdfeaceb77aa82847e974f2306c20b4` | `cid2code.txt` of Adobe-GB1-6, Adobe-CNS1-7, Adobe-Japan1-7 and Adobe-Korea1-2 (CID to Unicode), and 22 of the predefined legacy CMaps (code to CID) | `crates/qingpdf-core/data/text/cid2uni-*.bin`, `cmaps-*.bin` |
| [agl-aglfn](https://github.com/adobe-type-tools/agl-aglfn) | `4036a9ca80a62f64f9de4f7321a9a045ad0ecfd6` | `glyphlist.txt` and `zapfdingbats.txt` (glyph name to Unicode) | `crates/qingpdf-core/data/text/agl.bin`, `crates/qingpdf-core/src/text/encodings.rs` |
| [CJKRadicals.txt](https://www.unicode.org/Public/UCD/latest/ucd/CJKRadicals.txt) (UCD 18.0.0) | - | which unified ideograph each Kangxi radical and CJK radical (U+2E80 to U+2FD5) stands for (the compatibility ideographs come from Unicode normalization form NFKC) | `crates/qingpdf-core/data/text/cjknorm.bin` |

`crates/qingpdf-core/data/gen_text_data.py` regenerates all of it from checkouts
of those two repositories at those commits, and the Unicode file. The code-to-name tables of the
Symbol and ZapfDingbats fonts (`data/src/*.txt`) are the tables of ISO 32000-1
Annex D.5 and D.6.

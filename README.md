# qingpdf

A small, fast, offline PDF tool with its own engine, written in Rust. *Qīng* (轻) means "light".

Most PDF tools today are hundreds of megabytes. qingpdf is one 2.2 MB executable that needs nothing installed, never goes online, and is built to do Chinese PDFs right.

**Status: early (layers 1, 1.5 and 2 done; layer 3 in progress).** What works today is reading, repairing and rewriting PDFs, the page operations below, text extraction (Chinese, Japanese and Korean, vertical text included), and drawing pages to PNG with `render`, and putting and changing text on pages. There is no conversion yet; see [the plan](PLAN.md). Password-protected PDFs open and the commands keep their protection (RC4 40/128-bit, AES-128, AES-256); only `decrypt` takes it off. Certificate (public-key) encryption is not supported.

## What it does now

```
qingpdf info a.pdf                                  version, pages, page sizes, document info, structure
qingpdf merge a.pdf b.pdf -o out.pdf                join files in order
qingpdf split a.pdf --pages 1-3,5 -o out.pdf        take pages out
qingpdf split a.pdf --every 10 -o part_%d.pdf       cut into files of 10 pages
qingpdf delete a.pdf --pages 2,4-6 -o out.pdf       remove pages
qingpdf rotate a.pdf --pages 1-3 --angle 90 -o out.pdf
qingpdf img2pdf 1.jpg 2.png -o out.pdf              one image per page (JPEG kept as-is, EXIF orientation honoured)
qingpdf decrypt a.pdf -o plain.pdf --password P     write an unencrypted copy (owner password, or a file that allows everything)
qingpdf text a.pdf -o a.txt                         text in reading order (scans give nothing: no OCR)
qingpdf render a.pdf --pages 1-3 --dpi 150 -o page_%d.png  draw pages to PNG
qingpdf watermark a.pdf --text "机密" --opacity 0.3 --angle 45 --tile -o out.pdf   text or picture watermark (--under the content, --pages 1-3)
qingpdf stamp a.pdf --image sig.png --page 3 --at 72,100 --width 120 -o out.pdf  put a picture on a page (a PNG's transparency is kept)
qingpdf addtext a.pdf --page 1 --at 72,720 --size 12 --text "你好" -o out.pdf     put text on a page (Chinese too)
qingpdf replace a.pdf --find "旧词" --replace "新词" --pages 1-3 -o out.pdf        change text that is already on the pages
```

`watermark`, `stamp` and `addtext` write into the pages' own content, so every other program and a printer show it too; the text is embedded as a font subset, so Chinese is shown everywhere. The output is the input with the change appended as an incremental update (the input's bytes stay exactly as they are; the input itself is never touched). Places are points from the lower left corner of the page as it is shown, and a watermark stays upright and centred on a turned page. A signed or certified file is refused, and so is an encrypted file whose author did not allow changing it (give the owner password).

`replace` changes text that is already on the pages, one line at a time (a line is what `text` and the viewer's search and selection call one). The old letters are taken out of the page's drawing instructions (they are not covered over, so copying and searching give the new text only) and the new ones are drawn where they were, in the same size, colour and spacing, with the font of the old text when it has every letter and with a matching system font subset (Song, Hei, Kai, Fang for Chinese) when it has not. Nothing is moved to make room, and a warning says when the new text is wider. The old text stays in the earlier bytes of the output (write the result again with `split --pages 1-` to drop it). Text in a form that a page draws more than once is not changed. In the viewer, *Insert > Edit text on the page* clicks a line and types it again (Enter confirms, Ctrl+Z undoes).

Every command that reads a PDF takes `--password <password>` for encrypted files (a password that is given is tried first, then the empty one); the results are encrypted the way the input was, with the same passwords and permissions. A file that restricts what may be done with it (it was not opened with its owner password and its author did not allow everything) can only be the first input of a merge, whose output then carries its protection, unless its owner password is given. `info` shows how a file is protected and what its author allows.

Outputs are never written over an input, and an existing file is only replaced with `--force`. Run `qingpdf <command> --help` for details.

`render` draws paths, colours, clipping, images (JPEG, JPEG 2000, CCITT fax, JBIG2, Flate and more), forms and text. Embedded TrueType, OpenType, CFF and Type 1 fonts are read by qingpdf's own code; fonts that are not embedded (common in Chinese government documents) are drawn with the system's fonts, such as SimSun, SimHei, KaiTi and FangSong on Windows. Transparency, gradients, patterns and hidden layers are drawn, and so are annotations (notes, stamps, highlights, filled form fields) from their appearance streams; `--no-annots` leaves them out. An image that cannot be decoded shows as a grey block. On this PC at 150 dpi drawing a text page takes 15–39 ms, a JPEG scan 38 ms and a CCITT fax A4 scan 23 ms.

## Hard rules

Every release is measured against these; a feature that breaks one is not added.

1. Windows download ≤ 20 MB (today: 2.2 MB).
2. First page of a 100-page PDF on screen in ≤ 1 s (today `info` on 1000 pages: ~50 ms).
3. Page turns ≤ 100 ms.
4. ≤ 200 MB memory for a 100-page document.
5. Offline, nothing to install, no telemetry.
6. Damaged or hostile files never crash or hang it: they open, or fail with a clear message.

## How it is checked

- The engine is written against ISO 32000-1 (PDF 1.7); code comments cite spec sections. Choices the spec leaves open are recorded in [docs/decisions.md](docs/decisions.md) (Chinese).
- Test corpus in [tests/corpus](tests/corpus): files from pdf.js, PDFium, the PDF Association and public Chinese sources (government documents, papers, books, vertical text, scans), with sources and licences listed per file.
- Every output in the test suite is checked with [qpdf](https://github.com/qpdf/qpdf) `--check`; rewritten files are rendered with PDFium (Chrome's PDF engine) and compared with the originals pixel by pixel ([tests/tools/render_compare.py](tests/tools/render_compare.py)). Render output is compared page by page with PDFium at the same dpi (`render_compare.py --engine-compare`).
- Thousands of randomly damaged files and a set of deliberately hostile ones (decompression bombs, reference cycles, huge cross-reference tables) must open or fail cleanly within time and memory limits.
- Each layer ends with an independent review against the spec.

## Build

Rust 1.99 (pinned in `rust-toolchain.toml`). On Windows the C runtime is linked statically, so the executable runs on a clean system.

```bash
cargo build --release
cargo test --release
```

The executable is `target/release/qingpdf.exe`. The qpdf checks in the tests look for qpdf in `QINGPDF_QPDF`, then `C:\Dev\tools\qpdf\bin\qpdf.exe`, then `PATH`; without it those checks are skipped with a note.

## Licence

MIT OR Apache-2.0, at your option. Test files keep their own licences; see [tests/corpus/SOURCES.md](tests/corpus/SOURCES.md), [tests/corpus/SOURCES-zh.md](tests/corpus/SOURCES-zh.md) and [tests/corpus/public/LICENSES](tests/corpus/public/LICENSES).

Third-party software and data, with their licence texts, are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

---

## 中文简介

qingpdf 是一个小、快、离线的 PDF 工具，引擎用 Rust 从头写，目标是把中文 PDF 做到最好。现在已经能查看信息、合并、拆分、删页、旋转、图片转 PDF，能修复常见的损坏文件；加密文件（密码、RC4、AES）也能打开，输出保留原来的加密，`decrypt` 命令可以去掉加密；还能提取文字，支持中文、日文、韩文和竖排。`render` 可以把页面画成 PNG，嵌入字体由自己的代码读取，没嵌字体的中文公文用系统自带的宋体、黑体、楷体、仿宋来画；透明、渐变还没做。`watermark`、`stamp`、`addtext` 三个命令可以给页面加水印（文字或图片，平铺、旋转、半透明）、图片和中文文字，写进页面自己的内容，用增量方式追加，原文件的字节不动。`replace` 命令可以改页面上已有的文字（旧字真的从内容里去掉，新字用原字体，缺字时用相近的系统字体子集）。阅读界面（含批注、填表和"插入"菜单，菜单里有"改页面上的文字"）在 Windows 版里，转换还没做。整体计划见 [PLAN.md](PLAN.md)，设计上的取舍见 [docs/decisions.md](docs/decisions.md)。

## 许可证

第三方软件和数据的许可证见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

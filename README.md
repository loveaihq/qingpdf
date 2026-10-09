# qingpdf

A small, fast, offline PDF tool with its own engine, written in Rust. *Qīng* (轻) means "light".

Most PDF tools today are hundreds of megabytes. qingpdf is one 2.2 MB executable that needs nothing installed, never goes online, and is built to do Chinese PDFs right.

**Status: early (layers 1, 1.5 and 2 done; layer 3 in progress).** What works today is reading, repairing and rewriting PDFs, the page operations below, text extraction (Chinese, Japanese and Korean, vertical text included), and drawing pages to PNG with `render`. There is no viewer, conversion or editing yet; see [the plan](PLAN.md). Password-protected PDFs open and the commands keep their protection (RC4 40/128-bit, AES-128, AES-256); only `decrypt` takes it off. Certificate (public-key) encryption is not supported.

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
```

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

qingpdf 是一个小、快、离线的 PDF 工具，引擎用 Rust 从头写，目标是把中文 PDF 做到最好。现在已经能查看信息、合并、拆分、删页、旋转、图片转 PDF，能修复常见的损坏文件；加密文件（密码、RC4、AES）也能打开，输出保留原来的加密，`decrypt` 命令可以去掉加密；还能提取文字，支持中文、日文、韩文和竖排。`render` 可以把页面画成 PNG，嵌入字体由自己的代码读取，没嵌字体的中文公文用系统自带的宋体、黑体、楷体、仿宋来画；透明、渐变还没做。阅读界面、转换和编辑还没做。整体计划见 [PLAN.md](PLAN.md)，设计上的取舍见 [docs/decisions.md](docs/decisions.md)。

## 许可证

第三方软件和数据的许可证见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

# qingpdf

A small, fast, offline PDF tool with its own engine, written in Rust. *Qīng* (轻) means "light".

Most PDF tools today are hundreds of megabytes. qingpdf is one 0.8 MB executable that needs nothing installed, never goes online, and is built to do Chinese PDFs right.

**Status: early (layer 1 of 5).** What works today is the file-structure layer: reading, repairing and rewriting PDFs, and the page operations below. There is no viewer, text extraction, conversion or editing yet; see [the plan](PLAN.md). Encrypted PDFs are not supported yet (next on the list).

## What it does now

```
qingpdf info a.pdf                                  version, pages, page sizes, document info, structure
qingpdf merge a.pdf b.pdf -o out.pdf                join files in order
qingpdf split a.pdf --pages 1-3,5 -o out.pdf        take pages out
qingpdf split a.pdf --every 10 -o part_%d.pdf       cut into files of 10 pages
qingpdf delete a.pdf --pages 2,4-6 -o out.pdf       remove pages
qingpdf rotate a.pdf --pages 1-3 --angle 90 -o out.pdf
qingpdf img2pdf 1.jpg 2.png -o out.pdf              one image per page (JPEG kept as-is, EXIF orientation honoured)
```

Outputs are never written over an input, and an existing file is only replaced with `--force`. Run `qingpdf <command> --help` for details.

## Hard rules

Every release is measured against these; a feature that breaks one is not added.

1. Windows download ≤ 20 MB (today: 0.8 MB).
2. First page of a 100-page PDF on screen in ≤ 1 s (today `info` on 1000 pages: ~50 ms).
3. Page turns ≤ 100 ms.
4. ≤ 200 MB memory for a 100-page document.
5. Offline, nothing to install, no telemetry.
6. Damaged or hostile files never crash or hang it: they open, or fail with a clear message.

## How it is checked

- The engine is written against ISO 32000-1 (PDF 1.7); code comments cite spec sections. Choices the spec leaves open are recorded in [docs/decisions.md](docs/decisions.md) (Chinese).
- Test corpus in [tests/corpus](tests/corpus): files from pdf.js, PDFium, the PDF Association and public Chinese sources (government documents, papers, books, vertical text, scans), with sources and licences listed per file.
- Every output in the test suite is checked with [qpdf](https://github.com/qpdf/qpdf) `--check`; rewritten files are rendered with PDFium (Chrome's PDF engine) and compared with the originals pixel by pixel ([tests/tools/render_compare.py](tests/tools/render_compare.py)).
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

---

## 中文简介

qingpdf 是一个小、快、离线的 PDF 工具，引擎用 Rust 从头写，目标是把中文 PDF 做到最好。现在完成了五层里的第 1 层（文件结构）：能查看信息、合并、拆分、删页、旋转、图片转 PDF，能修复常见的损坏文件。阅读界面、提取文字、转换和编辑在后面几层，加密文件马上就做。整体计划见 [PLAN.md](PLAN.md)，设计上的取舍见 [docs/decisions.md](docs/decisions.md)。

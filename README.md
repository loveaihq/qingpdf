# qingpdf

A small, fast, offline PDF tool with its own engine, written in Rust. *Qīng* (轻) means "light".

User guide: [English](docs/USER-GUIDE.md) · [中文](docs/使用说明.md)

Most PDF tools today are hundreds of megabytes. qingpdf is two small executables (the 3.9 MB reader and the 3.4 MB command-line tool) that need nothing installed, never go online, and are built to do Chinese PDFs right.

**Status: early (layers 1, 1.5 and 2 done; layer 3 in progress).** What works today is reading, repairing and rewriting PDFs, the page operations below, text extraction (Chinese, Japanese and Korean, vertical text included), and drawing pages to PNG with `render`, and putting and changing text on pages. There is no conversion yet; see [the plan](PLAN.md). Password-protected PDFs open and the commands keep their protection (RC4 40/128-bit, AES-128, AES-256); only `decrypt` takes it off, and `encrypt` sets or changes it (AES-256 or AES-128; RC4 is read but never written). Certificate (public-key) encryption is not supported.

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
qingpdf encrypt a.pdf -o out.pdf --user-password U --owner-password O [--aes128] [--print none|low|high] [--no-copy] [--no-modify] [--no-annotate] [--no-forms] [--no-assemble] [--no-accessibility] [--password-file F]   write a copy with passwords and permissions (AES-256, or AES-128 with --aes128; the whole file is written again; signed files are refused)
qingpdf compress a.pdf -o out.pdf [--preset high|balanced|small] [--lossless] [--password P]   write a smaller copy (what repeats is written once, the file is packed into object streams, Flate data is deflated again, pictures drawn at more dots per inch than the preset keeps are averaged down and coded as JPEG; the output is never bigger than the input; signed files are refused; a file without the right to change its content can only be compressed with --lossless)
qingpdf text a.pdf -o a.txt                         text in reading order (a scan gives nothing here; recognise it in the viewer: Tools > Recognize text)
qingpdf render a.pdf --pages 1-3 --dpi 150 -o page_%d.png  draw pages to PNG
qingpdf watermark a.pdf --text "机密" --opacity 0.3 --angle 45 --tile -o out.pdf   text or picture watermark (--under the content, --pages 1-3)
qingpdf pagenum a.pdf --footer-center "第 {page} 页 共 {pages} 页" --mirror -o out.pdf   page numbers, headers, footers (six places; {page} {pages} {date} {file}; --pages 3- --start 1 --skip-first)
qingpdf stamp a.pdf --image sig.png --page 3 --at 72,100 --width 120 -o out.pdf  put a picture on a page (a PNG's transparency is kept)
qingpdf addtext a.pdf --page 1 --at 72,720 --size 12 --text "你好" -o out.pdf     put text on a page (Chinese too)
qingpdf replace a.pdf --find "旧词" --replace "新词" --pages 1-3 -o out.pdf        change text that is already on the pages
qingpdf docx a.pdf --pages 1-10 -o a.docx            make a Word file: paragraphs, headings, lists, tables, pictures
qingpdf xlsx a.pdf --pages 1-10 -o a.xlsx            make an Excel file: a sheet for each table (with lines or without)
```

`watermark`, `pagenum`, `stamp` and `addtext` write into the pages' own content, so every other program and a printer show it too; the text is embedded as a font subset, so Chinese is shown everywhere. The output is the input with the change appended as an incremental update (the input's bytes stay exactly as they are; the input itself is never touched). Places are points from the lower left corner of the page as it is shown, and a watermark stays upright and centred on a turned page. A signed or certified file is refused, and so is an encrypted file whose author did not allow changing it (give the owner password).

`replace` changes text that is already on the pages, one line at a time (a line is what `text` and the viewer's search and selection call one). The old letters are taken out of the page's drawing instructions (they are not covered over, so copying and searching give the new text only) and the new ones are drawn where they were, in the same size, colour and spacing, with the font of the old text when it has every letter and with a matching system font subset (Song, Hei, Kai, Fang for Chinese) when it has not. Nothing is moved to make room, and a warning says when the new text is wider. The old text stays in the earlier bytes of the output (write the result again with `split --pages 1-` to drop it). Text in a form that a page draws more than once is not changed. In the viewer, *Insert > Edit text on the page* clicks a line and types it again (Enter confirms, Ctrl+Z undoes).

`docx` makes a Word file (.docx) you can edit. Lines are joined into paragraphs (words split by a hyphen at the end of a line are joined, Chinese lines are joined without spaces), big or bold lines become Heading 1 to 4, bullets and numbers become Word lists, two-column pages are read one column after the other, tables drawn with lines become Word tables (merged cells too) with the text in the right cells, and so do tables with no lines round them (a bank statement, a financial report) where the text lines up in rows and columns, pictures are put in where they are (JPEG as they are, the others as PNG), and running page headers, footers and page numbers go to Word's header and footer. Chinese text keeps its kind of font (Song, Hei, Kai, Fang) and its full-width punctuation; a page of vertical text becomes a vertical section. A page that is only a picture (a scan) becomes a picture. It is a Word file to work on, not a picture of the page: nothing is positioned to match the page exactly, and drawings and charts are left out. In the viewer: *File > Export to Word*.

`xlsx` makes an Excel file (.xlsx) of the tables in the pages, one sheet for each table, named after its page ("第3页-表1"). Tables drawn with lines are found from the lines (merged cells are kept); tables with no lines round them are found where the text lines up in columns, as in a bank statement or a financial report (a cell folded in two lines is one cell, a heading across columns is a merged cell). Amounts, decimals, thousands separators (`1,234.50`), negative numbers (`-12`; `(12)` and `12-` when the column is made of amounts), percents and amounts with a currency sign are stored as numbers in the format they were written in, and dates (`2024-05-01`, `2024年5月1日`) as dates; identity and phone numbers, codes that begin with a zero and numbers of more than 15 digits stay text, so Excel cannot spoil them. A table that goes on over several pages (a statement) is one sheet. A page with no table gives no sheet; a file with no table at all gets one sheet with its text, a line to a row. In the viewer: *File > Export to Excel* (Esc stops it).

The viewer recognises the text of scanned pages with the text recogniser that comes with Windows (`Windows.Media.Ocr`; nothing is downloaded, nothing leaves the computer): *Tools > Recognize text in all scanned pages* (pages with no text that are mostly a picture; pages that have text are skipped) or *on this page*. The pages are drawn at about 300 dpi, one at a time, and read in the background; the status bar says how far it is and Esc stops it (what was read is kept). The words are written into the page as an invisible text layer over the picture (text rendering mode 3, one embedded font subset for all the pages, every word stretched to its box), so that this viewer's find, selection and copy, and other readers' and *Export to Word*, work on the scan. It is one step for *Undo*; saving appends it to the file (the original bytes are not touched). A file that is signed or that may not be changed gets the text for the open window only, and it is not saved. Chinese needs the Chinese recogniser (*Settings > Time & language > Language & region > add Chinese*; the viewer says so and lists what is installed, it never changes system settings). Windows reads horizontal text; vertical text (old books) comes out as rows across the columns. A page takes about half a second to a second and a half.

The viewer has a toolbar under the menu (open, print, previous and next page, a page box that takes a page number, zoom, fit width, fit page, a search box where Enter finds the next place, and the highlight, note and draw tools; *View > Toolbar* hides it; the pictures are drawn from the icon font of Windows, and the buttons say their names on a computer that has none). A right click on the page gives copy, highlight the selection, add a note here and select all on the page; a double click selects a word (a whole run of Chinese up to the punctuation), a triple click a line. *View > Language* is the language of Windows, Chinese or English, and takes effect at once. The bookmarks beside the pages are made wider or narrower by dragging their edge. The size, place and maximized state of the window, the toolbar, the language and the width of the bookmarks are kept for the next run in `%APPDATA%\qingpdf\author.txt` (the first line of it is still the name put on the annotations).

Every command that reads a PDF takes `--password <password>` for encrypted files (a password that is given is tried first, then the empty one); the results are encrypted the way the input was, with the same passwords and permissions. A file that restricts what may be done with it (it was not opened with its owner password and its author did not allow everything) can only be the first input of a merge, whose output then carries its protection, unless its owner password is given. `info` shows how a file is protected and what its author allows.

Outputs are never written over an input, and an existing file is only replaced with `--force`. Run `qingpdf <command> --help` for details.

`render` draws paths, colours, clipping, images (JPEG, JPEG 2000, CCITT fax, JBIG2, Flate and more), forms and text. Embedded TrueType, OpenType, CFF and Type 1 fonts are read by qingpdf's own code; fonts that are not embedded (common in Chinese government documents) are drawn with the system's fonts, such as SimSun, SimHei, KaiTi and FangSong on Windows. Transparency, gradients, patterns and hidden layers are drawn, and so are annotations (notes, stamps, highlights, filled form fields) from their appearance streams; `--no-annots` leaves them out. An image that cannot be decoded shows as a grey block. On this PC at 150 dpi drawing a text page takes 15–39 ms, a JPEG scan 38 ms and a CCITT fax A4 scan 23 ms.

## Hard rules

Every release is measured against these; a feature that breaks one is not added.

1. Windows download ≤ 20 MB (today: a 3.9 MB zip with both programs).
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

qingpdf 是一个小、快、离线的 PDF 工具，引擎用 Rust 从头写，目标是把中文 PDF 做到最好。现在已经能查看信息、合并、拆分、删页、旋转、图片转 PDF，能修复常见的损坏文件；加密文件（密码、RC4、AES）也能打开，输出保留原来的加密，`decrypt` 命令可以去掉加密，`encrypt` 命令可以加密码、关权限（AES-256，或兼容旧软件的 AES-128；整个文件重写，有签名的文件拒绝），阅读器的“文件 → 密码和权限…”也能做；还能提取文字，支持中文、日文、韩文和竖排。`render` 可以把页面画成 PNG，嵌入字体由自己的代码读取，没嵌字体的中文公文用系统自带的宋体、黑体、楷体、仿宋来画；透明、渐变还没做。`watermark`、`stamp`、`addtext` 三个命令可以给页面加水印（文字或图片，平铺、旋转、半透明）、图片和中文文字，写进页面自己的内容，用增量方式追加，原文件的字节不动。`replace` 命令可以改页面上已有的文字（旧字真的从内容里去掉，新字用原字体，缺字时用相近的系统字体子集）。`docx` 命令把 PDF 转成可以编辑的 Word 文件（.docx）：行合成段落，大字和粗体变标题，项目符号和编号变 Word 列表，两栏按阅读顺序，有框线的表格变 Word 表格，图片按位置放进去，页眉页脚和页码放进 Word 的页眉页脚，中文保留宋体、黑体、楷体、仿宋，竖排页变竖排分节；阅读器的"文件"菜单里有"导出为 Word"。没有框线的表格（银行流水、财务报表）按文字的对齐找列，也会变成 Word 表格。`xlsx` 命令把 PDF 里的表格转成 Excel 文件（.xlsx），一个表格一个工作表（"第3页-表1"）：有框线的表格按线拼，没框线的按文字的对齐找列，折成两行的单元格合成一格，跨列的标题合并单元格；整数、小数、千分位、负数（`-12`、`(12)`）、百分比、带货币符号的金额存成数字并保留原来的格式，日期（`2024-05-01`、`2024年5月1日`）存成日期，身份证号、手机号、以 0 开头的编号、超过 15 位的数字保持文本；整个文件没有表格时把各页文字按行放进一个工作表并提示；阅读器的"文件"菜单里有"导出为 Excel"（能取消）。阅读器的"工具"菜单可以识别扫描页的文字：用 Windows 自带的文字识别（不联网、不下载），整页按约 300 dpi 一页一页在后台识别，识别出的字写成盖在图上的看不见的文字层（一个共用的字体子集，每个词拉伸到识别框的宽度），之后这个阅读器的查找、选字、复制，别的软件和"导出为 Word"都能用；撤销一步去掉，保存是增量追加；有签名或不许修改的文件只在本窗口里用、不保存。中文要装 Windows 的中文识别（设置 → 时间和语言 → 语言和区域），没装会提示并列出现有的语言。只认横排，竖排的古籍识别出来是跨栏的横行。阅读界面（含批注、填表和"插入"菜单，菜单里有"改页面上的文字"，"文件"菜单里有"导出为 Word"）在 Windows 版里。菜单下面有工具栏（打开、打印、上一页和下一页、可以输入页码的页码框、缩放、适合宽度和整页、回车找下一处的搜索框、高亮和便签和手写工具，"视图 → 工具栏"可以隐藏；图标是运行时从 Windows 的图标字体画的，没有这种字体就让按钮写字）；页面上右键有复制、高亮所选文字、在此添加便签、选择本页全部文字；双击选词（中文是到标点为止的一整段），三击选行；"视图 → 语言"可选跟随系统、中文、English，马上生效；书签栏的边可以拖着改宽窄；窗口的大小、位置、是否最大化，工具栏、语言和书签栏的宽度记在 `%APPDATA%\qingpdf\author.txt` 里（第一行仍是批注人的名字）。整体计划见 [PLAN.md](PLAN.md)，设计上的取舍见 [docs/decisions.md](docs/decisions.md)。

## 许可证

第三方软件和数据的许可证见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

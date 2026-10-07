# 测试语料来源与许可证

生成日期 2026-10-06。三个来源都已核对许可证，每个文件的来源 URL 都固定到提交号。

- `public/`：可以进公开 MIT/Apache 仓库。**只放**项目自己写的文件（PDFium 用 `.in` 模板生成的、PDFium/pdf.js 提交者手写的合成文件）和有明确开源许可的文件（PDF Association 示例，CC BY-SA 4.0）。许可证文本和署名放在 `public/LICENSES/`。
- `local/`：已被 `.gitignore` 忽略，只在本机。来源或许可不明的都在这里；拿不准的一律放这里。
- 文件原样复制，一个字节没改（复制后逐个核对了 SHA-256）。`tests/corpus/.gitattributes` 把 `*.pdf` 设成 binary，防止 Windows 上 git 换行转换破坏 xref 偏移。

## 概览

| 分类 | 含义 | public 个数 | public 大小 | local 个数 | local 大小 |
|---|---|---:|---:|---:|---:|
| `xref-classic` | 经典 xref 表 | 6 | 0.05 MB | 2 | 1.61 MB |
| `xref-stream-objstm` | xref 流 / 对象流 | 1 | 0.00 MB | 5 | 0.11 MB |
| `hybrid` | 混合 xref（/XRefStm） | 1 | 0.00 MB | 5 | 0.20 MB |
| `incremental` | 增量更新（多个 %%EOF） | 3 | 0.01 MB | 4 | 1.22 MB |
| `linearized` | 线性化 | 0 | 0.00 MB | 5 | 0.05 MB |
| `damaged` | 损坏/畸形 | 18 | 0.03 MB | 5 | 0.61 MB |
| `encrypted` | 加密（检测、解密；另有 `qpdf-generated/` 31 个自己生成的，见下） | 7 | 0.01 MB | 6 | 0.28 MB |
| `large` | 大页数 / 大文件 | 0 | 0.00 MB | 3 | 10.75 MB |
| `cjk` | 中日韩（CJK） | 8 | 0.02 MB | 17 | 2.32 MB |
| `pdf20` | PDF 2.0 | 9 | 0.05 MB | 2 | 1.27 MB |
| `page-tree` | 页树（继承属性/旋转/奇怪的树） | 8 | 0.01 MB | 2 | 0.05 MB |
| `scanned` | 扫描件形态（整页图片） | 0 | 0.00 MB | 1 | 1.10 MB |
| `outline-form-attach` | 带书签/表单/附件（合并时要提示丢失） | 4 | 0.01 MB | 0 | 0.00 MB |
| **合计** | | **65** | **0.18 MB** | **57** | **19.57 MB** |

- 第 1.5 层又加了 `public/encrypted/qpdf-generated/` 的 31 个自己生成的加密文件（0.12 MB），上表的个数和合计没有算它们（中文组的文件也没有算，见 SOURCES-zh.md）。
- 总共 122 个文件。public 0.18 MB（上限 50 MB），local 19.57 MB（上限 200 MB）。最大的单个文件是 issue3188.pdf（local/，7.75 MB），没有超过 30 MB 的。
- **页数最多的是 `freeculture.pdf`，352 页**（`local/large/`）。扫完全部候选，≥300 页的只有这一个（≥100 页的也只有它）。
- 来源分布：public 里 pdf.js 16 个、PDFium 42 个、pdf20examples 7 个；local 里 pdf.js 51 个、PDFium 6 个。
- 候选池：pdf.js `test/pdfs/` 里实际存着的 988 个 PDF（另有 460 个 `.link` 指向外部网址，按要求跳过），PDFium `testing/resources/` 顶层 297 个 PDF（子目录里只有 `.in` 模板和 XFA/像素测试，没取）。共扫描 1285 个，其中：xref 流 302、对象流 293、混合 xref 16、线性化 195、加密 35、mupdf 需要重建 xref 的 142、PDF 2.0 9、前 6 页有中日韩文字的 42。

## 固定版本与取回方式

| 来源 | 固定到 | 许可证 | 说明 |
|---|---|---|---|
| [Mozilla pdf.js](https://github.com/mozilla/pdf.js) | `17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7`（2026-10-05） | Apache-2.0（仓库 LICENSE） | 仓库没有单独声明 `test/pdfs/` 的许可，里面混着团队自己写的文件和 bug 报告里的第三方文件，所以只把能确认是手写合成的放 public |
| [PDFium](https://pdfium.googlesource.com/pdfium) | `84f950b4793b26b916db8853791f3475b826fdc7`（2026-10-05） | BSD-3-Clause（LICENSE 文件另附了 Apache-2.0 文本，都是宽松许可） | GitHub 镜像 chromium/pdfium 最后更新是 2025-11-19，落后将近一年，所以文件取自 googlesource 的 `testing/resources` 归档；GitHub 镜像只用来查“谁添加了哪个文件”（镜像历史到 2025-11 是完整的） |
| [pdf-association/pdf20examples](https://github.com/pdf-association/pdf20examples) | `c20f2c17bfcc4baab7cfe62e70fae64caf14d5fa` | CC BY-SA 4.0（仓库 LICENSE.md） | 7 个官方 PDF 2.0 示例，全部放 public，原样分发并保留署名 |

分类方法：只用 pypdf 6.16 / pymupdf 1.28 做本地检查。xref 类型、对象流、线性化、`%%EOF` 个数、损坏迹象（startxref 偏移、xref 偏移、/Length、文件头、截断）用原始字节扫描；页数、字体、文字、旋转用 mupdf 读取；页树深度、属性继承用 pypdf 遍历。“mupdf 需重建 xref”表示 mupdf 打开时走了修复流程；“pypdf 严格模式失败”表示 `PdfReader(strict=True)` 抛了异常。表格里的“页数”是 mupdf 的结果，损坏文件里可能读不出。“页继承属性”指页字典自己没有、从上级 /Pages 节点继承来的键。

## 放 public 的依据

| 依据 | 把握 | 说明 |
|---|---|---|
| PDFium 的 `.in` 模板生成 | 高 | 同目录有 `<名字>.in`，是 PDFium 团队自己写的文本，用 `fixup_pdf_template.py` 生成 PDF |
| PDFium 提交者手写的最小文件 | 中 | 没有 `.in`，但体积很小（多数 <2KB）、没有第三方内容、由 PDFium 提交者在修 bug 的提交里添加（表里写了添加者和提交号），按此推断是项目自己写的 |
| pdf.js 贡献者手写的合成文件 | 中 | 体积很小（<7KB）、无 Producer/Creator（`vertical.pdf` 是 dvipdfmx 编的但正文是自编的 `あいうえお日本語`）、正文是自编的标记文字（如 `Issue 8088 - Page 2`、`Bug1606566`、`あいうえお`），按此推断是贡献者自己写的；表里写了添加者和提交号 |
| PDF Association 示例 | 高 | 仓库 LICENSE.md 明确 CC BY-SA 4.0 |

凡是 Acrobat / Word / LibreOffice / Skia / TeX 等工具产生的真实文档、`issueNNNN` 这类从 bug 报告里缩出来的文件、来源不明的，都放 local。

## 文件清单

“特征”列由检查工具自动得出；加粗的是人工补的说明。路径相对 `tests/corpus/`。

### public（可进公开仓库）

#### `public/xref-classic/` — 经典 xref 表（6 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `asciihexdecode.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/asciihexdecode.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Saebekassebil 2011-07-08（f88d05e3b） | v1.0；1页；经典xref；**内容流 ASCIIHexDecode** |
| `hello_world.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/hello_world.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 hello_world.in | v1.7；1页；经典xref；页继承属性(/MediaBox) |
| `hello_world_2_pages.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/hello_world_2_pages.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 hello_world_2_pages.in | v1.7；2页；经典xref；页继承属性(/MediaBox) |
| `hello_world_compressed_stream.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/hello_world_compressed_stream.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2019-07-03（2697cb144） | v1.7；1页；经典xref；页继承属性(/MediaBox)；**内容流 FlateDecode** |
| `many_rectangles.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/many_rectangles.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 many_rectangles.in | v1.7；1页；经典xref；页继承属性(/MediaBox) |
| `rectangles.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/rectangles.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 rectangles.in | v1.7；1页；经典xref；页继承属性(/MediaBox) |

#### `public/xref-stream-objstm/` — xref 流 / 对象流（1 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug_757705.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_757705.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2017-08-24（91f443f4f） | v1.7；1页；xref流；对象流；线性化；**xref 流 + 对象流 + 线性化，只有 1.5KB 的 Hello world** |

#### `public/hybrid/` — 混合 xref（/XRefStm）（1 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug_1324503.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_1324503.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_1324503.in | v1.7；页数:读不出(mupdf=0)；混合xref；增量更新(2个%EOF)；损坏:mupdf需重建xref,pypdf严格模式失败；**伪混合 xref：第二个 trailer 带 /XRefStm -1（非法偏移）；目录对象把 << 写成 <；2 个 %%EOF；mupdf 要重建，不是真正可用的混合文件** |

#### `public/incremental/` — 增量更新（多个 %%EOF）（3 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `signature_no_sub_filter.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/signature_no_sub_filter.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 signature_no_sub_filter.in | v1.7；1页；经典xref；增量更新(2个%EOF)；页继承属性(/MediaBox) |
| `signature_reason.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/signature_reason.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 signature_reason.in | v1.7；1页；经典xref；增量更新(2个%EOF)；页继承属性(/MediaBox) |
| `two_signatures.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/two_signatures.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 two_signatures.in | v1.7；1页；经典xref；增量更新(3个%EOF)；页继承属性(/MediaBox) |

#### `public/damaged/` — 损坏/畸形（18 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug1606566.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/bug1606566.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Jonas Jenwald 2020-02-05（88c35d872） | 1页；经典xref；损坏:缺文件头,startxref偏移错,mupdf需重建xref,pypdf严格模式失败；**文件头 %PDF- 后没有版本号** |
| `bug_1301.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_1301.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_1301.in | v1.7；1页；经典xref；损坏:缺startxref,mupdf需重建xref,pypdf严格模式失败；**缺 startxref** |
| `bug_216.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_216.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_216.in | v1.7；1页；无可用xref；损坏:缺startxref,mupdf需重建xref,pypdf严格模式失败；**缺 startxref 和 xref** |
| `bug_325_a.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_325_a.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2016-01-05（63f9be7ce） | 页数:读不出(mupdf=None)；经典xref；损坏:缺文件头,缺%%EOF(截断),缺startxref,缺trailer,xref偏移错,pypdf严格模式失败；**82 字节；缺文件头、缺 trailer** |
| `bug_343.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_343.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Wei Li 2016-01-08（8e3f8931c） | v1.6；页数:读不出(mupdf=0)；无可用xref；损坏:文件头前有1字节,缺startxref,mupdf需重建xref,pypdf严格模式失败；**文件头前有 1 字节垃圾** |
| `bug_360.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_360.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Wei Li 2016-01-14（215816b74） | v1.2；页数:读不出(mupdf=0)；经典xref；损坏:缺%%EOF(截断),缺startxref,xref偏移错,mupdf需重建xref,pypdf严格模式失败；**截断；xref 偏移错** |
| `bug_42270471.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_42270471.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_42270471.in | v1.7；1页；经典xref；损坏:startxref偏移错,xref偏移错,mupdf需重建xref,pypdf严格模式失败；**startxref 偏移错且 xref 偏移错** |
| `bug_451830.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_451830.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Tom Sepez 2015-01-27（e80685c12） | v1.2；页数:读不出(mupdf=0)；无可用xref；损坏:缺startxref,mupdf需重建xref,pypdf严格模式失败；**126 字节；缺 startxref** |
| `bug_459580.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_459580.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_459580.in | v1.7；1页；经典xref；损坏:mupdf需重建xref；页继承属性(/MediaBox)；**对象缺 endobj** |
| `bug_544880.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_544880.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_544880.in | v1.7；1页；经典xref；损坏:pypdf严格模式失败；**页树成环** |
| `bug_664284.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_664284.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_664284.in | v1.7；1页；经典xref；损坏:startxref偏移错,xref偏移错,/Length错(1/1),mupdf需重建xref,pypdf严格模式失败；**20KB；startxref、xref 偏移、/Length 都错** |
| `empty_xref.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/empty_xref.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2019-01-08（7a16671bd） | v1.7；1页；经典xref；增量更新(2个%EOF)；损坏:mupdf需重建xref；页继承属性(/MediaBox)；**xref 表为空** |
| `issue6069.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue6069.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Rob Wu 2015-07-10（fd29bb0c5） | v1.1；1页；经典xref；损坏:文件头前有29字节,缺%%EOF(截断),缺startxref,xref偏移错,/Length错(1/1),mupdf需重建xref,pypdf严格模式失败；**startxref 缺值；xref 偏移要减去文件头偏移** |
| `issue9105_reduced.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue9105_reduced.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Jonas Jenwald 2017-12-08（1dc54ddb4） | v1.7；1页；经典xref；损坏:startxref偏移错,mupdf需重建xref,pypdf严格模式失败；**缺 endobj** |
| `parser_rebuildxref_error_notrailer.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/parser_rebuildxref_error_notrailer.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Wei Li 2015-12-02（7d1578a9a） | v1.7；1页；无可用xref；损坏:缺startxref,缺trailer,mupdf需重建xref,pypdf严格模式失败；**缺 xref 和 trailer，必须重建** |
| `sci-notation.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/sci-notation.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Jeff Muizelaar 2026-02-25（8fa6ef36e） | v1.0；1页；经典xref；损坏:xref偏移错,mupdf需重建xref；**内容流里的数字用了科学计数法（1e2），xref 偏移错** |
| `trailer_unterminated.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/trailer_unterminated.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 trailer_unterminated.in | v1.7；1页；经典xref；损坏:mupdf需重建xref,pypdf严格模式失败；**trailer 字典没闭合** |
| `xref_command_missing.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/xref_command_missing.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Jonas Jenwald 2015-10-01（192907e0d） | v1.7；1页；无可用xref；损坏:startxref偏移错,mupdf需重建xref,pypdf严格模式失败；**缺 xref 关键字** |

#### `public/encrypted/` — 加密（7 个，打开密码见下）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug_644.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_644.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_644.in | v1.7；页数:需密码才能读；经典xref；加密 V5/R5 AESV3；损坏:startxref偏移错,xref偏移错,mupdf需重建xref；**V5/R5 AES-256；startxref 也错** |
| `encrypted_hello_world_r2.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/encrypted_hello_world_r2.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2019-01-14（b76e45126） | v1.7；页数:需密码才能读；经典xref；加密 V1/R2 RC4；**V1/R2 RC4-40** |
| `encrypted_hello_world_r2_bad_okey.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/encrypted_hello_world_r2_bad_okey.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2019-12-18（d690c3d32） | v1.7；经典xref；加密 V1/R2 RC4；**V1/R2，/O 值被改坏** |
| `encrypted_hello_world_r3.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/encrypted_hello_world_r3.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2019-01-14（b76e45126） | v1.7；页数:需密码才能读；经典xref；加密 V2/R3 RC4；**V2/R3 RC4-128** |
| `encrypted_hello_world_r3_bad_okey.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/encrypted_hello_world_r3_bad_okey.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2019-12-18（d690c3d32） | v1.7；经典xref；加密 V2/R3 RC4；**V2/R3，/O 值被改坏** |
| `encrypted_hello_world_r5.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/encrypted_hello_world_r5.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2019-01-14（b76e45126） | v1.7；页数:需密码才能读；经典xref；加密 V5/R5 AESV3；**V5/R5 AES-256（Adobe 扩展）** |
| `encrypted_hello_world_r6.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/encrypted_hello_world_r6.pdf) | PDFium 提交者手写的最小测试文件，无第三方内容（BSD-3-Clause；按提交记录和内容推断）；添加者 Lei Zhang 2019-01-14（b76e45126） | v1.7；页数:需密码才能读；经典xref；加密 V5/R6 AESV3；**V5/R6 AES-256（PDF 2.0）** |

打开密码（第 1.5 层查到的；`crates/qingpdf-core/tests/common/mod.rs` 里的 `KNOWN` 是同一张表）：

| 文件 | 用户密码 | 所有者密码 | 出处 |
|---|---|---|---|
| `encrypted_hello_world_r2.pdf`、`_r3.pdf`、`_r5.pdf`、`_r6.pdf` | `hôtel` | `âge` | PDFium 的 `cpdf_security_handler_embeddertest.cpp`（R2、R3 的文件用 Latin-1 写，R5、R6 用 UTF-8；两种我们都能开） |
| `encrypted_hello_world_r2_bad_okey.pdf`、`_r3_bad_okey.pdf` | 没有能开的 | 没有能开的 | `/O` 被改坏了，PDFium 用它们测"不崩溃"；qpdf 和我们都说密码不对 |
| `bug_644.pdf` | `a` | `b` | 用 qpdf 猜出来的（R5，xref 坏了，要先修复） |
| `local/` 的 `bug900822.pdf`、`empty_protected.pdf`、`issue17215.pdf`、`issue19484_1.pdf` | 空密码 | 不知道 | pdf.js 清单里没写密码；`issue19484_1.pdf` 是 V4 加 5 字节 RC4 密钥，只有 pdf.js 和我们能开，qpdf、MuPDF、PDFium 开不了 |
| `local/issue3371.pdf` | 不知道 | `ELXRTQWS` | pdf.js 的清单 |
| `local/pr6531_1.pdf` | `asdfasdf` | `asdfasdf` | pdf.js 的清单；用户和所有者是同一个密码 |

#### `public/encrypted/qpdf-generated/` — 自己生成的加密文件（31 个）

我们自己做的，用 qpdf 12.4.2 把 `public/` 里的小文件加密得到（来源写在 `manifest.tsv` 的第 5 列）：许可证同来源文件（都是 `public/` 里的，可以再分发）。`generate.py` 是生成脚本，`manifest.tsv` 是清单（文件名、用户密码、所有者密码、另外能打开它的密码、来源、用途），测试读这张清单。覆盖：RC4 40 位（R2）、RC4 128 位（R3，和 V4 的 `/CF` RC4）、AES-128（R4）、AES-256（R5、R6），有无用户密码，几种权限（`--print=none`、`--modify=none`、`--extract=n`、`--assemble=n`、全部禁止），中文密码（R6 的用户和所有者密码；R4 的 UTF-8 字节），超过 32 字节的密码，正好 127 字节的 R6 密码（`utf8.r6-aes256-user-127bytes.pdf`：`0123456789` 重复 12 遍再加 `0123456`，R6 只认前 127 字节，qpdf 自己也不收更长的），全角写法的密码（`ＡＢＣ１２３` 对应 `ABC123`，靠轻量 SASLprep 打开），`--cleartext-metadata`（`/EncryptMetadata false`），所有者密码也为空，带签名字典、表单、嵌入文件的文件，带对象流和交叉引用流的文件。这些密码是为测试编的。`python generate.py` 重新生成（每次的 `/ID` 和密钥不同，文件内容会变，清单不变）。

#### `public/cjk/` — 中日韩（CJK）（8 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `90ms_rksj_h_sample.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/90ms_rksj_h_sample.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Calixte Denizet 2026-05-25（8f85e3f20） | v1.4；1页；经典xref；字体:HeiseiMin-W3(Type0,未嵌入,90ms-RKSJ-H)；**日文；90ms-RKSJ-H；未嵌入 HeiseiMin-W3** |
| `bug_420508260.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_420508260.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_420508260.in | v1.7；1页；经典xref；前6页文字:汉字4；**Helvetica 英文，中文“我的”“食物”只出现在 /ActualText（UTF-16BE）里，用来测文字提取** |
| `bug_431824298.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_431824298.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_431824298.in | v1.7；1页；经典xref；字体:SimSun(Type0,未嵌入,Identity-H)；前6页文字:汉字2；**汉字“借款”；Type0/CIDFontType2 的 SimSun，名字带子集前缀但没有嵌入字体文件；Identity-H + /ToUnicode** |
| `issue11555.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue11555.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Takashi Tamura 2020-01-31（d8c9f119b） | v1.7；1页；经典xref；字体:KozMinPro6N-Regular(Type0,未嵌入,90ms-RKSJ-V)；前6页文字:假名6；竖排；**日文竖排；90ms-RKSJ-V；未嵌入** |
| `noembed-identity-2.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/noembed-identity-2.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 vyv03354 2013-01-24（0df411a3d） | v1.2；1页；经典xref；字体:MS-PGothic(Type0,未嵌入,Identity-H)；前6页文字:假名1；页继承属性(/MediaBox)；**日文；未嵌入 MS-PGothic；Identity-H** |
| `noembed-sjis.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/noembed-sjis.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 vyv03354 2013-01-17（2ef50c506） | v1.2；1页；经典xref；字体:MS-Gothic(Type0,未嵌入,90ms-RKSJ-H)；前6页文字:假名5；页继承属性(/MediaBox)；**日文；未嵌入 MS-Gothic；90ms-RKSJ-H** |
| `vertical.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/vertical.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 vyv03354 2013-02-08（c5b8ee6a9） | v1.4；3页；经典xref；字体:AokinMincho(Type0,嵌入,Identity-V)；前6页文字:汉字3/假名5；竖排；页继承属性(/MediaBox)；Producer:dvipdfmx (20090506)；**日文竖排；Identity-V；3 页；dvipdfmx 生成** |
| `vertical_text.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/vertical_text.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 vertical_text.in | v1.7；1页；经典xref；字体:Test(Type0,嵌入,UniGB-UTF16-V)；竖排；页继承属性(/MediaBox)；**竖排机制：UniGB-UTF16-V + Adobe-GB1 + /DW2 /W2 竖排度量，嵌入测试字体；画的是拉丁字母 Hello World!，不是汉字** |

#### `public/pdf20/` — PDF 2.0（9 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `issue14755.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue14755.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Calixte Denizet 2023-05-18（3091e70aa） | v2.0；1页；经典xref；损坏:xref偏移错 |
| `issue19176.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue19176.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Jonas Jenwald 2024-12-08（c6e3fc4fe） | v2.0；1页；经典xref；**手写；/UserUnit** |
| `PDF 2.0 image with BPC.pdf` | [pdf20examples@c20f2c1](https://github.com/pdf-association/pdf20examples/blob/c20f2c17bfcc4baab7cfe62e70fae64caf14d5fa/PDF%202.0%20image%20with%20BPC.pdf) | PDF Association 官方示例（CC BY-SA 4.0，原样分发并保留署名） | v2.0；1页；经典xref |
| `PDF 2.0 UTF-8 string and annotation.pdf` | [pdf20examples@c20f2c1](https://github.com/pdf-association/pdf20examples/blob/c20f2c17bfcc4baab7cfe62e70fae64caf14d5fa/PDF%202.0%20UTF-8%20string%20and%20annotation.pdf) | PDF Association 官方示例（CC BY-SA 4.0，原样分发并保留署名） | v2.0；1页；经典xref |
| `PDF 2.0 via incremental save.pdf` | [pdf20examples@c20f2c1](https://github.com/pdf-association/pdf20examples/blob/c20f2c17bfcc4baab7cfe62e70fae64caf14d5fa/PDF%202.0%20via%20incremental%20save.pdf) | PDF Association 官方示例（CC BY-SA 4.0，原样分发并保留署名） | v1.7；1页；经典xref；增量更新(2个%EOF)；**1.7 文件增量保存成 2.0（目录 /Version /2.0）** |
| `PDF 2.0 with offset start.pdf` | [pdf20examples@c20f2c1](https://github.com/pdf-association/pdf20examples/blob/c20f2c17bfcc4baab7cfe62e70fae64caf14d5fa/PDF%202.0%20with%20offset%20start.pdf) | PDF Association 官方示例（CC BY-SA 4.0，原样分发并保留署名） | v2.0；1页；经典xref；损坏:文件头前有656字节,pypdf严格模式失败；**文件头不在字节 0（偏移 656），xref 偏移相对 %PDF- 起点；文件名含空格** |
| `PDF 2.0 with page level output intent.pdf` | [pdf20examples@c20f2c1](https://github.com/pdf-association/pdf20examples/blob/c20f2c17bfcc4baab7cfe62e70fae64caf14d5fa/PDF%202.0%20with%20page%20level%20output%20intent.pdf) | PDF Association 官方示例（CC BY-SA 4.0，原样分发并保留署名） | v2.0；2页；经典xref |
| `pdf20-utf8-test.pdf` | [pdf20examples@c20f2c1](https://github.com/pdf-association/pdf20examples/blob/c20f2c17bfcc4baab7cfe62e70fae64caf14d5fa/pdf20-utf8-test.pdf) | PDF Association 官方示例（CC BY-SA 4.0，原样分发并保留署名） | v2.0；1页；经典xref；Producer:By hand |
| `Simple PDF 2.0 file.pdf` | [pdf20examples@c20f2c1](https://github.com/pdf-association/pdf20examples/blob/c20f2c17bfcc4baab7cfe62e70fae64caf14d5fa/Simple%20PDF%202.0%20file.pdf) | PDF Association 官方示例（CC BY-SA 4.0，原样分发并保留署名） | v2.0；1页；经典xref |

#### `public/page-tree/` — 页树（继承属性/旋转/奇怪的树）（8 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug_1506.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_1506.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bug_1506.in | v1.7；4页；经典xref；损坏:pypdf严格模式失败；页树3层；**3 层页树，4 页** |
| `cropped_no_overlap.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/cropped_no_overlap.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 cropped_no_overlap.in | v1.7；1页；经典xref；页继承属性(/CropBox,/MediaBox)；**继承 /MediaBox 和 /CropBox** |
| `hello_world_rotated.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/hello_world_rotated.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 calixteman 2026-03-22（243659380） | v1.4；5页；经典xref；/Rotate=90；**5 页，每页都是 /Rotate 90** |
| `issue8088.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue8088.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Jonas Jenwald 2017-02-23（1ce295541） | v1.7；3页；经典xref；页树4层；**4 层页树，3 页** |
| `no_page_count.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/no_page_count.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 no_page_count.in | v1.7；0页；经典xref；损坏:pypdf严格模式失败；页继承属性(/MediaBox)；页树3层；**页树没有 /Count** |
| `page_tree_empty_node.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/page_tree_empty_node.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 page_tree_empty_node.in | v1.7；0页；经典xref；页继承属性(/MediaBox)；页树3层；**页树有空节点，3 层** |
| `Pages-tree-refs.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/Pages-tree-refs.pdf) | pdf.js 贡献者手写的合成测试文件，内容是自编标记文字（Apache-2.0；按提交记录和内容推断）；添加者 Jonas Jenwald 2020-02-08（3c7b7be10） | v1.7；2页；经典xref；损坏:pypdf严格模式失败；页继承属性(/MediaBox)；页树4层；**/Pages 树里有循环引用，4 层** |
| `rectangles_multi_pages.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/rectangles_multi_pages.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 rectangles_multi_pages.in | v1.7；5页；经典xref；/Rotate=90；页继承属性(/MediaBox)；**5 页，带 /Rotate** |

#### `public/outline-form-attach/` — 带书签/表单/附件（合并时要提示丢失）（4 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bookmarks.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bookmarks.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 bookmarks.in | v1.7；2页；经典xref；**2 页；有书签（/Outlines）；合并时会丢** |
| `embedded_attachments_with_desc.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/embedded_attachments_with_desc.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 embedded_attachments_with_desc.in | v1.7；1页；经典xref；**有内嵌附件（/EmbeddedFiles）；合并时会丢** |
| `multiple_form_types.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/multiple_form_types.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 multiple_form_types.in | v1.7；1页；经典xref；**有 AcroForm 表单（多种控件）；合并时会丢** |
| `utf-8.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/utf-8.pdf) | PDFium 用自己的 `.in` 文本模板生成（BSD-3-Clause）；同目录有 utf-8.in | v1.7；1页；经典xref；Producer:Manüally Created；**书签标题和 /Producer 是带 BOM 的 UTF-8 字符串（PDF 2.0 写法）；合并时书签会丢** |

### local（仅本机，git 忽略）

#### `local/xref-classic/` — 经典 xref 表（2 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `TAMReview.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/TAMReview.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.3；23页；经典xref；增量更新(2个%EOF)；Producer:htmldoc 1.8.27 Copyright；**23 页** |
| `tracemonkey.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/tracemonkey.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；14页；经典xref；页树3层；Producer:pdfeTeX-1.21a；**14 页 TeX 论文（有图有公式），pdf.js 的招牌测试文件** |

#### `local/xref-stream-objstm/` — xref 流 / 对象流（5 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug1811510.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/bug1811510.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；1页；xref流；Producer:Acrobat Pro 22.3.20281；**只有 xref 流，没有对象流** |
| `issue11279.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue11279.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.5；1页；xref流；对象流；页继承属性(/MediaBox)；Producer:dvipdfmx (20180506)；**dvipdfmx 生成** |
| `issue14165.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue14165.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.5；1页；xref流；对象流；Producer:Microsoft® Word 2016；**Word 2016 生成** |
| `issue16038.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue16038.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.5；1页；xref流；对象流；Producer:pdfTeX-1.40.24；**pdfTeX 生成** |
| `issue17492.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue17492.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；1页；xref流；增量更新(2个%EOF)；Producer:OpenOffice 4.1.7；**OpenOffice；xref 流，没有对象流；增量更新** |

#### `local/hybrid/` — 混合 xref（/XRefStm）（5 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug_717.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_717.pdf) | PDFium 仓库里非 `.in` 生成的文件，由 Acrobat/Word 等工具产生，来源不明；添加者 wileyrya 2017-05-31（e858aa4b7） | v1.5；1页；混合xref；对象流；增量更新(2个%EOF)；Producer:Microsoft® Office Word 2；**Word 生成；混合 + 增量更新** |
| `issue11656.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue11656.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；混合xref；对象流；增量更新(2个%EOF)；Producer:Microsoft® Word for Offi；**Word 生成** |
| `issue12706.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue12706.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；1页；混合xref；对象流；线性化；增量更新(4个%EOF)；Producer:Acrobat Distiller 8.1.0；**Acrobat Distiller；线性化 + 混合；4 个 %%EOF** |
| `issue17147.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue17147.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；混合xref；对象流；增量更新(2个%EOF)；损坏:startxref偏移错,mupdf需重建xref,pypdf严格模式失败；Producer:Microsoft® Word 2019；**Word 2019；混合 xref，startxref 偏移错** |
| `issue20324.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue20324.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；1页；混合xref；对象流；线性化；Producer:Adobe PDF Library 17.0；**Adobe PDF Library；线性化 + 混合** |

#### `local/incremental/` — 增量更新（多个 %%EOF）（4 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `annotation-highlight-without-appearance.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/annotation-highlight-without-appearance.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.5；1页；经典xref；增量更新(4个%EOF)；Producer:LibreOffice 6.2；**LibreOffice；4 个 %%EOF** |
| `bug1708041.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/bug1708041.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；6页；xref流；对象流；增量更新(4个%EOF)；Producer:Adobe Acrobat 25.1.20529；**Acrobat；4 个 %%EOF，6 页** |
| `comments.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/comments.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；14页；xref流；对象流；增量更新(12个%EOF)；页树3层；Producer:macOS Version 14.4.1 (Bu；**macOS 预览批注，12 个 %%EOF，14 页** |
| `issue15629.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue15629.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；增量更新(5个%EOF)；**5 个 %%EOF** |

#### `local/linearized/` — 线性化（5 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `feature_linearized_loading.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/feature_linearized_loading.pdf) | PDFium 仓库里非 `.in` 生成的文件，由 Acrobat/Word 等工具产生，来源不明；添加者 Jun Fang 2015-11-10（df7f36633） | v1.6；2页；xref流；对象流；线性化；Producer:Acrobat Web Capture 9.0；**Acrobat Web Capture；xref 流 + 对象流 + 线性化** |
| `labelled_pages.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/labelled_pages.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；11页；xref流；对象流；线性化；页树3层；Producer:Adobe Acrobat (64-bit) 2；**Acrobat；11 页，页标签，3 层页树** |
| `linearized.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/linearized.pdf) | PDFium 仓库里非 `.in` 生成的文件，由 Acrobat/Word 等工具产生，来源不明；添加者 Henrique Nakashima 2017-11-10（9fa503624） | v1.6；3页；经典xref；线性化；**3 页；经典 xref 线性化；无 Producer，头部是 Acrobat 风格** |
| `linearized_bug_1055.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/linearized_bug_1055.pdf) | PDFium 仓库里非 `.in` 生成的文件，由 Acrobat/Word 等工具产生，来源不明；添加者 Lei Zhang 2018-04-05（10f9fb3f1） | v1.6；3页；经典xref；线性化；损坏:xref偏移错,mupdf需重建xref；**线性化；hint 表数据损坏，xref 偏移错** |
| `rotation.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/rotation.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；2页；经典xref；线性化；/Rotate=90；Producer:Acrobat Distiller 8.1.0；**Acrobat Distiller；经典 xref 线性化，2 页带旋转** |

#### `local/damaged/` — 损坏/畸形（5 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug1795263.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/bug1795263.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.3；1页；经典xref；损坏:startxref偏移错,xref偏移错,/Length错(1/40),mupdf需重建xref,pypdf严格模式失败；Producer:PyPDF2；**PyPDF2 生成；startxref、xref 偏移、/Length 都错** |
| `bug_601362.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/bug_601362.pdf) | PDFium 仓库里非 `.in` 生成的文件，由 Acrobat/Word 等工具产生，来源不明；添加者 ochang 2016-04-11（b8627c9d1） | v1.5；1页；经典xref；损坏:startxref偏移错,xref偏移错,/Length错(1/1),mupdf需重建xref,pypdf严格模式失败；**Word 生成；startxref、xref 偏移、/Length 都错** |
| `PDFBOX-3148-2-fuzzed.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/PDFBOX-3148-2-fuzzed.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；1页；xref流；损坏:mupdf需重建xref,pypdf严格模式失败；Producer:Adobe PDF Library 15.0；**模糊测试产物；ASCII85 数据损坏** |
| `scan-bad.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/scan-bad.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.3；1页；经典xref；损坏:缺%%EOF(截断),尾部截断,mupdf需重建xref；**截断：没有 startxref 和 %%EOF** |
| `text_font.pdf` | [PDFium@84f950b](https://pdfium.googlesource.com/pdfium/+/84f950b4793b26b916db8853791f3475b826fdc7/testing/resources/text_font.pdf) | PDFium 仓库里非 `.in` 生成的文件，由 Acrobat/Word 等工具产生，来源不明；添加者 Miklos Vajna 2018-08-01（53d4f0a45） | v1.5；1页；经典xref；损坏:xref偏移错；Producer:LibreOfficeDev 6.2；**LibreOffice 生成；xref 偏移错** |

#### `local/encrypted/` — 加密（6 个，打开密码见下）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug900822.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/bug900822.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.3；1页；经典xref；加密 V1/R2 RC4；Producer:ReportBuilder；**V1/R2 RC4-40；用户密码为空** |
| `empty_protected.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/empty_protected.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；xref流；对象流；线性化；加密 V5/R6 AESV3；Producer:Adobe Acrobat Pro (64-bi；**Acrobat；V5/R6 AES-256；线性化** |
| `issue17215.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue17215.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.5；1页；经典xref；加密 V2/R3 RC4；Producer:PDFsharp 1.50.5147 (www.；**V2/R3 RC4-128；PDFsharp** |
| `issue19484_1.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue19484_1.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；0页；xref流；对象流；线性化；加密 V4/R4 V2；**V4/R4，CFM=V2（RC4），密钥只有 5 字节** |
| `issue3371.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue3371.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；页数:需密码才能读；xref流；对象流；线性化；加密 V4/R4 AESV2，密码 'ELXRTQWS'；**V4/R4 AESV2；线性化；打开密码 ELXRTQWS** |
| `pr6531_1.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/pr6531_1.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；页数:需密码才能读；xref流；对象流；线性化；加密 V5/R6 AESV3；**V5/R6 AES-256；线性化** |

#### `local/large/` — 大页数 / 大文件（3 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `freeculture.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/freeculture.pdf) | 《Free Culture》，文件自带声明为 CC BY-NC 1.0：非商业条款，不能进公开库 | v1.5；352页；经典xref；线性化；页树4层；Producer:Acrobat Distiller 5.0.5；**352 页，4 层页树，38 个中间节点（全库页数最多）** |
| `issue3188.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue3188.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；1页；xref流；对象流；线性化；增量更新(6个%EOF)；Producer:Scribus PDF Library 1.4.；**7.75 MB（8,125,374 字节，全库最大）；线性化；6 个 %%EOF；xref 流** |
| `pdkids.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/pdkids.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；55页；经典xref；Producer:OpenOffice.org 3.1；**55 页** |

#### `local/cjk/` — 中日韩（CJK）（17 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `bug1245391_reduced.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/bug1245391_reduced.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；1页；经典xref；前6页文字:汉字3；**简体中文“集油器”，字形用 Type3 画出，没有文字映射** |
| `bug1627427_reduced.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/bug1627427_reduced.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；字体:BatangChe(Type0,嵌入,Identity-H)；前6页文字:谚文27；**韩文；BatangChe** |
| `issue12823.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue12823.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；线性化；字体:TimesNewRomanPS-BoldMT(Type0,嵌入,Identity-H); HYShuSongErKW(Type0,嵌入,Identity-H); NotoColorEmoji(Type0,未嵌入,Identity-H)；前6页文字:汉字86；**简体中文；86 字；嵌入 HYShuSongErKW** |
| `issue13316_reduced.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue13316_reduced.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；损坏:mupdf需重建xref；字体:YouYuan(TrueType,嵌入)；前6页文字:汉字5；页继承属性(/MediaBox)；**简体中文；“开票通知单”票据；YouYuan 字体；xref 有问题** |
| `issue18117.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue18117.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；**繁体中文；思源黑体 TC** |
| `issue19182.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue19182.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；1页；xref流；对象流；字体:PMingLiU(Type0,未嵌入,UniCNS-UTF16-H); MingLiU(Type0,嵌入,Identity-H); MingLiU(TrueType,嵌入)；前6页文字:汉字6；Producer:iText® 5.5.9 ©2000-2015；**繁体中文；UniCNS-UTF16-H；MingLiU 未嵌入 + 子集；对象流** |
| `issue19360.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue19360.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.5；1页；经典xref；字体:FZKTK--GBK1-0(Type0,嵌入,Identity-H); SourceHanSansCN-Regula(Type0,嵌入,Identity-H); FZKTK--GBK1-0(TrueType,嵌入)；前6页文字:汉字23；Producer:Adobe PDF Library 15.0；**简体中文；方正楷体 GBK + 思源黑/宋 CN；1.2MB** |
| `issue20489.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue20489.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.2；1页；经典xref；Producer:[ClibPDF Library 2.01-r2；**简体中文；宋体/SimSun-ExtB 未嵌入的 TrueType，文字是 GBK 字节但声明 WinAnsi（按 Latin-1 解会乱码：±±¶·ÐÇ）** |
| `issue20504.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue20504.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；1页；xref流；对象流；线性化；增量更新(3个%EOF)；前6页文字:汉字4；Producer:Adobe Acrobat Pro (64-bi；**多文种测试（你好世界 + 阿拉伯文 + 桑塔利文 + 曼尼普尔文）；无嵌入字体信息** |
| `issue2128r.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue2128r.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；字体:黑体,Bold(Type0,未嵌入,GBK-EUC-H)；前6页文字:汉字20；**简体中文；GBK-EUC-H；未嵌入** |
| `issue3405r.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue3405r.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；**文泉驿正黑 Identity-H 嵌入；取不出文字（无 ToUnicode）** |
| `issue3521.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue3521.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.3；1页；经典xref；字体:黑体,Bold-GBKp-EUC-H(Type0,未嵌入,GBKp-EUC-H); 黑体,BoldItalic-GBKp-EUC(Type0,未嵌入,GBKp-EUC-H); 黑体,Italic-GBKp-EUC-H(Type0,未嵌入,GBKp-EUC-H)；前6页文字:汉字7；页继承属性(/MediaBox)；Producer:FPDF 1.52；**简体中文；GBKp-EUC-H；未嵌入；FPDF 生成** |
| `issue4061.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue4061.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；字体:Ming-Lt-HKSCS-UNI-H(Type0,嵌入,Identity-H)；前6页文字:汉字14；**繁体中文；Ming-Lt-HKSCS-UNI-H（香港增补字符集）** |
| `issue6387.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue6387.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.5；1页；经典xref；线性化；字体:NotoSansCJKjp-Bold(Type0,嵌入,Identity-H); NotoSansCJKjp-Bold(Type0,嵌入,Identity-V)；前6页文字:汉字11/假名146；竖排；Producer:Adobe PDF library 11.00；**日文竖排；Identity-V；NotoSansCJKjp** |
| `issue8372.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue8372.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；经典xref；字体:AdobeHeitiStd-Regular(Type0,未嵌入,UniGB-UTF16-H)；前6页文字:汉字2；**简体中文；UniGB-UTF16-H；未嵌入 AdobeHeitiStd** |
| `SimFang-variant.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/SimFang-variant.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；1页；经典xref；字体:黑体(TrueType,未嵌入)；前6页文字:汉字10；Producer:Pdftools SDK；**简体中文标语，10 字；未嵌入黑体 TrueType，文字是 GBK 双字节十六进制串；pdf.js 提交说明称是仿宋变体测试** |
| `XiaoBiaoSong.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/XiaoBiaoSong.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.4；1页；经典xref；字体:仿宋_GB2312(TrueType,未嵌入); 宋体(TrueType,未嵌入)；前6页文字:汉字207；Producer:Pdftools SDK；**简体中文目录页，207 字；未嵌入仿宋_GB2312 + 宋体的 TrueType，文字是 GBK 双字节十六进制串（/TT0，声明 WinAnsi）；pdf.js 提交说明称是小标宋字体测试** |

#### `local/pdf20/` — PDF 2.0（2 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `Brotli-Prototype-FileA.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/Brotli-Prototype-FileA.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v2.0；25页；xref流；对象流；损坏:pypdf严格模式失败；/Rotate=270；Producer:pdfplot11.hdi 11.1.18.0；**PDF 2.0；25 页；用了 Brotli 过滤器（非标准）；/Rotate** |
| `bug2025674.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/bug2025674.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v2.0；1页；xref流；对象流；Producer:luahbtex-1.24.0；**LuaTeX；PDF 2.0；xref 流 + 对象流** |

#### `local/page-tree/` — 页树（继承属性/旋转/奇怪的树）（2 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `font_ascent_descent.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/font_ascent_descent.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.7；1页；xref流；对象流；线性化；/Rotate=90；页树3层；Producer:Acrobat Distiller 8.1.0；**/Rotate；3 层页树** |
| `page_with_number.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/page_with_number.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.6；17页；xref流；对象流；线性化；页树3层；Producer:Adobe PDF Library 25.1.5；**17 页；3 层页树（4 个中间节点）；Adobe PDF Library 25** |

#### `local/scanned/` — 扫描件形态（整页图片）（1 个）

| 文件 | 来源（固定到提交） | 许可 / 放这里的原因 | 特征 |
|---|---|---|---|
| `issue7229.pdf` | [pdf.js@17bb244](https://github.com/mozilla/pdf.js/blob/17bb2442fe53f348ee8e2e0b3e7c1c7d09468cd7/test/pdfs/issue7229.pdf) | pdf.js 仓库里的文件，来源/许可不明（多半来自 bug 报告或第三方文档） | v1.3；2页；经典xref；增量更新(2个%EOF)；**2 页 A4 扫描件形态：整页 1654x2338 DCT 图片，无文字（不是中文扫描件）** |

## 缺口（这几个来源覆盖不到的）

用户自己提供的中文 PDF 放仓库外面，用 `QINGPDF_PRIVATE_CORPUS` 指过去。下面这些是特别需要补的。

### 中文类

1. **公文**：没有。只有 `local/cjk/XiaoBiaoSong.pdf`、`SimFang-variant.pdf` 两个碎片（宋体、仿宋_GB2312、黑体是未嵌入的 TrueType，文字是 GBK 字节，这是中文公文的典型情形），但它们是 1 页的字体测试，不是公文。缺：多页、红头、公章（半透明图章）、WPS/Word 导出且缺 `/ToUnicode`、页码和文号布局。
2. **论文**：只有英文 TeX 论文（`local/xref-classic/tracemonkey.pdf`，14 页）。缺中文论文：知网下载版、LaTeX ctex（xdvipdfmx/XeTeX 的 CID 子集）、双栏、脚注、大量公式、参考文献超链接和书签。
3. **电子书**：没有。缺超星/读秀/方正的扫描书（几百页、图片加隐藏文字层、书签树）。能用的最大页数文件是英文《Free Culture》（352 页，CC BY-NC，只能 local）。
4. **竖排**：有日文竖排（`public/cjk/vertical.pdf`、`issue11555.pdf`、`local/cjk/issue6387.pdf`）和 PDFium 的 GB1 竖排机制样本（`public/cjk/vertical_text.pdf`，UniGB-UTF16-V + `/DW2 /W2`，但画的是拉丁字母，不是汉字）。缺真正有汉字的中文竖排版面：繁体古籍、标点竖排、`/WMode 1` 的内嵌 CMap、`/Rotate` 与竖排叠加。
5. **扫描件**：只有 `local/scanned/issue7229.pdf` 一个（A4，整页 DCT 图，无文字，不是中文）。缺中文扫描件：JBIG2 / CCITT G4 黑白 600dpi、MRC 分层、OCR 隐藏文字层（`Tr 3`）、几十 MB 的多页扫描、带 `/Rotate` 的扫描方向。
6. **繁体和港台**：只有 3 个 local 的小文件（MingLiU、HKSCS、思源黑体 TC），缺 Big5/ETen 编码的老文件。
7. **public 里和中文相关的只有 3 个极小的 PDFium 文件**（`vertical_text.pdf` 画拉丁字母、`bug_420508260.pdf` 的中文只在 `/ActualText` 里、`bug_431824298.pdf` 有“借款”2 个汉字），其余有真实汉字的文件都来自 bug 报告，只能放 local。公开的中文组主要靠你自己的文件补。

### 其他

- **大页数**：全部候选里 ≥300 页的只有 1 个（352 页）。验收里的“1000 页 info ≤0.3 秒”“两个 500 页合并 ≤2 秒”必须自己生成文件（例如用脚本合成页树，或把 `freeculture.pdf` 反复合并）。
- **线性化**：public 里没有线性化的纯净样本（只有 `public/xref-stream-objstm/bug_757705.pdf` 恰好也是线性化）；其余线性化文件都是 Acrobat 等工具产物，只能放 local。
- **xref 流 / 对象流 / 混合 xref**：public 里 xref 流/对象流只有 1 个（`bug_757705.pdf`）；混合 xref 在 public 里**没有真正可用的**，`bug_1324503.pdf` 的 `/XRefStm` 是 -1，是个坏文件。真实工具产物（Word、Acrobat、pdfTeX 等）的混合/xref 流文件全在 local。
- **增量更新**：public 只有 2~3 个 `%%EOF` 的签名类文件和 PDF 2.0 的增量示例；local 有 `comments.pdf`（12 个 `%%EOF`）。缺“反复编辑、有被删除对象、`/Prev` 链很长”的公开样本。
- **加密**：打开密码都查到了（上面的表）；V4（AESV2）、V5/R6 的真实文档仍然只在 local。`qpdf-generated/` 补上了各种方式、权限、中文密码、`/EncryptMetadata false`；`/Identity` 加密过滤器和流自己的 `/Crypt` 过滤器 qpdf 生成不出来，测试里改生成文件的字节来做。缺别的软件（Acrobat、Word、WPS）生成的 V4/R6 文件的公开样本。
- **大文件**：最大 7.75 MB（`issue3188.pdf`）。缺 >30 MB 的文件和 >10 万对象的文件，压力测试得自己造。
- **书签/表单/附件**：`public/outline-form-attach/` 只放了 4 个很小的文件，够测“合并时提示丢失了什么”，但没有带真实多级书签的大文件。

## 要留意的不确定点

1. **PDFium `LICENSE` 文件**：BSD-3-Clause 之后又附了一份 Apache-2.0 全文（GitHub 因此判成 NOASSERTION）。两个都是宽松许可，不影响 MIT/Apache 仓库；整份文件已放进 `public/LICENSES/pdfium-LICENSE.txt`。
2. **“手写合成”是推断**：pdf.js 和 PDFium 的测试目录都没有逐文件的作者/许可声明。放 public 的依据是体积、内容和提交记录，不是作者本人的声明。想更保守的话，把 pdf.js 的 16 个 public 文件（`public/cjk/` 5 个、`public/damaged/` 5 个，其余在 `page-tree`、`pdf20`、`xref-classic`）和 PDFium 无 `.in` 的 14 个 public 文件挪到 local，其余 public 文件的依据不变（PDFium `.in` 生成的、pdf20examples）。
3. **CC BY-SA 4.0**：`public/pdf20/` 里来自 pdf20examples 的 7 个文件。ShareAlike 只约束改编；原样放进来、保留署名是合规的，但这些文件不能被改过之后再当成 MIT/Apache 发布。拿不准的话也可以整体挪到 local。
4. **`freeculture.pdf`**：文件第 2 页自带声明为 CC BY-NC 1.0，非商业，不能进公开库，已放 local。
5. **中文 local 文件**：`issue2128r.pdf` 含真实人名和单位名，`XiaoBiaoSong.pdf` 是第三方文章，只放 local，不要公开。

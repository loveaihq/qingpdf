# 中文测试文件来源与许可证（SOURCES-zh）

生成日期 2026-10-06。本文件只管 `tests/corpus/public/zh/` 和 `tests/corpus/local/zh/`；通用测试文件见同目录的 `SOURCES.md`。

## 摘要

> **2026-10-06 调整**：《鹽政辭典》（1229 页，23 MB）许可证没问题（Commons 公有领域），但为了让仓库保持小，挪到了 `local/zh/large/`，不进 git。需要时按下面的网址重新下载，用 SHA-256 核对。

- 共 27 个 PDF：`public/zh/` 16 个（手写 5 + 下载 11），`local/zh/` 11 个。
- **大小**：`public/zh/` 全部文件合计 38.75 MB（含手写文件的脚本和 .expected.txt；上限 40 MB）；`local/zh/` 合计 57.52 MB（上限 200 MB）；单个文件最大 25.81 MB，没有超过 30 MB 的。
- **最大页数**：`large-yanzheng-cidian-1229p.pdf`，**1229 页**，在 `public/zh/large/`（扫描件，23.0 MB）。local 里还有 1073 页的《醫壘元戎》（扫描件）和 468 页的《中国医籍考》（**最长的文字层文件**）。≥ 300 页的一共 3 个。
- **public 的依据只有五类**：自己手写的文件；国家机关文件（著作权法第五条，公文）；arXiv 上 CC BY / CC0 的论文；Wikimedia Commons 上标为公有领域的古籍/民国书扫描和国务院公报扫描；Wikisource 导出的公有领域作品和法律文本。拿不准的一律放 `local/zh/`。
- 所有下载文件都是**原样保存**，一个字节没改（SHA-256 见文末）。不同于 `SOURCES.md`，这里没有固定到提交号，因为来源是网站文件而不是仓库：能固定的我固定到了版本（arXiv 版本号、Wikisource 修订号），gov.cn 和 Commons 文件以 SHA-256 为准。
- 手写文件的生成脚本是 `public/zh/handmade/make_handmade.py`，没有用任何 PDF 库，字节完全已知。
- 公文、论文、电子书、竖排、扫描件五类都已覆盖；同一类里尽量选了不同的生成器（WPS、Word/PDFMaker、LaTeX、iText、CNKI、Foxit、FreePic2Pdf、Chrome/Skia），结构差别见“值得注意的结构特征”。

## 概览

| 分类 | 含义 | public 个数 | public 大小 | local 个数 | local 大小 |
|---|---|---:|---:|---:|---:|
| `handmade` | 手写的非内嵌 CID 字体 PDF（含竖排、ToUnicode、繁体） | 5 | 7.9 KB | 0 | 0 |
| `gongwen` | 公文（含图片型公报扫描） | 4 | 3.05 MB | 3 | 7.25 MB |
| `lunwen` | arXiv 中文论文（LaTeX / Word / WPS / 方正） | 3 | 5.19 MB | 3 | 3.77 MB |
| `ebook` | Wikisource 导出的电子书（文字层） | 2 | 5.13 MB | 1 | 8.16 MB |
| `vertical` | 竖排文字层（Identity-V） | 1 | 2.36 MB | 1 | 1.30 MB |
| `scanned` | 整页图片扫描（JBIG2 / JPX） | 0 | 0 | 2 | 11.22 MB |
| `large` | ≥ 1000 页（性能测试用） | 1 | 23.00 MB | 1 | 25.81 MB |
| **合计（只算 PDF）** | | **16** | **38.74 MB** | **11** | **57.52 MB** |

说明：public 里的扫描件放在 `gongwen/`（1954 公报）和 `large/`（鹽政辭典）里，所以 `scanned/` 在 public 里是空的；竖排文字层的真实样本在 `vertical/`。

对照任务给的覆盖目标：

| 目标 | 实际 |
|---|---|
| 公文 ×3–4（文字层、内嵌中文字体） | public 4（文字层 3 + 图片型 1），local 3 |
| 论文 ×2–3 | public 3（LaTeX、Word 简体、Word 繁体），local 3（CNKI 排版、方正飞翔、WPS） |
| 电子书 ×2（文字层） | public 2（民法典 128 页、阿Q正传 23 页），local 1（468 页） |
| 竖排 ×2–3 | 文字层竖排：public 1（合川縣誌卷七十）+ 手写 1 + local 1；竖排扫描：public 1（鹽政辭典）+ local 3 |
| 扫描件 ×2–3 | public 2（1954 公报 CCITT、鹽政辭典 JBIG2），local 3（JBIG2、JPX、CCITT） |
| ≥ 300 页，最好 ≥ 1000 页 | public 1229 页；local 1073 页和 468 页 |
| 手写 4 个 | 5 个（多加了一个 BaseFont 命名惯例的变体） |

## 手写文件（`public/zh/handmade/`）

许可：本项目自有，随仓库许可证。全部由 `make_handmade.py` 生成，纯字符串拼接，对象布局固定：1 Catalog、2 Pages、3 Page、4 Type0 字体、5 CIDFont、6 FontDescriptor、7 内容流、8 ToUnicode（有才有）。每个 PDF 旁边有 `<名字>.expected.txt`（UTF-8），写着页面上应该提取出的文字，一行一个文本行（竖排文件一行一列）。

| 路径 | 页数 | 大小 | 内容 |
|---|---:|---:|---|
| `public/zh/handmade/zh-gb1-h-noembed.pdf` | 1 | 1354 字节 | 非内嵌 CID 字体：Type0 + CIDFontType0，/BaseFont /STSong-Light，/Encoding /UniGB-UCS2-H，CIDSystemInfo (Adobe)(GB1) 补充号 2，无 ToUnicode，横排 5 行 |
| `public/zh/handmade/zh-gb1-v-noembed.pdf` | 1 | 1255 字节 | 同上但 /Encoding /UniGB-UCS2-V（竖排，WMode 1），3 列，第一列在最右；没写 /DW2，用默认 [880 -1000] |
| `public/zh/handmade/zh-gb1-h-tounicode.pdf` | 1 | 2543 字节 | 同第 1 个，另带 /ToUnicode（bfchar + bfrange 两种写法都用到） |
| `public/zh/handmade/zh-cns1-h-noembed.pdf` | 1 | 1353 字节 | 繁体：/BaseFont /MSung-Light，/Encoding /UniCNS-UCS2-H，Ordering (CNS1) 补充号 3 |
| `public/zh/handmade/zh-gb1-h-basefont-suffix.pdf` | 1 | 1367 字节 | 额外加的：同第 1 个，但 Type0 的 /BaseFont 按 ISO 32000-1 表 121 的惯例写成 STSong-Light-UniGB-UCS2-H（多数真实生成器这样写） |
| `public/zh/handmade/zh-gb1-h-tounicode-differs.pdf` | 1 | 2.6 KB | **2026-10-07 第 2 层加的第 6 个**（上面和概览里的个数、大小仍按 5 个算）：同第 3 个，但 ToUnicode 故意和预定义 CMap 不一致（床→牀、明→朙、汉→漢字，后一个是两个字符），证明 ToUnicode 优先；`.expected.txt` 是 ToUnicode 的结果 |

**对 ISO 32000-1 的取舍**（`docs/decisions.md` 可以引用）：

- Type0 的 `/BaseFont`：9.7.6 表 121 说“应当”写成 `CIDFont名-CMap名`，但随后的注说明它实际上可以是任意名字。前 4 个文件按任务要求直接写 `/STSong-Light`、`/MSung-Light`，第 5 个文件按惯例写 `STSong-Light-UniGB-UCS2-H`，两种都有。
- CIDSystemInfo 的补充号：GB1 用 2（任务指定），CNS1 用 3。表 119 说 UniGB-UCS2-H 对应 Adobe-GB1-4，补充号 2 小于 4，严格讲 CMap 的补充号比字体的高；真实生成器常这样写，阅读器都接受，所以保留。
- 竖排文件没有写 `/DW2` 和 `/W2`，走默认值 `[880 -1000]`（表 117）。CMap 名以 `-V` 结尾即竖排，不需要别的标志。
- 非内嵌 + 预定义 CMap：字形要靠阅读器自己找系统字体（STSong-Light 对应宋体）。渲染不出来是字体问题，不是文件问题。

**PyMuPDF 1.28.2（MuPDF 1.28）和 pypdf 6.16.1 的核对结果**（2026-10-06）：

| 文件 | 打开 | 提取的文字与 expected.txt | 渲染 |
|---|---|---|---|
| `zh-gb1-h-noembed.pdf` | 无错误、无警告，`is_repaired = False` | **完全一致**（逐行） | 有字（MuPDF 自带 CJK 后备字体） |
| `zh-gb1-h-tounicode.pdf` | 同上 | **完全一致**；MuPDF 对非内嵌预定义 CMap 字体本来就用 CMap 自带的 CID→Unicode 表，所以带不带 ToUnicode 结果相同，**这个文件测不出 ToUnicode 是否被读取**（想测要把 ToUnicode 改成与 CMap 不同的映射） | 同上 |
| `zh-cns1-h-noembed.pdf` | 同上 | **完全一致**（繁体） | 同上 |
| `zh-gb1-h-basefont-suffix.pdf` | 同上 | **完全一致** | 同上 |
| `zh-gb1-v-noembed.pdf` | 同上 | 字符和顺序都对（右列先、列内自上而下），但 PyMuPDF 把**每个字符单独输出一行**（竖排文字的默认分行方式），所以逐行比对不一致，去掉空白后一致；pypdf 输出连成一串，也一致 | 竖排正确：3 列从右到左，字符自上而下 |

pypdf 在 strict 模式下也能打开全部 5 个并提取出相同文字。没有发现需要报告的字体可用性问题。

## public 里的下载文件

`public/zh/` 下载文件合计 38.73 MB。

| 路径 | 内容 | 来源 URL | 许可依据 | 页数 | 大小 | 特征 |
|---|---|---|---|---:|---:|---|
| `public/zh/gongwen/gongwen-1954-gazette02-scan.pdf` | 《国务院公报》1954年第2号（含国家统计局关于全国人口调查登记结果的公报） | https://www.gov.cn/gongbao/shuju/1954/gwyb195402.pdf | 著作权法第五条第（一）项：国家机关的决议、决定、命令和其他具有立法、行政、司法性质的文件，不适用该法。Commons 上有字节完全相同的副本（File:State Council Gazette - 1954 - Issue 02.pdf，标 {{PD-PRC-exempt}}，SHA-256 一致） | 27 | 1.21 MB | PDF-1.6、经典 xref；字体：无字体；图像滤镜：CCITTFax；无文字层；生成器：CNKI®ReaderEx(2.0.0 Build 2688)。整页 CCITT G4 黑白图，没有字体，没有文字层；CNKI ReaderEx 2.0 生成 |
| `public/zh/gongwen/gongwen-2024-ai-standards-guide.pdf` | 《国家人工智能产业综合标准化体系建设指南（2024版）》 | https://www.gov.cn/zhengce/zhengceku/202407/P020240702716282797987.pdf | 著作权法第五条第（一）项：国家机关的决议、决定、命令和其他具有立法、行政、司法性质的文件，不适用该法。工业和信息化部、中央网信办、国家发展改革委、国家标准委联合印发（工信部联科〔2024〕113号）通知的附件 | 13 | 0.72 MB | PDF-1.7、经典 xref；字体：Type0/Identity-H，内嵌；图像滤镜：DCT；有文字层；书签 17 条；生成器：WPS Office 专业版。WPS 生成；方正小标宋、仿宋_GB2312、楷体_GB2312、黑体等内嵌 CID 字体 |
| `public/zh/gongwen/gongwen-2024-nicheng-work-report.pdf` | 泥城镇人民政府《政府工作报告》（2024年12月19日，泥城镇第五届人民代表大会第八次会议） | https://www.pudong.gov.cn/zwgk/ghjh-ncz/2025/8/335775/4aafb516715f4db38baa313db46579fe.pdf | 著作权法第五条第（一）项：国家机关的决议、决定、命令和其他具有立法、行政、司法性质的文件，不适用该法（政府工作报告）。浦东新区泥城镇人民政府在 pudong.gov.cn 公开 | 32 | 0.94 MB | PDF-1.6、线性化、xref 流、对象流、3 个 %%EOF（有增量更新）；字体：Type0/Identity-H，内嵌；TrueType/WinAnsiEncoding，内嵌；有文字层；有 AcroForm 字典；生成器：Acrobat PDFMaker 21 Word 版 / Adobe PDF Library 21.5.80。Word 经 Acrobat PDFMaker 转出；线性化；带空 AcroForm 字典 |
| `public/zh/gongwen/gongwen-2025-service-mfg-plan.pdf` | 《深入推动服务型制造创新发展实施方案（2025—2028年）》 | https://www.gov.cn/zhengce/zhengceku/202510/P020251011753216176619.pdf（所在页 https://www.gov.cn/zhengce/zhengceku/202510/content_7043986.htm） | 著作权法第五条第（一）项：国家机关的决议、决定、命令和其他具有立法、行政、司法性质的文件，不适用该法。工业和信息化部等七部门印发通知的附件 | 9 | 0.19 MB | PDF-1.7、经典 xref；字体：Type0/Identity-H，内嵌；有文字层；书签 18 条；生成器：WPS 文字。WPS 文字生成；字体同上 |
| `public/zh/lunwen/lunwen-arxiv-2401.01717-word.pdf` | 《基于事实信息核查的虚假新闻检测综述》（Fact-checking based fake news detection: a review），中文全文 | https://arxiv.org/pdf/2401.01717v1（摘要页 https://arxiv.org/abs/2401.01717） | arXiv 摘要页标 CC BY 4.0（http://creativecommons.org/licenses/by/4.0/）；作者：杨昱洲、周杨铭、应祺超等。许可由投稿人在 arXiv 授予，只覆盖论文本身；论文里引用的第三方图片版权不在此列 | 11 | 0.74 MB | PDF-1.6、线性化、xref 流、对象流、2 个 %%EOF；字体：Type0/Identity-H，内嵌；TrueType/WinAnsiEncoding，内嵌；图像滤镜：Flate；有文字层；书签 7 条；生成器：Acrobat PDFMaker 23 Word 版 / Adobe PDF Library 23.6.156。Word 经 Acrobat PDFMaker 转出；PDF 元数据里的标题是模板残留（与正文无关） |
| `public/zh/lunwen/lunwen-arxiv-2403.14268-word-tc.pdf` | 《以輔助損失函數引導注意力機制之端對端語者自動分段標記技術》（Speech-Aware Neural Diarization …），**繁体中文**，台湾 TAAI 论文 | https://arxiv.org/pdf/2403.14268v1（摘要页 https://arxiv.org/abs/2403.14268） | arXiv 摘要页标 CC BY 4.0；作者：李佩穎、郭浩雲、陳柏琳。许可由投稿人在 arXiv 授予，只覆盖论文本身；论文里引用的第三方图片版权不在此列 | 6 | 0.51 MB | PDF-1.7、xref 流、对象流、2 个 %%EOF（有增量更新）；字体：Type0/Identity-H，内嵌；TrueType/WinAnsiEncoding，**非内嵌**；TrueType/WinAnsiEncoding，内嵌；图像滤镜：Flate；有文字层；生成器：Microsoft® Word 2019 / Microsoft® Word 2019。Word 2019 直接导出 PDF |
| `public/zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf` | 赵华伟等《二次玻色系统非厄米动力学研究进展》（Advances in non-Hermitian dynamics of quadratic bosonic systems），中文全文 | https://arxiv.org/pdf/2601.14329v2（摘要页 https://arxiv.org/abs/2601.14329） | arXiv 摘要页标 CC0 1.0（http://creativecommons.org/publicdomain/zero/1.0/）。许可由投稿人在 arXiv 授予，只覆盖论文本身；论文里引用的第三方图片版权不在此列 | 24 | 3.95 MB | PDF-1.7、经典 xref；字体：Type0/Identity-H，内嵌；Type1，内嵌；图像滤镜：DCT、Flate；有文字层；书签 6 条；生成器：arXiv GenPDF (tex2pdf:57610bf) / pikepdf 8.15.1。LaTeX（ctex + Fandol 字体，xdvipdfmx）经 arXiv GenPDF/pikepdf 重写 |
| `public/zh/ebook/ebook-wikisource-aq-zhengzhuan.pdf` | 鲁迅《阿Q正传》（页面显示为繁体字形） | https://zh.wikisource.org/api/rest_v1/page/pdf/%E9%98%BFQ%E6%AD%A3%E5%82%B3（Wikisource 修订 2633268，2025-12-15 07:44 UTC；2026-10-06 取回） | 鲁迅（1936 年去世），Wikisource 页面标 {{PD-old-80-1996}}，属公有领域；Wikisource 页面文字按 CC BY-SA 4.0 提供（需署名，相同方式共享） | 23 | 2.33 MB | PDF-1.4、经典 xref；字体：Type3（字形内联在 PDF 里）；Type0/Identity-H，内嵌；图像滤镜：Flate；有文字层；生成器：Skia/PDF m154（无头 Chrome）。Wikimedia 的 PDF 导出；Type0 与 Type3 字体混用 |
| `public/zh/ebook/ebook-wikisource-minfadian.pdf` | 《中华人民共和国民法典》（简体） | https://zh.wikisource.org/api/rest_v1/page/pdf/%E4%B8%AD%E5%8D%8E%E4%BA%BA%E6%B0%91%E5%85%B1%E5%92%8C%E5%9B%BD%E6%B0%91%E6%B3%95%E5%85%B8（Wikisource 修订 2590895，2025-08-15 15:08 UTC；2026-10-06 取回） | 法律文本，著作权法第五条第（一）项：国家机关的决议、决定、命令和其他具有立法、行政、司法性质的文件，不适用该法；Wikisource 页面文字按 CC BY-SA 4.0 提供（需署名，相同方式共享） | 128 | 2.81 MB | PDF-1.4、经典 xref；字体：Type3（字形内联在 PDF 里）；图像滤镜：Flate；有文字层；生成器：Skia/PDF m154（无头 Chrome）。Wikimedia 的 PDF 导出（无头 Chrome，Skia/PDF m154）；有内部链接 |
| `public/zh/vertical/vertical-nlc-hechuan-juan70.pdf` | 《民國新修合川縣誌》卷七十（民国十年，1921），竖排古籍的矢量重排版 | https://commons.wikimedia.org/wiki/File:NLC403-311998005256-83304_民國新修合川縣誌_民國10年(1921)_卷七十.pdf；文件地址 https://upload.wikimedia.org/wikipedia/commons/4/4f/NLC403-311998005256-83304_%E6%B0%91%E5%9C%8B%E6%96%B0%E4%BF%AE%E5%90%88%E5%B7%9D%E7%B8%A3%E8%AA%8C_%E6%B0%91%E5%9C%8B10%E5%B9%B4%281921%29_%E5%8D%B7%E4%B8%83%E5%8D%81.pdf | Commons 文件页标 {{PD-scan}} + {{PD-China}}：1921 年出版的《民國新修合川縣誌》，Commons 判定在中国大陆保护期已届满；底本来自中国国家图书馆（NLC）地方志数字化项目 | 71 | 2.36 MB | PDF-1.4、线性化、经典 xref、2 个 %%EOF；字体：Type1/WinAnsiEncoding，**非内嵌**；Type0/Identity-V，内嵌；图像滤镜：Flate；有文字层；**竖排（-V 编码）**；书签 3 条；生成器：iTextSharp 4.1.2 (based on iText 2.1.2u)。**真正的竖排文字层**：内嵌 Type0 字体 FZSongS、/Encoding /Identity-V（WMode 1）、带 ToUnicode；页面内容放在 Form XObject 里，外面套旋转矩阵；表格线是矢量路径 |
| `local/zh/large/large-yanzheng-cidian-1229p.pdf` | 《鹽政辭典》（1928），竖排辞典扫描，**1229 页** | https://commons.wikimedia.org/wiki/File:鹽政辭典.pdf；文件地址 https://upload.wikimedia.org/wikipedia/commons/a/a5/%E9%B9%BD%E6%94%BF%E8%BE%AD%E5%85%B8.pdf | Commons 文件页标 {{PD-scan|PD-China-1996}}：林振翰著，1928 年出版（在美国也已因超过 95 年而公有）；保护期届满的判断由 Commons 作出 | 1229 | 23.00 MB | PDF-1.5、经典 xref；字体：无字体；图像滤镜：JBIG2、JPX；无文字层；书签 1214 条；生成器：Pdg2Pic / FreePic2Pdf_Lib - v3.16。整页 JBIG2（1 位）图，封面 JPX；FreePic2Pdf / Pdg2Pic（超星/读秀 PDG 格式转出）；有 1214 条书签；没有文字层 |

## local 里的文件

`local/zh/` 被 `.gitignore` 忽略，只在本机。放这里有两种原因，表里“许可依据”一栏写明了是哪一种：**授权存疑**（出版社版式权利、文件类型不在第五条之内、现代整理本），或者**授权没有疑点，只是 public 的 40 MB 预算用完**。

| 路径 | 内容 | 来源 URL | 许可依据 | 页数 | 大小 | 特征 |
|---|---|---|---|---:|---:|---|
| `local/zh/gongwen/gongwen-2019-guoban-design-scan.pdf` | 附件《政府信息公开栏目页面设计参考方案》，4 页 | https://www.gov.cn/zhengce/content/2019-12/03/5457588/files/f78997ecef234e928a16bd438552846e.pdf（所在页 https://www.gov.cn/zhengce/content/2019-12/03/content_5457588.htm） | 国务院办公厅政府信息与政务公开办公室通知的附件，但内容是“中国政府网运行中心制”的页面设计示意图，不是文件文字，且图里有国徽 → 放 local | 4 | 0.65 MB | PDF-1.6、线性化、xref 流、对象流、2 个 %%EOF；字体：无字体；图像滤镜：DCT；无文字层；书签 4 条；生成器：Adobe Acrobat 19.21 / Adobe Acrobat 19.21 Image Conversion Plug-in。整页 JPEG（DCT RGB，1420×2265），Acrobat Image Conversion 生成；线性化 + xref 流 + 对象流；2 个 %%EOF；4 条书签 |
| `local/zh/gongwen/gongwen-guoban-mulu-2015.pdf` | 《国务院办公厅 政府信息公开目录 二〇一五年》 | https://www.gov.cn/zhengce/pdfFile/2015_PDF.pdf | 国务院办公厅发布的“政府信息公开目录”，是文件清单而非第五条列举的文件类型，依据不够明确 → 放 local | 67 | 0.32 MB | PDF-1.4、经典 xref；字体：Type0/Identity-H，内嵌；图像滤镜：DCT；有文字层；书签 1 条；生成器：iText® 5.5.8 ©2000-2015 iText Group NV (AGPL-version)。iText 5.5.8 生成；SimSun 内嵌 Type0；长表格 |
| `local/zh/gongwen/gongwen-wuhan-gazette-2024-05.pdf` | 《武汉市人民政府公报》2024年第5、6期（总第738期），130 页 | https://www.wuhan.gov.cn/zwgk/xxgk/zfgb/202404/P020240425384835203138.pdf | 收录政府工作报告和市政府文件，内容属第五条所指文件；但整期公报的编排版式是否另有权利没有核实，且 6.3 MB 占预算 → 放 local | 130 | 6.28 MB | PDF-1.4、线性化、经典 xref、3 个 %%EOF（有增量更新）；字体：Type0/Identity-H，内嵌；Type1，内嵌；图像滤镜：CCITTFax、DCT；有文字层；生成器：元数据是占位字符串“创建者/生产者”。线性化，3 个 %%EOF；方正字体 Type0；CCITT G4 与 DCT CMYK 图；没有书签 |
| `local/zh/lunwen/lunwen-arxiv-2410.20383-cnki-ttkn.pdf` | 《基于全局融合的多核概念分解算法》（Multiple kernel concept factorization …），中文全文 | https://arxiv.org/pdf/2410.20383v1（摘要页 https://arxiv.org/abs/2410.20383） | arXiv 摘要页标 CC BY 4.0，作者：李菲、杜亮、任超红。但 PDF 是 CNKI/TTKN 出版系统排的版（文末附 CNKI 的 <FileProperty> XML），出版社可能握有版式权利，授权存疑 → 放 local | 6 | 0.20 MB | PDF-1.6、经典 xref；字体：Type1，内嵌；Type0/GBK-EUC-H，**非内嵌**；有文字层；生成器：ReaderEx_DIS 2.5.0 Build 4087 / TTKN。**结构怪癖很多**（见“值得注意的结构特征”）：CR 换行、`stream` 后缺 LF、%%EOF 后拖尾 XML、页字典重复 /MediaBox；字体用预定义 CMap GBK-EUC-H |
| `local/zh/lunwen/lunwen-arxiv-2411.00268-founder-fly.pdf` | 《基于高阶一致性学习的聚类集成算法》（Clustering ensemble algorithm with high-order consistency learning），中文全文 | https://arxiv.org/pdf/2411.00268v1（摘要页 https://arxiv.org/abs/2411.00268） | arXiv 摘要页标 CC BY 4.0，作者：甘建文、陈燕、周鹏。但 PDF 是方正飞翔排版的期刊版，出版社版式权利存疑 → 放 local | 8 | 2.12 MB | PDF-1.4、经典 xref；字体：Type0/Identity-H，内嵌；TrueType/WinAnsiEncoding，内嵌；图像滤镜：DCT；有文字层；生成器：方正飞翔8.2 XML版 8.2.0.1731 / Founder。方正飞翔 8.2；字体描述符里有重复键 /Ascent（pypdf strict 报错） |
| `local/zh/lunwen/lunwen-arxiv-2503.12308-wps.pdf` | 《AI 驱动的 6G 空口：技术应用场景与均衡设计》（AI-driven 6G Air Interface …），中文全文 | https://arxiv.org/pdf/2503.12308v1（摘要页 https://arxiv.org/abs/2503.12308） | arXiv 摘要页标 CC BY 4.0，作者：王晓云、韩双锋、刘志明等。授权没有疑点，只是 public 40 MB 预算用完 → 放 local | 19 | 1.45 MB | PDF-1.7、经典 xref；字体：Type0/Identity-H，内嵌；图像滤镜：DCT；有文字层；书签 9 条；生成器：WPS 文字。WPS 文字；Type0 Identity-H；9 条书签 |
| `local/zh/ebook/ebook-wikisource-yijikao-468p.pdf` | 丹波元胤《中国医籍考》（单页导出，**468 页**，最长的文字层文件） | https://zh.wikisource.org/api/rest_v1/page/pdf/%E4%B8%AD%E5%9B%BD%E5%8C%BB%E7%B1%8D%E8%80%83（Wikisource 修订 2490687，2024-11-13 02:02 UTC；2026-10-06 取回） | 丹波元胤（1827 年去世）原著，属公有领域；Wikisource 页面文字按 CC BY-SA 4.0 提供（需署名，相同方式共享）。授权没有疑点，放 local 只因为 8.2 MB 占 public 预算 | 468 | 8.16 MB | PDF-1.4、经典 xref；字体：Type3（字形内联在 PDF 里）；Type0/Identity-H，内嵌；图像滤镜：Flate；有文字层；生成器：Skia/PDF m154（无头 Chrome）。Wikimedia 的 PDF 导出；Type0 + Type3 字体 |
| `local/zh/scanned/scan-nlc-hechuan-juan49-jpx.pdf` | 《民國新修合川縣誌》卷四十九，竖排扫描，24 页 | https://commons.wikimedia.org/wiki/File:NLC403-311998005256-45009_民國新修合川縣誌_民國10年(1921)_卷四十九.pdf | 同 public 里的合川縣誌卷七十（{{PD-scan}} + {{PD-China}}）。授权没有疑点，放 local 只因为 6.0 MB 占 public 预算 | 24 | 6.00 MB | PDF-1.7、xref 流、对象流；字体：无字体；图像滤镜：JPX；无文字层；生成器：Foxit GSDK - Foxit Software Inc.。整页 JPX（JPEG 2000）；Foxit GSDK；xref 流 + 对象流；没有文字层 |
| `local/zh/scanned/scan-nlc-jiankang-207p.pdf` | 《健康小辭典》（又名《醫藥小辭典》，民国），竖排扫描，207 页 | https://commons.wikimedia.org/wiki/File:NLC416-08jh014038-37806_健康小辭典,又名,醫藥小辭典.pdf；文件地址 https://upload.wikimedia.org/wikipedia/commons/b/b2/NLC416-08jh014038-37806_%E5%81%A5%E5%BA%B7%E5%B0%8F%E8%BE%AD%E5%85%B8%2C%E5%8F%88%E5%90%8D%2C%E9%86%AB%E8%97%A5%E5%B0%8F%E8%BE%AD%E5%85%B8.pdf | Commons 标 {{PD-scan}} + {{PD-China}}；中国国家图书馆民国图书（志平编）。授权没有疑点，放 local 只因为 5.2 MB 占 public 预算 | 207 | 5.22 MB | PDF-1.7、xref 流、对象流；字体：无字体；图像滤镜：JBIG2；无文字层；书签 880 条；生成器：Foxit GSDK - Foxit Software Inc.。整页 JBIG2（1 位灰度）；Foxit GSDK；xref 流 + 对象流；880 条书签；没有文字层 |
| `local/zh/vertical/vertical-nlc-hechuan-juan22.pdf` | 《民國新修合川縣誌》卷二十二，竖排矢量重排版，55 页 | https://commons.wikimedia.org/wiki/File:NLC403-311998005256-106324_民國新修合川縣誌_民國10年(1921)_卷二十二.pdf | 同 public 里的合川縣誌卷七十。授权没有疑点，放 local 只因为 public 预算用完 | 55 | 1.30 MB | PDF-1.4、线性化、经典 xref、2 个 %%EOF；字体：Type1/WinAnsiEncoding，**非内嵌**；Type0/Identity-V，内嵌；图像滤镜：Flate；有文字层；**竖排（-V 编码）**；书签 3 条；生成器：iTextSharp 4.1.2 (based on iText 2.1.2u)。与卷七十同一结构（Identity-V 内嵌字体、线性化） |
| `local/zh/large/large-yilei-yuanrong-1073p.pdf` | 王好古《醫壘元戎》（2000 年整理本扫描），**1073 页** | https://commons.wikimedia.org/wiki/File:SSID-10858378_醫壘元戎.pdf | Commons 标 {{PD-scan|PD-old}}（元代王好古原著），但扫描的是上海科学技术出版社 2000 年的整理本，现代整理/标点可能仍受保护 → 放 local | 1073 | 25.81 MB | PDF-1.5、经典 xref；字体：Type0/GBK-EUC-H，**非内嵌**；图像滤镜：CCITTFax、DCT；有文字层；书签 1060 条；生成器：Pdg2Pic / FreePic2Pdf_Lib - v3.09。整页 CCITT G4 + 封面 DCT；FreePic2Pdf v3.09；1060 条书签；极少量文字用非内嵌 Type0（STSong-Light + 预定义 CMap GBK-EUC-H） |

## 许可证疑点

1. **arXiv 的 CC BY 由投稿人声明**。arXiv 只保证摘要页上显示的许可是投稿人选的，不保证投稿人有权这样选。`public/` 里的 3 篇论文都是作者自己用 LaTeX/Word 做的 PDF，风险小；两篇出版社排版的（CNKI/TTKN、方正飞翔）放了 local。
2. **CC BY / CC BY-SA 要署名**。public 里 3 篇论文的作者、2 个 Wikisource 导出（CC BY-SA 4.0，页面文字的贡献者是维基文库志愿者）都需要在 `public/LICENSES/` 或 `NOTICE` 里补署名；我没有碰那个目录（不在我的写入范围内）。Wikisource 的 CC BY-SA 是否传染到整个 PDF 文件有争议；如果项目不想承担，就把这 2 个电子书挪到 local。
3. **Commons 的“公有领域”是 Commons 的判断**。民国书籍（1921、1928 年出版）保护期是否届满取决于作者卒年，Commons 的 {{PD-China}} / {{PD-China-1996}} 模板只给出一般性论证。两个文件都超过 95 年，在美国一定是公有领域；在中国大陆，作者需在 1976 年之前去世。已经按任务要求核对过文件页写着公有领域，但这个依据比 gov.cn / 第五条弱。
4. **公文的第五条依据**：我把部委“实施方案”“指南”这类通知附件当作“具有行政性质的文件”。这是常见解读，但不是法律明文列出的类型（明文是法律、法规、决议、决定、命令）。《国务院公报》1954 年第 2 号和政府工作报告属于最清楚的一类。
5. **《政府信息公开目录》、武汉市政府公报、含国徽的设计稿**：见 local 表，都放了 local。
6. **《醫壘元戎》**：Commons 标 PD-old，但扫描的是 2000 年的整理本，放 local。同一个来源（`SSID-` 开头的文件，来自读秀/超星的扫描）里的现代出版物都要小心，我只用了确认是民国以前原刻本或 NLC 民国书的。

## 值得注意的结构特征

给第 1 层测试规划用。只列对解析/写回有影响的：

- **`lunwen-arxiv-2410.20383-cnki-ttkn.pdf`（local）是个“真实世界畸形文件”**：用 `\r`（单个 CR）当行结束符；部分流的 `stream` 关键字后面只有 `\r` 没有 `\n`（违反 7.3.8.1，MuPDF 报 “line feed missing after stream begin marker”）；`%%EOF` 之后还拖着一段 `<FileProperty>…</FileProperty>` XML；页字典里 `/MediaBox` 重复定义（pypdf strict 报 “Multiple definitions in dictionary”）；文件头第二行是二进制注释（以 CR 结尾）。MuPDF 能正常打开且不触发重建。非常适合测“宽松解析”。
- **`lunwen-arxiv-2411.00268-founder-fly.pdf`（local）**：字体描述符里 `/Ascent` 重复（pypdf strict 报错）。
- **线性化文件**：`vertical-nlc-hechuan-juan70.pdf`、`gongwen-2024-nicheng-work-report.pdf`、`lunwen-arxiv-2401.01717-word.pdf`、`gongwen-wuhan-gazette-2024-05.pdf` 等。第一段 xref 从高编号开始（如 574 起 18 项）、主 xref 从 0 开始，pypdf strict 会打印 “Xref table not zero-indexed”，这是线性化的正常样子，不是损坏。
- **增量更新**：`gongwen-2024-nicheng-work-report.pdf`（线性化后又有 1 次更新，共 3 个 `%%EOF`）、`gongwen-wuhan-gazette-2024-05.pdf`（3 个）、`lunwen-arxiv-2403.14268-word-tc.pdf`（xref 流 + 2 个 `%%EOF`，非线性化）。
- **xref 流 + 对象流**：`lunwen-arxiv-2403.14268-word-tc.pdf`、`lunwen-arxiv-2401.01717-word.pdf`、`gongwen-2024-nicheng-work-report.pdf`、local 里的 Foxit 扫描（`scan-nlc-*`）等。
- **巨大的书签树**（合并时会被丢掉，要提示用户）：`large-yanzheng-cidian-1229p.pdf` 1214 条、`scan-nlc-jiankang-207p.pdf` 880 条、`large-yilei-yuanrong-1073p.pdf` 1060 条。
- **竖排**：`vertical-nlc-hechuan-juan70.pdf` 的字体是 `/Encoding /Identity-V`（预定义的竖排 Identity CMap，WMode 1），内嵌 CID 字体，带 ToUnicode；页面内容放在被旋转矩阵包着的 Form XObject 里。这是第 2 层提取文字、第 3 层渲染的关键用例。
- **预定义 CMap + 非内嵌 CID 字体**：手写的 5 个文件；`large-yilei-yuanrong-1073p.pdf` 里少量文字用 `STSong-Light` + `GBK-EUC-H`；`lunwen-arxiv-2410.20383-cnki-ttkn.pdf` 用 `GBK-EUC-H`（字体内嵌）。
- **Type3 字体里放中文字形**：Wikisource 导出的几个文件（Chrome/Skia 对没法子集化的后备字体用 Type3）。文字提取要靠 ToUnicode 或 ActualText；MuPDF 对 `minfadian` 报 “ActualText with no position”。
- **图像滤镜**：CCITT G4（1954 公报、醫壘元戎、武汉公报）、JBIG2（鹽政辭典、健康小辭典）、JPX（合川縣誌卷四十九、鹽政辭典封面）、DCT（含 CMYK）、Flate；图片型文件的页面尺寸从几百点到几千像素都有。
- **空的 AcroForm**：`gongwen-2024-nicheng-work-report.pdf` 带 `/AcroForm` 字典，但 `/Fields []` 是空的，只有默认资源（Helv、ZaDb）和 `/DA`，是 Acrobat 的惯常残留。合并时它是个“有表单字典但没表单域”的边界情形。
- **Wikisource 导出不是稳定字节**：每次请求生成的 PDF 可能不同（时间戳、对象顺序），只有仓库里这份是固定的；需要重新取回时请接受差异。

## 缺口

- **没有 ≥ 1000 页的文字层 PDF**。≥ 1000 页的只有扫描件（鹽政辭典 1229 页、醫壘元戎 1073 页）；文字层最长的是 468 页（Wikisource 单页导出）。Wikisource 里最长的单个页面约 1.9 MB 源码，导出都在 400–500 页量级；要凑到 1000 页得合并多个页面，“只下载”做不到。要测“1000 页文字层”的性能，建议用自己的合并功能把几个 Wikisource 导出拼起来，或在第 1 层之外另想办法。
- **没有全国政府工作报告的单独 PDF**。gov.cn 上的国务院公报 PDF 很大（2024 年的单期 21–181 MB，超过 30 MB 上限），而全国政府工作报告只在公报里；找到的政府工作报告是镇级的（泥城镇）。
- **没有收 Foxit OFDToPDF 生成的图片型公文**：gov.cn 上这类文件（例如《林业草原生态保护恢复资金管理办法》附件，33.9 MB）超过 30 MB 上限，只下载过一次后删除，未入库。
- **没有带书签/表单/附件的“典型”政府表单**：书签有（见上），AcroForm 只有一个空的，没找到带可填写表单域或嵌入附件的中文 PDF。
- **没有加密的中文 PDF**（本组也不要求）。
- **没有真正的“文字层竖排 + 扫描底图”的 OCR 文件**（隐形文字叠在竖排扫描图上）。鹽政辭典等扫描件没有文字层；合川縣誌卷七十是矢量重排版，不是 OCR。
- **论文 LaTeX 只有一篇**。2601.14329 是 arXiv 重新生成的 PDF；没有找到原始 `pdflatex + CJK` 路线的中文论文（那种用 Type1/Type3 位图字体）。
- gov.cn 上的 `.ofd` 文件按规则（只收 `.pdf`）没有下载。
- 可再取的素材：`https://www.gov.cn/gongbao/shuju/<年>/gwyb<年><期两位>.pdf`（《国务院公报》1954–1999 年，多数年份存在，每期 0.6–2.4 MB，CCITT G4 图片型，CNKI ReaderEx 生成）。同一来源、同一结构，我只收了 1954 年第 2 号一个，其余没必要。

## 取回方式与校验

- gov.cn、pudong.gov.cn、wuhan.gov.cn：`curl -L` 直接取 PDF（浏览器 User-Agent），2026-10-06。
- arXiv：`https://arxiv.org/pdf/<编号><版本>`，取回后核对过与摘要页最新版本一致；字节与不带版本号的 URL 一致。
- Wikimedia Commons：先用 `action=query&prop=imageinfo` 取 `upload.wikimedia.org` 的文件地址，再下载；许可模板用 `action=parse` 核对。
- Wikisource：`https://zh.wikisource.org/api/rest_v1/page/pdf/<标题>`，同时记了该页当时的修订号。
- 文件逐字节保存，`tests/corpus/.gitattributes` 已把 `*.pdf` 设为 binary。

| 路径 | SHA-256 |
|---|---|
| `local/zh/ebook/ebook-wikisource-yijikao-468p.pdf` | `3d95cb164ebf5d3b187f12bd19d3a932eb558af62ceca2087c95df2736505386` |
| `local/zh/gongwen/gongwen-2019-guoban-design-scan.pdf` | `c7b3033d24aeddf334a7c3623c6a37902a5152151beacd442a22a4c8073a697f` |
| `local/zh/gongwen/gongwen-guoban-mulu-2015.pdf` | `f863d40990d9582cdd7c251a00a50837c68da5219991dd726a41505ce844915e` |
| `local/zh/gongwen/gongwen-wuhan-gazette-2024-05.pdf` | `029cbd73551224d5e2e4dacf5abfe7445b0a967f79f0a02026c5d5fe009cbefc` |
| `local/zh/large/large-yilei-yuanrong-1073p.pdf` | `8ad6873be6aa6928c016d6d56ec42791af6c34869bf342d23912bd257df996fc` |
| `local/zh/lunwen/lunwen-arxiv-2410.20383-cnki-ttkn.pdf` | `b38dc737d7dd4eccd3315aea312b3c9cef7f188d15ca31e7086985ad927210dc` |
| `local/zh/lunwen/lunwen-arxiv-2411.00268-founder-fly.pdf` | `a5fb7fff2f2e1997f038ee8e0fa5e9b1152ec6650a565f77be41b38823c0ba79` |
| `local/zh/lunwen/lunwen-arxiv-2503.12308-wps.pdf` | `e8e7f0f42dfad98991ef73a701079e422db63b5be4a0736c79a4b8359a0878af` |
| `local/zh/scanned/scan-nlc-hechuan-juan49-jpx.pdf` | `a3f8e8ec9a4207f5390d9be243b4c5629ea08c7c19899dd383be598ddd578ae8` |
| `local/zh/scanned/scan-nlc-jiankang-207p.pdf` | `28267b51afcd0aebb2ca801094fdb5151ee080bc28da7c5b4dbd262cfc277167` |
| `local/zh/vertical/vertical-nlc-hechuan-juan22.pdf` | `dedf91e4eb126f693be1583c9e6ec5a23327a1b09f159e68f2c370cdccd24cdd` |
| `public/zh/ebook/ebook-wikisource-aq-zhengzhuan.pdf` | `60618c86461396e925701cb377acbb69c93792d4a03b413f78ddd23313d50039` |
| `public/zh/ebook/ebook-wikisource-minfadian.pdf` | `92af04bdd6d1be9b46fc76588cc1d3649c1f429598c19bec919b542d7ab543cb` |
| `public/zh/gongwen/gongwen-1954-gazette02-scan.pdf` | `f9045f7fa0109a509c0f9ba296d46aed3f9cc62401331eb8f3fb522f7e305f8a` |
| `public/zh/gongwen/gongwen-2024-ai-standards-guide.pdf` | `1b706578655b5888e694cca58495d9b83df09101d6c3eff2a8560479af5316a6` |
| `public/zh/gongwen/gongwen-2024-nicheng-work-report.pdf` | `190a41307f3b653cb857e4c161f30bad775461bbf37e958862c83f81750e715f` |
| `public/zh/gongwen/gongwen-2025-service-mfg-plan.pdf` | `a57a672517f2fdf1bbf28073fc7f269ce1de1fe8e0eb18717103c3a8e4c7b4a2` |
| `public/zh/handmade/zh-cns1-h-noembed.pdf` | `02391af2b627c8747f7633c06fa902d1d2d64e7da7e11daa7d68d931a12d28ba` |
| `public/zh/handmade/zh-gb1-h-basefont-suffix.pdf` | `48a0c659a5a8c48d74907412cf7900e4fef35a33c8ffd39dbdf3dca7fc7a5061` |
| `public/zh/handmade/zh-gb1-h-noembed.pdf` | `8fcc49ae2c32f25cf1f6128fb43e6a78696209efe3b558168a7c3d3a7599094a` |
| `public/zh/handmade/zh-gb1-h-tounicode.pdf` | `997aa4e005cea5394aa03a6a93c64e81149cd92b0d2532c1d7ea57e0a84e349c` |
| `public/zh/handmade/zh-gb1-v-noembed.pdf` | `04bcba2cb4e5f6b0a4e8574d25535a69dcf63b8f9a2c85a889bfd1d9efd41afe` |
| `local/zh/large/large-yanzheng-cidian-1229p.pdf` | `03a88312216be8e9c65a94d84e6f935e41b8d25251d5d3941e82262ca47f7342` |
| `public/zh/lunwen/lunwen-arxiv-2401.01717-word.pdf` | `ccae330b09df2ba2e539d214d6e3025d4074aa040c214388bc427afc0e934cc8` |
| `public/zh/lunwen/lunwen-arxiv-2403.14268-word-tc.pdf` | `7b5a3ee909c419ed50164bb54314d02e221deee40364ad95a7e478a18260daa5` |
| `public/zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf` | `16a691ab757beb1770d6a24b849b1bda3554e778cc6d0f01c99443ab737f61f1` |
| `public/zh/vertical/vertical-nlc-hechuan-juan70.pdf` | `509837eeef977c8a5bbeb7875fc1bac8bdd5123239e5ed3a573df4f340b751f4` |

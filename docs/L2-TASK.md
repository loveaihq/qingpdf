# 第 2 层：提取文字

做完后，`qingpdf text` 能把 PDF 里的文字按阅读顺序提取出来，**中文要对**：嵌入和没嵌入字体的都要对，有没有字符映射表（ToUnicode）都要对，竖排也要对。这一层不画页面。

## 规范

ISO 32000-1：7.8（内容流和资源）、8.2–8.4（图形状态、坐标变换）、9.2–9.4（文字对象、文字状态、文字定位）、9.5–9.8（字体：简单字体、复合字体、CMap）、9.10（提取文字内容：ToUnicode、9.10.2 的查找顺序）、附录 D（字符集和编码）。代码里注明章节号。

## 交付

### 1. 内容流解释（只到文字需要的程度）

- 解析内容流的操作符和操作数；内嵌图片（`BI … ID … EI`）要正确跳过。
- 图形状态：`q`/`Q`、`cm`；文字状态：`Tc Tw Tz TL Tf Tr Ts`；文字定位和输出：`BT ET Td TD Tm T* Tj TJ ' "`。
- 表单 XObject（`Do`）里的文字也要取：嵌套最多 32 层，环要检测出来。
- 隐藏文字（`Tr 3`，扫描件 OCR 层就是这种）照样提取。
- 每页设上限：操作符个数、输出字数；超了返回明确的错误，不卡死。

### 2. 字体和编码

- 简单字体：Type1（含标准 14 种）、TrueType、Type3。`/Encoding`：StandardEncoding、WinAnsiEncoding、MacRomanEncoding、`/Differences`；字形名转 Unicode 用 Adobe Glyph List（AGL，BSD-3），以及 `uniXXXX`、`uXXXX[XX]` 这类名字。
- 复合字体（Type0 + CIDFontType0/2）：`Identity-H/V`；预定义 CMap（`UniGB-UCS2-H`、`UniGB-UTF16-H`、`GBK-EUC-H`、`GB-EUC-H`、`B5pc-H`、`ETen-B5-H`、`UniCNS-UCS2-H` 等，以及对应的 `-V`）；文件里嵌入的 CMap（`/UseCMap` 要支持）。
- CID 转 Unicode：先看 ToUnicode；没有时，按字体的 `/CIDSystemInfo` 用 Adobe 的 `Adobe-GB1`、`Adobe-CNS1`、`Adobe-Japan1`、`Adobe-Korea1` 对照表。
- 查找顺序按 9.10.2。ToUnicode 和预定义 CMap 冲突时以 ToUnicode 为准。
- 数据来源：Adobe 的 `cmap-resources`（BSD-3，已在 SOURCES 里登记过）和 `agl-aglfn`（BSD-3）。只挑实际用得上的表，**压缩后嵌进程序**（用已有的 miniz_oxide），不带外部文件。挑了哪些、为什么，记进 `docs/decisions.md`。

### 3. 排版还原

- 按基线把字分成行，行内按位置排序；字间距大到一定程度插入空格（拉丁文用），阈值记进 `decisions.md`。
- **竖排**（WMode 1）：字从上往下，列从右往左；输出时一列一行。
- 一行里横竖混排、旋转的文字（`Tm` 带旋转）也要排进正确的位置。
- 伪粗体（同一个字在几乎同一位置重复画几次）去重。
- 连字 U+FB00–FB06（ﬁ、ﬂ 等）拆成普通字母。
- 页与页之间用换页符 `\f` 分开（和 pdftotext 一样）。

### 4. 命令

```
qingpdf text a.pdf                    打印到屏幕
qingpdf text a.pdf -o a.txt           写到文件（UTF-8）
qingpdf text a.pdf --pages 3-5        只取几页
```

加密文件照第 1.5 层：`--password`。作者禁止了"复制或提取内容"的（权限第 5 位），用户密码打开时拒绝提取，提示需要所有者密码。

## 测试

- **手写文件的标准答案**：`tests/corpus/public/zh/handmade/` 里 5 个文件各有 `.expected.txt`，提取结果必须一字不差。另外手写一个 ToUnicode 和预定义 CMap **故意不一致**的文件，证明 ToUnicode 优先（上次收集测试文件时发现现有那个测不出来）。
- **和别人对照**：每个测试文件都和 pdftotext（Git 自带的那个）、PyMuPDF 的结果比，按字符算相似度；中文文件以 PyMuPDF 为主。差得多的逐个看，要么是我们的错，要么说清楚为什么我们对。
- **中文组**：公文、论文、电子书（Type3 字体的维基文库导出）、竖排县志、繁体论文。
- **坏文件**：内容流截断、操作数类型乱、字体字典缺项、CMap 写坏、表单 XObject 互相引用成环——都要明确出错或跳过，不崩、不卡。

## 验收

1. `cargo test --release` 全部通过，`cargo clippy` 没有警告。
2. 手写文件的标准答案全部一致。
3. 和 PyMuPDF 对照：中文组每个文件的字符相似度不低于 98%，达不到的说明原因。
4. **快和小**，在你这台电脑上量：
   - 468 页的中文电子书全书提取 ≤ 1 秒；
   - 内存 ≤ 200 MB；
   - `qingpdf.exe` 这一层增加 ≤ 1 MB（大部分是压缩后的中文对照表）。
5. 一次简短的审查通过（按省用量的规矩：只查清单上的项目）。

## 不做

- 渲染（第 3 层）。
- 保留版面的输出（把空白也排成原来的样子）、表格识别、按段落合并：以后再说。
- 扫描件识别文字（第 5 层）：没有文字层的扫描件，提取结果是空的。

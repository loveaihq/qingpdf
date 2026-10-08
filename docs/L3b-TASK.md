# 第 3 层（渲染）任务单 3b：字体

3a 已完成：图形、图片、Type3 字体都能画，普通字体画成占位框。3b 把占位框换成真字形。

## 规范

ISO 32000-1：9.2–9.4（字体、文本状态、文本对象和渲染模式 `Tr` 0–7）、9.6（简单字体，含 9.6.6 编码和 9.6.6.4 TrueType 字形选择）、9.7（复合字体，含 `CIDToGIDMap`、`W`/`W2`、竖排）、9.8（字体描述符，`/Flags`）、9.9（嵌入字体程序）。字体格式本身参考 Adobe Type 1 Font Format、CFF（Adobe TN 5176）、Type 2 Charstring（TN 5177）、OpenType/TrueType 规范（`glyf`/`loca`/`cmap`/`post`/`hmtx`）。代码注明章节号。

## 交付

1. **字形读取自己写，不加依赖**。`text/fontprog.rs` 已经能解析 Type1、CFF、sfnt 的外层结构，在它基础上加：
   - TrueType `glyf`/`loca`（简单字形和组合字形，二次曲线）；
   - CFF Type 2 charstring（含 subrs、`seac` 式重音、flex、hintmask 跳过），CID-keyed CFF（FDArray/FDSelect、charset CID→GID）；
   - Type1 charstring（eexec 和 charstring 解密、`lenIV`、Subrs、OtherSubrs 0–3 的 flex/换 hint、`seac`），兼容嵌入 PFB 段头；
   - OpenType 包装的 CFF（`FontFile3 /OpenType`）、TTC 字体集合。
   不做 hinting（和 pdf.js、Chrome 高分辨率时一样，直接抗锯齿）。
2. **字形选择**按规范：简单字体的编码和 `/Differences`、字形名到 charstring；TrueType 按 9.6.6.4 选 cmap（(3,0)/(1,0)/(3,1)，symbolic 标志、`post` 字形名）；CIDFontType0/2、`CIDToGIDMap`（Identity 和流）。前进宽度以 PDF 的 `/Widths`、`W` 为准，不用字体自己的。
3. **没嵌字体的回退**：
   - 标准 14 字体和常见西文字体，用系统里对应的字体（Arial/Times New Roman/Courier New/Symbol 等），按 `/Flags`（衬线、等宽、粗、斜）挑；
   - 中日韩字体：CID→Unicode 用第 2 层已有的表，再查系统字体的 cmap。按字体名和 `/Ordering` 挑：GB1 用宋体/黑体/楷体/仿宋/微软雅黑，CNS1 用细明体或微软正黑，Japan1 用 MS Mincho/Gothic 或 Yu 系列，Korea1 用 Malgun Gothic/Batang；
   - Windows 从 `%WINDIR%\Fonts` 找；macOS、Linux 扫常见目录，尽力而为；
   - 系统里找不到合适字体时，仍画占位框并警告。
   系统字体文件按需读、整个进程只读一次，不要在启动时扫描全部字体。
4. **渲染模式** `Tr` 0–7 全部支持（4–7 把字形加入裁剪）。竖排位置按 `W2`/`DW2`；竖排专用字形替换（GSUB `vert`）不做，记在 decisions.md。
5. **字形缓存**：轮廓按（字体，字形）缓存；小字号可以再缓存位图（按字号和子像素位置），要有内存上限。
6. **限制**：字体程序大小、每个字形的 charstring 操作数、subr 嵌套深度（Type2 规范上限 10）、操作数栈、组合字形深度、轮廓点数都设上限。坏字体只影响那个字体（退回系统字体或占位框并警告），不崩、不卡。

## 验收

1. `cargo test --release` 全部通过，`cargo clippy` 无警告；坏字体的回归测试在代码里生成。
2. **和 PDFium 对照**：`render_compare.py --engine-compare` 现在包括文字页。全部页面平均像素差要低于阈值；差得多的逐个说明原因。统计整个测试库里还有多少字形画成占位框，逐类说明。
3. **中文公文**：语料库里的中文文件（含没嵌字体的宋体公文）逐页看过，字形正确、位置不偏。
4. **快和小**（在这台电脑上量）：
   - 一般文字页 150 dpi 渲染一页 ≤ 50 毫秒（含第一次读系统字体）；
   - 468 页那本书前 100 页的平均每页时间写进报告；
   - 内存 ≤ 200 MB；
   - `qingpdf.exe` 增加 ≤ 0.6 MB。
5. 一次简短审查通过。

## 不做

透明、渐变、图案、JPX/JBIG2（3c）；界面（3d）；hinting；GSUB 竖排替换；批注的外观流。

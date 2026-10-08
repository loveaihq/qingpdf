# 第 3 层任务单 3b2：要运行指令才能画对的字体

3b 完成后发现：台湾常用的一些 TrueType 字体（标楷体 DFKai-SB / DFKaiShu-SB-Estd-BF、细明体 MingLiU / PMingLiU、DFMing、DFHei、华康系列等）把笔画拆成零件，靠字体里的 TrueType 指令把零件摆到位置上。不运行指令，笔画就缺、错位。FreeType 把这类字体叫 "tricky"，对它们强制运行指令。3b2 只解决这件事。

## 规范

Apple / Microsoft TrueType 规范的指令部分（instruction set、graphics state、`fpgm`/`prep`/`cvt `、`maxp` 里的上限），OpenType 规范 `glyf`。代码注明出处。

## 交付

1. **TrueType 指令解释器，自己写**，放在 `render/` 下。支持 `fpgm`、`prep`、字形指令；26.6 定点数；图形状态、函数定义和调用、CVT、存储区、twilight 区、组合字形逐个零件运行再合并。
2. **只对 tricky 字体运行**，其他字体照旧不 hinting。识别方法：
   - 字体名（去掉子集前缀 `ABCDEF+`）、`name` 表里的家族名，和 FreeType 的 tricky 名单比对；
   - PDF 里的子集字体常常改了名或没有 `name` 表，所以再按 `cvt `/`fpgm`/`prep` 表的校验和比对（FreeType 公开的那份数字可以照用，代码自己写）。
3. **在固定的大字号上运行一次**（比如每 em 2048 像素，取整误差可以忽略），得到摆好位置的轮廓，按普通轮廓缓存和缩放。不要为每个字号重新运行。
4. **回退路径也要检查**：3b 里 CNS1 没嵌字体时用系统的细明体。看系统里的 MingLiU 需不需要运行指令，需要就同样处理。
5. **限制**：每个字形的指令条数、调用深度、循环次数（`LOOPCALL`、向后跳转）、栈大小、存储区和 twilight 点数、函数定义个数都设上限。超了就退回不运行指令的轮廓并警告，不崩、不卡。坏字体的回归测试在代码里生成。

## 验收

1. `cargo test --release` 全部通过，`cargo clippy` 无警告。
2. 语料库里用到这些字体的页面（先搜出来，至少包括 `lunwen-arxiv-2403`）和 PDFium 对照，字形完整、位置正确，平均像素差和普通文字页相当。其他页面的对照结果不能变差（`render_compare.py --engine-compare` 全量跑一次，和 3b 的 2.55 比）。
3. 速度：用这些字体的页面 150 dpi ≤ 50 毫秒；`qingpdf.exe` 增加 ≤ 0.15 MB。
4. 一次简短审查通过。

## 不做

普通字体的 hinting、3c 的内容。

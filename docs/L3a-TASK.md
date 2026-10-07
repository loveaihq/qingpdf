# 第 3 层（渲染）任务单 3a：图形和图片

第 3 层太大，拆成四步，每步单独验收：

| 步 | 内容 |
|---|---|
| **3a（本单）** | 路径、颜色、裁剪、图片、表单 XObject、Type3 字体，`qingpdf render` 命令输出 PNG |
| 3b | 字体：嵌入的 TrueType / OpenType / CFF / Type1，没嵌字体的中文用 Windows 自带字体顶替 |
| 3c | 透明、渐变、图案、混合模式，JPX / JBIG2 图片 |
| 3d | 阅读界面 |

3a 里普通字体（非 Type3）先画成占位框，3b 再换成真字形。

## 规范

ISO 32000-1：8.2–8.7（图形状态、坐标、路径、裁剪、颜色空间）、8.9（图片，含内嵌图片和 `/Decode`、`/ImageMask`、`/SMask` 只读不混合，混合在 3c）、8.10（表单 XObject）、9.6.5（Type3 字体）。代码注明章节号。

## 交付

1. **渲染器**：内容流里的图形操作符全部解释（路径构造和绘制、`re`、`W`/`W*` 裁剪、线宽线帽线连接虚线、`gs` 里本步用得到的项）。颜色空间：DeviceGray/RGB/CMYK、CalGray/CalRGB（按设备色近似）、ICCBased（按通道数近似）、Indexed、Separation/DeviceN（用替代颜色空间和函数，函数类型 0/2/3/4 都要）。
2. **图片**：Flate/LZW 等已有过滤器解出来的、DCTDecode（JPEG）、CCITTFax（扫描件常用，自己写解码），1/2/4/8/16 位，`/Decode`，图片蒙版。JPX、JBIG2 先画成灰块并警告。
3. **光栅化用 tiny-skia**（BSD-3，计划里已定）；JPEG 解码用 zune-jpeg（宽松许可证，加之前核对）。不再加别的依赖。
4. **命令**：`qingpdf render a.pdf --pages 1-3 --dpi 150 -o page_%d.png`。
5. **限制**：每页操作符数、路径段数、图片像素总数、内存都设上限；超了明确报错或跳过那个对象并警告，不崩、不卡。

## 验收

1. `cargo test --release` 全部通过，`cargo clippy` 无警告。
2. **和 Chrome 引擎（PDFium）对照**：`tests/tools/render_compare.py` 加一种模式，把我们的渲染图和 PDFium 的逐页比较。只看"不含普通字体的页面"（3a 不画真字形），这些页面平均像素差要低于阈值；差得多的逐个说明原因。
3. **快和小**（在这台电脑上量）：
   - 一般文字页（占位框）150 dpi 渲染一页 ≤ 50 毫秒，扫描件一页 ≤ 150 毫秒；
   - 内存 ≤ 200 MB；
   - `qingpdf.exe` 增加 ≤ 1 MB。
4. 一次简短审查通过。

## 不做

字体字形（3b）、透明和渐变（3c）、界面（3d）。

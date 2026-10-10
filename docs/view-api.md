# 阅读引擎接口（`qingpdf_core::view`）

给阅读窗口用的一套消息式接口。界面（Windows 的 `qingpdf-view.exe`，以后鸿蒙的 ArkTS 界面）只通过它和引擎打交道，永远不等引擎。

## 怎么工作

`Engine::start(notify)` 开一个**引擎线程**，文档只在这个线程里打开、只在这个线程里用。界面发请求，引擎把结果放进队列；每放进一个结果就调一次 `notify`（只许叫醒窗口，比如 `PostMessage`，不许在里面做事），界面再用 `poll()` 一个一个取，`wait(毫秒)` 给没有消息循环的程序（测试、批处理）用。

## 请求（界面 → 引擎）

| 调用 | 作用 |
|---|---|
| `open_file(doc, 路径, 密码, 总预算, 窗口像素)` | 引擎线程自己读文件并打开。`doc` 是界面起的文档号，之后所有请求和结果都带它。再次打开会先停下正在画的请求、关掉原来的文档，并丢掉它的请求。`窗口像素` 是窗口最多能显示多少像素（界面用屏幕工作区的像素数；0 是不知道），位图缓存按它定大小（见下面"内存"）。 |
| `open_bytes(doc, 字节, 密码, 总预算, 窗口像素)` | 同上，文件已经在内存里。 |
| `render(RenderRequest)` | 画一页的一块（见下）。 |
| `cancel(id)` | 撤回请求（画页、搜索、复制、打印都用它）：没开始的直接丢；正在做的在下一个操作符处停下。撤回的请求没有结果（已经画完的可能还会到，界面按 `id` 认不出就丢掉）。 |
| `close()` | 关文档，丢掉所有请求。 |
| `outline(doc, id)` | （3d-2）取书签，答 `Outline`。 |
| `links(doc, id, page)` | （3d-2）取一页的链接，答 `Links`。 |
| `char_boxes(doc, id, page)` | （3d-2）取一页每个字的框（选字用），答 `CharBoxes`。 |
| `search(doc, id, 查询, 起始页)` | （3d-2）从起始页往后搜、绕回来，答一串 `SearchHits`、几个 `SearchProgress`、一个 `SearchDone`。 |
| `copy_text(doc, id, (页, 字), (页, 字))` | （3d-2）取两处之间的文字（含前不含后），答 `Copied`。 |
| `print(doc, id, [PrintPage])` / `print_next(id)` | （3d-2）打印：一条一条送 `PrintBand`，界面用完一条才调 `print_next`，引擎才画下一条；最后 `PrintDone`。 |
| `start_error()` | 引擎线程起不来（系统不给线程）时的原因，正常是 `None`。起不来时所有调用都不会有回应，界面应当报错而不是一直等。 |

`RenderRequest`：`doc`、`id`（界面起，互不相同）、`page`（从 0 数）、`dpi`（浮点，1 到 2400）、`rotation`（0/90/180/270，加在页面自己的 `/Rotate` 上）、`x y width height`（在"整页按这个 dpi 画出来的位图"里取的一块，像素，从左上角数；宽或高为 0 是整页；超出页面的部分被截掉）、`priority`（数字越小越先画，相同的先来先画；小于 `BACKGROUND_PRIORITY`（1000）的排在书签、链接、字框这些快请求之前，不小于它的排在这些之后）。一块最多 `max_tile_pixels` 像素（`Opened` 里给，满预算时 400 万；引擎自己检查，超了回 `Failed`，不靠界面守规矩；整页可以更大，所以高倍放大时要分块）。

## 结果（引擎 → 界面）

- `Opened { doc, pages: [{宽, 高}], file_bytes, bitmap_cache_bytes, max_tile_pixels }`：每页大小（点，1/72 英寸，已按 `/Rotate` 转过；每边最多 14400，更大的页按 14400 截（取页面左上角），`/UserUnit` 引擎不处理）；`bitmap_cache_bytes` 是在总预算里留给界面存位图的字节数；`max_tile_pixels` 是一个请求最多该要多少像素。
- `OpenFailed { doc, failure, message }`：`failure` 是 `NeedsPassword`（要密码，界面问了再 `open_file` 一次）、`WrongPassword`、`TooLarge`（文件超过 100 MB）、`Unreadable`（读不了，`message` 是系统的话）、`Damaged`（不是 PDF 或坏得太厉害）。
- `Rendered { doc, id, page, status, message, width, height, bgra }`：**BGRA**，8 位，从上到下，没有行填充，不透明。`status`：`Complete`；`Incomplete`（位图有效，但这一页没画完：工作量超了或内容流中途出错，画到哪算哪）；`Failed`（没有位图，`message` 说原因）。

## 3d-2 的结果和限制

- `Opened` 多一个 `rights { copy, print, print_high_quality }`：和 `qingpdf text` 的做法一样（`text::check_extraction_allowed`；有所有者密码或所有者密码为空就全部允许，否则看权限位）。不让复制：`char_boxes` 和 `copy_text` 答 `Denied`，一个字也不给；不让打印：`print` 立刻 `PrintDone(Failed)`；只许低质量打印（第 12 位没开）：最高 150 dpi。搜索不受限（它只给位置，不给文字）。
- `Outline { items: [{title, depth, page?, y?, open}], truncated }`：展平的前序列表，深度从 0 数；`page` 是页码（从 0），`y` 是该页上往下多少点（按页面显示后的样子）。最多 `MAX_OUTLINE_ITEMS`（20000）项、`MAX_OUTLINE_DEPTH`（32）层，标题最多 `MAX_TITLE_CHARS`（300）字，兄弟链或子链绕回已读过的项就在那里断。目标先看 `/Dest`，再看 `/A` 里的 GoTo，命名目标走 `dests.rs`。
- `Links { links: [{rect, kind, page, y?, uri}] }`：`rect` 是页面显示后的点（左、上、右、下）；`kind` 是 `Page`（页内）或 `Uri`。每页最多 `MAX_LINKS`（2000）个；隐藏的、不显示的、没面积的不给。外部地址只留 http、https、mailto，并且全是 ASCII 可见字符、不含空格和 `" < > \ ^ ` { | }`、不超过 `MAX_URI_BYTES`（2048），`%` 只许作百分号转义（`%` 加两个十六进制数字，所以 `%USERNAME%` 这类会被系统当环境变量展开的写法不给），其余的丢掉；`view::safe_uri` 是同一套检查，界面在打开前再查一遍。
- `CharBoxes { boxes }`：`boxes[i]` 是页文字（和 `text` 命令同一个顺序）第 i 个字符的 `[左, 上, 右, 下]`，单位点，**页面显示后的坐标（已按页自己的 /Rotate 转过，左上角为原点，y 向下）**，窗口再加的旋转由窗口自己转。排版插的空格和行尾是没有面积的点。每页最多 `text::MAX_PAGE_CHARS`（40 万）字。`view::line_rects` 把一段字的框并成每行一个矩形，`view::nearest_char` 找离某点最近的字（逐个看）；`view::CharIndex::new(&boxes)` 把一页的框切成一行一行，`nearest(&boxes, x, y)` 答案和 `nearest_char` 一样，但只看可能最近的几行，鼠标每动一下不必扫整页。
- `SearchHits { page, hits: [{start, len, rects}] }`：`start`、`len` 是页文字里的字符位置，`rects` 是要涂色的矩形（每行一个）。搜索规则：大小写不分；全角半角一样（`２０２４` 和 `2024`，`，` 和 `,`，全角空格和空格），康熙部首和兼容汉字当成它代表的字，弯引号当直引号；换行是空格，汉字旁边的空格不算（中文行连起来，数字和汉字之间排版插的空格也不挡）；连续空格是一个，两端的空格不算。查询最多 `MAX_QUERY_CHARS`（200）字，每页最多 `MAX_HITS_PER_PAGE`（2000）处、全书最多 `MAX_SEARCH_HITS`（10000）处（到了就 `SearchDone(HitLimit)`）。搜索一页一步，夹在画页之间：先画 `priority` 小于 `BACKGROUND_PRIORITY` 的块（窗口里看得见的），再做快的请求（书签、链接、字框），再画其余的块，都没有时才搜一页。
- `Copied { text, truncated }`：从第一处到第二处（顺序随便给），页与页之间补一个换行；最多 `MAX_COPY_PAGES`（100）页、`MAX_COPY_CHARS`（100 万）字，超了 `truncated`。文字排法和 `text` 命令完全一样（同一个函数，测试里逐字对过）。
- `PrintBand { index, page, y, width, height, page_width, page_height, dpi, bgra }`：`dpi` 是这页**实际**画的分辨率（要的 dpi 被限到 300，只许低质量打印的文件限到 150，再限到渲染器画得出的范围），窗口按它把页放到纸上，不按自己要的；`index` 是这次打印里的第几页，`y` 起的 `height` 行，宽是整页宽；一条最多 `max_tile_pixels` 像素（和画页的块同一块画布），所以打印时内存不比看书多。dpi 最高 `MAX_PRINT_DPI`（300）。`PrintDone { status, pages_done, message }`。
- 引擎在线程里留着最近读过的页文字和字框（`view/textcache.rs`，16 MiB，算在内存预算的引擎部分里）：搜索、选字、复制共用，第二次搜同一本书不再重新读页（468 页的书：第一次 372 毫秒，第二次 15 毫秒）。搜索和复制这种一口气读很多页的，只在有空位时才留；选字要的页会顶掉最久没用的。一页超过四分之一的就不留。
- 每页的搜索、取字、书签、链接都有自己的工作计量器（`render::work::Work`，和画页是同一个）：文字的运算符、字形、字体第一次载入、表单、排版、搜索的每个字和每次比较、书签的每一项都往里记账（书签标题、链接的 `/A`、`/URI`、`/Rect` 和命名目标的读取也走它：几万项共用的一个大对象只从文件读一次、按大小记账，再用只记一点点）；用完了这一页就不再做（搜索算它"跳过的页"，`SearchDone.pages_skipped`），取消标志和画页用的是同一个，所以取消在一个操作符之内就生效。数字和量法见 `docs/decisions.md` 的"第 3 层 3d-2"。

## 对接别的语言

接口里只有整数、浮点、字符串、字节数组和没有数据的枚举，没有 Rust 特有的东西（借用、闭包、泛型）。包成 C 接口：每个调用一个函数，事件取出来摊平成一个结构（`kind` 加上各字段）、位图用"指针加长度"交出去并配一个释放函数，`notify` 换成"函数指针加一个 `void*`"。鸿蒙的 NAPI 同理：`poll` 的结果转成 JS 对象，`notify` 用线程安全函数回到主线程。**这两个包装 3d-1、3d-2 都没有写。**

## 内存（总预算 200 MB）

`view::plan(总预算, 文件字节, 窗口像素)` 把总预算分给：文件本身、文档的对象流缓存（16 MiB）、渲染缓存（40 MiB）、页文字缓存（16 MiB）、当前页的一幅大图（24 MiB，一页的几块共用，只解一次）、离屏层（16 MiB）、剪裁蒙版（8 MiB）、JPX/JBIG2 解码器（32 MiB）、一块位图的画布（最多 400 万像素 = 16 MiB），剩下的才是界面的位图缓存。窗口的位图先定：窗口像素 ×12 字节（一屏的块，伸出窗口的部分算半屏，再加一屏预取），引擎各项一起按比例缩小（每 1/100 一档，最低留四分之一）直到位图放得下，并且至少放得下两整块。文件超过 40 MB 时各项也按比例缩小，超过 100 MB 不打开。分法的理由和数字见 `docs/decisions.md` 的"第 3 层 3d-1"一节。

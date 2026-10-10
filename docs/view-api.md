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

- `Opened` 多一个 `rights { copy, print, print_high_quality, annotate }`（`annotate` 是 4a 加的，见下）：和 `qingpdf text` 的做法一样（`text::check_extraction_allowed`；有所有者密码或所有者密码为空就全部允许，否则看权限位）。不让复制：`copy_text` 答 `Denied`，一个字也不给；`char_boxes` 在**也不许批注**时答 `Denied`，许批注时照样给字框（只有位置，没有文字：选中文字来标记要用，复制仍然拒绝）；不让打印：`print` 立刻 `PrintDone(Failed)`；只许低质量打印（第 12 位没开）：最高 150 dpi。搜索不受限（它只给位置，不给文字）。
- `Outline { items: [{title, depth, page?, y?, open}], truncated }`：展平的前序列表，深度从 0 数；`page` 是页码（从 0），`y` 是该页上往下多少点（按页面显示后的样子）。最多 `MAX_OUTLINE_ITEMS`（20000）项、`MAX_OUTLINE_DEPTH`（32）层，标题最多 `MAX_TITLE_CHARS`（300）字，兄弟链或子链绕回已读过的项就在那里断。目标先看 `/Dest`，再看 `/A` 里的 GoTo，命名目标走 `dests.rs`。
- `Links { links: [{rect, kind, page, y?, uri}] }`：`rect` 是页面显示后的点（左、上、右、下）；`kind` 是 `Page`（页内）或 `Uri`。每页最多 `MAX_LINKS`（2000）个；隐藏的、不显示的、没面积的不给。外部地址只留 http、https、mailto，并且全是 ASCII 可见字符、不含空格和 `" < > \ ^ ` { | }`、不超过 `MAX_URI_BYTES`（2048），`%` 只许作百分号转义（`%` 加两个十六进制数字，所以 `%USERNAME%` 这类会被系统当环境变量展开的写法不给），其余的丢掉；`view::safe_uri` 是同一套检查，界面在打开前再查一遍。
- `CharBoxes { boxes }`：`boxes[i]` 是页文字（和 `text` 命令同一个顺序）第 i 个字符的 `[左, 上, 右, 下]`，单位点，**页面显示后的坐标（已按页自己的 /Rotate 转过，左上角为原点，y 向下）**，窗口再加的旋转由窗口自己转。排版插的空格和行尾是没有面积的点。每页最多 `text::MAX_PAGE_CHARS`（40 万）字。`view::line_rects` 把一段字的框并成每行一个矩形，`view::nearest_char` 找离某点最近的字（逐个看）；`view::CharIndex::new(&boxes)` 把一页的框切成一行一行，`nearest(&boxes, x, y)` 答案和 `nearest_char` 一样，但只看可能最近的几行，鼠标每动一下不必扫整页。
- `SearchHits { page, hits: [{start, len, rects}] }`：`start`、`len` 是页文字里的字符位置，`rects` 是要涂色的矩形（每行一个）。搜索规则：大小写不分；全角半角一样（`２０２４` 和 `2024`，`，` 和 `,`，全角空格和空格），康熙部首和兼容汉字当成它代表的字，弯引号当直引号；换行是空格，汉字旁边的空格不算（中文行连起来，数字和汉字之间排版插的空格也不挡）；连续空格是一个，两端的空格不算。查询最多 `MAX_QUERY_CHARS`（200）字，每页最多 `MAX_HITS_PER_PAGE`（2000）处、全书最多 `MAX_SEARCH_HITS`（10000）处（到了就 `SearchDone(HitLimit)`）。搜索一页一步，夹在画页之间：先画 `priority` 小于 `BACKGROUND_PRIORITY` 的块（窗口里看得见的），再做快的请求（书签、链接、字框），再画其余的块，都没有时才搜一页。
- `Copied { text, truncated }`：从第一处到第二处（顺序随便给），页与页之间补一个换行；最多 `MAX_COPY_PAGES`（100）页、`MAX_COPY_CHARS`（100 万）字，超了 `truncated`。文字排法和 `text` 命令完全一样（同一个函数，测试里逐字对过）。
- `PrintBand { index, page, y, width, height, page_width, page_height, dpi, bgra }`：`dpi` 是这页**实际**画的分辨率（要的 dpi 被限到 300，只许低质量打印的文件限到 150，再限到渲染器画得出的范围），窗口按它把页放到纸上，不按自己要的；`index` 是这次打印里的第几页，`y` 起的 `height` 行，宽是整页宽；一条最多 `max_tile_pixels` 像素（和画页的块同一块画布），所以打印时内存不比看书多。dpi 最高 `MAX_PRINT_DPI`（300）。`PrintDone { status, pages_done, message }`。
- 引擎在线程里留着最近读过的页文字和字框（`view/textcache.rs`，16 MiB，算在内存预算的引擎部分里）：搜索、选字、复制共用，第二次搜同一本书不再重新读页（468 页的书：第一次 372 毫秒，第二次 15 毫秒）。搜索和复制这种一口气读很多页的，只在有空位时才留；选字要的页会顶掉最久没用的。一页超过四分之一的就不留。
- 每页的搜索、取字、书签、链接都有自己的工作计量器（`render::work::Work`，和画页是同一个）：文字的运算符、字形、字体第一次载入、表单、排版、搜索的每个字和每次比较、书签的每一项都往里记账（书签标题、链接的 `/A`、`/URI`、`/Rect` 和命名目标的读取也走它：几万项共用的一个大对象只从文件读一次、按大小记账，再用只记一点点）；用完了这一页就不再做（搜索算它"跳过的页"，`SearchDone.pages_skipped`），取消标志和画页用的是同一个，所以取消在一个操作符之内就生效。数字和量法见 `docs/decisions.md` 的"第 3 层 3d-2"。

## 4a 的请求和结果：批注、撤销、保存

编辑也走同一套消息。所有位置都是"页面显示后的点"（和 `CharBoxes` 一样：左上角为原点、y 向下、已按页自己的 `/Rotate` 和裁剪框转过），引擎自己转回用户空间。

| 调用 | 作用 |
|---|---|
| `annotations(doc, id, page)` | 取一页上能选中、能删的批注（除链接、表单控件、弹出框外都算；隐藏的不给），答 `Annotations { items: [{index, num, subtype, rect, parts, contents, author}] }`。`index` 是它在 `/Annots` 里的位置，`num` 是对象号（直接写在数组里的是 0）；`parts` 是文字标记的每一行（命中测试和描边用）；`contents` 最多 `MAX_CONTENTS_CHARS`（200）字。最多 `MAX_ANNOT_INFOS`（2000）个。 |
| `add_markup(doc, id, page, kind, rects, color, author)` | 高亮/下划线/删除线/波浪线（`MarkupKind`）。`rects` 是每行一个 `[左,上,右,下]`（界面用 `line_rects` 并选中字的框得到），最多 `MAX_QUADS`（4000）行。 |
| `add_ink(doc, id, page, strokes, color, width, author)` | 手写。`strokes` 是几笔，每笔一串点；最多 `MAX_INK_STROKES`（64）笔，总点数超过 `MAX_INK_POINTS`（8192）就等距抽稀（每笔的头尾留着）；线宽夹在 `MIN_INK_WIDTH`..`MAX_INK_WIDTH`（0.25 到 36 点）。 |
| `add_note(doc, id, page, at, text, color, author)` | 便签（Text 批注加 Popup）。`at` 是图标左上角（图标 20 点见方，会被挪进页内）；`text` 最多 `MAX_NOTE_CHARS`（4000）字，全空白的不收。 |
| `delete_annotation(doc, id, page, index, num)` | 删一个批注（包括别的软件加的）。`index` 和 `num` 要和 `annotations` 说的一样，对不上（列表过期了）就拒绝；链接、表单控件、弹出框不让删；`/F` 的 Locked 位（128）开着的不让删（`Failed`，`message` 里有 "locked"）。一起删的有：它的弹出框（只有弹出框的 `/Parent` 指回它时才删，指向别的批注的留着）、回复它的批注（`/IRT` 指向它，回复的回复也算，最多 8 层；回复里有锁住的，整个不删）。页的 `/Annots` 数组如果是几页共用的（或看不出是不是），这一页拿到自己的一份拷贝，别的页不变。 |
| `undo(doc, id)` / `redo(doc, id)` | 撤销/重做上一步编辑（最多 `MAX_UNDO_STEPS`=100 步；步和没保存的改动合起来超过内存预算里给编辑的 16 MiB，最旧的一步就不能再撤销了，它做的还在）。保存之后还能撤销：撤销到保存之前的状态，`dirty` 变真，再存写出的是改回去的样子。 |
| `save(doc, id, 路径)` | 把文件写到路径（原文件，或另存为的新文件）：原文件的全部字节，后面接**一段**增量更新（所有没保存的改动的最终样子）；先写同目录的临时文件、`fsync`、再改名替换，出错原文件不动。写成之后，写出的文件就是文档的新底本（以后的保存接在它后面，前一次保存的字节是后一次的前缀）。没改过的文件也能存（原样写出）。保存时有打印任务在跑，答 `Failed`（要换文档的底本）。 |

`author` 最多 `MAX_AUTHOR_CHARS`（64）字，写进 `/T`；`/M` 和 `/CreationDate` 是引擎取的当前时间（UTC）。边界值（点不是数、超大、没面积的矩形、空的手写）一律答 `Failed`，不 panic。

结果：

- `Edited { doc, id, status, message, page, can_undo, can_redo, dirty, rewrote }`：加、删、撤销、重做都答它。`status`：`Done`、`Denied`（文件作者不让加批注，且没用所有者密码打开）、`Failed`（`message` 说原因，文件没变）、`Nothing`（没有可撤销/重做的）、`Busy`（正在打印）。`Done` 之后 `page` 那页画过的位图作废（别的页没变），排着的请求照常做；文档没有被换掉，改动在内存里（"怎么做的"），文件要等保存。`Done` 的 `message` 通常是空的，只有一种情况有字：文件有数字签名，保存后签名会显示文件被改过，这句话每个文件只说一次。`Failed` 的原因里有这几种：文件有 `/Perms /DocMDP` 认证且 `/P` 为 1 或 2（不许加批注）；签名文件的结构坏了、需要整份重写（重写会破坏签名，不重写）；没保存的改动太多（超过预算，先保存）。`dirty` 是文件和上次保存（或打开）时不一样；`rewrote` 是文件的交叉引用坏了（或 `%PDF-` 前有东西），第一次编辑前整份重写了，保存时也是整份写，原字节不保留。
- `Saved { doc, id, status, message, bytes }`：`status` 是 `Done` 或 `Failed`。
- `Opened.rights.annotate`：文件没加密、或有所有者密码、或权限位第 6 位开着时为真。不为真时 `add_*`/`delete_annotation` 答 `Denied`（`undo`/`redo`/`save` 不查）。

怎么做的：文件打开后一直是原样的 `Document`；每次编辑算出它做的和改的对象（新批注、外观流、改过的页或 `/Annots` 数组），作为"盖在原对象前面的一层"放在内存里（`Document` 的 overlay，读对象先看这层）。画页、批注列表都从这一层读；只有被改的那一页的页字典重读、字缓存作废。撤销/重做是把这一层里那几个对象换回去/换上，历史里每一步存"这些对象改之前和改之后"，和 overlay 共用同一份对象，不拷贝。保存时把这一层的最终样子写成**一段**增量更新（一个交叉引用段），接在原文件字节后面，原文件的字节永远是新文件的前缀；写成后这个文件成为新的底本，这一层清空。内存是原文件加这一层（再加一份撤销历史），后两者合起来最多 16 MiB（算在内存预算里；超了先挤掉最旧的撤销步，还超就拒绝编辑、让用户先保存）。细节和理由见 `docs/decisions.md` 的"第 4 层 4a"。

## 对接别的语言

接口里只有整数、浮点、字符串、字节数组和没有数据的枚举，没有 Rust 特有的东西（借用、闭包、泛型）。包成 C 接口：每个调用一个函数（`add_markup`、`add_ink`、`add_note`、`delete_annotation` 各是一个函数，不是一个带数据的 `Edit` 枚举），事件取出来摊平成一个结构（`kind` 加上各字段）、位图用"指针加长度"交出去并配一个释放函数，`notify` 换成"函数指针加一个 `void*`"。鸿蒙的 NAPI 同理：`poll` 的结果转成 JS 对象，`notify` 用线程安全函数回到主线程。**这两个包装 3d-1、3d-2、4a 都没有写。**

## 内存（总预算 200 MB）

`view::plan(总预算, 文件字节, 窗口像素)` 把总预算分给：文件本身、文档的对象流缓存（16 MiB）、渲染缓存（40 MiB）、页文字缓存（16 MiB）、没保存的改动和撤销历史（16 MiB）、当前页的一幅大图（24 MiB，一页的几块共用，只解一次）、离屏层（16 MiB）、剪裁蒙版（8 MiB）、JPX/JBIG2 解码器（32 MiB）、一块位图的画布（最多 400 万像素 = 16 MiB），剩下的才是界面的位图缓存。窗口的位图先定：窗口像素 ×12 字节（一屏的块，伸出窗口的部分算半屏，再加一屏预取），引擎各项一起按比例缩小（每 1/100 一档，最低留四分之一）直到位图放得下，并且至少放得下两整块。文件超过 40 MB 时各项也按比例缩小，超过 100 MB 不打开。分法的理由和数字见 `docs/decisions.md` 的"第 3 层 3d-1"一节。

# 阅读引擎接口（`qingpdf_core::view`）

给阅读窗口用的一套消息式接口。界面（Windows 的 `qingpdf-view.exe`，以后鸿蒙的 ArkTS 界面）只通过它和引擎打交道，永远不等引擎。

## 怎么工作

`Engine::start(notify)` 开一个**引擎线程**，文档只在这个线程里打开、只在这个线程里用。一个引擎可以同时拿着几个文档（窗口的几个标签，见下面“多个文档”）：一次只**服务**一个，别的存着等；请求和结果都带 `doc`。界面发请求，引擎把结果放进队列；每放进一个结果就调一次 `notify`（只许叫醒窗口，比如 `PostMessage`，不许在里面做事），界面再用 `poll()` 一个一个取，`wait(毫秒)` 给没有消息循环的程序（测试、批处理）用。

## 请求（界面 → 引擎）

| 调用 | 作用 |
|---|---|
| `open_file(doc, 路径, 密码, 总预算, 窗口像素)` | 引擎线程自己读文件并打开，并服务它。`doc` 是界面起的文档号（不重复用），之后所有请求和结果都带它。原来服务的文档**不关**，存起来（2026-10-17 起）：正在画的请求停下、它的请求丢掉，文档和没保存的改动留着，等 `activate`；同号的文档会被关掉。`总预算` 是所有文档合起来的内存目标，存着的文档的字节先从里面扣（见"多个文档"）。`窗口像素` 是窗口最多能显示多少像素（界面用屏幕工作区的像素数；0 是不知道），位图缓存按它定大小（见下面"内存"）。打不开时（答 `OpenFailed`）**没有文档在被服务**，界面要 `activate` 一个。 |
| `activate(doc, 总预算, 窗口像素)` | 服务存着的文档 `doc`，正服务的那个改成存着：正在画的请求停下，原来那个文档的请求丢掉（搜索、打印这类要界面自己先撤掉；排在前面的编辑、保存、导出先做完并答复）。答 `Activated`；`doc` 不在了（被放掉了）答 `Dropped`；`doc` 就是正服务的那个，原样再答一次 `Activated`，别的什么也不做。 |
| `close_doc(doc)` | 关一个文档（正服务的或存着的），连没保存的改动。不答。关的是正服务的，就没有文档在被服务，直到 `activate`。 |
| `open_bytes(doc, 字节, 密码, 总预算, 窗口像素)` | 同上，文件已经在内存里。 |
| `render(RenderRequest)` | 画一页的一块（见下）。 |
| `cancel(id)` | 撤回请求（画页、搜索、复制、打印都用它）：没开始的直接丢；正在做的在下一个操作符处停下。撤回的请求没有结果（已经画完的可能还会到，界面按 `id` 认不出就丢掉）。 |
| `close()` | 关所有文档（存着的也关），丢掉所有请求。 |
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
- `Opened` 多两项（2026-10-20，看的方式）：`spread_hint`（`u8`：`SPREAD_NONE` = 0 一栏、也包括文件什么都没写；`SPREAD_FIRST_LEFT` = 1 两栏、第一页在左，`/PageLayout` 是 `TwoColumnLeft` 或 `TwoPageLeft`；`SPREAD_FIRST_RIGHT` = 2 两栏、第一页在右并且单独一行，是 `TwoColumnRight` 或 `TwoPageRight`；不认识的名字、类型不对的条目都当没写）和 `right_to_left`（`/ViewerPreferences` 的 `/Direction` 是 `/R2L`；可以是直接写的字典，也可以是引用）。引擎不排版，只告诉界面文件的意思；界面一页一页怎么摆见 `docs/decisions.md` 的“看的方式（2026-10-20）”。
- 屏幕上页面的颜色（夜间、护眼）不在这个接口里：位图原样交给界面，界面在存进缓存之前调 `qingpdf_core::render::recolor::recolor(&mut bgra, Tint)`（`Tint` 是 `Original`/`Night`/`Sepia`，`#[repr(u8)]`；`Tint::paper()` 是白纸变成什么颜色）。打印（`print` 的 `PrintBand`）、`copy_text`、导出和保存都不经过它。
- `OpenFailed { doc, failure, message }`：`failure` 是 `NeedsPassword`（要密码，界面问了再 `open_file` 一次）、`WrongPassword`、`TooLarge`（文件超过 100 MB，或在总预算里怎么也放不下）、`Unreadable`（读不了，`message` 是系统的话）、`Damaged`（不是 PDF 或坏得太厉害）、`NoRoom`（文件本身放得下，但别的存着的文档占了地方，又都放不掉：有没保存的改动、或用密码打开的；界面说“先保存或关闭一些标签”）。
- `Activated { doc, bitmap_cache_bytes, thumb_cache_bytes, max_tile_pixels }`：`activate` 的答复：这个文档又被服务了，按**现在**的总预算算出的数（别的存着的文档占着地方，所以不一定和打开时一样）。界面在收到它以前先不要排画页的请求（排了也行，按顺序做，但位图缓存的大小要按这里的数）。
- `Dropped { doc }`：引擎为了给新文件腾地方，把存着的文档 `doc` 放掉了（它不再打开）；界面在要用这个标签时用 `open_file`（新的文档号）重新读文件。只会放：没有没保存的改动、不是用密码打开的、没有“只留在内存里”的识别文字的文档。`activate` 一个不在的文档也答它。
- `Rendered { doc, id, page, status, message, width, height, bgra }`：**BGRA**，8 位，从上到下，没有行填充，不透明。`status`：`Complete`；`Incomplete`（位图有效，但这一页没画完：工作量超了、内容流中途出错，或这页的批注、没有外观的表单控件超过了上限，`message` 说是哪一种；画到哪算哪，不会悄悄少画）；`Failed`（没有位图，`message` 说原因）。

## 3d-2 的结果和限制

- `Opened` 多一个 `rights { copy, print, print_high_quality, annotate, fill_forms, modify, assemble }`（`annotate` 是 4a 加的，`fill_forms` 是 4b 加的，`modify` 是 4c 加的，`assemble` 是页面管理加的，见下）：和 `qingpdf text` 的做法一样（`text::check_extraction_allowed`；有所有者密码或所有者密码为空就全部允许，否则看权限位）。不让复制：`copy_text` 答 `Denied`，一个字也不给；`char_boxes` 在**也不许批注**时答 `Denied`，许批注时照样给字框（只有位置，没有文字：选中文字来标记要用，复制仍然拒绝）；不让打印：`print` 立刻 `PrintDone(Failed)`；只许低质量打印（第 12 位没开）：最高 150 dpi。搜索不受限（它只给位置，不给文字）。
- `Outline { items: [{title, depth, page?, y?, open}], truncated }`：展平的前序列表，深度从 0 数；`page` 是页码（从 0），`y` 是该页上往下多少点（按页面显示后的样子）。最多 `MAX_OUTLINE_ITEMS`（20000）项、`MAX_OUTLINE_DEPTH`（32）层，标题最多 `MAX_TITLE_CHARS`（300）字，兄弟链或子链绕回已读过的项就在那里断。目标先看 `/Dest`，再看 `/A` 里的 GoTo，命名目标走 `dests.rs`。
- `Links { links: [{rect, kind, page, y?, uri}] }`：`rect` 是页面显示后的点（左、上、右、下）；`kind` 是 `Page`（页内）或 `Uri`。每页最多 `MAX_LINKS`（2000）个；隐藏的、不显示的、没面积的不给。外部地址只留 http、https、mailto，并且全是 ASCII 可见字符、不含空格和 `" < > \ ^ ` { | }`、不超过 `MAX_URI_BYTES`（2048），`%` 只许作百分号转义（`%` 加两个十六进制数字，所以 `%USERNAME%` 这类会被系统当环境变量展开的写法不给），其余的丢掉；`view::safe_uri` 是同一套检查，界面在打开前再查一遍。
- `CharBoxes { boxes, text }`（`text` 是和 `boxes` 一一对应的字符，空格是排版插的空格、行尾是换行符，双击选词、三击选行用；只许批注不许复制的文件 `text` 为空）：`boxes[i]` 是页文字（和 `text` 命令同一个顺序）第 i 个字符的 `[左, 上, 右, 下]`，单位点，**页面显示后的坐标（已按页自己的 /Rotate 转过，左上角为原点，y 向下）**，窗口再加的旋转由窗口自己转。排版插的空格和行尾是没有面积的点。每页最多 `text::MAX_PAGE_CHARS`（40 万）字。`view::line_rects` 把一段字的框并成每行一个矩形，`view::nearest_char` 找离某点最近的字（逐个看）；`view::CharIndex::new(&boxes)` 把一页的框切成一行一行，`nearest(&boxes, x, y)` 答案和 `nearest_char` 一样，但只看可能最近的几行，鼠标每动一下不必扫整页。
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
| `save(doc, id, 路径)` | 把文件写到路径（原文件；另存为用下面的 `save_as`）：原文件的全部字节，后面接**一段**增量更新（所有没保存的改动的最终样子）；先写同目录的临时文件、`fsync`、再改名替换，出错原文件不动。写成之后，写出的文件就是文档的新底本（以后的保存接在它后面，前一次保存的字节是后一次的前缀）。没改过的文件也能存（原样写出）。保存时有打印任务在跑，答 `Failed`（要换文档的底本）。**保存是增量**：用 `replace_line` 改过文字的，旧字还在更新之前的那些字节里（要去掉用 `save_as`）。 |
| `save_as(doc, id, 路径)` | 另存为（5a 审查后加）：答 `Saved`，但**整个文件重写一遍**，不是“原文件的字节加一段增量”——用拆分的那套写法（`ops::copy_all`：只留页树够得着的对象、重新编号、经典交叉引用表，加密的照原来的密钥和权限加密），改一行文字拿掉的旧字不在新文件的任何对象和流里。写成之后文档就是这个新文件：撤销/重做的历史清空（阅读器把菜单里的撤销、重做灰掉），以后的保存接在它后面。有签名的文件仍按 `save` 写（整份重写会让签名失效）；打印任务在跑时答 `Failed`。界面的“另存为”用它（对话框标题里有一句说明），“保存”仍是增量。 |

`author` 最多 `MAX_AUTHOR_CHARS`（64）字，写进 `/T`；`/M` 和 `/CreationDate` 是引擎取的当前时间（UTC）。边界值（点不是数、超大、没面积的矩形、空的手写）一律答 `Failed`，不 panic。

结果：

- `Edited { doc, id, status, message, page, can_undo, can_redo, dirty, rewrote }`：加、删、撤销、重做都答它。`status`：`Done`、`Denied`（文件作者不让加批注，且没用所有者密码打开）、`Failed`（`message` 说原因，文件没变）、`Nothing`（没有可撤销/重做的）、`Busy`（正在打印）。`Done` 之后 `page` 那页画过的位图作废（别的页没变），排着的请求照常做；文档没有被换掉，改动在内存里（"怎么做的"），文件要等保存。`Done` 的 `message` 通常是空的，只有两种情况有字：文件有数字签名，保存后签名会显示文件被改过，这句话每个文件只说一次；`replace_line`（5a）的新文字比旧文字宽（见下面 5a 一节）。`Failed` 的原因里有这几种：文件有 `/Perms /DocMDP` 认证且 `/P` 为 1 或 2（不许加批注）；签名文件的结构坏了、需要整份重写（重写会破坏签名，不重写）；没保存的改动太多（超过预算，先保存）。`dirty` 是文件和上次保存（或打开）时不一样；`rewrote` 是文件的交叉引用坏了（或 `%PDF-` 前有东西），第一次编辑前整份重写了，保存时也是整份写，原字节不保留。
- `Saved { doc, id, status, message, bytes }`：`status` 是 `Done` 或 `Failed`。
- `Opened.rights.annotate`：文件没加密、或有所有者密码、或权限位第 6 位开着时为真。不为真时 `add_*`/`delete_annotation` 答 `Denied`（`undo`/`redo`/`save` 不查）。`Opened.rights.fill_forms`（4b）见下。

怎么做的：文件打开后一直是原样的 `Document`；每次编辑算出它做的和改的对象（新批注、外观流、改过的页或 `/Annots` 数组），作为"盖在原对象前面的一层"放在内存里（`Document` 的 overlay，读对象先看这层）。画页、批注列表都从这一层读；只有被改的那一页的页字典重读、字缓存作废。撤销/重做是把这一层里那几个对象换回去/换上，历史里每一步存"这些对象改之前和改之后"，和 overlay 共用同一份对象，不拷贝。保存时把这一层的最终样子写成**一段**增量更新（一个交叉引用段），接在原文件字节后面，原文件的字节永远是新文件的前缀；写成后这个文件成为新的底本，这一层清空。内存是原文件加这一层（再加一份撤销历史），后两者合起来最多 16 MiB（算在内存预算里；超了先挤掉最旧的撤销步，还超就拒绝编辑、让用户先保存）。细节和理由见 `docs/decisions.md` 的"第 4 层 4a"。

## 4b 的请求和结果：填表、文字框、签名图片

同样走消息，位置同样是"页面显示后的点"。

| 调用 | 作用 |
|---|---|
| `fields(doc, id, page)` | 取一页的表单字段（每个控件一项，按 `/Annots` 的顺序，也就是 Tab 的顺序），答 `Fields { items: [FieldInfo], truncated }`。`FieldInfo`：`index`、`num`（控件在 `/Annots` 里的位置和对象号，填表时原样交回）、`kind`（`Text`、`Check`、`Radio`、`Push`、`Combo`、`List`、`Signature`）、`rect`、`read_only`、`multiline`、`password`、`comb`、`max_len`（0 为不限）、`value`（文字、下拉、列表的当前值，选项显示成它的显示文字）、`checked`（勾选框和单选）、`editable`（可输入的下拉）、`name`。**列表里没有下拉和列表的选项**（几十个字段共用一个很长的 `/Opt` 时，每个字段各带一份会占几百 MB）：要选的时候用 `options` 单取。字段从控件沿 `/Parent` 往上找（最多 32 层，遇到重复就停），`/FT`、`/Ff`、`/V`、`/DA`、`/Q`、`/MaxLen`、`/Opt` 取最近的，没有就用 `/AcroForm` 的。每页最多 `MAX_FIELD_INFOS`（3000）项，整张列表（文字加每项结构体的大小）最多 `MAX_FIELDS_BYTES`（256 KB），超过的不给，`truncated` 为真；隐藏的控件不给。内存计划（`MemoryPlan`）为它们留了 `form_bytes`（8 MiB：25 页的列表加一次选项）。 |
| `options(doc, id, page, index, num)` | 取一个下拉或列表的选项（`index`、`num` 是 `Fields` 给的那个字段），答 `Options { page, index, num, items: [(导出值, 显示文字)], truncated }`。最多 2000 项，文字合计最多 `MAX_OPTION_BYTES`（256 KB），多的不给，`truncated` 为真；字段不在了或不是选择字段给空表。选项只在人要选的时候取，不留着。 |
| `fill_field(doc, id, page, index, num, FillValue)` | 填一个字段：`Text(字符串)`（文本框的文字，或下拉、列表的一个选项，可以是导出值也可以是显示文字；可输入的下拉接受任何文字）、`Checked(开关)`（勾选框、单选）。文字最多 `MAX_VALUE_CHARS`（4000）字，超过 `/MaxLen` 答 `Failed`；单行字段里的换行变成空格；只读的、签名、按钮不填。单选组里选一个，同组别的变成 `Off`；`NoToggleToOff` 的组不能关。答 `Edited`。 |
| `add_freetext(doc, id, page, rect, text, size, color, author)` | 文字框（FreeText）：`rect` 是 `[左,上,右,下]`，文字 `size` 点（夹在 `MIN_FONT_SIZE` 4 到 `MAX_FONT_SIZE` 144），最多 `MAX_FREETEXT_CHARS`（4000）字，字符种类最多 3000；放不下时框向下长，不出页；太小的框撑到 8 点，出页的挪回页内。答 `Edited`。 |
| `add_stamp(doc, id, page, rect, 图片字节, author)` | 图片（Stamp）：PNG（透明部分保持透明）或 JPEG，放成在 `rect` 里最大的、保持图片形状、居中的一块。图片最多 `MAX_STAMP_PIXELS`（1600 万）像素、`MAX_STAMP_FILE_BYTES`（12 MB）；认不出的、被截断的、超限的答 `Failed`。界面用 `qingpdf_core::image::probe(字节)`（只读文件头）取图片的宽高来定框的形状。答 `Edited`。 |

`Rights` 多一项 `fill_forms`（权限位 9；版本 2 的文件看位 6；没加密或有所有者密码为真）。`fill_field` 看 `fill_forms`，`add_freetext`、`add_stamp` 看 `annotate`；不让时答 `Denied`。认证过的文件（`/Perms /DocMDP`）：`/P` 为 1 什么都不许；为 2 只许 `fill_field`，`add_freetext`、`add_stamp`、画、便签、删批注答 `Failed`（`message` 里写着 DocMDP）；为 3 都许。

每次编辑的工作量记在计量器里：一次编辑最多一页的工作量（`PAGE_WORK`），一个会话里所有编辑加起来最多二十页的；用完了答 `Failed`。画页时没有外观的控件现场合成外观（只画，不写文件），每个文档最多 2 万个，每页再受页面计量器限制。

填表、文字框、图片都写进"盖在原对象前面的一层"，和 4a 一样撤销、重做、保存；一次填写撤销一步，同一字段的几个控件（和字体、外观）一起回来。表单里的 JavaScript（`/AA`、格式化、计算、校验）不执行。

## 4c 的请求和结果：往页面内容里加文字、图片、水印

这三样写进页面**本身的内容**（不是批注）：打印、别的软件都看得到；文字和图片也是页面的内容，提取文字、查找、选择都包括；**水印除外**：它的画法标成 `/Artifact`（`/Type /Pagination /Subtype /Watermark`，ISO 32000-1 14.8.2.2），引擎的取字（`TextExtractor`、阅读器的查找和选择）跳过标成这种水印的内容，别的软件通常也一样；**页码和页眉页脚也一样**（`add_pagination`，`/Subtype /Header` 或 `/Footer`），取字现在跳过所有 `/Type /Pagination` 的 `/Artifact`，包括别的软件写的页眉页脚。位置同样是"页面显示后的点"（左上角为原点、y 向下、已按页自己的 `/Rotate` 和裁剪框转过），引擎自己转回用户空间：页面有 `/Rotate`、裁剪框不从 0 起、有 `/UserUnit`（引擎不处理它）时，文字和图片照样正着摆在要的地方，水印照样正着、在裁剪框的正中。

| 调用 | 作用 |
|---|---|
| `add_text(doc, id, page, at, text, size, color)` | 文字：`at` 是**第一行基线的左端**，`text` 里的换行是换行（一行一行往下排，行距是字体的上升加下降），最多 `MAX_FREETEXT_CHARS`（4000）字、2000 行；`size` 夹在 `MIN_FONT_SIZE`..`MAX_FONT_SIZE`；字体是 4b 的系统字体子集（中文能显示，复制出来是对的）。答 `Edited`。 |
| `add_image(doc, id, page, rect, 图片字节)` | 图片：放成在 `rect` 里最大、保持形状、居中的一块；不挪进页内（出页的部分被页面剪掉）。PNG 的透明保持，限制和 `add_stamp` 一样（1600 万像素、12 MB）。答 `Edited`。 |
| `add_watermark(doc, id, Watermark)` | 水印，作用于 `pages`（从 0 数，最多 `MAX_WATERMARK_PAGES`=100000 页（再大由计量器和编辑的内存预算拦），同一页写几次只做一次）。`Watermark { text, image, opacity, angle, tile, under, pages }`：`text` 最多 `MAX_WATERMARK_CHARS`（200）字，`image` 是 PNG/JPEG 字节（可以为空；文字和图片都有时图片在上），两者不能都空；`opacity` 大于 0、不超过 1（`ca`/`CA`）；`angle` 是度数，按页面看到的样子逆时针；`tile` 铺满整页（错开排，每页最多 1600 个格子里取，格子大了就放粗）；`under` 垫在原内容下面（默认盖在上面）。一个水印一次撤销一步，`Edited.page` 是 `ALL_PAGES`（`u32::MAX`）：界面把每一页的位图都作废。 |
| `add_pagination(doc, id, Pagination)` | 页码、页眉页脚，作用于 `pages`（从 0 数，最多 `MAX_PAGINATION_PAGES`=100000 页）。`Pagination { slots, size, color, margin, pages, start, mirror, skip_first, file, date }`：`slots` 是六处的模板（页眉左、中、右，页脚左、中、右；空白就是那处不写；每个最多 `MAX_TEMPLATE_CHARS`=200 字），模板里的 `{page}`（页码）、`{pages}`（范围里最后一页的页码）、`{date}`（用 `date` 的原文）、`{file}`（用 `file` 的原文）换成内容，别的字（包括花括号）原样写；`size` 点（夹在 4..144）；`margin` 毫米（0..`MAX_MARGIN_MM`=500），页眉的上沿、页脚的下沿、左右两处的外沿离页边这么远，中间那处居中；`start` 范围里第一页的编号（最大 `MAX_START`=1000000），范围里的第一页是 `pages` 里页号最小的一页，别的页是 `start` 加上它离第一页的页数；`mirror` 文件里第 2、4、6…页左右两处互换；`skip_first` 范围里的第一页不写（仍计数）。位置按页面显示后的样子（可见框、`/Rotate`），文字正着。字体是一个系统字体子集，所有页共用；每页一条自己的内容流，标成页眉或页脚的 `/Artifact`，页字典里用私有键 `/QpPagination` 记下这些流。答一个 `Edited`，`page` 是 `ALL_PAGES`，撤销一步。 |
| `remove_pagination(doc, id)` | 删掉 `add_pagination` 写的页码和页眉页脚（所有页，不管哪次写的）：把页字典 `/QpPagination` 里记的流从 `/Contents` 拿掉；别的软件写的不动；一处也没找到答 `Failed`。需要的权限和 `add_pagination` 一样。 |

字体、图片和水印的标记（一个带自己资源的 Form XObject）**只做一次**，每页只引用它；被几页共用或继承来的 `/Resources` 复制一份给这些页，不改原对象；原内容的流（共用的也一样）原样留着；盖在上面时，引擎把原内容的每个流读一次（按对象号缓存），数出它留着没关的 `q`、没有 `q` 对应的 `Q`、没结束的 `BT` 和没画的路径：前面补上同样多的 `q`，后面先补 `n`、`ET` 再补同样多的 `Q`，新内容就在页面开始时的图形状态里画（垫在下面的不用）。细节见 `docs/decisions.md` 的"第 4 层 4c"。

`Rights` 多一项 `modify`（权限位 4，没加密或有所有者密码为真）：三个调用看它，不让时答 `Denied`。**有签名的文件（有签了名的签名字段，或有 `/Perms /DocMDP`，任何级别）一律答 `Failed`**，`message` 里写着 signed 或 certified：页面内容的改动哪一级认证都不允许，签了名的文件也不动。`Failed` 的别的原因：没有文字也没有图片、文字太长、不透明度或角度不是数、页码不在、页数超限、图片认不出或超限、位置不是数、这次改动的对象总共超过编辑的内存预算还剩的部分（16 MiB 减去已经放在文件前面还没存的，做的时候就数，超了就停，不是做完再拒；`message` 是 `the change is too big`，后面写怎么办：文件里已有没存的改动就是"存盘后再做，或分几次做"，没有就是"分几次做（每次少几页）"）、字体找不到（和 4b 一样列出缺哪些字）。

每次编辑的工作量记在计量器里：每页 6000 单位（再加页面字典和资源的字节数乘 8）、平铺的每一块 2500、文字每字 600、图片每像素 12、字体每个字形 4 万、盖在上面时原内容的每个字节 10（读一遍数 `q`/`Q`）；实测记的账是真实用时的 1 到 1.4 倍（空页 2.5 倍）。命令行用同一套代码（`qingpdf_core::edit::add_content`），见 `docs/decisions.md`。

## 5a 的请求和结果：改页面上已有的文字

改的是页面**本身的内容**（和 4c 一样：打印、别的软件、提取文字、查找、选择看到的都是新字；旧字真的从内容流里去掉，不是盖一个白框）。位置同样是"页面显示后的点"（左上角为原点、y 向下、已按页自己的 `/Rotate` 和裁剪框转过）。

| 调用 | 作用 |
|---|---|
| `text_line(doc, id, page, at)` | 问点 `at` 那一行的字：答 `TextLine { doc, id, page, status, message, line }`。"一行"和文字提取、查找、选字是同一个（`TextExtractor` 的排版：沿同一条基线连着画出的字）；点在离某个字不超过 0.7 个字号的地方就算点中。`line: Option<LineView { text, rect, why }>`：`text` 是这一行的文字，`rect` 是它占的 `[左, 上, 右, 下]`，`why` 非空就是这一行改不了的原因（在多处被画的表单里、字体找不到、几个流共用同一个对象……）。附近没有字时 `line` 是 `None`。`status`：`Ok`；`Denied`（没有"允许修改内容"权限）；`Failed`（有签名、页读不出，`message` 说原因）。 |
| `replace_line(doc, id, page, at, old, text)` | 把点 `at` 那一行（现在读出来要是 `old`，否则当作过期，拒绝）改成 `text`：只改变中间不同的一段（头尾相同的字不动，字节也不动），旧字从内容流里去掉，新字画在旧字原来的地方，字号、颜色、字间距、词间距、水平缩放、上升、文字矩阵都沿用；删了字再放新字的，它后面同一个操作符里的字留在原处（补一个 `TJ` 数字抵掉新旧字宽的差；纯插入的不补，后面的字顺延；用 `Td`/`Tm` 另外定位的不动）。字在带 `/ActualText` 的标记序列里时，序列正好只有这些字的，`/ActualText` 改成新文字，否则去掉这一项（提取文字不会再读出旧字）。不重排。`text` 最多 `MAX_TEXT_CHARS`（4000）字，不能有换行或别的控制字符。答 `Edited`。 |

`Edited.message`：改成功时，新字比旧字宽就写 `the new text is wider than the old by N points; it may run into what follows`（`Done` 的 `message` 以前只有签名警告一种，现在还可以有这一句，两句都有时用空格连着）。

新字用什么字体：原字体里有每一个字形就用原字体（简单字体按编码表、复合字体按 `/ToUnicode` 反查出编码，再问字体程序里真有这个字形，因为子集里没用过的字形只是空的；Type 3 字体看它的 `/Widths`）；缺的字用一个相近的系统字体子集（按原字体的名字和 `/Flags` 挑衬线/无衬线/等宽，中文是宋体/黑体/楷体/仿宋），只对缺的字用 `Tf` 换过去再换回来；原字体是 Type 3 时，补的字体也是 Type 3（同样的 `/FontMatrix`，字形取自系统字体的轮廓），不会倒过来；竖排用 `Identity-V`。原字体没有可用的空格（TeX 的字体空格宽度是 0）时，用 `TJ` 的数字挪开一个空格的宽度。

权限和签名同 4c：要"允许修改内容"（`Rights.modify`），有签名的文件（任何级别的认证）一律 `Failed`。改写的每个内容流最多 64 MiB，一页最多 2 万处，工作量记在计量器里（读页的量、每处 2 万、每个字在字体里找 20 万、字体第一次读 300 万、每字节 14），新对象的字节算在编辑的内存预算里（超了答 `the change is too big`）。命令行的 `qingpdf replace` 用同一套代码（`qingpdf_core::edit::replace_text`，一次读一页，不用的页不碰），细节见 `docs/decisions.md` 的"第 5 层 5a"。

**旧文字留在旧版本里：** 保存是增量更新，原文件的字节原样在前面，所以旧文字还在文件前面那部分字节里（新版本的页面里已经没有了）。要彻底去掉，用 `qingpdf split a.pdf --pages 1- -o b.pdf` 把整份重写一遍。

## 5b 的请求和结果：导出为 Word

| 调用 | 作用 |
|---|---|
| `export_docx(doc, id, 路径)` | 把整份文档（连没保存的改动一起）做成 Word 文件（.docx）写到路径，答 `Exported { doc, id, status, message, pages, tables, bytes }`。`status` 是 `Done`（`message` 是转换的提醒，比如某页太大被略过，没有就是空；`pages` 是进了 Word 文件的页数，`tables` 是文件里 Word 表格的个数（5d 加的字段），`bytes` 是文件大小）或 `Failed`（`message` 说原因，没有写出文件）。和保存一样先写同目录的临时文件、再改名替换，出错原文件不动。 |

和 `save` 一样，一次一个、马上做（排在画页前面），做的时候引擎线程不画页（几页的文件零点几秒，100 页的论文约一秒；窗口自己不会卡）；`cancel(id)` 在页与页之间（和一页里的下一个操作符处）停下，停下的请求没有结果、也不留临时文件（窗口里按 Esc 就是它；已经做完的请求照样会答复，收到时认得 id 不对就忽略）。文件作者不让复制文字、又没用所有者密码打开的，答 `Failed`（和 `copy_text` 同一个检查：`text::check_extraction_allowed`）；打印在进行时答 `Failed`（`a print job is running`）。命令行 `qingpdf docx` 用同一个函数 `qingpdf_core::docx::export_to_path`。

做法和限制见 `docs/decisions.md` 的"第 5 层 5b"。5d 起没有框线的表格也变 Word 表格（`tables::find`，见 "第 5 层 5d"）。最多 5000 页（`docx::MAX_PAGES`）、800 万字、3000 张不同的图；每页一份 `PAGE_WORK` 的工作量，整次最多 40 页的量（`docx` 的 `JOB_WORK`；整页是图的扫描件每页约 0.25 秒，所以一次最多转几百页），一页读不下去的略过那页，一张图超了限（解码后 48 MiB、1.28 亿像素、一页的工作量、3000 张、图共 1 GiB）只略过那张图、页的文字照留，都在 `message` 里写出来（每页最多一条）；整次用完则 `Failed`。每页有自己的解码额度（和画页一样），长的扫描件不会因为前面的页用完了额度而丢后面页的文字。

## 5c 的请求和结果：识别扫描页的文字

识别文字本身（Windows 的 `Windows.Media.Ocr`）是窗口那一边的事，引擎不碰它；引擎只管三件事：找出哪些页是扫描页、把一页画成识别用的灰度图、把识别出的字写成看不见的文字层。窗口把三件事串成一条链，一次一页（同时只留一幅页图），识别在窗口自己的另一个线程里做，引擎线程和窗口都不等它。

| 调用 | 作用 |
|---|---|
| `ocr_scan(doc, id, only)` | 找要识别的页，答 `OcrPages { pages, looked, unreadable, truncated }`。`only` 是 `None` 时找全部扫描页：**没有文字**（可见字符少于 `MAX_SCAN_TEXT_CHARS`=10，扫描件上盖的页码不算文字）**而且图片盖住页面一半以上**（`MIN_IMAGE_COVER`）的页；是 `Some(页)` 时只看那一页，只要求没有文字（人自己点了这一页，图片小也算）。已经有文字的页一律跳过；已经有我们自己的层（页面资源里带 `/QingpdfOCR` 标记的表单）的页也算做完了，不管层里有几个字，所以重复识别不会叠加。一页一页地做，插在别的请求之间，`cancel(id)` 能停。最多 `MAX_OCR_PAGES`（500）页，多了 `truncated`。 |
| `ocr_image(doc, id, page, max_side)` | 把整页（按显示后的样子，已按 `/Rotate` 和裁剪框转过）画成灰度图，答 `OcrImage { status, message, width, height, dpi, gray }`：8 位、0 是黑、从上到下、没有行填充。分辨率 `TARGET_DPI`=300，图的一边不超过 `max_side`（识别接口的 `MaxImageDimension`，这台机器 10000）、总像素不超过 `MAX_IMAGE_PIXELS`（900 万，A4 的 300 dpi 是 870 万），再低到 `MIN_OCR_DPI`（36）以下就不画（`Failed`，"too large"）。一次一个波段（和打印同一块画布），所以内存只多一幅灰度图；`cancel(id)` 停下就没有结果。 |
| `ocr_layer(doc, id, pages)` | 把识别出的字写进页面：`pages` 是 `[OcrPage { page, width, height, lines: [OcrLine { words: [OcrWord { text, rect }] }] }]`，`width`、`height` 是 `ocr_image` 给的图的像素，`rect` 是 `[左, 上, 右, 下]`（图的像素，左上角为原点）。答 `Edited`（`page` 是 `ALL_PAGES`），一步撤销，和别的编辑一样要 `Rights.modify`、有签名的文件不让改。最多 `MAX_OCR_PAGES` 页、每页 `MAX_WORDS_PER_PAGE`（3 万）个词、一次共 `MAX_WORDS_PER_RUN`（40 万）个词，超了答 `Failed`，文件不变。已经有层的页不再写第二层（消息里说跳过了几页；所有的页都有层时答 `Failed`）。 |

**文字层怎么写：** 一次编辑做所有页：一个字体（`fontsub` 的系统字体子集，装下所有页用到的字；字数超过 2500 个不同字时，用得最少的几个字不进文字层）和每页一个 Form XObject，用 `q 矩阵 cm /Qo… Do Q` 盖在原内容后面（4c 的 `put_on_page`：原内容包 `q`…`Q`，共用的 `/Resources` 复制一份）。表单里是一个文字对象，`3 Tr`（不画）。每个词放在它的框里：字号是它所在那一行（那一行所有词的框合起来）的高度，基线按我们自己的字框（`Shown::glyph_box`：基线下 0.2 个字号、上 0.8 个字号）放，水平缩放 `Tz` 按"字体里的自然宽度"算好，让词正好填满框；西文词后面跟一个空格字符，一直伸到下一个词（行里没有缝，别的软件读出来有空格）；中文、日文、韩文的词后面没有空格，但伸到下一个词的左边（没有缝，PDFium 才不会在字间插空格）。页面的 `/Rotate`、裁剪框、MediaBox 不从 0 起，都由 `upright` 矩阵转回用户空间；图就是显示后的页，所以框按（页宽 / 图宽）、（页高 / 图高）缩放就是显示后的点。字体里没有的字（任何系统字体都没有）从文字层里去掉，`Edited.message` 说去掉了几个。

**不让改的文件（有签名，或没有"允许修改内容"）：** `ocr_layer` 仍然答 `Done`，但文件一个字节都不写：识别出的字按页放在会话里（最多 20 万个字符），只给我们自己的搜索、选字、复制用（`page_chars` 看到页面自己没有文字时用它），不能撤销、不算"有改动"、保存出的文件和原来一样；`message` 里说"used in this window only"。超出内存上限的页不留，`message` 说留了几页。

阅读器：菜单"工具"→"识别所有扫描页的文字"、"识别当前页的文字"。进度显示在状态栏，Esc 停下并保留已识别的页（写成文字层）。识别用 `Windows.Media.Ocr`：中文界面用简体中文识别器，没有就提示在"设置 → 时间和语言 → 语言和区域"里添加中文语言包并列出现有的语言（我们不改系统设置）；别的界面语言用用户的语言。识别接口出错或单页超过 30 秒就取消这一操作、关掉它、跳过这一页并提示。隐藏参数 `qingpdf-view --ocr-test 输入.pdf 输出.pdf [--ocr-pages 首-末] [--ocr-lang zh|en]` 不开窗口走同一条链、存盘、打印每页用时。

## 5d 的请求和结果：导出为 Excel

| 调用 | 作用 |
|---|---|
| `export_xlsx(doc, id, 路径)` | 把整份文档（连没保存的改动一起）里的表格做成 Excel 文件（.xlsx）写到路径，一个表格一个工作表；整份文件没有表格时做一个放各页文字的工作表。答的和 `export_docx` 一样：`Exported { doc, id, status, message, pages, tables, bytes }`，`tables` 是有表格的工作表个数，**没有表格时是 0**（窗口据此提示"没有找到表格，工作表里是各页的文字"）；`message` 是转换的提醒（某页太大被略过、单元格超过 32,767 字被截断、没有表格），没有就是空。 |

排队、取消（`cancel(id)`，窗口里按 Esc）、临时文件、版权检查（`text::check_extraction_allowed`）、打印时答 `Failed` 都和 `export_docx` 一样（同一个引擎请求，只多一个"Word 还是 Excel"的标志）。命令行 `qingpdf xlsx` 用同一个函数 `qingpdf_core::xlsx::export_to_path`。窗口：文件菜单"导出为 Excel"（命令号 `CMD_EXPORT_XLSX` = 28），保存框起始名是 PDF 的名字换成 `.xlsx`。

做法和限制见 `docs/decisions.md` 的"第 5 层 5d"。最多 5000 页（`xlsx::MAX_PAGES`）、800 万字、1000 个表格（工作表）、一个单元格 32,767 个字、共享字符串共 128 MiB、工作表 XML 共 256 MiB；每页一份 `PAGE_WORK` 的工作量，整次最多 40 页的量，一页读不下去的略过那页，写在 `message` 里。

## 页面的请求和结果：转、删、调顺序、插入、提取（2026-10-15）

改的是**页面的清单**（页面树和几个相关的对象），不碰页面上的内容：页上的批注、表单、已经做的改字都跟着页走（它们挂在页面对象上，不按页码）。页码都从 0 数。和别的编辑一样走同一条撤销记录（`undo`/`redo`）、同一段增量更新（`save`）、同一个整份重写（`save_as`）。

| 调用 | 作用 |
|---|---|
| `rotate_pages(doc, id, pages, degrees)` | 把 `pages`（每页一次，顺序随便）按顺时针转 `degrees`（90 的倍数，负数反方向），加在页面自己的 `/Rotate` 上；转回页面本来（从页面树继承来）的方向时不写 `/Rotate`。答 `Edited`。 |
| `delete_pages(doc, id, pages)` | 删页，**不许删光**。从各级 `/Kids` 里拿掉、`/Count` 一路改对，一个节点没页了就整个拿掉；只在被删页上有控件的表单字段从 `/AcroForm` 的 `/Fields`、`/CO` 和父字段的 `/Kids` 里去掉（`prune.rs` 那份逻辑）；`/PageLabels`（按页码编号）在有留下的页换了位置时去掉（`Edited.message` 里说一句），`/Threads`（文章线索，里面的珠子列着页）总是去掉。指向被删页的书签和链接留着，只是不再指到任何页（`Outline.page` 是 `None`，那个链接不出现在 `Links` 里，不能点）。**被删的页还在保存后文件的旧版本部分里，`save_as` 才真正去掉。** |
| `move_pages(doc, id, pages, to)` | 把 `pages`（保持原来的先后）放到第 `to` 页前面（`to` 按移动前的页码，页数就是放到最后）。放得和原来一样答 `Failed`。页面树**保持形状**：树里放页的那些位置依次换成排好的页，所以平的树还是平的，节点里的页数不变；换了节点的页，原来从上面继承来的 `/Resources`、`/MediaBox`、`/CropBox`、`/Rotate` 写到页上，新节点给的不会改变它。`order_after_move(页数, pages, to)` 给出移动后每个位置原来是哪一页（界面用它算出移动后哪几页被选中）。 |
| `insert_pages(doc, id, at, 路径, 密码)` | 把那个文件的**全部页**放到第 `at` 页前面（页数就是放到最后）。带进来的有：页的批注，表单字段（并进 `/AcroForm`，没有就新建；根字段的 `/T` 和本文档里已有的重名就改成 `名字_2`、`_3`……；`/DR` 里没有的资源并进来（只并 Font、XObject、ExtGState、ColorSpace、Pattern、Shading 六类，条目合计超过 1 万就一概不并），`/DA`、`/Q`、`/NeedAppearances` 取没有的；**签名字段不带签名**：`/FT /Sig` 的字段（根或子字段，类型可以从上面继承）去掉 `/V`，它和它的子控件的 `/AP`（签名的样子）也去掉，字段留着但是没签名，不然保存重开后这份文件会算“有签名”，页面和内容的改动都会被拒绝），链接，用名字指向目的地的链接（换成明确的目的地数组，和 `merge` 一样）；不带的有：书签、结构树（`/StructParents` 去掉）、对方的命名目的地和 `/Names`。导入走的是命令行合并同一份代码（`Builder`、`ops::import_page`）。文件要密码而没给（或给的不对）：`Edited.status` 是 `NeedsPassword`（`WrongPassword`），界面问了再调一次。对方有限制（权限不全）又没给所有者密码：`Failed`（和 `merge` 同一条 `ops::restricts`；两个文件加密一样的除外）。对方版本比本文档高，目录里写上它的 `/Version`。 |
| `extract_pages(doc, id, pages, 路径)` | 把 `pages` 写成一个新文件，答 `Exported { pages: 写了几页, tables: 0, bytes }`。整份重写（`ops::extract_pages`，就是 `qingpdf split --pages`），算上还没保存的改动，加密的保持原来的加密；**不碰打开的文件**，路径是不是同一个文件由界面管。需要 `Rights.copy`（和 `text` 命令、导出 Word 同一条规矩）；正在打印时答 `Failed`。 |

结果：

- `Edited` 多一项 `pages: Option<Vec<PageInfo>>`：页面清单变了（上面四个调用，和它们的撤销、重做）时是新的整张清单（每页显示后的大小，和 `Opened.pages` 同一种），`page` 是 `ALL_PAGES`。界面据此把按页码存的东西全丢掉（位图、缩略图、链接、字框、批注和字段列表、搜索结果），按新清单重新排版，并重新要书签（书签的目标是页码）。没变的编辑里是 `None`。
- `EditStatus` 多三个值：`NeedsPassword`、`WrongPassword`、`TooBig`（只有 `insert_pages` 会答；`TooBig` 是对方文件的页放不进没保存的改动所占的内存，不能分几次插，所以不用 `Failed` 的通用说法：界面有自己的中英文提示，意思是先保存文件或者插小一点的文件）。
- `Rights` 多一项 `assemble`：权限位 11（修订版 3 以上）、修订版 2 看位 4；没加密或有所有者密码为真。四个改页面的调用看它，不让时答 `Denied`。**有签名的文件（任何签名、任何级别的认证）一律答 `Failed`，消息里写着 signed 或 certified。**
- 页面清单在引擎线程里改完马上重读（`Document::pages()`）并检查页数：删了几页就少几页，转和调顺序不变，插入变多；对不上就把这次改动撤掉、答 `Failed`（文件不会因此坏）。重读之后，字框缓存、没保存的识别文字（只在窗口里用的那种）都按页码存，一并丢掉。

限制（超了答 `Failed`，文件不变）：一次插入最多 `MAX_INSERT_PAGES`（5000）页、对方文件最多 `MAX_INSERT_FILE_BYTES`（16 MiB，就是没保存的改动的内存预算，两个数字是同一个：插进来的对象和文件差不多大，放不下的文件根本插不进；文件按剩下的预算和这个上限里小的那个读，多读一字节就知道超了，不看读之前量的长度），表单的字段最多 2 万个、对方的 `/DR` 条目合计最多 1 万个（超了：字段多了拒绝，`/DR` 不并）；改完全文最多 `MAX_PAGES_AFTER`（10 万）页；插进来的对象和这次改动的对象一起算在没保存的改动的内存预算里（16 MiB，超了答 `the change is too big`，后面写怎么办：先保存或分几次）；对方的命名目的地走原来的字节预算；工作量记在计量器里（每次改动 500 万单位加每页 2 万，插入再加对方每字节 8，所以一个会话里改页面的次数也受那 20 页的总额度限制）；页面树有一页或一个节点列了两遍、有环或超过 256 层，一律拒绝（读页面时那样的树能读，改它不行）。数字和理由见 `docs/decisions.md` 的“页面管理（2026-10-15）”。

## 密码和权限的请求和结果（2026-10-16）

给 文件 → 密码和权限… 用。引擎自己不产生随机数：随机数从界面来，界面从系统的密码学随机源取（Windows 的 `BCryptGenRandom`）。

| 调用 | 作用 |
|---|---|
| `protect(doc, id, 路径, 改法, 种子)` | 把文档（连没保存的改动一起）整个重写成一个新文件写到路径（路径可以就是打开的那个文件），答 `Protected { doc, id, status, message, bytes }`（`status` 是 `Done` 或 `Failed`，`message` 在 `Failed` 时说原因、在 `Done` 时是提醒）。`改法` 是 `ProtectChange::Set(Box<Protection>)`（设新的保护，替换原来的，原来没有也行）或 `ProtectChange::Remove`（去掉加密，就是 `qingpdf decrypt`）。**不碰打开的文件，也不换掉引擎里的文档**：界面写成功后自己用 `open_file` 打开写出的文件（带上新密码）。写法和保存一样：先写同目录的临时文件、再改名替换，出错原文件不动。按改动的顺序做；打印在跑时答 `Failed`。 |

`Protection { method, user_password, owner_password, permissions, encrypt_metadata }`（`qingpdf_core::security`）：`method` 是 `Method::AesV3`（AES-256，修订版 6，默认）或 `Method::AesV2`（AES-128，修订版 4）；密码是文本，空的是没有；`permissions` 是 `Permissions`（打印、高质量打印、修改内容、复制、批注、填表、组装、辅助功能提取，八个布尔）。规矩（`ops::protect`、`Security::create`，不合规矩答 `Failed`，文件没写）：关了任何一项权限就必须有所有者密码；没有任何密码又没关任何权限，什么也保护不了，拒绝；没给所有者密码时它等于用户密码；AES-256 的密码是 UTF-8（先做轻量的 SASLprep），最多 127 字节；AES-128 的密码是 PDFDocEncoding（汉字写不进去，消息里让用 AES-256），最多 32 字节；输入是加密的文件要有所有者的权利（所有者密码，或文件允许一切，和 `decrypt` 同一条）；有签名的文件（任何签名、任何级别的认证；表单之外挂着的签名字典，整个重写时也会碰到并拒绝；`Remove` 同样）拒绝。`Protection` 的 `Debug` 不显示密码，丢弃时把密码字节改写成零。`种子` 是 32 字节，来自系统随机源，每次调用新取一个，用完引擎把它改写成零；全零的种子（调用者忘了填）拒绝。同一个种子、同一个文件、同样的设置得到同样的字节（随机数全是从种子来的，所以测试可以复现）。

`Opened` 多一项 `protection: Protected { cipher, needs_password, permissions, may_change, signed }`：`cipher` 是 `None`（没加密）、`Aes256`、`Aes128`、`Rc4`（不分位数）或 `Other`；`needs_password` 是空密码打不开；`permissions` 是文件里写的权限（不是打开的人有的权利：用所有者密码打开的，`Rights` 全开，这里仍照实写）；`may_change` 是没加密、或用所有者密码打开的、或文件允许一切（`protect` 和 `decrypt` 同一条规矩）；`signed` 是有签名或认证。界面用它画对话框的"现在的保护"，并在 `may_change` 为假或 `signed` 为真时只说明原因、不去问文件名。

## 压缩的请求和结果（2026-10-16）

给 文件 → 压缩… 用。引擎做的是 `qingpdf_core::compress::compress`（命令行 `qingpdf compress` 是同一个函数）。

| 调用 | 作用 |
|---|---|
| `compress(doc, id, 路径, CompressHow { preset, lossless_only })` | 把文档（连没保存的改动一起）整个重写成一个更小的文件写到路径（路径可以就是打开的那个文件），先写同目录的临时文件、再改名替换，出错原文件不动。`preset` 是 `compress::Preset::{High, Balanced, Small}`（200 dpi 质量 85 / 150 dpi 质量 75 / 100 dpi 质量 60），`lossless_only` 为真时图片不动。做的时候隔一会儿发 `CompressProgress { doc, id, percent }`（0 到 100，只升不降，同一个数不重复发），做完答 `Compressed { doc, id, status, message, before, after, kept }`：`Done` 时 `before` 是原文件字节数、`after` 是新文件字节数、`kept` 为假；**压完不比原来小时 `Done`、`kept` 为真、`after == before`，没写任何文件**；`Failed` 时 `message` 说原因（`Done` 时 `message` 是做的提醒，没有就是空）。**不碰打开的文件，也不换掉引擎里的文档**：界面做完后自己决定要不要用 `open_file` 打开新文件。 |

规矩（不合规矩答 `Failed`，文件没写）：缩小图片要 `Rights.modify`（权限位 4，所有者密码能解除，和加水印、加页码同一条），`CompressHow.lossless_only` 不要（那是普通的整份重写，和转页、删页一样），所以界面菜单不灰，没有修改权的文件只给无损那一档；有签名的文件（任何签名、任何级别的认证）拒绝，整个重写会弄坏签名；文件超过 `view::MAX_COMPRESS_FILE`（30 MB）拒绝，消息里让用命令行；再按 `MemoryPlan::compress_room` 算：原文件、新文件、新图片（各最多一个文件那么大）、没保存的改动、窗口留着的（位图、缩略图、对象流、表单字段表）加 12 MiB 余量加起来不能超过 `TOTAL_BYTES`，超过就拒绝（大窗口里文件不小时会这样），不超过的话借给图片的是引擎画一页用的那部分（解码器、离屏层、蒙版）和剩下的两者中小的；开始前画页的缓存（字体、图片、表单、文字）先放掉，之后翻页时再填；打印在跑时答 `Failed`。加密的文件保持原来的加密（同一把文件密钥、同样的密码和权限，不需要所有者的权利，和转页、删页的整份重写一样）。按改动的顺序做；`cancel(id)`（窗口里按 Esc）在下一步停下，不答，临时文件删掉；一个在 `cancel` 之前已经做完的请求仍可能答一次，界面忽略。

引擎线程做压缩的时候不画页（和导出 Word 一样）；做图片的几根线程（最多 8 根）借用画页的内存：`decoders + image_slot + layers + masks`，再减去 3 倍文件大小的余地，不少于 16 MiB（`Options.picture_memory`）；一幅需要的内存比这还多的图不动。数字和理由见 `docs/decisions.md` 的"压缩 PDF（2026-10-16）"。

## 批注加强的请求和结果（2026-10-17）

形状、改已经在页面上的批注（我们加的和别的软件加的）、全部批注的清单。位置同样是"页面显示后的点"。这些都是 `EditClass::Annotate`：权限看 `Rights.annotate`（位 6），有所有者密码都放开，认证的文件（`/Perms /DocMDP`）`/P` 为 3 才许、为 1 或 2 答 `Failed`（消息里写着 DocMDP），和 4a 的批注一样；撤销、重做、增量保存（一次保存一段）都和 4a 共用，一次编辑一步。

| 调用 | 作用 |
|---|---|
| `add_shape(doc, id, page, kind, from, to, color, width, fill, author)` | 形状：`kind` 是 `ShapeKind::{Rectangle, Ellipse, Line, Arrow}`；矩形、椭圆的 `from`、`to` 是对角，直线、箭头是两端（箭头头在 `to`，`/LE [/None /OpenArrow]`、`/IT /LineArrow`）。`width` 夹在 `MIN_INK_WIDTH`..`MAX_INK_WIDTH`（0.25 到 36 点，不是数当 1 点）；`fill` 只对矩形、椭圆：`/IC` 是描边颜色向白色靠 70%，外观里用 `/BM /Multiply` 画，下面的字透得出来。位置夹进页内；边长（直线是长度）不到 2 点答 `Failed`。外观 `/BBox` 等于 `/Rect`、矩阵是单位阵；矩形、椭圆的路径在 `/Rect` 里缩进半个线宽，`/RD` 照实写（12.5.6.8），所以 `/Rect` 是形状向外长半个线宽；`/BS /W`、`/Contents ()`、`/Subj`、作者、`/M`、`/CreationDate` 和别的批注一样。答 `Edited`。 |
| `move_annotation(doc, id, page, index, num, delta)` | 把批注（`annotations` 给的 `index`、`num`）移动 `delta`（页面显示后的点）：改 `/Rect` 和类型的点（`/QuadPoints`、`/InkList`、`/L`、`/Vertices`、`/CL`，它的弹出框的 `/Rect`），外观原样留着（算法 8.1 把外观的框放到新的 `/Rect` 上，所以它跟着走）。移动的距离用 `view::keep_on_page` 夹：至少留 8 点（或整个，不到 8 点时）在页内。不转向的批注（`/F` 位 5，便签）照样按用户空间左上角移。答 `Edited`。 |
| `resize_annotation(doc, id, page, index, num, rect)` | 把批注放进 `rect`（`[左, 上, 右, 下]`，最小 8 点、夹进页内）。矩形、椭圆、直线、手写、文字框是我们会画的：外观**重画**（线宽、字号不跟着变；直线、手写的点按旧框到新框的比例放，框是重画后外观的框），别的（图片、多边形、折线）外观不动，由 `/Rect` 的变化把它拉伸（算法 8.1），点也按比例变。便签的图标、文字标记不能缩放。答 `Edited`。 |
| `restyle_annotation(doc, id, page, index, num, color, width, fill)` | 重画：新颜色、新线宽、填充与否（`None` 是不变）。只做我们会画的：文字标记（四种）、手写、矩形、椭圆、直线、便签、文字框；别的类型答 `Failed`。文字标记的四边形不论文件里什么顺序都先按页面上看到的重排（每行取四个角的外接框）。文字框的颜色是 `/DA` 里的文字颜色（`/C` 是背景，不动），字号和文字不变，`/RC` 去掉。线宽太粗放不进框的形状答 `Failed`。答 `Edited`。 |
| `set_annotation_text(doc, id, page, index, num, text)` | 给批注新的文字（`/Contents`，去掉 `/RC`）；文字框按新文字重画（框向下长，最多 `MAX_FREETEXT_CHARS` 字，全空答 `Failed`）。答 `Edited`。 |
| `annotation_text(doc, id, page, index, num)` | 取批注的全部文字（最多 `MAX_FULL_TEXT_CHARS`=4000 字，换行保留成换行符），答 `AnnotText { doc, id, page, index, num, found, text }`；`found` 为假是批注已经不在那个位置了，或者它的字超过 4000 字（给前 4000 字、存回去会丢后面的，所以不给；`set_annotation_text` 对这样的也答 `Failed`，消息里有 "too long"）。 |
| `annot_list(doc, id)` | 全文档的批注清单，答 `AnnotList { doc, id, items: [ListedAnnot { page, info: AnnotInfo }], truncated }`：按页、再按页里的顺序；不列链接、表单控件、弹出框和隐藏的；每页最多 `MAX_ANNOT_INFOS`（2000）个，总共最多 `MAX_LISTED_ANNOTS`（20000）个，超了 `truncated`；所有页合计看过的 `/Annots` 条目也有上限（`MAX_LIST_VISITS`=1,500,000，一万页共用一个大数组不会读两亿次），超了同样 `truncated`。一页一步，夹在画页之间，每页有自己的计量器。`info.contents` 只有第一行（最多 `MAX_LISTED_CHARS`=120 字），`parts` 是空的。 |

`AnnotInfo` 多了：`line`（直线的两端，用来点中直线本身而不是它的框）、`modified`（`/M`，写成 `2026-10-17 14:30`，不是日期就原样截短）、`flags`（`/F`），和几个问类型的方法：`locked()`（Locked 位 8 或 ReadOnly 位 7：不能移动、缩放、重画、删除）、`text_locked()`（ReadOnly 位 7 或 LockedContents 位 10：不能改字；Locked 不管内容）、`restylable()`、`has_width()`、`fillable()`、`resizable()`、`typed()`。

**被拒绝的：** 锁住的（位 8）批注，移动、缩放、重画、删除答 `Failed`（消息里有 "locked"），改字可以；只读的（位 7）都拒绝；文字锁住的（位 10）只拒绝改字。重画会丢东西的（虚线、没有边框的矩形和椭圆、云边、引线、说明文字、别的线头、手写的 `/Path`、别的名字的便签、有 `/RC`、边框、底色、`/CL`、`/Q`、`/CA` 的文字框，见 `docs/decisions.md`）重画、缩放答 `Failed`（"cannot draw again"），移动可以；移动时回复它的批注、回复的弹出框一起走，回复里有锁住的，整个移动答 `Failed`（"locked"）。文字框改字时：Locked 的框不变，排不下答 `Failed`（"does not fit"）；框挂在页外的答 `Failed`（"edge of the page"）。旧框有一边不到 1 点的不缩放，点超出 ±3.4×10^38 的不移动也不缩放（"too far"，"too small"）。链接、表单控件、弹出框答 `Failed`（"part of the page"）。直接写在页的 `/Annots` 数组里的批注（没有对象号）不能改（删除可以）。`index`、`num` 和页面现在的列表对不上（列表过期）答 `Failed`（"not there any more"）。每个点、每笔手写的数都设了上限（一个批注最多 10 万个数）并计入工作计量器；一次编辑最多一页的工作量，和 4a 一样。

## 多个文档（2026-10-17）

一个引擎拿着几个文档，窗口的每个标签一个。**服务**的只有一个（它有渲染器、字体和对象缓存、页文字缓存、排着的请求）；别的**存着**：只有文件字节、没保存的改动（撤销记录也在）和打开用的密码，渲染器、各种缓存、解码出来的对象流都放掉了（`set_object_stream_cache_limit(0)`）。换文档就是把正服务的存起来、取出另一个服务（`activate`，或 `open_file` 一个新的）。存起来的文档按“最久没服务的在前”排着（`view/store.rs`）。

**内存（总预算 200 MB 不变）：**

- 一个存着的文档占：文件字节 + 没保存的改动 + 撤销记录 + 固定的 1.25 MiB（保存时接在文件后面的 1 MiB 空位，和页面树之类的 0.25 MiB）+ 文件里每个对象 16 字节（交叉引用表的格子和流结尾的位置）。对象数取自文件，按文件大小封顶（文件里的 `/Size` 可以乱写）。还没读的新文件按每 100 字节一个对象估（`store::guess_for`）。（第一版是一律 2 MiB，大文件和对象多的文件占的比这大得多。）
- 正服务的文档的计划（`plan`）用 总预算 − 所有别的存着的文档占的，所以服务的那个得到的是“剩下的全部”，但别的文档越多，它的缓存越小。界面有两个以上标签时，一开始就把存着的标签最多能留的画面（见使用说明，40 MB）整个从总预算里扣掉，再把余下的交给引擎，不是它当时留着多少扣多少：引擎按这个数收新文件，而画面是后来随着切标签一点点长到 40 MB 的，引擎里没有什么能再让出来（第一版按当时的数扣，收满不能放的文件后总量能到 240 MB 左右）。只有一个标签时，引擎拿到整个总预算。
- 收一个新文件的条件：`存着的合计 + 新文件 + ACTIVE_FLOOR ≤ 总预算`。`ACTIVE_FLOOR`（73 MB，实测最小是 72.66 MB，所有窗口大小一样：引擎各部分砍到底线、位图缓存砍到 `plan` 肯接受的最小）是留给“下一个要被服务的文档”的：只要条件成立，任何一个存着的文档以后被服务时，`plan` 一定有解。
- 不满足时，按最久没服务的顺序放掉“可以放”的文档（没有没保存的改动、不是用密码打开的、没有只留在内存里的识别文字），每放一个先发 `Dropped`，放到够为止；放光了还不够答 `NoRoom`。文件本身超过 100 MB 或总预算根本放不下，仍答 `TooLarge`。
- 激活时重新算计划（`wake_up`）；真的算不出（不该发生）就用最小的计划，宁可超过总预算一点，也不让切换失败。

**命令顺序：** 结束服务的命令（`open_file`、`activate`、`close_doc`）排在它前面的编辑、保存、导出之后（它们先做完、先答复）；搜索、打印、识别这类长任务不续着做，界面在切走以前自己撤掉或不让切（见使用说明的“正忙的标签”）。存着的文档不处理请求：对它的请求直接丢掉。

**界面怎么用：** 新标签 = `open_file`；切到另一个标签 = `activate`（收到 `Activated` 再排画页的请求）；关标签 = `close_doc`；收到 `Dropped` 的标签，下次要用时 `open_file` 重新读（界面自己记着那个标签的页、放大、滚动位置、旋转，读完恢复）。同一时刻最多 16 个文档（界面的限制，引擎本身只受内存限制）。测试：`view/tests.rs` 里新增的 6 个，和 `view/store.rs`、`view/plan.rs` 里的算术测试。

## 对接别的语言

接口里只有整数、浮点、字符串、字节数组和没有数据的枚举，没有 Rust 特有的东西（借用、闭包、泛型）。包成 C 接口：每个调用一个函数（`add_markup`、`add_ink`、`add_note`、`delete_annotation` 各是一个函数，不是一个带数据的 `Edit` 枚举），事件取出来摊平成一个结构（`kind` 加上各字段）、位图用"指针加长度"交出去并配一个释放函数，`notify` 换成"函数指针加一个 `void*`"。鸿蒙的 NAPI 同理：`poll` 的结果转成 JS 对象，`notify` 用线程安全函数回到主线程。**这两个包装 3d-1、3d-2、4a 都没有写。**

## 内存（总预算 200 MB）

`view::plan(总预算, 文件字节, 窗口像素)` 把总预算分给：文件本身、文档的对象流缓存（16 MiB）、渲染缓存（40 MiB）、页文字缓存（16 MiB）、没保存的改动和撤销历史（16 MiB）、当前页的一幅大图（24 MiB，一页的几块共用，只解一次）、离屏层（16 MiB）、剪裁蒙版（8 MiB）、JPX/JBIG2 解码器（32 MiB）、一块位图的画布（最多 400 万像素 = 16 MiB），剩下的才是界面的位图缓存。窗口的位图先定：窗口像素 ×12 字节（一屏的块，伸出窗口的部分算半屏，再加一屏预取），引擎各项一起按比例缩小（每 1/100 一档，最低留四分之一）直到位图放得下，并且至少放得下两整块。文件超过 40 MB 时各项也按比例缩小，超过 100 MB 不打开。分法的理由和数字见 `docs/decisions.md` 的"第 3 层 3d-1"一节。**有几个文档时**（标签页，2026-10-17）：存着的文档的字节先从总预算里扣，服务的那个按剩下的算（上面的分法一样）；见上一节“多个文档”。

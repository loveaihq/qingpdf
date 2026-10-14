//! The words on the screen, in Chinese or English by the language Windows is set to.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    /// From a Windows language id (the low ten bits are the language, 0x04 is Chinese).
    pub fn from_langid(id: u16) -> Lang {
        if id & 0x3ff == 0x04 { Lang::Zh } else { Lang::En }
    }
}

/// What the person chose in View > Language: the language of Windows, or one of the two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LangChoice {
    #[default]
    System,
    Zh,
    En,
}

impl LangChoice {
    /// The language to show: the chosen one, or `system` (the language of Windows) when the choice is to follow it.
    pub fn resolve(self, system: Lang) -> Lang {
        match self {
            LangChoice::System => system,
            LangChoice::Zh => Lang::Zh,
            LangChoice::En => Lang::En,
        }
    }

    /// The word kept in the settings file.
    pub fn as_str(self) -> &'static str {
        match self {
            LangChoice::System => "system",
            LangChoice::Zh => "zh",
            LangChoice::En => "en",
        }
    }

    pub fn parse(s: &str) -> Option<LangChoice> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Some(LangChoice::System),
            "zh" => Some(LangChoice::Zh),
            "en" => Some(LangChoice::En),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    File,
    Open,
    Recent,
    ClearRecent,
    NoRecent,
    Print,
    Exit,
    View,
    ZoomIn,
    ZoomOut,
    FitWidth,
    FitPage,
    ActualSize,
    Rotate,
    ShowBookmarks,
    Edit,
    Copy,
    GoMenu,
    PageNumber,
    Find,
    FindNext,
    FindPrev,
    Help,
    About,
    AboutTitle,
    OpenTitle,
    FilterPdf,
    FilterAll,
    PasswordTitle,
    PasswordPrompt,
    PasswordWrongPrompt,
    GoToTitle,
    Ok,
    Cancel,
    Error,
    Opening,
    Hint,
    NoticeIncomplete,
    NoticeFailed,
    CannotRead,
    NotPdf,
    TooLarge,
    PageWord,
    InternalErrorTitle,
    InternalError,
    EngineFailed,
    LinkTitle,
    LinkPrompt,
    LinkAsk,
    NoteCopyDenied,
    NoteCopyFailed,
    NotePrintDenied,
    NotePrintLow,
    NoteCopyCut,
    NotePrinted,
    NotePrintCancelled,
    PrintFailed,
    FindNone,
    FindSearching,
    Untitled,
    NoPrinter,
    Save,
    SaveAs,
    Undo,
    Redo,
    CommentMenu,
    ToolSelect,
    ToolHighlight,
    ToolUnderline,
    ToolStrike,
    ToolSquiggly,
    ToolInk,
    ToolNote,
    ColorMenu,
    ColYellow,
    ColGreen,
    ColBlue,
    ColPink,
    ColRed,
    ColBlack,
    WidthMenu,
    WidthThin,
    WidthMedium,
    WidthThick,
    WidthBold,
    AuthorItem,
    RemoveAnnot,
    SaveTitle,
    AuthorTitle,
    AuthorPrompt,
    NoteTitle,
    NotePrompt,
    UnsavedTitle,
    UnsavedAsk,
    NoteSaved,
    NoteSaveFailed,
    NoteAnnotDenied,
    NoteRewrite,
    NoteMarkHint,
    NoteInkHint,
    NoteNoteHint,
    NoteEditFailed,
    NoteBusy,
    ToolTextBox,
    ToolStamp,
    TextSizeMenu,
    SizeSmall,
    SizeMedium,
    SizeLarge,
    SizeHuge,
    NoteTextBoxHint,
    NoteStampHint,
    NoteStampBad,
    FilterImages,
    StampTitle,
    NoteFilled,
    NoteFillDenied,
    NoteReadOnlyField,
    NoteSignatureField,
    NoteAdded,
    NoteRemoved,
    NoteNothingUndo,
    NoteNothingRedo,
    NoteSigned,
    InsertMenu,
    InsertText,
    InsertImage,
    InsertWatermark,
    WatermarkPrompt,
    WmOpacity,
    WmAngle,
    WmTile,
    WmUnder,
    WmPage,
    NoteModifyDenied,
    ToolEditText,
    NoteEditTextHint,
    NoteNoTextHere,
    NoteLineChanged,
    NoteLineNo,
    ShutdownReason,
    ExportWord,
    ExportTitle,
    FilterDocx,
    ExportExcel,
    ExportExcelTitle,
    FilterXlsx,
    NoteExporting,
    NoteExported,
    NoteExportedExcel,
    NoteExportedNoTables,
    NoteExportFailed,
    NoteExportCancelled,
    ToolsMenu,
    OcrAll,
    OcrPageOnly,
    OcrTitle,
    OcrLooking,
    OcrWorking,
    OcrWriting,
    OcrNothing,
    OcrBusy,
    OcrDone,
    OcrStopped,
    OcrStoppedNone,
    OcrNoText,
    OcrNotWritten,
    OcrMemoryOnly,
    OcrTooMany,
    OcrSkipped,
    OcrSkippedWhy,
    OcrIncomplete,
    OcrIncompleteWhy,
    OcrPageN,
    OcrFailed,
    OcrNoChinese,
    OcrNoLanguage,
    OcrAvailable,
    OcrNoneAvailable,
    PrevPage,
    NextPage,
    ShowToolbar,
    LanguageMenu,
    LangSystem,
    LangChinese,
    LangEnglish,
    CtxHighlight,
    CtxNote,
    CtxSelectAll,
}

/// Every message, for the tests.
#[cfg(test)]
pub const ALL: [Msg; 189] = [
    Msg::File,
    Msg::Open,
    Msg::Recent,
    Msg::ClearRecent,
    Msg::NoRecent,
    Msg::Print,
    Msg::Exit,
    Msg::View,
    Msg::ZoomIn,
    Msg::ZoomOut,
    Msg::FitWidth,
    Msg::FitPage,
    Msg::ActualSize,
    Msg::Rotate,
    Msg::ShowBookmarks,
    Msg::Edit,
    Msg::Copy,
    Msg::GoMenu,
    Msg::PageNumber,
    Msg::Find,
    Msg::FindNext,
    Msg::FindPrev,
    Msg::Help,
    Msg::About,
    Msg::AboutTitle,
    Msg::OpenTitle,
    Msg::FilterPdf,
    Msg::FilterAll,
    Msg::PasswordTitle,
    Msg::PasswordPrompt,
    Msg::PasswordWrongPrompt,
    Msg::GoToTitle,
    Msg::Ok,
    Msg::Cancel,
    Msg::Error,
    Msg::Opening,
    Msg::Hint,
    Msg::NoticeIncomplete,
    Msg::NoticeFailed,
    Msg::CannotRead,
    Msg::NotPdf,
    Msg::TooLarge,
    Msg::PageWord,
    Msg::InternalErrorTitle,
    Msg::InternalError,
    Msg::EngineFailed,
    Msg::LinkTitle,
    Msg::LinkPrompt,
    Msg::LinkAsk,
    Msg::NoteCopyDenied,
    Msg::NoteCopyFailed,
    Msg::NotePrintDenied,
    Msg::NotePrintLow,
    Msg::NoteCopyCut,
    Msg::NotePrinted,
    Msg::NotePrintCancelled,
    Msg::PrintFailed,
    Msg::FindNone,
    Msg::FindSearching,
    Msg::Untitled,
    Msg::NoPrinter,
    Msg::Save,
    Msg::SaveAs,
    Msg::Undo,
    Msg::Redo,
    Msg::CommentMenu,
    Msg::ToolSelect,
    Msg::ToolHighlight,
    Msg::ToolUnderline,
    Msg::ToolStrike,
    Msg::ToolSquiggly,
    Msg::ToolInk,
    Msg::ToolNote,
    Msg::ColorMenu,
    Msg::ColYellow,
    Msg::ColGreen,
    Msg::ColBlue,
    Msg::ColPink,
    Msg::ColRed,
    Msg::ColBlack,
    Msg::WidthMenu,
    Msg::WidthThin,
    Msg::WidthMedium,
    Msg::WidthThick,
    Msg::WidthBold,
    Msg::AuthorItem,
    Msg::RemoveAnnot,
    Msg::SaveTitle,
    Msg::AuthorTitle,
    Msg::AuthorPrompt,
    Msg::NoteTitle,
    Msg::NotePrompt,
    Msg::UnsavedTitle,
    Msg::UnsavedAsk,
    Msg::NoteSaved,
    Msg::NoteSaveFailed,
    Msg::NoteAnnotDenied,
    Msg::NoteRewrite,
    Msg::NoteMarkHint,
    Msg::NoteInkHint,
    Msg::NoteNoteHint,
    Msg::NoteEditFailed,
    Msg::NoteBusy,
    Msg::ToolTextBox,
    Msg::ToolStamp,
    Msg::TextSizeMenu,
    Msg::SizeSmall,
    Msg::SizeMedium,
    Msg::SizeLarge,
    Msg::SizeHuge,
    Msg::NoteTextBoxHint,
    Msg::NoteStampHint,
    Msg::NoteStampBad,
    Msg::FilterImages,
    Msg::StampTitle,
    Msg::NoteFilled,
    Msg::NoteFillDenied,
    Msg::NoteReadOnlyField,
    Msg::NoteSignatureField,
    Msg::NoteAdded,
    Msg::NoteRemoved,
    Msg::NoteNothingUndo,
    Msg::NoteNothingRedo,
    Msg::NoteSigned,
    Msg::InsertMenu,
    Msg::InsertText,
    Msg::InsertImage,
    Msg::InsertWatermark,
    Msg::WatermarkPrompt,
    Msg::WmOpacity,
    Msg::WmAngle,
    Msg::WmTile,
    Msg::WmUnder,
    Msg::WmPage,
    Msg::NoteModifyDenied,
    Msg::ToolEditText,
    Msg::NoteEditTextHint,
    Msg::NoteNoTextHere,
    Msg::NoteLineChanged,
    Msg::NoteLineNo,
    Msg::ShutdownReason,
    Msg::ExportWord,
    Msg::ExportTitle,
    Msg::FilterDocx,
    Msg::ExportExcel,
    Msg::ExportExcelTitle,
    Msg::FilterXlsx,
    Msg::NoteExporting,
    Msg::NoteExported,
    Msg::NoteExportedExcel,
    Msg::NoteExportedNoTables,
    Msg::NoteExportFailed,
    Msg::NoteExportCancelled,
    Msg::ToolsMenu,
    Msg::OcrAll,
    Msg::OcrPageOnly,
    Msg::OcrTitle,
    Msg::OcrLooking,
    Msg::OcrWorking,
    Msg::OcrWriting,
    Msg::OcrNothing,
    Msg::OcrBusy,
    Msg::OcrDone,
    Msg::OcrStopped,
    Msg::OcrStoppedNone,
    Msg::OcrNoText,
    Msg::OcrNotWritten,
    Msg::OcrMemoryOnly,
    Msg::OcrTooMany,
    Msg::OcrSkipped,
    Msg::OcrSkippedWhy,
    Msg::OcrIncomplete,
    Msg::OcrIncompleteWhy,
    Msg::OcrPageN,
    Msg::OcrFailed,
    Msg::OcrNoChinese,
    Msg::OcrNoLanguage,
    Msg::OcrAvailable,
    Msg::OcrNoneAvailable,
    Msg::PrevPage,
    Msg::NextPage,
    Msg::ShowToolbar,
    Msg::LanguageMenu,
    Msg::LangSystem,
    Msg::LangChinese,
    Msg::LangEnglish,
    Msg::CtxHighlight,
    Msg::CtxNote,
    Msg::CtxSelectAll,
];

pub fn t(lang: Lang, m: Msg) -> &'static str {
    let (en, zh) = match m {
        Msg::File => ("&File", "文件(&F)"),
        Msg::Open => ("&Open...\tCtrl+O", "打开(&O)...\tCtrl+O"),
        Msg::Recent => ("Open &recent", "最近打开(&R)"),
        Msg::ClearRecent => ("&Clear the list", "清空列表(&C)"),
        Msg::NoRecent => ("(none)", "（没有）"),
        Msg::Print => ("&Print...\tCtrl+P", "打印(&P)...\tCtrl+P"),
        Msg::Exit => ("E&xit", "退出(&X)"),
        Msg::View => ("&View", "视图(&V)"),
        Msg::ZoomIn => ("Zoom &in\tCtrl++", "放大(&I)\tCtrl++"),
        Msg::ZoomOut => ("Zoom &out\tCtrl+-", "缩小(&O)\tCtrl+-"),
        Msg::FitWidth => ("Fit &width\tCtrl+1", "适合宽度(&W)\tCtrl+1"),
        Msg::FitPage => ("Fit &page\tCtrl+2", "适合整页(&P)\tCtrl+2"),
        Msg::ActualSize => ("&Actual size\tCtrl+0", "实际大小(&A)\tCtrl+0"),
        Msg::Rotate => ("&Rotate clockwise\tCtrl+R", "顺时针旋转(&R)\tCtrl+R"),
        Msg::ShowBookmarks => ("Show &bookmarks\tF4", "显示书签(&B)\tF4"),
        Msg::Edit => ("&Edit", "编辑(&E)"),
        Msg::Copy => ("&Copy\tCtrl+C", "复制(&C)\tCtrl+C"),
        Msg::GoMenu => ("&Go", "转到(&G)"),
        Msg::PageNumber => ("&Page number...\tCtrl+G", "页码(&P)...\tCtrl+G"),
        Msg::Find => ("&Find...\tCtrl+F", "查找(&F)...\tCtrl+F"),
        Msg::FindNext => ("Find &next\tF3", "查找下一个(&N)\tF3"),
        Msg::FindPrev => ("Find p&revious\tShift+F3", "查找上一个(&R)\tShift+F3"),
        Msg::Help => ("&Help", "帮助(&H)"),
        Msg::About => ("&About qingpdf...", "关于 qingpdf(&A)..."),
        Msg::AboutTitle => ("About qingpdf", "关于 qingpdf"),
        Msg::OpenTitle => ("Open PDF", "打开 PDF"),
        Msg::FilterPdf => ("PDF files", "PDF 文件"),
        Msg::FilterAll => ("All files", "所有文件"),
        Msg::PasswordTitle => ("Password", "密码"),
        Msg::PasswordPrompt => ("This file needs a password:", "这个文件需要密码："),
        Msg::PasswordWrongPrompt => ("That password does not open the file. Try again:", "密码不对，请再输入一次："),
        Msg::GoToTitle => ("Go to page", "跳到页码"),
        Msg::Ok => ("OK", "确定"),
        Msg::Cancel => ("Cancel", "取消"),
        Msg::Error => ("qingpdf", "qingpdf"),
        Msg::Opening => ("Opening...", "正在打开…"),
        Msg::Hint => ("Press Ctrl+O to open a PDF, or drop one here", "按 Ctrl+O 打开 PDF，或把文件拖到这里"),
        Msg::NoticeIncomplete => ("This page is too complex to draw in full; part of it is shown.", "这一页内容太复杂，没有画完，只显示了一部分。"),
        Msg::NoticeFailed => ("This page cannot be drawn.", "这一页画不出来。"),
        Msg::CannotRead => ("The file cannot be read:", "读不了这个文件："),
        Msg::NotPdf => ("This is not a PDF file, or it is too damaged to open.", "这不是 PDF 文件，或者损坏得太厉害，打不开。"),
        Msg::TooLarge => ("This file is too big for the reader (the most is 100 MB).", "这个文件太大，阅读器打不开（上限 100 MB）。"),
        Msg::PageWord => ("Page", "第"),
        Msg::InternalErrorTitle => ("qingpdf - internal error", "qingpdf - 内部错误"),
        Msg::InternalError => ("qingpdf ran into an internal error and has to close. Nothing in your file was changed.\nWhere:", "qingpdf 遇到内部错误，必须关闭。你的文件没有被改动。\n出错的位置："),
        Msg::EngineFailed => ("The reader could not start its drawing thread. Close other programs and try again.", "阅读器启动不了画页的线程。请关掉别的程序再试。"),
        Msg::LinkTitle => ("Open link", "打开链接"),
        Msg::LinkPrompt => ("This file wants to open this address in your browser or mail program:", "这个文件要用浏览器或邮件程序打开下面的地址："),
        Msg::LinkAsk => ("Open it?", "要打开吗？"),
        Msg::NoteCopyDenied => ("The file's author does not allow copying text (the owner password is needed).", "文件作者不允许复制文字（需要所有者密码）。"),
        Msg::NoteCopyFailed => ("The text could not be put on the clipboard (another program may be using it). Try again.", "文字放不上剪贴板（可能别的程序正在用它），请再试一次。"),
        Msg::NotePrintDenied => ("The file's author does not allow printing (the owner password is needed).", "文件作者不允许打印（需要所有者密码）。"),
        Msg::NotePrintLow => ("The file's author allows only low-quality printing: at most 150 dpi.", "文件作者只允许低质量打印：最高 150 dpi。"),
        Msg::NoteCopyCut => ("The selection is very long; only the first part was copied.", "选的内容太长，只复制了前面一部分。"),
        Msg::NotePrinted => ("Printing finished.", "打印完成。"),
        Msg::NotePrintCancelled => ("Printing cancelled.", "已取消打印。"),
        Msg::PrintFailed => ("The file could not be printed:", "打印失败："),
        Msg::FindNone => ("Not found", "未找到"),
        Msg::FindSearching => ("Searching...", "正在搜索…"),
        Msg::Untitled => ("(untitled)", "（无标题）"),
        Msg::NoPrinter => ("There is no printer to print to.", "没有可用的打印机。"),
        Msg::Save => ("&Save	Ctrl+S", "保存(&S)	Ctrl+S"),
        Msg::SaveAs => ("Save &as...	Ctrl+Shift+S", "另存为(&A)...	Ctrl+Shift+S"),
        Msg::Undo => ("&Undo	Ctrl+Z", "撤销(&U)	Ctrl+Z"),
        Msg::Redo => ("Re&do	Ctrl+Y", "重做(&D)	Ctrl+Y"),
        Msg::CommentMenu => ("C&omment", "批注(&M)"),
        Msg::ToolSelect => ("&Select and pick	Esc", "选择(&S)	Esc"),
        Msg::ToolHighlight => ("&Highlight", "高亮(&H)"),
        Msg::ToolUnderline => ("&Underline", "下划线(&U)"),
        Msg::ToolStrike => ("S&trikeout", "删除线(&T)"),
        Msg::ToolSquiggly => ("Squi&ggly", "波浪线(&G)"),
        Msg::ToolInk => ("&Draw", "手写(&D)"),
        Msg::ToolNote => ("&Note", "便签(&N)"),
        Msg::ColorMenu => ("&Colour", "颜色(&C)"),
        Msg::ColYellow => ("Yellow", "黄色"),
        Msg::ColGreen => ("Green", "绿色"),
        Msg::ColBlue => ("Blue", "蓝色"),
        Msg::ColPink => ("Pink", "粉色"),
        Msg::ColRed => ("Red", "红色"),
        Msg::ColBlack => ("Black", "黑色"),
        Msg::WidthMenu => ("Line &width", "线宽(&W)"),
        Msg::WidthThin => ("Thin (1 pt)", "细（1 磅）"),
        Msg::WidthMedium => ("Medium (2 pt)", "中（2 磅）"),
        Msg::WidthThick => ("Thick (4 pt)", "粗（4 磅）"),
        Msg::WidthBold => ("Extra thick (8 pt)", "特粗（8 磅）"),
        Msg::AuthorItem => ("&Author name...", "作者名(&A)..."),
        Msg::RemoveAnnot => ("&Remove annotation	Del", "删除批注(&R)	Del"),
        Msg::SaveTitle => ("Save as (the whole file is written again)", "另存为（整个文件重写，改掉的旧字不留）"),
        Msg::AuthorTitle => ("Author name", "作者名"),
        Msg::AuthorPrompt => ("The name put on the annotations you make:", "你加的批注上署的名字："),
        Msg::NoteTitle => ("Note", "便签"),
        Msg::NotePrompt => ("The text of the note:", "便签的文字："),
        Msg::UnsavedTitle => ("Unsaved changes", "有没保存的改动"),
        Msg::UnsavedAsk => ("Save the changes to this file before it is closed?", "关闭前要保存对这个文件的改动吗？"),
        Msg::NoteSaved => ("Saved.", "已保存。"),
        Msg::NoteSaveFailed => ("The file could not be saved:", "保存失败："),
        Msg::NoteAnnotDenied => ("The file's author does not allow adding annotations (the owner password is needed).", "文件作者不允许加批注（需要所有者密码）。"),
        Msg::NoteRewrite => ("This file's structure was damaged: saving writes the whole file again instead of adding to it.", "这个文件的结构有损坏：保存时会重写整个文件，而不是在后面追加。"),
        Msg::NoteMarkHint => ("Drag over the text to mark it (Esc ends).", "拖过要标记的文字（按 Esc 结束）。"),
        Msg::NoteInkHint => ("Drag to draw (Esc ends).", "按住鼠标拖动来画（按 Esc 结束）。"),
        Msg::NoteNoteHint => ("Click where the note goes (Esc ends).", "点一下放便签的位置（按 Esc 结束）。"),
        Msg::NoteEditFailed => ("The change was not made:", "没能改成："),
        Msg::NoteBusy => ("Wait for the printing to finish first.", "请先等打印结束。"),
        Msg::ToolTextBox => ("&Text box", "文字框(&T)"),
        Msg::ToolStamp => ("Signature &image...", "签名图片(&I)..."),
        Msg::TextSizeMenu => ("Text si&ze", "字号(&Z)"),
        Msg::SizeSmall => ("Small (10 pt)", "小（10 磅）"),
        Msg::SizeMedium => ("Medium (12 pt)", "中（12 磅）"),
        Msg::SizeLarge => ("Large (16 pt)", "大（16 磅）"),
        Msg::SizeHuge => ("Huge (24 pt)", "特大（24 磅）"),
        Msg::NoteTextBoxHint => ("Drag a box for the text (or click), type, then click outside or press Ctrl+Enter. Esc ends.", "拖出放文字的框（或点一下），输入文字，点框外或按 Ctrl+Enter 完成，Esc 结束。"),
        Msg::NoteStampHint => ("Drag the picture to move it, drag a corner to resize it, Enter places it, Esc cancels.", "拖动图片来移动，拖角来缩放，按 Enter 放好，按 Esc 取消。"),
        Msg::NoteStampBad => ("That file is not a usable PNG or JPEG picture:", "这不是能用的 PNG 或 JPEG 图片："),
        Msg::FilterImages => ("Pictures (PNG, JPEG)", "图片（PNG、JPEG）"),
        Msg::StampTitle => ("Choose a picture", "选一张图片"),
        Msg::NoteFilled => ("Field filled in (Ctrl+Z undoes it).", "已填写（Ctrl+Z 撤销）。"),
        Msg::NoteFillDenied => ("The file's author does not allow filling in forms (the owner password is needed).", "文件作者不允许填写表单（需要所有者密码）。"),
        Msg::NoteReadOnlyField => ("This field is read-only.", "这个字段是只读的。"),
        Msg::NoteSignatureField => ("Signature fields cannot be signed here.", "这里不能签署签名字段。"),
        Msg::NoteAdded => ("Annotation added (Ctrl+Z undoes it).", "已加批注（Ctrl+Z 撤销）。"),
        Msg::NoteRemoved => ("Annotation removed (Ctrl+Z undoes it).", "已删除批注（Ctrl+Z 撤销）。"),
        Msg::NoteSigned => ("This file is signed: after the changes are saved, the signature will show the file as changed.", "这个文件有签名：保存改动后，签名会显示文件被修改过。"),
        Msg::InsertMenu => ("&Insert", "插入(&I)"),
        Msg::ToolEditText => ("&Edit text on the page", "改页面上的文字(&E)"),
        Msg::NoteEditTextHint => ("Click a line of text, type what it is to say, press Enter (Esc cancels). The old letters are taken out of the page.", "点一行文字，输入新的内容，按回车确认（Esc 取消）。原来的字会从页面里去掉。"),
        Msg::NoteNoTextHere => ("There is no text there.", "这里没有文字。"),
        Msg::NoteLineChanged => ("Text changed (Ctrl+Z undoes it). The old text stays in the earlier part of the saved file.", "文字已改（Ctrl+Z 撤销）。旧文字仍在保存后文件的旧版本部分里。"),
        Msg::NoteLineNo => ("This text cannot be changed:", "这行文字不能改："),
        Msg::InsertText => ("Add &text", "加文字(&T)"),
        Msg::InsertImage => ("Add &picture...", "加图片(&P)..."),
        Msg::InsertWatermark => ("Add &watermark...", "加水印(&W)..."),
        Msg::WatermarkPrompt => ("The text of the watermark:", "水印文字："),
        Msg::WmOpacity => ("&Opacity of the watermark", "水印透明度(&O)"),
        Msg::WmAngle => ("&Angle of the watermark", "水印角度(&A)"),
        Msg::WmTile => ("Ti&led over the page", "平铺满页(&L)"),
        Msg::WmUnder => ("&Under the page's content", "垫在页面内容下面(&U)"),
        Msg::WmPage => ("This pa&ge only", "只加在当前页(&G)"),
        Msg::NoteModifyDenied => ("The file's author does not allow changing its content (the owner password is needed).", "文件作者不允许修改内容（需要所有者密码）。"),
        Msg::ShutdownReason => ("This PDF has unsaved changes.", "这个 PDF 有没保存的改动。"),
        Msg::ExportWord => ("Export to &Word...", "导出为 Word(&W)..."),
        Msg::ExportTitle => ("Export to Word", "导出为 Word"),
        Msg::FilterDocx => ("Word documents", "Word 文档"),
        Msg::ExportExcel => ("Export to &Excel...", "导出为 Excel(&E)..."),
        Msg::ExportExcelTitle => ("Export to Excel", "导出为 Excel"),
        Msg::FilterXlsx => ("Excel workbooks", "Excel 工作簿"),
        Msg::NoteExporting => ("Exporting...", "正在导出……"),
        Msg::NoteExported => ("Exported to Word.", "已导出为 Word。"),
        Msg::NoteExportedExcel => ("Exported to Excel.", "已导出为 Excel。"),
        Msg::NoteExportedNoTables => ("Exported to Excel: no table was found, so the sheet has the text of the pages.", "已导出为 Excel：没有找到表格，工作表里是各页的文字。"),
        Msg::NoteExportFailed => ("The file could not be exported:", "导出失败："),
        Msg::NoteExportCancelled => ("Export cancelled.", "已取消导出。"),
        Msg::ToolsMenu => ("&Tools", "工具(&T)"),
        Msg::OcrAll => ("Recognize text in all &scanned pages", "识别所有扫描页的文字(&S)"),
        Msg::OcrPageOnly => ("Recognize text on this &page", "识别当前页的文字(&P)"),
        Msg::OcrTitle => ("Recognize text", "识别文字"),
        Msg::OcrLooking => ("Looking for scanned pages... (Esc to stop)", "正在查找扫描页……（按 Esc 停止）"),
        Msg::OcrWorking => ("Recognizing text: page {n} of {total} (Esc to stop)", "正在识别文字：第 {n} / {total} 页（按 Esc 停止）"),
        Msg::OcrWriting => ("Adding the text layer...", "正在加入文字层……"),
        Msg::OcrNothing => ("No page to recognize: pages that already have text are skipped.", "没有要识别的页：已有文字的页会被跳过。"),
        Msg::OcrBusy => ("Text is being recognized already (Esc stops it).", "正在识别文字（按 Esc 可停止）。"),
        Msg::OcrDone => ("Recognized {pages} page(s), {chars} characters. They can be searched and copied; Undo takes the text out.", "已识别 {pages} 页、{chars} 个字符，现在可以搜索和复制；撤销可去掉识别出的文字。"),
        Msg::OcrStopped => ("Stopped. {pages} page(s) were recognized ({chars} characters); Undo takes the text out.", "已停止。已识别 {pages} 页（{chars} 个字符）；撤销可去掉识别出的文字。"),
        Msg::OcrStoppedNone => ("Stopped; no text was recognized.", "已停止，没有识别出文字。"),
        Msg::OcrNoText => ("No text was found on the pages.", "页面上没有识别出文字。"),
        Msg::OcrNotWritten => ("The recognized text could not be added:", "识别出的文字没能加入文件："),
        Msg::OcrMemoryOnly => ("The file may not be changed, so the text is used in this window only and is not saved.", "文件不允许修改，识别出的文字只在本窗口里使用，不会保存。"),
        Msg::OcrTooMany => ("There were more pages than one run does; run it again for the rest.", "页数超过一次能识别的数量，剩下的请再识别一次。"),
        Msg::OcrSkipped => ("{n} page(s) were skipped.", "跳过了 {n} 页。"),
        Msg::OcrSkippedWhy => ("These pages were skipped:", "这些页被跳过了："),
        Msg::OcrIncomplete => ("{n} page(s) could be drawn only in part, so some of their text may be missing.", "有 {n} 页只画出了一部分，其中的文字可能不全。"),
        Msg::OcrIncompleteWhy => ("These pages were drawn only in part:", "这些页只画出了一部分："),
        Msg::OcrPageN => ("Page {n}", "第 {n} 页"),
        Msg::OcrFailed => ("Text recognition failed:", "文字识别失败："),
        Msg::OcrNoChinese => ("Windows has no Chinese text recognition installed. Add the Chinese language in Settings > Time & language > Language & region, then try again. (qingpdf does not change system settings.)", "这台电脑没有安装中文文字识别。请在“设置 → 时间和语言 → 语言和区域”里添加中文语言包，然后再试。（qingpdf 不会改动系统设置。）"),
        Msg::OcrNoLanguage => ("Windows has no text recognition installed. Add a language in Settings > Time & language > Language & region, then try again. (qingpdf does not change system settings.)", "这台电脑没有安装文字识别。请在“设置 → 时间和语言 → 语言和区域”里添加语言包，然后再试。（qingpdf 不会改动系统设置。）"),
        Msg::OcrAvailable => ("Languages with text recognition on this computer:", "这台电脑上可用于识别的语言："),
        Msg::OcrNoneAvailable => ("none", "没有"),
        Msg::NoteNothingUndo => ("Nothing to undo.", "没有可撤销的。"),
        Msg::NoteNothingRedo => ("Nothing to redo.", "没有可重做的。"),
        Msg::PrevPage => ("Pre&vious page	Ctrl+PgUp", "上一页(&U)	Ctrl+PgUp"),
        Msg::NextPage => ("&Next page	Ctrl+PgDn", "下一页(&N)	Ctrl+PgDn"),
        Msg::ShowToolbar => ("&Toolbar", "工具栏(&T)"),
        Msg::LanguageMenu => ("&Language", "语言(&L)"),
        Msg::LangSystem => ("Follow &system", "跟随系统(&S)"),
        Msg::LangChinese => ("中文", "中文"),
        Msg::LangEnglish => ("English", "English"),
        Msg::CtxHighlight => ("&Highlight selection", "高亮所选文字(&H)"),
        Msg::CtxNote => ("Add &note here", "在此添加便签(&N)"),
        Msg::CtxSelectAll => ("Select &all on this page", "选择本页全部文字(&A)"),
    };
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// "Page 3 / 468" or "第 3 / 468 页".
pub fn page_of(lang: Lang, page: usize, total: usize) -> String {
    match lang {
        Lang::En => format!("{} {page} / {total}", t(lang, Msg::PageWord)),
        Lang::Zh => format!("{} {page} / {total} 页", t(lang, Msg::PageWord)),
    }
}

/// "Copied 120 characters" or "已复制 120 个字".
pub fn copied(lang: Lang, characters: usize) -> String {
    match lang {
        Lang::En => format!("Copied {characters} character{}", if characters == 1 { "" } else { "s" }),
        Lang::Zh => format!("已复制 {characters} 个字"),
    }
}

/// "Printing page 2 of 3 (Esc cancels)" or "正在打印第 2 / 3 页（按 Esc 取消）".
pub fn printing(lang: Lang, page: usize, total: usize) -> String {
    match lang {
        Lang::En => format!("Printing page {page} of {total} (Esc cancels)"),
        Lang::Zh => format!("正在打印第 {page} / {total} 页（按 Esc 取消）"),
    }
}

/// The count shown by the find box: "3 / 17", "3 / 10000+" when the search stopped at its limit, and with how far the search
/// is while it still runs ("3 / 17 - searching 120/468").
pub fn found(lang: Lang, number: usize, total: usize, limit: bool, progress: Option<(u32, u32)>) -> String {
    let plus = if limit { "+" } else { "" };
    let mut s = format!("{number} / {total}{plus}");
    if let Some((done, of)) = progress {
        match lang {
            Lang::En => s.push_str(&format!(" - searching {done}/{of}")),
            Lang::Zh => s.push_str(&format!(" - 搜索中 {done}/{of}")),
        }
    }
    s
}

/// The text of the About box.
pub fn about(lang: Lang) -> String {
    let version = env!("CARGO_PKG_VERSION");
    match lang {
        Lang::En => format!("qingpdf {version}\nA small, fast, offline PDF reader.\n\nThe licences of the parts it is built from are in THIRD-PARTY-NOTICES.md."),
        Lang::Zh => format!("qingpdf {version}\n小、快、离线的 PDF 阅读器。\n\n它用到的第三方部分的许可证见 THIRD-PARTY-NOTICES.md。"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_has_text_in_both_languages() {
        for m in ALL {
            assert!(!t(Lang::En, m).is_empty() && !t(Lang::Zh, m).is_empty(), "{m:?}");
            // The names of the languages are written in each language itself, in every language.
            assert!(t(Lang::En, m).is_ascii() || m == Msg::LangChinese, "{m:?}");
        }
        // The Chinese ones are Chinese (except the name of the program).
        for m in ALL {
            if m != Msg::Error {
                assert!(!t(Lang::Zh, m).is_ascii() || t(Lang::Zh, m) == t(Lang::En, m), "{m:?}");
            }
        }
        assert_eq!(ALL.len(), 189);
        // No message is listed twice.
        for (i, a) in ALL.iter().enumerate() {
            assert!(!ALL[i + 1..].contains(a), "{a:?} twice");
        }
    }

    #[test]
    fn menu_items_have_their_shortcut_after_a_tab_and_no_two_in_a_menu_share_a_letter() {
        let go = [Msg::Copy, Msg::Find, Msg::FindNext, Msg::FindPrev];
        for lang in [Lang::En, Lang::Zh] {
            let mnemonics: Vec<char> = go.iter().filter_map(|&m| t(lang, m).split('\t').next().and_then(|s| s.split('&').nth(1)).or_else(|| t(lang, m).split('&').nth(1)).and_then(|s| s.chars().next())).collect();
            let mut sorted = mnemonics.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), go.len(), "{lang:?}: {mnemonics:?}");
        }
    }

    #[test]
    fn the_language_follows_the_windows_language_id() {
        assert_eq!(Lang::from_langid(0x0804), Lang::Zh);
        assert_eq!(Lang::from_langid(0x0404), Lang::Zh);
        assert_eq!(Lang::from_langid(0x0409), Lang::En);
        assert_eq!(Lang::from_langid(0), Lang::En);
    }

    #[test]
    fn a_chosen_language_wins_over_the_system_one() {
        assert_eq!(LangChoice::System.resolve(Lang::Zh), Lang::Zh);
        assert_eq!(LangChoice::System.resolve(Lang::En), Lang::En);
        assert_eq!(LangChoice::Zh.resolve(Lang::En), Lang::Zh);
        assert_eq!(LangChoice::En.resolve(Lang::Zh), Lang::En);
        for c in [LangChoice::System, LangChoice::Zh, LangChoice::En] {
            assert_eq!(LangChoice::parse(c.as_str()), Some(c));
        }
        assert_eq!(LangChoice::parse(" ZH "), Some(LangChoice::Zh));
        assert_eq!(LangChoice::parse("fr"), None);
    }

    #[test]
    fn numbers_in_sentences_read_naturally() {
        assert_eq!(page_of(Lang::En, 3, 468), "Page 3 / 468");
        assert_eq!(page_of(Lang::Zh, 3, 468), "第 3 / 468 页");
        assert_eq!(copied(Lang::En, 1), "Copied 1 character");
        assert_eq!(copied(Lang::Zh, 12), "已复制 12 个字");
        assert_eq!(printing(Lang::Zh, 2, 3), "正在打印第 2 / 3 页（按 Esc 取消）");
        assert_eq!(found(Lang::En, 3, 17, false, None), "3 / 17");
        assert_eq!(found(Lang::En, 3, 10000, true, Some((120, 468))), "3 / 10000+ - searching 120/468");
        assert!(about(Lang::En).contains("THIRD-PARTY-NOTICES.md") && about(Lang::Zh).contains(env!("CARGO_PKG_VERSION")));
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    File,
    Open,
    Exit,
    View,
    ZoomIn,
    ZoomOut,
    FitWidth,
    FitPage,
    ActualSize,
    Rotate,
    GoToPage,
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
}

/// Every message, for the tests.
#[cfg(test)]
pub const ALL: [Msg; 32] = [
    Msg::File,
    Msg::Open,
    Msg::Exit,
    Msg::View,
    Msg::ZoomIn,
    Msg::ZoomOut,
    Msg::FitWidth,
    Msg::FitPage,
    Msg::ActualSize,
    Msg::Rotate,
    Msg::GoToPage,
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
];

pub fn t(lang: Lang, m: Msg) -> &'static str {
    let (en, zh) = match m {
        Msg::File => ("&File", "文件(&F)"),
        Msg::Open => ("&Open...\tCtrl+O", "打开(&O)...\tCtrl+O"),
        Msg::Exit => ("E&xit", "退出(&X)"),
        Msg::View => ("&View", "查看(&V)"),
        Msg::ZoomIn => ("Zoom &in\tCtrl++", "放大(&I)\tCtrl++"),
        Msg::ZoomOut => ("Zoom &out\tCtrl+-", "缩小(&O)\tCtrl+-"),
        Msg::FitWidth => ("Fit &width\tCtrl+1", "适合宽度(&W)\tCtrl+1"),
        Msg::FitPage => ("Fit &page\tCtrl+2", "适合整页(&P)\tCtrl+2"),
        Msg::ActualSize => ("&Actual size\tCtrl+0", "实际大小(&A)\tCtrl+0"),
        Msg::Rotate => ("&Rotate clockwise\tCtrl+R", "顺时针旋转(&R)\tCtrl+R"),
        Msg::GoToPage => ("&Go to page...\tCtrl+G", "跳到页码(&G)...\tCtrl+G"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_has_text_in_both_languages() {
        for m in ALL {
            assert!(!t(Lang::En, m).is_empty() && !t(Lang::Zh, m).is_empty(), "{m:?}");
            assert!(t(Lang::En, m).is_ascii(), "{m:?}");
        }
        // The Chinese ones are Chinese (except the name of the program).
        for m in ALL {
            if m != Msg::Error {
                assert!(!t(Lang::Zh, m).is_ascii() || t(Lang::Zh, m) == t(Lang::En, m), "{m:?}");
            }
        }
        assert_eq!(ALL.len(), 32);
    }

    #[test]
    fn the_language_follows_the_windows_language_id() {
        assert_eq!(Lang::from_langid(0x0804), Lang::Zh);
        assert_eq!(Lang::from_langid(0x0404), Lang::Zh);
        assert_eq!(Lang::from_langid(0x0409), Lang::En);
        assert_eq!(Lang::from_langid(0), Lang::En);
    }

    #[test]
    fn page_numbers_read_naturally() {
        assert_eq!(page_of(Lang::En, 3, 468), "Page 3 / 468");
        assert_eq!(page_of(Lang::Zh, 3, 468), "第 3 / 468 页");
    }
}

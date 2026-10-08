//! Fonts the PDF does not carry (ISO 32000-1 9.6.2.2 for the standard 14, 9.8 for the flags of the font
//! descriptor): the system font that stands in for them.
//!
//! Nothing is read until a page needs a font. A request is turned into a list of file names, the first
//! that exists in the system font folders is opened, and the result (even a miss) is kept for the rest of
//! the process. Only the small tables of a font file are read; its glyph records stay in the file
//! (`glyf::TtFont::from_file`). The fonts kept together are limited to [`MAX_BYTES`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::text::data::Ordering;

use super::glyf::TtFont;

/// Bytes of font tables kept for the process.
const MAX_BYTES: usize = 96 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Script {
    Gb1,
    Cns1,
    Japan1,
    Korea1,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Cjk {
    Song,
    Hei,
    Kai,
    Fang,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Kind {
    Sans,
    Serif,
    Mono,
    Georgia,
    Verdana,
    Tahoma,
    Calibri,
    Cambria,
    Consolas,
    SegoeUi,
    Symbol,
    Dingbats,
    Cjk(Script, Cjk),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Request {
    pub kind: Kind,
    pub bold: bool,
    pub italic: bool,
}

// --- what a font name and descriptor ask for ---------------------------------------------------------

/// The lower-case ASCII letters and digits of a name.
fn squash(name: &[u8]) -> String {
    name.iter().filter(|b| b.is_ascii_alphanumeric()).map(|b| char::from(b.to_ascii_lowercase())).collect()
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Which Chinese, Japanese or Korean font a name asks for (names in ASCII, GBK or UTF-8).
fn cjk_of_name(raw: &[u8], squashed: &str, ordering: Option<Ordering>) -> Option<(Script, Cjk)> {
    let any = |keys: &[&str]| keys.iter().any(|k| squashed.contains(k));
    let gbk = |b: &[u8]| has(raw, b);
    let utf8 = |s: &str| has(raw, s.as_bytes());
    // 宋体 黑体 楷体 仿宋 in GBK and in UTF-8
    let song = gbk(b"\xCB\xCE\xCC\xE5") || utf8("宋") || any(&["simsun", "nsimsun", "stsong", "songti", "fzsong", "song"]);
    let hei = gbk(b"\xBA\xDA\xCC\xE5") || utf8("黑") || utf8("雅黑") || any(&["simhei", "heiti", "stheiti", "yahei", "msyh", "fzhei", "dengxian", "pingfang"]);
    let kai = gbk(b"\xBF\xAC\xCC\xE5") || utf8("楷") || any(&["simkai", "kaiti", "stkaiti", "fzkai"]);
    let fang = gbk(b"\xB7\xC2\xCB\xCE") || utf8("仿宋") || any(&["fangsong", "simfang", "fzfangsong"]);
    let by_name = if fang {
        Some((Script::Gb1, Cjk::Fang))
    } else if kai {
        Some((Script::Gb1, Cjk::Kai))
    } else if hei {
        Some((Script::Gb1, Cjk::Hei))
    } else if song {
        Some((Script::Gb1, Cjk::Song))
    } else if any(&["jhenghei", "msjh", "mingliuhkscs"]) {
        Some((Script::Cns1, Cjk::Hei))
    } else if any(&["mingliu", "pmingliu", "msung", "mingti", "kaiu", "stfangsong"]) {
        Some((Script::Cns1, Cjk::Song))
    } else if any(&["mincho", "heiseimin", "ryumin", "kozmin", "yumin", "ipamincho"]) {
        Some((Script::Japan1, Cjk::Song))
    } else if any(&["msgothic", "heiseikaku", "kakugo", "kozgo", "yugothic", "meiryo", "ipagothic", "gothicbbb", "midashigo"]) {
        Some((Script::Japan1, Cjk::Hei))
    } else if any(&["myeongjo", "batang", "hysmyeongjo", "stfangsong"]) {
        Some((Script::Korea1, Cjk::Song))
    } else if any(&["malgun", "gulim", "dotum", "hygothic", "hyhead", "hygtr", "gungsuh"]) {
        Some((Script::Korea1, Cjk::Hei))
    } else {
        None
    };
    match (by_name, ordering) {
        // The collection decides the script; the name, the style within it.
        (Some((_, style)), Some(o)) => Some((script_of(o), style)),
        (Some(found), None) => Some(found),
        (None, Some(o)) => Some((script_of(o), Cjk::Song)),
        (None, None) => None,
    }
}

fn script_of(o: Ordering) -> Script {
    match o {
        Ordering::Gb1 => Script::Gb1,
        Ordering::Cns1 => Script::Cns1,
        Ordering::Japan1 => Script::Japan1,
        Ordering::Korea1 => Script::Korea1,
    }
}

/// The Chinese fonts that PDFium (and the viewers of Chinese PDFs before it) read a simple TrueType font
/// of as GBK text: 宋体, 楷体, 黑体, 仿宋, 新宋 in GBK, as the first four bytes of the name.
pub(crate) fn is_gbk_font_name(raw: &[u8]) -> bool {
    const NAMES: [&[u8]; 5] = [b"\xCB\xCE\xCC\xE5", b"\xBF\xAC\xCC\xE5", b"\xBA\xDA\xCC\xE5", b"\xB7\xC2\xCB\xCE", b"\xD0\xC2\xCB\xCE"];
    NAMES.iter().any(|n| raw.starts_with(n))
}

/// What a font that is not embedded asks for: its name (the raw bytes of `/BaseFont`, subset tag
/// removed), the descriptor's flags (9.8.2), weight and italic angle, and the character collection.
pub(crate) fn request_for(raw_name: &[u8], flags: i64, weight: f64, italic_angle: f64, ordering: Option<Ordering>) -> Request {
    let squashed = squash(raw_name);
    let any = |keys: &[&str]| keys.iter().any(|k| squashed.contains(k));
    let bold = any(&["bold", "black", "heavy", "demi", "semibold"]) || flags & (1 << 18) != 0 || weight >= 700.0;
    let italic = any(&["italic", "oblique", "ital"]) || flags & 64 != 0 || italic_angle != 0.0;
    let mono = flags & 1 != 0 || any(&["courier", "mono", "consolas", "typewriter", "lucidaconsole"]);
    let serif = flags & 2 != 0 || any(&["times", "serif", "roman", "georgia", "garamond", "palatino", "bookman", "century", "minion", "cambria"]);
    let kind = if let Some((script, style)) = cjk_of_name(raw_name, &squashed, ordering) {
        Kind::Cjk(script, style)
    } else if squashed.starts_with("zapfdingbats") || squashed.contains("dingbats") {
        Kind::Dingbats
    } else if squashed.starts_with("symbol") && !squashed.contains("symbolset") {
        Kind::Symbol
    } else if any(&["georgia"]) {
        Kind::Georgia
    } else if any(&["verdana"]) {
        Kind::Verdana
    } else if any(&["tahoma"]) {
        Kind::Tahoma
    } else if any(&["calibri"]) {
        Kind::Calibri
    } else if any(&["cambria"]) {
        Kind::Cambria
    } else if any(&["consolas"]) {
        Kind::Consolas
    } else if any(&["segoeui"]) {
        Kind::SegoeUi
    } else if mono {
        Kind::Mono
    } else if any(&["arial", "helvetica", "sans", "swiss", "calibri"]) {
        Kind::Sans
    } else if serif {
        Kind::Serif
    } else {
        Kind::Sans
    };
    Request { kind, bold, italic }
}

// --- where the files are ---------------------------------------------------------------------------------

type Style = &'static [(&'static str, usize)];

/// Candidate files of a face by style: regular, bold, italic, bold italic (file, font number in the file).
fn files(kind: Kind) -> [Style; 4] {
    match kind {
        Kind::Sans => [
            &[("arial.ttf", 0), ("Arial.ttf", 0), ("LiberationSans-Regular.ttf", 0), ("DejaVuSans.ttf", 0)],
            &[("arialbd.ttf", 0), ("Arial Bold.ttf", 0), ("LiberationSans-Bold.ttf", 0), ("DejaVuSans-Bold.ttf", 0)],
            &[("ariali.ttf", 0), ("Arial Italic.ttf", 0), ("LiberationSans-Italic.ttf", 0), ("DejaVuSans-Oblique.ttf", 0)],
            &[("arialbi.ttf", 0), ("Arial Bold Italic.ttf", 0), ("LiberationSans-BoldItalic.ttf", 0), ("DejaVuSans-BoldOblique.ttf", 0)],
        ],
        Kind::Serif => [
            &[("times.ttf", 0), ("Times New Roman.ttf", 0), ("LiberationSerif-Regular.ttf", 0), ("DejaVuSerif.ttf", 0)],
            &[("timesbd.ttf", 0), ("Times New Roman Bold.ttf", 0), ("LiberationSerif-Bold.ttf", 0), ("DejaVuSerif-Bold.ttf", 0)],
            &[("timesi.ttf", 0), ("Times New Roman Italic.ttf", 0), ("LiberationSerif-Italic.ttf", 0), ("DejaVuSerif-Italic.ttf", 0)],
            &[("timesbi.ttf", 0), ("Times New Roman Bold Italic.ttf", 0), ("LiberationSerif-BoldItalic.ttf", 0), ("DejaVuSerif-BoldItalic.ttf", 0)],
        ],
        Kind::Mono => [
            &[("cour.ttf", 0), ("Courier New.ttf", 0), ("LiberationMono-Regular.ttf", 0), ("DejaVuSansMono.ttf", 0)],
            &[("courbd.ttf", 0), ("Courier New Bold.ttf", 0), ("LiberationMono-Bold.ttf", 0), ("DejaVuSansMono-Bold.ttf", 0)],
            &[("couri.ttf", 0), ("Courier New Italic.ttf", 0), ("LiberationMono-Italic.ttf", 0), ("DejaVuSansMono-Oblique.ttf", 0)],
            &[("courbi.ttf", 0), ("Courier New Bold Italic.ttf", 0), ("LiberationMono-BoldItalic.ttf", 0), ("DejaVuSansMono-BoldOblique.ttf", 0)],
        ],
        Kind::Georgia => [&[("georgia.ttf", 0)], &[("georgiab.ttf", 0)], &[("georgiai.ttf", 0)], &[("georgiaz.ttf", 0)]],
        Kind::Verdana => [&[("verdana.ttf", 0)], &[("verdanab.ttf", 0)], &[("verdanai.ttf", 0)], &[("verdanaz.ttf", 0)]],
        Kind::Tahoma => [&[("tahoma.ttf", 0)], &[("tahomabd.ttf", 0)], &[("tahoma.ttf", 0)], &[("tahomabd.ttf", 0)]],
        Kind::Calibri => [&[("calibri.ttf", 0)], &[("calibrib.ttf", 0)], &[("calibrii.ttf", 0)], &[("calibriz.ttf", 0)]],
        Kind::Cambria => [&[("cambria.ttc", 0)], &[("cambriab.ttf", 0)], &[("cambriai.ttf", 0)], &[("cambriaz.ttf", 0)]],
        Kind::Consolas => [&[("consola.ttf", 0)], &[("consolab.ttf", 0)], &[("consolai.ttf", 0)], &[("consolaz.ttf", 0)]],
        Kind::SegoeUi => [&[("segoeui.ttf", 0)], &[("segoeuib.ttf", 0)], &[("segoeuii.ttf", 0)], &[("segoeuiz.ttf", 0)]],
        Kind::Symbol => [&[("symbol.ttf", 0), ("Symbol.ttf", 0)], &[("symbol.ttf", 0), ("Symbol.ttf", 0)], &[("symbol.ttf", 0), ("Symbol.ttf", 0)], &[("symbol.ttf", 0), ("Symbol.ttf", 0)]],
        Kind::Dingbats => {
            const D: Style = &[("seguisym.ttf", 0), ("Zapf Dingbats.ttf", 0), ("ZapfDingbats.ttf", 0), ("DejaVuSans.ttf", 0)];
            [D, D, D, D]
        }
        Kind::Cjk(script, style) => {
            let list: Style = match (script, style) {
                (Script::Gb1, Cjk::Hei) => &[("simhei.ttf", 0), ("msyh.ttc", 0), ("Songti.ttc", 0), ("simsun.ttc", 0)],
                (Script::Gb1, Cjk::Kai) => &[("simkai.ttf", 0), ("STKaiti.ttf", 0), ("simsun.ttc", 0), ("msyh.ttc", 0)],
                (Script::Gb1, Cjk::Fang) => &[("simfang.ttf", 0), ("STFangsong.ttf", 0), ("simsun.ttc", 0), ("msyh.ttc", 0)],
                (Script::Gb1, Cjk::Song) => &[("simsun.ttc", 0), ("simsun.ttf", 0), ("STSong.ttf", 0), ("Songti.ttc", 0), ("simhei.ttf", 0), ("msyh.ttc", 0)],
                (Script::Cns1, Cjk::Hei) => &[("msjh.ttc", 0), ("mingliu.ttc", 0), ("kaiu.ttf", 0), ("msyh.ttc", 0)],
                // (mingliub.ttc holds only the Extension B characters of the MingLiU family.)
                (Script::Cns1, _) => &[("mingliu.ttc", 0), ("msjh.ttc", 0), ("kaiu.ttf", 0), ("msyh.ttc", 0)],
                (Script::Japan1, Cjk::Hei) => &[("msgothic.ttc", 0), ("YuGothR.ttc", 0), ("meiryo.ttc", 0), ("msmincho.ttc", 0)],
                (Script::Japan1, _) => &[("msmincho.ttc", 0), ("yumin.ttf", 0), ("msgothic.ttc", 0), ("YuGothR.ttc", 0)],
                (Script::Korea1, Cjk::Hei) => &[("malgun.ttf", 0), ("gulim.ttc", 0), ("batang.ttc", 0), ("AppleGothic.ttf", 0)],
                (Script::Korea1, _) => &[("batang.ttc", 0), ("malgun.ttf", 0), ("gulim.ttc", 0), ("AppleGothic.ttf", 0)],
            };
            [list, list, list, list]
        }
    }
}

fn home(rest: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest))
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["WINDIR", "SystemRoot"] {
            if let Some(w) = std::env::var_os(var) {
                dirs.push(PathBuf::from(w).join("Fonts"));
                break;
            }
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(l).join("Microsoft").join("Windows").join("Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        for d in ["/System/Library/Fonts", "/System/Library/Fonts/Supplemental", "/Library/Fonts"] {
            dirs.push(PathBuf::from(d));
        }
        dirs.extend(home("Library/Fonts"));
    } else {
        for d in [
            "/usr/share/fonts/truetype/liberation",
            "/usr/share/fonts/truetype/liberation2",
            "/usr/share/fonts/liberation",
            "/usr/share/fonts/truetype/dejavu",
            "/usr/share/fonts/dejavu",
            "/usr/share/fonts/truetype/wqy",
            "/usr/share/fonts/truetype/arphic",
            "/usr/share/fonts/truetype/noto",
            "/usr/share/fonts/truetype",
            "/usr/share/fonts",
            "/usr/local/share/fonts",
        ] {
            dirs.push(PathBuf::from(d));
        }
        dirs.extend(home(".fonts"));
        dirs.extend(home(".local/share/fonts"));
    }
    dirs
}

// --- the process-wide cache -------------------------------------------------------------------------------

#[derive(Default)]
struct Cache {
    dirs: Option<Vec<PathBuf>>,
    /// Per request: the font, or `None` for no usable file.
    found: HashMap<Request, Option<Arc<TtFont>>>,
    /// Per (file, number): the opened font, so that two requests for one file share it.
    opened: HashMap<(String, usize), Option<Arc<TtFont>>>,
    bytes: usize,
}

static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

/// The system font for a request, `None` when the system has none that fits.
pub(crate) fn load(req: Request) -> Option<Arc<TtFont>> {
    let mut cache = CACHE.get_or_init(|| Mutex::new(Cache::default())).lock().ok()?;
    if let Some(hit) = cache.found.get(&req) {
        return hit.clone();
    }
    let dirs = cache.dirs.get_or_insert_with(font_dirs).clone();
    let all = files(req.kind);
    // The style asked for first, then the nearer ones.
    let order: [usize; 4] = match (req.bold, req.italic) {
        (true, true) => [3, 1, 2, 0],
        (true, false) => [1, 0, 3, 2],
        (false, true) => [2, 0, 3, 1],
        (false, false) => [0, 1, 2, 3],
    };
    let mut result = None;
    'search: for style in order {
        for &(file, face) in all.get(style).copied().unwrap_or_default() {
            for dir in &dirs {
                let path = dir.join(file);
                if !path.is_file() {
                    continue;
                }
                let key = (path.to_string_lossy().into_owned(), face);
                let font = match cache.opened.get(&key) {
                    Some(f) => f.clone(),
                    None => {
                        let opened = if cache.bytes >= MAX_BYTES { None } else { TtFont::from_file(&path, face).ok().map(Arc::new) };
                        if let Some(f) = &opened {
                            cache.bytes = cache.bytes.saturating_add(f.memory());
                        }
                        cache.opened.insert(key, opened.clone());
                        opened
                    }
                };
                if font.is_some() {
                    result = font;
                    break 'search;
                }
            }
        }
    }
    cache.found.insert(req, result.clone());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_ask_for_the_right_family() {
        let r = |name: &str, flags: i64, ordering: Option<Ordering>| request_for(name.as_bytes(), flags, 0.0, 0.0, ordering);
        assert_eq!(r("Arial-BoldMT", 32, None), Request { kind: Kind::Sans, bold: true, italic: false });
        assert_eq!(r("TimesNewRomanPS-ItalicMT", 34, None), Request { kind: Kind::Serif, bold: false, italic: true });
        assert_eq!(r("CourierNew", 33, None).kind, Kind::Mono);
        assert_eq!(r("Helvetica", 32, None).kind, Kind::Sans);
        assert_eq!(r("ZapfDingbats", 4, None).kind, Kind::Dingbats);
        assert_eq!(r("Symbol", 4, None).kind, Kind::Symbol);
        assert_eq!(r("SimSun", 34, Some(Ordering::Gb1)).kind, Kind::Cjk(Script::Gb1, Cjk::Song));
        assert_eq!(r("SimHei", 32, Some(Ordering::Gb1)).kind, Kind::Cjk(Script::Gb1, Cjk::Hei));
        assert_eq!(r("KaiTi_GB2312", 32, Some(Ordering::Gb1)).kind, Kind::Cjk(Script::Gb1, Cjk::Kai));
        assert_eq!(r("FangSong_GB2312", 32, Some(Ordering::Gb1)).kind, Kind::Cjk(Script::Gb1, Cjk::Fang));
        assert_eq!(r("STSong-Light", 32, Some(Ordering::Gb1)).kind, Kind::Cjk(Script::Gb1, Cjk::Song));
        assert_eq!(r("MSung-Light", 32, Some(Ordering::Cns1)).kind, Kind::Cjk(Script::Cns1, Cjk::Song));
        assert_eq!(r("HeiseiKakuGo-W5", 32, Some(Ordering::Japan1)).kind, Kind::Cjk(Script::Japan1, Cjk::Hei));
        assert_eq!(r("SomethingElse", 32, Some(Ordering::Korea1)).kind, Kind::Cjk(Script::Korea1, Cjk::Song));
        // GBK-coded names: 宋体, 黑体.
        let gbk = |bytes: &[u8]| request_for(bytes, 32, 0.0, 0.0, Some(Ordering::Gb1)).kind;
        assert_eq!(gbk(b"\xCB\xCE\xCC\xE5"), Kind::Cjk(Script::Gb1, Cjk::Song));
        assert_eq!(gbk(b"\xBA\xDA\xCC\xE5"), Kind::Cjk(Script::Gb1, Cjk::Hei));
    }

    #[test]
    fn a_system_font_is_opened_once_and_only_its_small_tables_are_read() {
        let req = request_for(b"Arial", 32, 0.0, 0.0, None);
        let (a, b) = (load(req), load(req));
        match (&a, &b) {
            (Some(a), Some(b)) => assert!(Arc::ptr_eq(a, b), "one font for two requests"),
            (None, None) => {}
            _ => panic!("the same request gave two different answers"),
        }
        // Where the machine has a Chinese font: the cmap has a character and the outline comes out of the file.
        if let Some(song) = load(request_for(b"SimSun", 34, 0.0, 0.0, Some(Ordering::Gb1))) {
            let gid = song.cmap.as_ref().and_then(|c| c.unicode(0x5B8B)).expect("a Chinese font has this character");
            let mut b = super::super::outline::Builder::new(song.em_matrix());
            song.outline(gid, &mut b).expect("the glyph is read from the file");
            assert!(b.finish().is_some());
            assert!(song.memory() < 8 << 20, "{} bytes in memory for a font file of many megabytes", song.memory());
        }
        // A request nothing satisfies is a miss, and stays one.
        let nothing = Request { kind: Kind::Dingbats, bold: true, italic: true };
        assert_eq!(load(nothing).is_some(), load(nothing).is_some());
    }
}


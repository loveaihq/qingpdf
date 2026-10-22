//! The reader of qingpdf: a window around the engine of `qingpdf-core`: open a file, scroll, zoom, turn pages, mark, fill in forms.
//! It is a library of its own so that it can be built for size (`opt-level = "z"` for this package, in the workspace's
//! `Cargo.toml`) while the program that links it (`qingpdf-view-bin`, which makes `qingpdf-view.exe`) is not: the link-time
//! optimization of the whole of the engine runs at the optimization level of the program, so a program built for size would have
//! the engine built for size as well, and the pages would take a quarter longer to draw.
//!
//! Layout: `app.rs` is the reader (state, scheduling, drawing) and knows nothing of Windows; `win32.rs` is the one module that
//! calls Windows (and the only one with `unsafe`); `ui.rs` is the plain-types contract between them. `layout`, `zoom`, `tiles`,
//! `cache` and `sched` are plain logic with tests, and so are `anim` (the clock of the short movements), `menustyle` and `dlgstyle` (the pop-up menus and the boxes, which the reader draws itself: how big, how drawn), `cmdbar` (the buttons of the command bar, where they are, when they can be used), `pill`, `toast`, `overbar` and `welcome`
//! (the page pill, the little messages, the thin scroll bars, the welcome page), `accent` and `logo` (the accent colour, and the mark drawn as strokes), `wordsel`
//! (the word and the line a double and a triple click take), `clicks`, `placement` (where the window starts), `settings`, `theme` (the colours of the window and the words for the
//! looks that are kept) and `thumbs` (the rows, the cache and the requests of the list of thumbnails in the sidebar).

pub mod accent;
pub mod anim;
pub mod app;
pub mod bench;
pub mod cache;
pub mod cmdbar;
pub mod copydata;
pub mod clicks;
pub mod dlgstyle;
pub mod find;
pub mod i18n;
pub mod layout;
pub mod logo;
pub mod menustyle;
pub mod ocr;
pub mod overbar;
pub mod placement;
pub mod pill;
pub mod places;
pub mod printing;
pub mod recent;
pub mod sched;
pub mod settings;
pub mod select;
pub mod tabs;
pub mod theme;
pub mod thumbs;
pub mod toast;
pub mod treestyle;
pub mod tiles;
pub mod treedrop;
pub mod ui;
#[allow(unsafe_code)]
pub mod win32;
pub mod welcome;
pub mod wordsel;
pub mod zoom;


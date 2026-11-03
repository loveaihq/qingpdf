//! qingpdf-view on macOS: the program of the Mac reader. The reader itself is the library `qingpdf-view` (shared with Windows);
//! the window, the menus and the system calls of macOS are in `macos.rs`, the only module here allowed `unsafe`.
//!
//! Run `qingpdf-view [file.pdf ...]`: each file opens in a tab. The hidden `--bench`, `--print-test` and `--ocr-test` of the
//! Windows program are taken too; printing and recognising text are not done on macOS yet.
//!
//! The package is built on every system so that the workspace builds anywhere, but it is the reader only on macOS;
//! elsewhere the program does nothing.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
#[cfg(target_os = "macos")]
mod reader;

#[cfg(target_os = "macos")]
fn main() {
    reader::main();
}

#[cfg(not(target_os = "macos"))]
fn main() {}

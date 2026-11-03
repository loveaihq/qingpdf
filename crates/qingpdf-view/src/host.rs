//! The two things the shared reader asks of the system it runs on outside the window: a text recogniser and the memory peak.
//! On Windows they are `win32`'s own; another system's program (the Mac reader, `qingpdf-view-mac`) gives its own once at start
//! with [`set`]. Until then there is no recogniser and the peak is 0.

#[cfg(windows)]
pub use crate::win32::{open_recognizer, peak_working_set};

#[cfg(not(windows))]
mod given {
    use std::sync::OnceLock;

    use crate::ocr::{OcrError, Recognizer};

    /// What the program of a system gives.
    pub struct Host {
        pub open_recognizer: fn(bool) -> Result<Box<dyn Recognizer>, OcrError>,
        pub peak_working_set: fn() -> u64,
    }

    static HOST: OnceLock<Host> = OnceLock::new();

    /// Give the system's functions; only the first call counts.
    pub fn set(host: Host) {
        let _ = HOST.set(host);
    }

    pub fn open_recognizer(chinese: bool) -> Result<Box<dyn Recognizer>, OcrError> {
        match HOST.get() {
            Some(h) => (h.open_recognizer)(chinese),
            None => Err(OcrError::NoLanguage { chinese, available: Vec::new() }),
        }
    }

    pub fn peak_working_set() -> u64 {
        HOST.get().map_or(0, |h| (h.peak_working_set)())
    }
}

#[cfg(not(windows))]
pub use given::*;

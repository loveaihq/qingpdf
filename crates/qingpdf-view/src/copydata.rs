//! The paths a second copy of the reader hands to the one that is running (2026-10-17): the files named on its command line, sent in a
//! `WM_COPYDATA` message, and what the running one believes of such a message. Any program on the machine can send that message to the
//! window, so the bytes are not trusted: they are only read as a list of paths of PDF files to open (never run, never anything else), and
//! a message that is not exactly that is thrown away whole.
//!
//! The message: `dwData` is [`ID`]; the data is UTF-16 (little endian), each path followed by a zero unit.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::tabs::MAX_TABS;

/// What `dwData` of the message is: "QPDF".
pub const ID: usize = 0x5150_4446;
/// Most bytes of the data (a path of the most units Windows allows, and its zero, just fit) and most paths in a message.
pub const MAX_BYTES: usize = 64 * 1024;
pub const MAX_PATHS: usize = MAX_TABS;

/// A path the reader may be asked to open from outside: whole (a drive and `\`, or `\\server\share`), with no control characters (a line
/// end or a zero would be a path of its own), ending in `.pdf`. Not a device (`\\.\pipe\x.pdf`, `\\.\GLOBALROOT\...`), nor a path spelt the long way
/// (`\\?\C:\x.pdf`): a pipe that nobody writes to holds up whoever reads it.
fn acceptable(path: &str) -> bool {
    let drive = matches!(path.as_bytes(), [letter, b':', b'\\' | b'/', ..] if letter.is_ascii_alphabetic());
    let unc = path.strip_prefix("\\\\").is_some_and(|rest| {
        let mut parts = rest.split('\\');
        parts.next().is_some_and(|server| !server.is_empty() && server != "." && server != "?") && parts.next().is_some_and(|share| !share.is_empty())
    });
    (drive || unc) && !path.chars().any(char::is_control) && path.get(path.len().saturating_sub(4)..).is_some_and(|ext| ext.eq_ignore_ascii_case(".pdf"))
}

/// The paths in a message of this `id` and these bytes. Anything wrong with any part of it (the wrong number, too many bytes, half a letter at the
/// end, a path not ended by a zero, an empty path, text that is not UTF-16, a path that [`acceptable`] does not take, more than [`MAX_PATHS`]
/// paths), and none of it is used.
pub fn decode(id: usize, data: &[u8]) -> Option<Vec<PathBuf>> {
    if id != ID || data.len() > MAX_BYTES || !data.len().is_multiple_of(2) {
        return None;
    }
    let units: Vec<u16> = data.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    // (The last unit is the zero that ends the last path: splitting on zeros leaves an empty piece after it.)
    let (&0, body) = units.split_last()? else { return None };
    let paths: Vec<PathBuf> = body.split(|&u| u == 0).map(|piece| String::from_utf16(piece).ok().filter(|text| acceptable(text)).map(PathBuf::from)).collect::<Option<_>>()?;
    (paths.len() <= MAX_PATHS).then_some(paths)
}

/// Add `files` to the paths that wait (for a box on the window to be closed), up to `max` in all. `false`: none could be added.
pub fn keep_waiting(waiting: &mut Vec<PathBuf>, files: Vec<PathBuf>, max: usize) -> bool {
    let room = max.saturating_sub(waiting.len());
    let taken = room > 0 && !files.is_empty();
    waiting.extend(files.into_iter().take(room));
    taken
}

/// The bytes of a message for these paths: the first [`MAX_PATHS`] that [`decode`] would take (the others are not sent). Empty when there is none.
pub fn encode(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut sent = 0;
    for p in paths {
        let text = p.to_string_lossy();
        if sent >= MAX_PATHS || !acceptable(&text) || out.len() + 2 * (text.encode_utf16().count() + 1) > MAX_BYTES {
            continue;
        }
        for unit in text.encode_utf16().chain(Some(0)) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        sent += 1;
    }
    out
}

/// The files named on the command line (everything that does not start with `--`), as whole paths, in order, without repeats. A relative path is
/// made whole from the folder the program was started in.
pub fn paths_of(args: impl IntoIterator<Item = OsString>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for arg in args {
        if arg.to_string_lossy().starts_with("--") {
            continue;
        }
        let path = std::path::absolute(&arg).unwrap_or_else(|_| PathBuf::from(&arg));
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(paths: &[&str]) -> Vec<u8> {
        encode(&paths.iter().map(PathBuf::from).collect::<Vec<_>>())
    }

    fn units(text: &str) -> Vec<u8> {
        text.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn paths_go_through_a_message_and_come_out_the_same() {
        let sent = ["C:\\Users\\张三\\公文 一.pdf", "D:\\a b\\c.PDF", "\\\\server\\share\\x.pdf"];
        let got = decode(ID, &bytes(&sent)).unwrap();
        assert_eq!(got, sent.iter().map(PathBuf::from).collect::<Vec<_>>());
        // Letters outside the first plane (a surrogate pair) too.
        assert_eq!(decode(ID, &units("C:\\\u{20000}.pdf")).unwrap(), vec![PathBuf::from("C:\\\u{20000}.pdf")]);
    }

    #[test]
    fn a_message_that_is_not_ours_or_has_no_paths_is_thrown_away() {
        assert!(decode(ID + 1, &bytes(&["C:\\a.pdf"])).is_none());
        assert!(decode(0, &bytes(&["C:\\a.pdf"])).is_none());
        assert!(decode(ID, &[]).is_none());
        assert!(bytes(&["C:\\a.txt", "relative.pdf"]).is_empty());
    }

    #[test]
    fn hostile_messages_are_thrown_away_whole() {
        let good = units("C:\\a.pdf");
        assert!(decode(ID, &good).is_some());
        // Huge: far over the limit, and just over.
        assert!(decode(ID, &vec![b'a'; 100 * MAX_BYTES]).is_none());
        assert!(decode(ID, &vec![0; MAX_BYTES + 2]).is_none());
        // An odd number of bytes: half a letter at the end.
        let mut odd = good.clone();
        odd.push(0x41);
        assert!(decode(ID, &odd).is_none());
        assert!(decode(ID, &[0x41]).is_none());
        // The last path is not ended with a zero.
        assert!(decode(ID, &good[..good.len() - 2]).is_none());
        // Zeros inside: an empty path in the middle, at the start, a string of zeros.
        assert!(decode(ID, &[units("C:\\a.pdf"), units(""), units("C:\\b.pdf")].concat()).is_none());
        assert!(decode(ID, &[0, 0]).is_none());
        assert!(decode(ID, &[0, 0, 0, 0, 0, 0]).is_none());
        // A path cut in two by a zero is two paths; the second one is not a path.
        assert!(decode(ID, &[units("C:\\a.pdf"), units("rest of it.pdf")].concat()).is_none());
        // Not a PDF, however it is written; not whole; not a path at all; with control characters.
        for bad in ["C:\\evil.exe", "C:\\a.pdf.exe", "C:\\a.pdf ", "C:\\a.pd", "C:\\.pdf.txt", "C:\\folder\\", "relative.pdf", "..\\a.pdf", "a:b.pdf", "\\a.pdf", "\\\\.pdf", "http://x/a.pdf", "-a.pdf", "\\\\.\\pipe\\x.pdf", "\\\\.\\GLOBALROOT\\Device\\x.pdf", "\\\\?\\C:\\x.pdf", "\\\\?\\UNC\\server\\share\\x.pdf"] {
            assert!(decode(ID, &units(bad)).is_none(), "{bad}");
        }
        for bad in ["C:\\a\n.pdf", "C:\\a\t.pdf", "C:\\\u{1b}[0m.pdf", "C:\\a\r.pdf", "C:\\a\u{7f}.pdf"] {
            assert!(decode(ID, &units(bad)).is_none(), "{bad:?}");
        }
        // A lone surrogate is not text.
        let broken: Vec<u8> = [0x43u16, 0x3a, 0x5c, 0xd800, 0x2e, 0x70, 0x64, 0x66, 0].iter().flat_map(|u| u.to_le_bytes()).collect();
        assert!(decode(ID, &broken).is_none());
        // The longest path that fits the message, and one unit more.
        let long = format!("C:\\{}.pdf", "x".repeat(MAX_BYTES / 2 - 8));
        assert_eq!(decode(ID, &units(&long)).map(|p| p.len()), Some(1));
        let long = format!("C:\\{}.pdf", "x".repeat(MAX_BYTES / 2 - 7));
        assert!(decode(ID, &units(&long)).is_none());
        // Too many paths: sixteen are fine, seventeen are not (and nothing of the message is used).
        let many = |n: usize| (0..n).map(|i| units(&format!("C:\\{i}.pdf"))).collect::<Vec<_>>().concat();
        assert_eq!(decode(ID, &many(MAX_PATHS)).map(|p| p.len()), Some(MAX_PATHS));
        assert!(decode(ID, &many(MAX_PATHS + 1)).is_none());
        assert!(decode(ID, &many(1000)).is_none());
    }

    #[test]
    fn what_is_sent_is_what_would_be_taken() {
        let many: Vec<PathBuf> = (0..40).map(|i| PathBuf::from(format!("C:\\f{i}.pdf"))).collect();
        let sent = encode(&many);
        assert_eq!(decode(ID, &sent).map(|p| p.len()), Some(MAX_PATHS));
        // One path too long to send is left out, the others go.
        let mixed = vec![PathBuf::from(format!("C:\\{}.pdf", "y".repeat(MAX_BYTES))), PathBuf::from("C:\\ok.pdf")];
        assert_eq!(decode(ID, &encode(&mixed)).unwrap(), vec![PathBuf::from("C:\\ok.pdf")]);
        assert!(encode(&[]).is_empty());
    }

    #[test]
    fn paths_that_wait_for_a_box_are_kept_up_to_a_limit() {
        let some = |n: usize| (0..n).map(|i| PathBuf::from(format!("C:\\w{i}.pdf"))).collect::<Vec<_>>();
        let mut waiting = Vec::new();
        assert!(keep_waiting(&mut waiting, some(3), 5) && waiting.len() == 3);
        // Taken as far as they go: the rest is left out, and the sender is told the files were taken (some were).
        assert!(keep_waiting(&mut waiting, some(4), 5) && waiting.len() == 5);
        // Full: nothing more is kept, and nothing is claimed.
        assert!(!keep_waiting(&mut waiting, some(1), 5) && waiting.len() == 5);
        assert!(!keep_waiting(&mut Vec::new(), Vec::new(), 5));
        assert!(!keep_waiting(&mut Vec::new(), some(2), 0));
    }

    #[test]
    fn the_command_line_gives_whole_paths_without_repeats_and_leaves_the_options_out() {
        let args = ["--bench", "C:\\a.pdf", "C:\\a.pdf", "b.pdf", "--x"].map(OsString::from);
        let got = paths_of(args);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], PathBuf::from("C:\\a.pdf"));
        assert!(got[1].is_absolute() && got[1].ends_with("b.pdf"), "{:?}", got[1]);
        assert!(paths_of(Vec::<OsString>::new()).is_empty());
    }
}

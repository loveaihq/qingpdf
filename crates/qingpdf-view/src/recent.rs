//! The files opened last, and where the reader was in each (3d-2): kept in a small UTF-8 text file under
//! `%APPDATA%\qingpdf\`. Nothing is sent anywhere. The file is not trusted either (another program, or a person, may have
//! written it): what cannot be read is skipped, and no line, no number and no count can make the reader do more than a
//! little.
//!
//! The format: the first line is `qingpdf-recent 1`, then one file to a line, newest first: `page`, `zoom`, `mode` and the path,
//! separated by tabs (the path last: it may hold anything but a tab or a line end).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::zoom;

/// Most files kept.
pub const MAX_RECENT: usize = 10;
/// Most bytes of the file that are read, and of a line, and of a path.
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_LINE_BYTES: usize = 4096;
const MAX_PATH_CHARS: usize = 1024;
const HEADER: &str = "qingpdf-recent 1";

/// How the zoom of a file was set, as the file keeps it.
pub const MODE_FIT_WIDTH: u8 = 0;
pub const MODE_FIT_PAGE: u8 = 1;
pub const MODE_CUSTOM: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    /// The page the reader was on, from 0.
    pub page: u32,
    /// The zoom, in thousandths (1000 is 100 percent), and how it was set ([`MODE_FIT_WIDTH`] and so on).
    pub zoom: u32,
    pub mode: u8,
}

#[cfg(test)]
impl Entry {
    pub fn new(path: &str) -> Entry {
        Entry { path: path.to_string(), page: 0, zoom: zoom::ACTUAL_SIZE, mode: MODE_FIT_WIDTH }
    }
}

/// Is `path` something the list may hold? Not empty, not too long, no control characters (a line end or a tab would
/// make a second entry of it).
fn path_ok(path: &str) -> bool {
    !path.is_empty() && path.chars().count() <= MAX_PATH_CHARS && !path.chars().any(char::is_control)
}

fn same_file(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Read the list from the text of the file. Lines that cannot be read are skipped; at most [`MAX_RECENT`] entries are
/// returned, a file named twice counts once (its first, newest, line).
pub fn parse(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in text.lines() {
        if out.len() >= MAX_RECENT {
            break;
        }
        if line.len() > MAX_LINE_BYTES || line == HEADER {
            continue;
        }
        let mut parts = line.splitn(4, '\t');
        let (Some(page), Some(zoom_text), Some(mode), Some(path)) = (parts.next(), parts.next(), parts.next(), parts.next()) else { continue };
        let (Ok(page), Ok(z), Ok(mode)) = (page.trim().parse::<u32>(), zoom_text.trim().parse::<u32>(), mode.trim().parse::<u8>()) else { continue };
        if !path_ok(path) || out.iter().any(|e| same_file(&e.path, path)) {
            continue;
        }
        let mode = if mode <= MODE_CUSTOM { mode } else { MODE_FIT_WIDTH };
        out.push(Entry { path: path.to_string(), page: page.min(10_000_000), zoom: zoom::clamp(z), mode });
    }
    out
}

/// The text of the file for a list.
pub fn serialize(entries: &[Entry]) -> String {
    let mut out = String::from(HEADER);
    out.push('\n');
    for e in entries.iter().filter(|e| path_ok(&e.path)).take(MAX_RECENT) {
        out.push_str(&format!("{}\t{}\t{}\t{}\n", e.page, e.zoom, e.mode, e.path));
    }
    out
}

/// Put `entry` first, replacing the one for the same file, and keep no more than [`MAX_RECENT`].
pub fn touch(list: &mut Vec<Entry>, entry: Entry) {
    list.retain(|e| !same_file(&e.path, &entry.path));
    list.insert(0, entry);
    list.truncate(MAX_RECENT);
}

pub fn find<'a>(list: &'a [Entry], path: &str) -> Option<&'a Entry> {
    list.iter().find(|e| same_file(&e.path, path))
}

pub fn forget(list: &mut Vec<Entry>, path: &str) {
    list.retain(|e| !same_file(&e.path, path));
}

/// Where the list is kept: `%APPDATA%\qingpdf\recent.txt` (none if there is no `APPDATA`).
pub fn file_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    (!base.is_empty()).then(|| Path::new(&base).join("qingpdf").join("recent.txt"))
}

/// The list in the file at `path`: empty if there is none or it cannot be read; at most [`MAX_FILE_BYTES`] of it are read.
pub fn load(path: &Path) -> Vec<Entry> {
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let mut bytes = Vec::new();
    if file.take(MAX_FILE_BYTES).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    // Bytes that are not UTF-8 are replaced; the lines they spoil are skipped by `parse`.
    parse(&String::from_utf8_lossy(&bytes))
}

/// How many times the rename is tried when another program has the list open for a moment, and how long to wait between.
const RENAME_TRIES: u32 = 5;
const RENAME_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

/// Write the list to `path` (the folder is made if need be): to a file of its own, whose name no other writer uses, written
/// out to the disk and then put in place, so that a crash half way leaves the old list and two readers running at once do not
/// write into each other's file: of two that save together the one that renames last wins, and the list is always whole. Errors
/// are told to the caller, who may ignore them (a list that cannot be kept is no reason to stop reading).
pub fn save(path: &Path, entries: &[Entry]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serialize(entries);
    // A name of this process and this call; `create_new` makes sure no other file (a left-over of an earlier run, say) is written into.
    let (temp, mut file) = loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{}.{n}.tmp", std::process::id()));
        let temp = PathBuf::from(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 1000 => {}
            Err(e) => return Err(e),
        }
    };
    let written = file.write_all(text.as_bytes()).and_then(|()| file.sync_all());
    drop(file);
    let mut result = written;
    if result.is_ok() {
        for attempt in 0..RENAME_TRIES {
            result = std::fs::rename(&temp, path);
            if result.is_ok() {
                break;
            }
            if attempt + 1 < RENAME_TRIES {
                std::thread::sleep(RENAME_WAIT);
            }
        }
    }
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, page: u32) -> Entry {
        Entry { path: path.to_string(), page, zoom: 1500, mode: MODE_CUSTOM }
    }

    #[test]
    fn a_list_goes_to_text_and_back() {
        let list = vec![entry("C:\\a\\一.pdf", 4), Entry::new("D:\\b c\\d.pdf")];
        let text = serialize(&list);
        assert!(text.starts_with("qingpdf-recent 1\n"));
        assert_eq!(parse(&text), list);
    }

    #[test]
    fn garbage_gives_nothing_or_the_lines_that_are_good() {
        assert!(parse("").is_empty());
        assert!(parse("\u{0}\u{1}\u{2}\n\n\t\t\t\n").is_empty());
        assert!(parse("not a list at all, just words").is_empty());
        let text = "qingpdf-recent 1\n3\t1000\t0\tC:\\good.pdf\nx\t1000\t0\tC:\\bad-page.pdf\n4\t-5\t0\tC:\\negative-zoom.pdf\n5\t1000\t0\t\n6\t1000\t0\tC:\\ctl\u{7}.pdf\n7\t1000\n8\t1000\t9\tC:\\unknown-mode.pdf\n";
        let got = parse(text);
        assert_eq!(got.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["C:\\good.pdf", "C:\\unknown-mode.pdf"]);
        assert_eq!(got[1].mode, MODE_FIT_WIDTH);
    }

    #[test]
    fn numbers_and_counts_are_held_to_their_limits() {
        let mut text = String::new();
        for i in 0..50 {
            text.push_str(&format!("{}\t999999999\t2\tC:\\f{i}.pdf\n", u32::MAX));
        }
        let got = parse(&text);
        assert_eq!(got.len(), MAX_RECENT);
        assert!(got.iter().all(|e| e.page <= 10_000_000 && e.zoom <= zoom::MAX_ZOOM && e.zoom >= zoom::MIN_ZOOM));
        // A line far too long, and a path far too long, are skipped.
        let long = format!("1\t1000\t0\t{}\n", "x".repeat(MAX_LINE_BYTES + 10));
        assert!(parse(&long).is_empty());
        let long_path = format!("1\t1000\t0\t{}\n", "y".repeat(MAX_PATH_CHARS + 1));
        assert!(parse(&long_path).is_empty());
        // The same file twice (in another case) counts once, the newest.
        let twice = parse("1\t1000\t0\tC:\\A.pdf\n2\t1000\t0\tc:\\a.PDF\n");
        assert_eq!(twice.len(), 1);
        assert_eq!(twice[0].page, 1);
    }

    #[test]
    fn touching_moves_a_file_to_the_front_and_keeps_ten() {
        let mut list: Vec<Entry> = (0..MAX_RECENT).map(|i| entry(&format!("C:\\{i}.pdf"), 0)).collect();
        touch(&mut list, entry("c:\\5.PDF", 9));
        assert_eq!((list.len(), list[0].page, list[0].path.as_str()), (MAX_RECENT, 9, "c:\\5.PDF"));
        touch(&mut list, entry("C:\\new.pdf", 0));
        assert_eq!(list.len(), MAX_RECENT);
        assert_eq!(list[0].path, "C:\\new.pdf");
        assert!(find(&list, "C:\\9.pdf").is_none(), "the oldest went");
        assert!(find(&list, "C:\\0.PDF").is_some());
        forget(&mut list, "c:\\0.pdf");
        assert!(find(&list, "C:\\0.pdf").is_none());
    }

    #[test]
    fn writers_at_the_same_time_leave_one_whole_list() {
        let dir = std::env::temp_dir().join(format!("qingpdf-recent-race-{}", std::process::id()));
        let path = dir.join("recent.txt");
        let lists: Vec<Vec<Entry>> = (0..6).map(|w| (0..MAX_RECENT).map(|i| entry(&format!("C:/writer{w}/{i}.pdf"), w)).collect()).collect();
        std::thread::scope(|s| {
            let (path, lists) = (&path, &lists);
            for list in lists {
                s.spawn(move || {
                    for _ in 0..40 {
                        // A saving that fails (the other writer has the file for a moment) is allowed; a list that is not whole is not.
                        let _ = save(path, list);
                        let got = load(path);
                        assert!(got.is_empty() || lists.contains(&got), "a list that is not whole: {got:?}");
                    }
                });
            }
        });
        assert!(lists.contains(&load(&path)));
        let left_over = std::fs::read_dir(&dir).map_or(0, |d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count());
        assert_eq!(left_over, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_is_written_whole_read_back_and_read_only_so_far() {
        let dir = std::env::temp_dir().join(format!("qingpdf-recent-test-{}", std::process::id()));
        let path = dir.join("sub").join("recent.txt");
        assert!(load(&path).is_empty(), "no file: no list");
        let list = vec![entry("C:\\one.pdf", 2), entry("C:\\two.pdf", 3)];
        save(&path, &list).expect("saves");
        assert_eq!(load(&path), list);
        let left_over = |dir: &Path| std::fs::read_dir(dir).map_or(0, |d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count());
        assert_eq!(left_over(path.parent().expect("a folder")), 0);
        // A huge file is read only to its limit; bytes that are not UTF-8 spoil only their line.
        let mut big = serialize(&list).into_bytes();
        big.extend_from_slice(b"1\t1000\t0\tC:\\bad\xff\xfe.pdf\n");
        big.resize(200_000, b'z');
        std::fs::write(&path, &big).expect("writes");
        let got = load(&path);
        assert_eq!(&got[..2], &list[..]);
        assert!(got.len() <= MAX_RECENT);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

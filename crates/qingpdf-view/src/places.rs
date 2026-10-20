//! Where the reader was in each of the last files it opened (2026-10-17): the page, the zoom, how far down and across, and the turn of
//! the view, kept per file in a small UTF-8 text file under `%APPDATA%\qingpdf\` so that a file opens where it was left. A file is known by
//! its full path, its size and the time it was changed: a file that has been changed since (by another program) is a new file, and opens
//! at the first page. Only numbers are kept, never a password and never anything of the file's contents. Nothing is sent anywhere.
//!
//! The file is not trusted (another program, or a person, may have written it): what cannot be read is skipped, no number is believed
//! further than its range, and at most [`MAX_PLACES`] lines are looked at.
//!
//! The format: the first line is `qingpdf-places 1`, then one file to a line, newest first: size, modified (seconds since 1970), page,
//! mode (0 fit width, 1 fit page, 2 a zoom of its own), zoom (thousandths), across and down (tenths of a percent), turn (degrees) and the
//! path, separated by tabs (the path last: it may hold anything but a tab or a line end).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::recent::{MODE_CUSTOM, write_atomic};
use crate::zoom;

/// Most files kept.
pub const MAX_PLACES: usize = 200;
/// Most bytes of the file that are read, of a line, and of a path.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_LINE_BYTES: usize = 4096;
const MAX_PATH_CHARS: usize = 1024;
const HEADER: &str = "qingpdf-places 1";
/// Across and down are kept in tenths of a percent.
pub const FULL: u32 = 1000;

/// The full path of a file in the one spelling that two spellings of the same file share: absolute, `\` between the parts, no `\\?\`, no `.` or
/// `..`, no `\` at the end, lower case (names on Windows are not case sensitive).
pub fn normalize(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let s = absolute.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix("\\\\?\\").unwrap_or(&s);
    let mut s = s.to_lowercase();
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// Are these two paths one file?
pub fn same_file(a: &Path, b: &Path) -> bool {
    a == b || normalize(a) == normalize(b)
}

/// What tells a file from the same file changed: its size and the time it was changed (seconds since 1970).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub modified: u64,
}

/// How long a look at a file may take before the file is taken for one that cannot be looked at (a file on a share that does not answer must
/// not hold up the window).
const LOOK_WAIT: std::time::Duration = std::time::Duration::from_millis(1500);

/// The stamp of the file at `path` now (none if it cannot be looked at, or does not answer in time).
pub fn stamp_of(path: &Path) -> Option<Stamp> {
    if crate::tabs::is_device_path(path) {
        return None;
    }
    let path = path.to_path_buf();
    let look: crate::tabs::Look = Box::new(move || {
        crate::tabs::Looked::Stamp((|| {
            let meta = std::fs::metadata(&path).ok()?;
            let modified = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            Some(Stamp { size: meta.len(), modified })
        })())
    });
    match crate::tabs::look_all(vec![look], LOOK_WAIT).pop() {
        Some(Some(crate::tabs::Looked::Stamp(stamp))) => stamp,
        _ => None,
    }
}

/// Where the reader was in a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The file, as [`normalize`] spells it.
    pub path: String,
    pub stamp: Stamp,
    /// The page at the top of the window, from 0.
    pub page: u32,
    /// How the zoom was set (`recent::MODE_*`) and the zoom in thousandths.
    pub mode: u8,
    pub zoom: u32,
    /// How far across the document the window is, and how far down the page at its top (both up to [`FULL`]).
    pub across: u32,
    pub down: u32,
    /// The turn of the view, in degrees: 0, 90, 180 or 270.
    pub rotation: u16,
}

impl Place {
    /// The place of the first page, at the zoom that fits the width.
    #[cfg(test)]
    pub fn start(path: &str, stamp: Stamp) -> Place {
        Place { path: path.to_string(), stamp, page: 0, mode: 0, zoom: zoom::ACTUAL_SIZE, across: 0, down: 0, rotation: 0 }
    }
}

/// One line of the file: eight numbers and a path, tab between; none if anything is wrong with it.
fn parse_line(line: &str) -> Option<Place> {
    let mut parts = line.splitn(9, '\t');
    let mut number = || parts.next()?.trim().parse::<u64>().ok();
    let (size, modified, page, mode, z, across, down, turn) = (number()?, number()?, number()?, number()?, number()?, number()?, number()?, number()?);
    let path = parts.next().filter(|p| line.len() <= MAX_LINE_BYTES && path_ok(p))?;
    let rotation = u16::try_from(turn).ok().filter(|t| [0, 90, 180, 270].contains(t))?;
    Some(Place {
        path: path.to_string(),
        stamp: Stamp { size, modified },
        page: u32::try_from(page).ok()?.min(10_000_000),
        mode: u8::try_from(mode).ok().filter(|&m| m <= MODE_CUSTOM)?,
        zoom: zoom::clamp(u32::try_from(z).ok()?),
        across: u32::try_from(across).ok()?.min(FULL),
        down: u32::try_from(down).ok()?.min(FULL),
        rotation,
    })
}

fn path_ok(path: &str) -> bool {
    !path.is_empty() && path.chars().count() <= MAX_PATH_CHARS && !path.chars().any(char::is_control)
}

/// The places kept, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    list: Vec<Place>,
}

impl Places {
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Read the list from the text of the file. Lines that cannot be read are skipped; the first [`MAX_PLACES`] lines are looked at; a file
    /// named twice counts once (its first, newest, line).
    pub fn parse(text: &str) -> Places {
        let mut list: Vec<Place> = Vec::new();
        for line in text.trim_start_matches('\u{feff}').lines().take(MAX_PLACES + 1) {
            if let Some(place) = parse_line(line).filter(|p| !list.iter().any(|q| q.path == p.path)) {
                list.push(place);
            }
        }
        list.truncate(MAX_PLACES);
        Places { list }
    }

    /// The text of the file.
    pub fn serialize(&self) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        for p in self.list.iter().filter(|p| path_ok(&p.path)).take(MAX_PLACES) {
            out.push_str(&format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n", p.stamp.size, p.stamp.modified, p.page, p.mode, p.zoom, p.across, p.down, p.rotation, p.path));
        }
        out
    }

    /// Where the reader was in the file whose path is `path` (as [`normalize`] spells it), if it is the same file still: the same size and the
    /// same time of change.
    pub fn find(&self, path: &str, stamp: Stamp) -> Option<&Place> {
        self.list.iter().find(|p| p.path == path && p.stamp == stamp)
    }

    /// Keep `place` first, replacing what was kept for the same file (whatever its size and time), and no more than [`MAX_PLACES`] files.
    pub fn touch(&mut self, place: Place) {
        self.list.retain(|p| p.path != place.path);
        self.list.insert(0, place);
        self.list.truncate(MAX_PLACES);
    }

    /// Forget the files (what Open recent > clear the list does).
    pub fn clear(&mut self) {
        self.list.clear();
    }
}

/// Where the places are kept: `%APPDATA%\qingpdf\places.txt` (none if there is no `APPDATA`). In the unit tests there is no such file: the places of
/// the tests that open the same files would otherwise be each other's (a test that wants a file says which).
#[cfg(not(test))]
pub fn file_path() -> Option<PathBuf> {
    Some(crate::settings::data_dir()?.join("places.txt"))
}

#[cfg(test)]
pub fn file_path() -> Option<PathBuf> {
    None
}

/// The places in the file at `path`: none if there is no such file or it cannot be read; at most [`MAX_FILE_BYTES`] of it are read.
pub fn load(path: &Path) -> Places {
    let Ok(file) = std::fs::File::open(path) else { return Places::default() };
    let mut bytes = Vec::new();
    if file.take(MAX_FILE_BYTES).read_to_end(&mut bytes).is_err() {
        return Places::default();
    }
    // Bytes that are not UTF-8 are replaced; the lines they spoil are skipped.
    Places::parse(&String::from_utf8_lossy(&bytes))
}

/// Write the places to `path`, whole or not at all (see `recent::write_atomic`).
pub fn save(path: &Path, places: &Places) -> std::io::Result<()> {
    write_atomic(path, &places.serialize())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMP: Stamp = Stamp { size: 4_400_000, modified: 1_760_000_000 };

    fn place(path: &str, page: u32) -> Place {
        Place { path: path.to_string(), stamp: STAMP, page, mode: MODE_CUSTOM, zoom: 1500, across: 250, down: 640, rotation: 90 }
    }

    #[test]
    fn paths_that_name_one_file_are_spelled_alike() {
        assert_eq!(normalize(Path::new("C:\\Docs\\A.PDF")), "c:\\docs\\a.pdf");
        assert_eq!(normalize(Path::new("C:/Docs/./sub/../A.pdf")), "c:\\docs\\a.pdf");
        assert_eq!(normalize(Path::new("\\\\?\\C:\\Docs\\a.pdf")), "c:\\docs\\a.pdf");
        assert_eq!(normalize(Path::new("C:\\Docs\\")), "c:\\docs");
        assert_eq!(normalize(Path::new("C:\\")), "c:\\");
        assert_eq!(normalize(Path::new("公文\\一.pdf")).rsplit('\\').next(), Some("一.pdf"));
        assert!(same_file(Path::new("C:\\a\\B.pdf"), Path::new("c:/a/b.PDF")));
        assert!(!same_file(Path::new("C:\\a\\b.pdf"), Path::new("C:\\a\\c.pdf")));
    }

    #[test]
    fn a_list_goes_to_text_and_back() {
        let mut places = Places::default();
        places.touch(place("c:\\docs\\公文 一.pdf", 4));
        places.touch(Place::start("d:\\b c\\d.pdf", STAMP));
        let text = places.serialize();
        assert!(text.starts_with("qingpdf-places 1\n"));
        assert_eq!(Places::parse(&text), places);
        assert_eq!(places.find("c:\\docs\\公文 一.pdf", STAMP).map(|p| p.page), Some(4));
    }

    #[test]
    fn a_file_that_has_changed_since_is_not_where_it_was() {
        let mut places = Places::default();
        places.touch(place("c:\\a.pdf", 7));
        assert!(places.find("c:\\a.pdf", STAMP).is_some());
        assert!(places.find("c:\\a.pdf", Stamp { size: STAMP.size + 1, ..STAMP }).is_none());
        assert!(places.find("c:\\a.pdf", Stamp { modified: STAMP.modified + 1, ..STAMP }).is_none());
        assert!(places.find("c:\\b.pdf", STAMP).is_none());
    }

    #[test]
    fn touching_moves_a_file_to_the_front_and_keeps_two_hundred() {
        let mut places = Places::default();
        for i in 0..MAX_PLACES {
            places.touch(place(&format!("c:\\{i}.pdf"), 1));
        }
        assert_eq!(places.len(), MAX_PLACES);
        places.touch(Place { stamp: Stamp { size: 1, modified: 2 }, ..place("c:\\5.pdf", 9) });
        assert_eq!(places.len(), MAX_PLACES);
        assert_eq!((places.list[0].path.as_str(), places.list[0].page, places.list[0].stamp.size), ("c:\\5.pdf", 9, 1), "the old line for the file is gone, whatever its size");
        places.touch(place("c:\\new.pdf", 0));
        assert_eq!(places.len(), MAX_PLACES);
        assert!(places.find("c:\\0.pdf", STAMP).is_none(), "the oldest went");
        assert!(places.find("c:\\1.pdf", STAMP).is_some());
        places.clear();
        assert!(places.is_empty());
    }

    #[test]
    fn a_damaged_or_hand_edited_file_gives_the_lines_that_are_good() {
        assert!(Places::parse("").is_empty());
        assert!(Places::parse("\u{0}\u{1}\u{2}\n\n\t\t\t\n").is_empty());
        assert!(Places::parse("not a list at all, just words\nand a second line").is_empty());
        let text = concat!(
            "\u{feff}qingpdf-places 1\r\n",
            "100\t200\t3\t0\t1000\t0\t0\t0\tC:\\good.pdf\r\n",
            "x\t200\t3\t0\t1000\t0\t0\t0\tC:\\bad-size.pdf\n",
            "100\t200\t-3\t0\t1000\t0\t0\t0\tC:\\negative-page.pdf\n",
            "100\t200\t3\t9\t1000\t0\t0\t0\tC:\\unknown-mode.pdf\n",
            "100\t200\t3\t0\t1000\t0\t0\t45\tC:\\odd-turn.pdf\n",
            "100\t200\t3\t0\t1000\t0\t0\t0\t\n",
            "100\t200\t3\t0\t1000\t0\t0\t0\tC:\\ctl\u{7}.pdf\n",
            "100\t200\t3\t0\t1000\t0\t0\n",
            "100\t200\t3\t0\t1000\t0\t0\t270\tC:\\second good.pdf\n",
            "100\t200\t3\t0\t1000\t0\t0\t0\tC:\\good.pdf\n",
            "# a comment a person added\n",
        );
        let got = Places::parse(text);
        assert_eq!(got.list.iter().map(|p| p.path.as_str()).collect::<Vec<_>>(), ["C:\\good.pdf", "C:\\second good.pdf"]);
        assert_eq!(got.list[1].rotation, 270);
        // The file twice: the first (newest) line counts.
        assert_eq!(got.list[0].page, 3);
    }

    #[test]
    fn numbers_and_counts_are_held_to_their_limits() {
        let mut text = String::new();
        for i in 0..500 {
            text.push_str(&format!("{}\t{}\t{}\t2\t999999999\t{}\t{}\t90\tC:\\f{i}.pdf\n", u64::MAX, u64::MAX, u32::MAX, u32::MAX, u32::MAX));
        }
        let got = Places::parse(&text);
        assert_eq!(got.len(), MAX_PLACES);
        assert!(got.list.iter().all(|p| p.page <= 10_000_000 && p.zoom <= zoom::MAX_ZOOM && p.zoom >= zoom::MIN_ZOOM && p.across <= FULL && p.down <= FULL));
        // A line far too long, a path far too long, and numbers that do not fit their type are skipped.
        let long = format!("1\t1\t1\t0\t1000\t0\t0\t0\t{}\n", "x".repeat(MAX_LINE_BYTES + 10));
        assert!(Places::parse(&long).is_empty());
        let long_path = format!("1\t1\t1\t0\t1000\t0\t0\t0\t{}\n", "y".repeat(MAX_PATH_CHARS + 1));
        assert!(Places::parse(&long_path).is_empty());
        assert!(Places::parse("1\t1\t99999999999\t0\t1000\t0\t0\t0\tC:\\a.pdf\n").is_empty());
        assert!(Places::parse("1\t1\t1\t0\t1000\t0\t0\t99999\tC:\\a.pdf\n").is_empty());
    }

    #[test]
    fn nothing_but_numbers_and_a_path_is_written() {
        let mut places = Places::default();
        places.touch(place("c:\\secret.pdf", 2));
        let text = places.serialize();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().nth(1).unwrap().split('\t').count() == 9);
        // A path with a line end in it is not written (it would make a second entry).
        places.touch(place("c:\\a\nb.pdf", 1));
        assert_eq!(places.serialize().lines().count(), 2);
    }

    #[test]
    fn the_file_is_written_whole_read_back_and_read_only_so_far() {
        let dir = std::env::temp_dir().join(format!("qingpdf-places-test-{}", std::process::id()));
        let path = dir.join("sub").join("places.txt");
        assert!(load(&path).is_empty(), "no file: no places");
        let mut places = Places::default();
        places.touch(place("c:\\one.pdf", 2));
        places.touch(place("c:\\two.pdf", 3));
        save(&path, &places).expect("saves");
        assert_eq!(load(&path), places);
        let left_over = std::fs::read_dir(path.parent().unwrap()).map_or(0, |d| d.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count());
        assert_eq!(left_over, 0);
        // A huge file is read only to its limit, and bytes that are not UTF-8 spoil only their line.
        let mut big = places.serialize().into_bytes();
        big.extend_from_slice(b"1\t1\t1\t0\t1000\t0\t0\t0\tC:\\bad\xff\xfe.pdf\n");
        big.resize(2_000_000, b'z');
        std::fs::write(&path, &big).expect("writes");
        let got = load(&path);
        assert_eq!(&got.list[..2], &places.list[..]);
        assert!(got.len() <= MAX_PLACES);
        // A folder where the file should be: nothing is read, and nothing breaks.
        let as_folder = dir.join("folder");
        std::fs::create_dir_all(&as_folder).unwrap();
        assert!(load(&as_folder).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stamp_of_a_file_is_its_size_and_time_and_a_missing_file_has_none() {
        let dir = std::env::temp_dir().join(format!("qingpdf-stamp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.pdf");
        std::fs::write(&file, b"12345").unwrap();
        let stamp = stamp_of(&file).expect("a stamp");
        assert_eq!(stamp.size, 5);
        assert!(stamp.modified > 1_700_000_000);
        assert!(stamp_of(&dir.join("missing.pdf")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

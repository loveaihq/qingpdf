//! The box that lists files for the jobs that join them (File > Merge PDFs, File > Images to PDF): the files in the order they will be joined
//! in, one of them chosen to be moved up or down, taken out, and more files to be added. Every button closes the box and the reader shows it
//! again with the list changed (the box itself has no list to edit); OK ends the choosing. The list and the rules about it are plain logic.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use super::*;
use crate::ui::{FormItem, FormKind::*, FormSpec};

/// The most files in one list (more are left out when files are added).
pub(super) const MAX_FILES: usize = 500;

/// The most lines of the list the box shows at once (the window follows the chosen file).
const SHOWN: usize = 10;

/// The buttons of the box, in the order they are made in (`FormAnswer::button` counts them).
pub(super) const ADD: usize = 0;
pub(super) const UP: usize = 1;
pub(super) const DOWN: usize = 2;
pub(super) const REMOVE: usize = 3;

/// The files in the order they will be joined in, and the one chosen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct FileList {
    pub files: Vec<PathBuf>,
    pub chosen: usize,
}

impl FileList {
    /// Put the chosen file one place nearer the start; it stays the chosen one.
    pub(super) fn up(&mut self) {
        if self.chosen > 0 && self.chosen < self.files.len() {
            self.files.swap(self.chosen, self.chosen - 1);
            self.chosen -= 1;
        }
    }

    /// Put the chosen file one place nearer the end; it stays the chosen one.
    pub(super) fn down(&mut self) {
        if self.chosen + 1 < self.files.len() {
            self.files.swap(self.chosen, self.chosen + 1);
            self.chosen += 1;
        }
    }

    /// Take the chosen file out; the one that took its place (else the last) is chosen.
    pub(super) fn remove(&mut self) {
        if self.chosen < self.files.len() {
            self.files.remove(self.chosen);
        }
        self.chosen = self.chosen.min(self.files.len().saturating_sub(1));
    }

    /// Put files at the end (as many as fit); the first of them is chosen.
    pub(super) fn add(&mut self, more: Vec<PathBuf>) {
        let room = MAX_FILES.saturating_sub(self.files.len());
        if more.is_empty() || room == 0 {
            return;
        }
        self.chosen = self.files.len();
        self.files.extend(more.into_iter().take(room));
    }
}

/// The name of a file as the list shows it: cut in the middle when it is long.
fn shown_name(path: &Path) -> String {
    let name = path.file_name().map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= 56 {
        return name;
    }
    chars.iter().take(30).chain(std::iter::once(&'\u{2026}')).chain(chars.iter().skip(chars.len() - 25)).collect()
}

/// The lines to show of a list of `count`, with `chosen` among them: the first line and the last (not included).
fn window(count: usize, chosen: usize) -> (usize, usize) {
    let first = (chosen + 1).saturating_sub(SHOWN).min(count.saturating_sub(SHOWN));
    (first, (first + SHOWN).min(count))
}

/// The box: what it is for, the list (the chosen file marked), the choice of the file to move and, for images, the choice of the page size
/// (`size` is the one chosen). The things that are filled in are, in order: the file chosen and (for images) the page size.
pub(super) fn form(lang: Lang, tag: u32, title: Msg, intro: Msg, ok: Msg, list: &FileList, size: Option<usize>) -> FormSpec {
    let (first, last) = window(list.files.len(), list.chosen);
    let mut text: Vec<String> = Vec::with_capacity(SHOWN + 2);
    if first > 0 {
        text.push("\u{2026}".to_string());
    }
    for (i, file) in list.files.iter().enumerate().take(last).skip(first) {
        text.push(format!("{} {}. {}", if i == list.chosen { ">" } else { "\u{a0}" }, i + 1, shown_name(file)));
    }
    if last < list.files.len() {
        text.push("\u{2026}".to_string());
    }
    let lines = text.len() as i32;
    let item = |kind, text: String, rect| FormItem { kind, text, rect, choices: Vec::new(), selected: 0 };
    let mut items = vec![item(Label, t(lang, intro).to_string(), (10, 6, 460, 36)), item(Label, text.join("\n"), (10, 48, 460, 18 * lines.max(1)))];
    let mut y = 48 + 18 * lines.max(1) + 12;
    let names: Vec<String> = list.files.iter().enumerate().map(|(i, f)| format!("{}. {}", i + 1, shown_name(f))).collect();
    items.push(item(Label, t(lang, Msg::JnChosen).to_string(), (10, y + 4, 100, 18)));
    items.push(FormItem { kind: Pick, text: String::new(), rect: (112, y, 358, 110), choices: names, selected: list.chosen });
    y += 34;
    if let Some(size) = size {
        items.push(item(Label, t(lang, Msg::ImPageSize).to_string(), (10, y + 4, 100, 18)));
        items.push(FormItem { kind: Pick, text: String::new(), rect: (112, y, 358, 110), choices: [Msg::ImA4, Msg::ImFit].map(|m| t(lang, m).to_string()).to_vec(), selected: size });
        y += 34;
    }
    for (k, m) in [Msg::JnAdd, Msg::JnUp, Msg::JnDown, Msg::JnRemove].into_iter().enumerate() {
        items.push(item(Button, t(lang, m).to_string(), (10 + 115 * k as i32, y + 4, 110, 28)));
    }
    FormSpec { tag, title: t(lang, title).to_string(), size: (480, y + 4 + 28 + 48), items, ok: t(lang, ok).to_string(), cancel: t(lang, Msg::Cancel).to_string(), preview: None }
}

/// The order a person expects for names with numbers in them: "2" before "10", capitals and small letters alike. Ties are left to the caller.
pub(super) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let number = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut digits = String::new();
                    while let Some(d) = it.next_if(char::is_ascii_digit) {
                        digits.push(d);
                    }
                    digits
                };
                let (x, y) = (number(&mut a), number(&mut b));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                a.next();
                b.next();
                let order = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// Files in the order of their names, numbers counted as numbers (an insertion sort: there are few files, and it is the least code).
pub(super) fn sort_naturally(files: &mut [PathBuf]) {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let later = |a: &PathBuf, b: &PathBuf| natural_cmp(&name(a), &name(b)).then_with(|| a.cmp(b)) == Ordering::Greater;
    for i in 1..files.len() {
        let mut at = i;
        while at > 0 && files.get(at - 1).zip(files.get(at)).is_some_and(|(before, this)| later(before, this)) {
            files.swap(at - 1, at);
            at -= 1;
        }
    }
}

/// The file is a picture the engine can put on a page (by its ending).
pub(super) fn is_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["jpg", "jpeg", "png"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// The name a new file starts with in the box that asks where to write: in the folder of `first`, its name without the ending, `suffix` and `.pdf`.
pub(super) fn default_name(first: &Path, suffix: &str) -> String {
    let stem = first.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = format!("{stem}{suffix}.pdf");
    match first.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir.join(name).to_string_lossy().into_owned(),
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str], chosen: usize) -> FileList {
        FileList { files: names.iter().map(PathBuf::from).collect(), chosen }
    }

    fn names(l: &FileList) -> Vec<&str> {
        l.files.iter().map(|f| f.to_str().unwrap_or("")).collect()
    }

    #[test]
    fn a_file_moves_up_and_down_and_stays_chosen_and_the_ends_hold() {
        let mut l = list(&["a", "b", "c"], 2);
        l.up();
        assert_eq!((names(&l), l.chosen), (vec!["a", "c", "b"], 1));
        l.up();
        l.up();
        assert_eq!((names(&l), l.chosen), (vec!["c", "a", "b"], 0));
        l.down();
        l.down();
        l.down();
        assert_eq!((names(&l), l.chosen), (vec!["a", "b", "c"], 2));
        let mut empty = FileList::default();
        empty.up();
        empty.down();
        empty.remove();
        assert!(empty.files.is_empty() && empty.chosen == 0);
    }

    #[test]
    fn a_file_is_taken_out_and_the_next_one_is_chosen_or_the_last() {
        let mut l = list(&["a", "b", "c"], 1);
        l.remove();
        assert_eq!((names(&l), l.chosen), (vec!["a", "c"], 1));
        l.remove();
        assert_eq!((names(&l), l.chosen), (vec!["a"], 0));
        l.remove();
        assert!(l.files.is_empty() && l.chosen == 0);
    }

    #[test]
    fn files_are_added_at_the_end_the_first_of_them_is_chosen_and_the_list_has_a_limit() {
        let mut l = list(&["a"], 0);
        l.add(vec![PathBuf::from("b"), PathBuf::from("c")]);
        assert_eq!((names(&l), l.chosen), (vec!["a", "b", "c"], 1));
        l.add(Vec::new());
        assert_eq!(l.chosen, 1);
        let mut full = FileList::default();
        full.add((0..MAX_FILES + 5).map(|i| PathBuf::from(i.to_string())).collect());
        assert_eq!(full.files.len(), MAX_FILES);
        full.add(vec![PathBuf::from("x")]);
        assert_eq!(full.files.len(), MAX_FILES);
    }

    #[test]
    fn numbers_in_names_are_ordered_as_numbers() {
        let mut files: Vec<PathBuf> = ["img10.png", "img2.png", "Img1.png", "img02.png", "b.png", "a.PNG", "img1a.png"].iter().map(PathBuf::from).collect();
        sort_naturally(&mut files);
        let sorted: Vec<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
        assert_eq!(sorted, vec!["a.PNG", "b.png", "Img1.png", "img1a.png", "img02.png", "img2.png", "img10.png"]);
        assert_eq!(natural_cmp("2", "10"), Ordering::Less);
        assert_eq!(natural_cmp("a", "A"), Ordering::Equal);
        assert_eq!(natural_cmp("x9", "x"), Ordering::Greater);
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
        // A number too long for any integer is still ordered by its digits.
        assert_eq!(natural_cmp("p99999999999999999999999", "p100000000000000000000000"), Ordering::Less);
    }

    #[test]
    fn only_jpeg_and_png_are_pictures() {
        for yes in ["a.jpg", "a.JPEG", "dir/a.Png"] {
            assert!(is_image(Path::new(yes)), "{yes}");
        }
        for no in ["a.pdf", "a.gif", "jpg", "a.jpg.txt", ""] {
            assert!(!is_image(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn the_new_file_starts_in_the_folder_of_the_first_with_its_name_and_the_suffix() {
        assert_eq!(default_name(Path::new("C:\\docs\\a.b.pdf"), "-merged"), "C:\\docs\\a.b-merged.pdf");
        assert_eq!(default_name(Path::new("a.png"), ""), "a.pdf");
    }

    #[test]
    fn the_box_shows_a_window_of_a_long_list_that_follows_the_chosen_file() {
        assert_eq!(window(3, 1), (0, 3));
        assert_eq!(window(30, 0), (0, SHOWN));
        assert_eq!(window(30, 15), (6, 16));
        assert_eq!(window(30, 29), (20, 30));
        let many: Vec<String> = (0..30).map(|i| format!("f{i}.pdf")).collect();
        let l = FileList { files: many.iter().map(PathBuf::from).collect(), chosen: 15 };
        let f = form(Lang::En, 1, Msg::MgTitle, Msg::MgIntro, Msg::MgOk, &l, None);
        let Some(shown) = f.items.get(1) else { panic!("no list") };
        assert!(shown.text.starts_with('\u{2026}') && shown.text.ends_with('\u{2026}') && shown.text.contains("> 16. f15.pdf") && !shown.text.contains("f5.pdf"));
        let picks: Vec<&FormItem> = f.items.iter().filter(|i| i.kind == Pick).collect();
        assert_eq!((picks.len(), picks[0].choices.len(), picks[0].selected), (1, 30, 15));
        assert_eq!(f.items.iter().filter(|i| i.kind == Button).count(), 4);
        assert!(f.items.iter().all(|i| i.rect.0 >= 0 && i.rect.0 + i.rect.2 <= f.size.0 && (i.kind == Pick || i.rect.1 + i.rect.3 <= f.size.1)));
        // With a page size the box has a second list.
        let g = form(Lang::Zh, 1, Msg::ImTitle, Msg::ImIntro, Msg::ImOk, &l, Some(1));
        assert_eq!(g.items.iter().filter(|i| i.kind == Pick).map(|i| i.selected).collect::<Vec<_>>(), vec![15, 1]);
    }
}

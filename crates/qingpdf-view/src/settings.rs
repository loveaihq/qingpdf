//! The name put on the annotations (4a): kept in a one-line UTF-8 text file, `%APPDATA%\qingpdf\author.txt`. Nothing is sent
//! anywhere. The file is not trusted (another program may have written it): a name that is too long or has control characters
//! in it is not used.

use std::io::Read;
use std::path::PathBuf;

use qingpdf_core::view::MAX_AUTHOR_CHARS;

/// Most bytes of the file that are read.
const MAX_FILE_BYTES: u64 = 1024;

/// Where the name is kept (none if there is no `APPDATA`).
pub fn file_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("qingpdf").join("author.txt"))
}

/// A name the annotations may carry: not empty, at most [`MAX_AUTHOR_CHARS`] characters, no control characters. Whitespace at the
/// ends is dropped.
pub fn clean(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.chars().count() <= MAX_AUTHOR_CHARS && !name.chars().any(char::is_control)).then(|| name.to_string())
}

/// The saved name, if there is one that [`clean`] accepts.
pub fn load() -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(file_path()?).ok()?.take(MAX_FILE_BYTES).read_to_string(&mut text).ok()?;
    clean(text.lines().next().unwrap_or(""))
}

/// Keep `name` for the next time (what cannot be written is not an error: the name is then only for this run).
pub fn save(name: &str) {
    let Some(path) = file_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("{name}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_trimmed_and_held_to_the_limits() {
        assert_eq!(clean("  Vincent  ").as_deref(), Some("Vincent"));
        assert_eq!(clean("张三").as_deref(), Some("张三"));
        assert_eq!(clean(""), None);
        assert_eq!(clean("   "), None);
        assert_eq!(clean("a\u{7}b"), None);
        assert_eq!(clean(&"x".repeat(MAX_AUTHOR_CHARS)).map(|s| s.len()), Some(MAX_AUTHOR_CHARS));
        assert_eq!(clean(&"x".repeat(MAX_AUTHOR_CHARS + 1)), None);
    }
}

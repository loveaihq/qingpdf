//! Helpers shared by the integration tests: where the test files are, how to
//! find qpdf, and a small seeded random number generator.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The workspace root (this file is included from crates two levels down).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

pub fn corpus_root() -> PathBuf {
    workspace_root().join("tests").join("corpus")
}

/// Every `.pdf` below `dir`, sorted. Missing folders give nothing. Read-only.
pub fn walk_pdfs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// `tests/corpus/public` only: files that may be committed. The public corpus
/// is part of the repository, so a missing or empty one is a failure, not a
/// reason to skip the test that needs it.
pub fn public_files() -> Vec<PathBuf> {
    let dir = corpus_root().join("public");
    let files = walk_pdfs(&dir);
    assert!(
        !files.is_empty(),
        "the public test corpus is missing or empty: {} (it is part of the repository)",
        dir.display()
    );
    files
}

/// One file of the public corpus, `rel` being its path below `tests/corpus/public`.
/// It must be there: a missing one fails the test.
pub fn public_file(rel: &str) -> PathBuf {
    let path = corpus_root().join("public").join(rel);
    assert!(path.is_file(), "public corpus file {} is missing", path.display());
    path
}

/// Public, local and private corpora (the last also from the folder named by
/// `QINGPDF_PRIVATE_CORPUS`). The public one must be there; the others are
/// used when they exist.
pub fn corpus_files() -> Vec<PathBuf> {
    let mut all = public_files();
    let mut dirs = vec![corpus_root().join("local"), corpus_root().join("private")];
    if let Some(extra) = std::env::var_os("QINGPDF_PRIVATE_CORPUS") {
        dirs.push(PathBuf::from(extra));
    }
    for d in dirs {
        all.extend(walk_pdfs(&d));
    }
    all
}

/// Does `haystack` contain `needle`?
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// A path relative to the corpus folder, with forward slashes, for reports.
pub fn corpus_name(path: &Path) -> String {
    let root = corpus_root();
    path.strip_prefix(&root).unwrap_or(path).to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/")
}

/// `tests/out/<name>`, created and emptied (it is git-ignored).
pub fn fresh_out_dir(name: &str) -> PathBuf {
    let dir = workspace_root().join("tests").join("out").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("cannot create the test output folder");
    dir
}

/// Where qpdf is: the file named by `QINGPDF_QPDF`, else the portable copy in
/// `C:\Dev\tools\qpdf\bin`, else the PATH, else `C:\Program Files\qpdf*\bin`.
pub fn find_qpdf() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "qpdf.exe" } else { "qpdf" };
    if let Some(named) = std::env::var_os("QINGPDF_QPDF") {
        let named = PathBuf::from(named);
        if named.is_file() {
            return Some(named);
        }
    }
    let portable = Path::new("C:/Dev/tools/qpdf/bin").join(exe);
    if portable.is_file() {
        return Some(portable);
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for base in [r"C:\Program Files", r"C:\Program Files (x86)"] {
        let Ok(entries) = std::fs::read_dir(base) else { continue };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().to_ascii_lowercase().starts_with("qpdf") {
                let candidate = entry.path().join("bin").join(exe);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// What `qpdf --check` said.
#[derive(Clone)]
pub struct QpdfVerdict {
    /// 0 fine, 2 errors, 3 warnings only.
    pub code: i32,
    pub text: String,
}

pub fn qpdf_check(qpdf: &Path, file: &Path) -> QpdfVerdict {
    let out = Command::new(qpdf).arg("--check").arg(file).output().expect("cannot run qpdf");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    QpdfVerdict { code: out.status.code().unwrap_or(-1), text }
}

/// A file holding `password` as UTF-8 bytes, for qpdf's `--password-file`
/// (command line arguments cannot carry every password the same way on every
/// system). Lives in the system's temporary folder.
pub fn password_file(password: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("qingpdf-test-password-{}-{n}.txt", std::process::id()));
    std::fs::write(&path, password.as_bytes()).expect("cannot write a password file");
    path
}

/// Run qpdf with the password `password` (taken from a file) and `args`.
pub fn qpdf_with_password(qpdf: &Path, password: &str, args: &[&std::ffi::OsStr]) -> std::process::Output {
    let file = password_file(password);
    let mut arg = std::ffi::OsString::from("--password-file=");
    arg.push(file.as_os_str());
    let out = Command::new(qpdf).arg(arg).args(args).output().expect("cannot run qpdf");
    let _ = std::fs::remove_file(&file);
    out
}

/// [`qpdf_check`] of a file that needs `password`.
pub fn qpdf_check_pw(qpdf: &Path, file: &Path, password: &str) -> QpdfVerdict {
    let out = qpdf_with_password(qpdf, password, &["--check".as_ref(), file.as_os_str()]);
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    QpdfVerdict { code: out.status.code().unwrap_or(-1), text }
}

/// What `qpdf --show-encryption --show-encryption-key` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QpdfEncryption {
    pub revision: i64,
    pub permissions: i64,
    /// Lower case hex.
    pub key: String,
    pub user_matched: bool,
    pub owner_matched: bool,
    /// The "stream/string/file encryption method" lines, such as `AESv3`.
    pub methods: Vec<String>,
}

/// `None` if qpdf does not accept the password (or the file is not encrypted).
pub fn qpdf_encryption(qpdf: &Path, file: &Path, password: &str) -> Option<QpdfEncryption> {
    let out = qpdf_with_password(
        qpdf,
        password,
        &["--show-encryption".as_ref(), "--show-encryption-key".as_ref(), file.as_os_str()],
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.contains("Incorrect password") || text.contains("invalid password") {
        return None;
    }
    let value = |prefix: &str| text.lines().find_map(|l| l.strip_prefix(prefix)).map(|v| v.trim().to_string());
    Some(QpdfEncryption {
        revision: value("R = ")?.parse().ok()?,
        permissions: value("P = ")?.parse().ok()?,
        key: value("Encryption key = ").unwrap_or_default().to_lowercase(),
        user_matched: text.contains("Supplied password is user password"),
        owner_matched: text.contains("Supplied password is owner password"),
        methods: text
            .lines()
            .filter(|l| l.contains("encryption method:"))
            .map(|l| l.rsplit(':').next().unwrap_or("").trim().to_string())
            .collect(),
    })
}

/// Make a copy of `file` in which qpdf has removed the encryption, as a QDF
/// file: streams as they are (so that every byte can be compared), every
/// object marked with the number it had in `file` (`%% Original object ID`).
pub fn qpdf_decrypt(qpdf: &Path, file: &Path, password: &str, out: &Path) -> bool {
    let run = qpdf_with_password(
        qpdf,
        password,
        &[
            "--qdf".as_ref(),
            "--stream-data=preserve".as_ref(),
            "--normalize-content=n".as_ref(),
            "--object-streams=disable".as_ref(),
            file.as_os_str(),
            out.as_os_str(),
        ],
    );
    // 0 is fine, 3 is "warnings only".
    matches!(run.status.code(), Some(0 | 3)) && out.is_file()
}

/// In a QDF file: the number each object has there, by the number it had in
/// the file it was made from (the `%% Original object ID: N G` comments).
pub fn qdf_original_numbers(bytes: &[u8]) -> std::collections::HashMap<u32, u32> {
    let text = String::from_utf8_lossy(bytes);
    let mut map = std::collections::HashMap::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("%% Original object ID: ") else { continue };
        let original = rest.split_whitespace().next().and_then(|n| n.parse::<u32>().ok());
        let new = lines.next().and_then(|l| l.split_whitespace().next()).and_then(|n| n.parse::<u32>().ok());
        if let (Some(original), Some(new)) = (original, new) {
            map.insert(original, new);
        }
    }
    map
}

/// [`qpdf_check`] of an input, remembered: the same input is judged again for
/// every output made from it, and qpdf is slow on big files.
fn qpdf_check_input(qpdf: &Path, file: &Path) -> QpdfVerdict {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, QpdfVerdict>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(known) = cache.lock().expect("cache lock").get(file) {
        return known.clone();
    }
    let verdict = qpdf_check(qpdf, file);
    cache.lock().expect("cache lock").insert(file.to_path_buf(), verdict.clone());
    verdict
}

/// One complaint of qpdf (a `WARNING:` line), reduced to what it says: the file
/// name (wherever it appears, as `file` is printed) and the `(object 5 0,
/// offset 120)` part are taken out and every number becomes `N`, so that the
/// same complaint about an input and about the file made from it compares
/// equal. Lower case.
fn normalise_complaint(line: &str, file: &Path) -> String {
    let named = file.display().to_string();
    let line = line.replace(&named, "FILE");
    let line = line.as_str();
    // `WARNING: FILE (<where>): <message>` or `WARNING: FILE: <message>`.
    let rest = line.split_once(": ").map_or(line, |(_, rest)| rest);
    let message = match rest.find("): ") {
        Some(i) if rest.get(..i).is_some_and(|head| head.contains(" (")) => rest.get(i + 3..).unwrap_or(rest),
        _ => rest.split_once(": ").map_or(rest, |(_, message)| message),
    };
    let mut out = String::new();
    let mut in_number = false;
    for c in message.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push('N');
            }
            in_number = true;
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// The normalised warnings in the text qpdf printed when it checked `file`.
pub fn qpdf_complaints(text: &str, file: &Path) -> Vec<String> {
    text.lines().filter(|l| l.starts_with("WARNING")).map(|l| normalise_complaint(l, file)).collect()
}

/// What to make of the output of an operation, judged by qpdf.
pub enum Judgement {
    /// qpdf has nothing to say.
    Clean,
    /// qpdf complains, but only about what it complains about in the inputs
    /// too, so the writer did not add to it.
    InputQuirk,
    /// qpdf complains about something that is not in the inputs.
    Fail(String),
}

/// The one complaint that is a documented limit and not a defect: `merge` does
/// not merge the form fields of the later files (the user is warned), so their
/// widgets are on the pages but not reachable from the output's `/AcroForm`.
const ORPHAN_WIDGETS: &str = "widget annotation is not reachable from /acroform";

/// Judge `output`, made from `inputs`: clean; or not clean but no worse than
/// the inputs (every normalised warning is one of an input's); or a failure.
/// An output qpdf calls broken (exit 2) is a failure unless an input is just
/// as broken. `later_inputs_have_forms` allows the orphan-widget warning for
/// merges whose later inputs have form fields.
pub fn judge_qpdf_output(qpdf: &Path, output: &Path, inputs: &[&Path], later_inputs_have_forms: bool) -> Judgement {
    let verdict = qpdf_check(qpdf, output);
    if verdict.code == 0 {
        return Judgement::Clean;
    }
    let seen: Vec<QpdfVerdict> = inputs.iter().map(|p| qpdf_check_input(qpdf, p)).collect();
    match verdict.code {
        2 if seen.iter().any(|v| v.code == 2) => return Judgement::InputQuirk,
        3 => {}
        _ => return Judgement::Fail(format!("exit {} (no input is that broken)\n{}", verdict.code, verdict.text)),
    }
    let known: std::collections::HashSet<String> =
        seen.iter().zip(inputs).flat_map(|(v, path)| qpdf_complaints(&v.text, path)).collect();
    let new: Vec<String> = qpdf_complaints(&verdict.text, output)
        .into_iter()
        .filter(|c| !known.contains(c) && !(later_inputs_have_forms && c.contains(ORPHAN_WIDGETS)))
        .collect();
    if new.is_empty() {
        Judgement::InputQuirk
    } else {
        Judgement::Fail(format!("warnings the inputs do not give: {new:?}\n{}", verdict.text))
    }
}

/// xorshift64*: tiny, seeded, repeatable. Not for anything but test data.
pub struct XorShift(pub u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        XorShift(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number in `0..n` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n.max(1) as u64)) as usize
    }
}

// --- the encrypted files and their passwords --------------------------------------------

/// One generated file, from `manifest.tsv`.
#[derive(Debug, Clone)]
pub struct Encrypted {
    pub path: PathBuf,
    pub user: String,
    pub owner: String,
    /// Other passwords that open it as the user (the full-width form of a
    /// password, which becomes the plain one by the light SASLprep).
    pub alternatives: Vec<String>,
}

pub fn generated_dir() -> PathBuf {
    corpus_root().join("public").join("encrypted").join("qpdf-generated")
}

pub fn generated() -> Vec<Encrypted> {
    let text = std::fs::read_to_string(generated_dir().join("manifest.tsv")).expect("manifest.tsv of the generated files");
    let rows: Vec<Encrypted> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            Encrypted {
                path: generated_dir().join(cols[0]),
                user: cols[1].to_string(),
                owner: cols[2].to_string(),
                alternatives: cols[3].split(',').filter(|s| !s.is_empty()).map(str::to_string).collect(),
            }
        })
        .collect();
    assert!(rows.len() >= 20, "the generated set is missing: {} rows", rows.len());
    for row in &rows {
        assert!(row.path.is_file(), "{} is missing", row.path.display());
    }
    rows
}

/// The files that were not made for this project, with the passwords of the
/// user and, if known, of the owner. `required`: in the public corpus (must be
/// there); the others are local.
pub struct Known {
    pub rel: &'static str,
    pub user: &'static str,
    pub owner: Option<&'static str>,
    pub required: bool,
}

/// Where the passwords come from: PDFium's tests (the `encrypted_hello_world`
/// files: user "hôtel", owner "âge", written in Latin-1 for revisions 2 and 3
/// and UTF-8 for 5 and 6), pdf.js's manifest (`ELXRTQWS`, `asdfasdf`), and
/// qpdf, which found `a` and `b` for `bug_644.pdf` among the first guesses.
pub const KNOWN: &[Known] = &[
    Known { rel: "public/encrypted/bug_644.pdf", user: "a", owner: Some("b"), required: true },
    Known { rel: "public/encrypted/encrypted_hello_world_r2.pdf", user: "hôtel", owner: Some("âge"), required: true },
    Known { rel: "public/encrypted/encrypted_hello_world_r3.pdf", user: "hôtel", owner: Some("âge"), required: true },
    Known { rel: "public/encrypted/encrypted_hello_world_r5.pdf", user: "hôtel", owner: Some("âge"), required: true },
    Known { rel: "public/encrypted/encrypted_hello_world_r6.pdf", user: "hôtel", owner: Some("âge"), required: true },
    Known { rel: "local/encrypted/bug900822.pdf", user: "", owner: None, required: false },
    Known { rel: "local/encrypted/empty_protected.pdf", user: "", owner: None, required: false },
    Known { rel: "local/encrypted/issue17215.pdf", user: "", owner: None, required: false },
    // V4 with a 5-byte RC4 key; qpdf wants 128 bits for every V4 file and cannot open it.
    Known { rel: "local/encrypted/issue19484_1.pdf", user: "", owner: None, required: false },
    Known { rel: "local/encrypted/issue3371.pdf", user: "ELXRTQWS", owner: None, required: false },
    Known { rel: "local/encrypted/pr6531_1.pdf", user: "asdfasdf", owner: Some("asdfasdf"), required: false },
];

/// A classic file whose objects have moved: the cross-reference table written
/// again from where the `n 0 obj` lines are now. (A file with a wrong table is
/// repaired by every reader, and qpdf does not decrypt everything it reads while
/// it repairs, so the edited files must be in order.)
pub fn with_fresh_xref(bytes: &[u8]) -> Vec<u8> {
    let find = |needle: &[u8], from: usize| {
        bytes.get(from..).and_then(|b| b.windows(needle.len()).position(|w| w == needle)).map(|p| p + from)
    };
    let xref_at = (0..bytes.len()).rev().find(|&i| bytes[i..].starts_with(b"\nxref\n")).expect("an xref table") + 1;
    let trailer_at = find(b"trailer", xref_at).expect("a trailer");
    let startxref_at = find(b"startxref", trailer_at).expect("startxref");
    let mut offsets: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    let mut i = 0;
    while i < xref_at {
        // A line `N 0 obj` at the start of a line.
        if (i == 0 || bytes[i - 1] == b'\n') && bytes[i].is_ascii_digit() {
            let line_end = bytes[i..].iter().position(|&b| b == b'\n').map_or(xref_at, |p| i + p);
            let line = &bytes[i..line_end];
            if line.ends_with(b" 0 obj") {
                let number = std::str::from_utf8(&line[..line.len() - 6]).ok().and_then(|t| t.parse::<u32>().ok());
                if let Some(n) = number {
                    offsets.insert(n, i);
                }
            }
        }
        i += 1;
    }
    let size = offsets.keys().max().map_or(1, |m| m + 1);
    let mut out = bytes[..xref_at].to_vec();
    let new_xref = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        match offsets.get(&n) {
            Some(at) => out.extend_from_slice(format!("{at:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(&bytes[trailer_at..startxref_at]);
    out.extend_from_slice(format!("startxref\n{new_xref}\n%%EOF\n").as_bytes());
    out
}

/// The password that opens `path` if it is one of the encrypted files whose
/// passwords are known; the empty password otherwise.
pub fn password_for(path: &Path) -> String {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let wanted = canonical(path);
    if let Some(known) = KNOWN.iter().find(|k| canonical(&corpus_root().join(k.rel)) == wanted) {
        return known.user.to_string();
    }
    if (path.starts_with(generated_dir()) || wanted.starts_with(canonical(&generated_dir())))
        && let Some(row) = generated().into_iter().find(|r| canonical(&r.path) == wanted)
    {
        return row.user;
    }
    String::new()
}

/// qpdf accepts `output` (opened with `out_password`), or complains only about
/// what it complains about in the inputs too.
pub fn qpdf_accepts(qpdf: &Path, output: &Path, out_password: &str, inputs: &[(&Path, &str)], what: &str) {
    let verdict = qpdf_check_pw(qpdf, output, out_password);
    if verdict.code == 0 {
        return;
    }
    let seen: Vec<QpdfVerdict> = inputs.iter().map(|(p, pw)| qpdf_check_pw(qpdf, p, pw)).collect();
    if verdict.code == 2 && seen.iter().any(|v| v.code == 2) {
        return;
    }
    assert_eq!(verdict.code, 3, "{what}: qpdf --check says:\n{}", verdict.text);
    let known: HashSet<String> =
        seen.iter().zip(inputs).flat_map(|(v, (p, _))| qpdf_complaints(&v.text, p)).collect();
    // A merge does not carry over the form fields of the files after the first (the
    // user is warned, decisions.md): their widgets are on the pages, not in /AcroForm.
    let merging = what.contains("merge") || what.starts_with("enc-");
    let new: Vec<String> = qpdf_complaints(&verdict.text, output)
        .into_iter()
        .filter(|c| !known.contains(c) && !(merging && c.contains("widget annotation is not reachable from /acroform")))
        .collect();
    assert!(new.is_empty(), "{what}: qpdf complains about what the inputs do not give: {new:?}\n{}", verdict.text);
}

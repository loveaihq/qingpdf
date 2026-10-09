//! The `qingpdf` command line.

mod args;
mod help;

use std::borrow::Cow;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use qingpdf_core::image::PageMode;
use qingpdf_core::info::{self, Report};
use qingpdf_core::security::{Method, PasswordKind};
use qingpdf_core::ops::{self, Input, LoadedImage, Output};
use qingpdf_core::{Document, Error, Warning, render, text};

use args::{Parsed, Request, UsageError};

/// An error message for the user; the exit code is 1.
type Failure = String;

fn main() -> ExitCode {
    match args::parse(std::env::args_os().skip(1)) {
        Ok(Parsed::Help(command)) => {
            say(&help::text(command));
            ExitCode::SUCCESS
        }
        Ok(Parsed::Version) => {
            say(&format!("qingpdf {}\n", env!("CARGO_PKG_VERSION")));
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(request)) => match run(request) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("error: {}", clean(&message));
                ExitCode::from(1)
            }
        },
        Err(UsageError { message, command }) => {
            eprintln!("qingpdf: {}", clean(&message));
            match command {
                Some(c) => eprintln!("Try 'qingpdf {} --help' for usage.", c.name()),
                None => eprintln!("Try 'qingpdf --help' for usage."),
            }
            ExitCode::from(2)
        }
    }
}

/// Text for a terminal: control characters (C0, DEL and C1, which include the
/// escape that starts terminal commands) are written as `\xNN`, except the
/// line feed. Everything the program prints can contain something taken from a
/// file (a name, a message about an object), and a file must not be able to
/// drive the terminal.
fn clean(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c.is_control() && c != '\n') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_control() && c != '\n' {
            out.push_str(&format!("\\x{:02x}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Print to standard output without panicking if the pipe is closed.
fn say(text: &str) {
    let _ = std::io::stdout().write_all(clean(text).as_bytes());
}

fn run(request: Request) -> Result<(), Failure> {
    match request {
        Request::Info { input, password } => info_command(&input, &password),
        Request::Merge { inputs, output, force, password } => merge_command(&inputs, &output, force, &password),
        Request::SplitPages { input, pages, output, force, password } => {
            let doc = open_unlocked(&input, &password)?;
            let wanted = page_list(&doc, &input, &pages)?;
            check_output(&output, std::slice::from_ref(&input), force)?;
            let out = ops::extract_pages(&doc, &wanted).map_err(|e| op_error(&input, &e))?;
            finish(&output, &out)
        }
        Request::SplitEvery { input, every, output, force, password } => {
            split_every_command(&input, every, &output, force, &password)
        }
        Request::Delete { input, pages, output, force, password } => {
            let doc = open_unlocked(&input, &password)?;
            let gone = page_list(&doc, &input, &pages)?;
            check_output(&output, std::slice::from_ref(&input), force)?;
            let out = ops::delete_pages(&doc, &gone).map_err(|e| op_error(&input, &e))?;
            finish(&output, &out)
        }
        Request::Rotate { input, pages, angle, output, force, password } => {
            let doc = open_unlocked(&input, &password)?;
            let turn = match pages {
                Some(list) => page_list(&doc, &input, &list)?,
                None => (0..doc.page_count().map_err(|e| op_error(&input, &e))?).collect(),
            };
            check_output(&output, std::slice::from_ref(&input), force)?;
            let out = ops::rotate_pages(&doc, &turn, angle).map_err(|e| op_error(&input, &e))?;
            finish(&output, &out)
        }
        Request::Decrypt { input, output, force, password } => {
            let doc = open_unlocked(&input, &password)?;
            check_output(&output, std::slice::from_ref(&input), force)?;
            if !doc.is_encrypted() {
                return Err(format!("{} is not encrypted; there is nothing to decrypt", input.display()));
            }
            let out = ops::decrypt(&doc, &password).map_err(|e| op_error(&input, &e))?;
            finish(&output, &out)
        }
        Request::Text { input, pages, output, force, password } => {
            text_command(&input, pages.as_deref(), output.as_deref(), force, &password)
        }
        Request::Render { input, pages, dpi, annots, output, force, password } => {
            render_command(&input, pages.as_deref(), dpi, annots, &output, force, &password)
        }
        Request::Img2pdf { inputs, mode, output, force } => images_command(&inputs, mode, &output, force),
    }
}

// --- reading and writing files -------------------------------------------------------

fn needs_password_message(path: &Path) -> String {
    format!("{} needs a password; give it with --password", path.display())
}

fn wrong_password_message(path: &Path) -> String {
    format!("wrong password for {}", path.display())
}

/// Say why a file could not be opened, for people.
fn open_error(path: &Path, e: &Error) -> String {
    match e {
        Error::Io(io) => format!("cannot read {}: {io}", path.display()),
        Error::PasswordRequired => needs_password_message(path),
        Error::WrongPassword => wrong_password_message(path),
        Error::Unsupported(m) => format!("{}: not supported: {m}", path.display()),
        Error::Limit(_) => format!("{} is not opened: it asks for more than is safe ({e})", path.display()),
        // The file is a PDF; what is wrong is how it says it is encrypted.
        Error::Syntax { message, .. }
            if message.starts_with("encryption dictionary") || message.starts_with("cannot read the encryption dictionary") =>
        {
            format!("{} cannot be opened: its encryption is damaged ({message})", path.display())
        }
        other => format!("{} is not a PDF file, or is too damaged to read ({other})", path.display()),
    }
}

/// Say why an operation on an open file failed.
fn op_error(path: &Path, e: &Error) -> String {
    match e {
        Error::PasswordRequired => needs_password_message(path),
        Error::WrongPassword => wrong_password_message(path),
        Error::Unsupported(m) => format!("{}: not supported yet: {m}", path.display()),
        Error::Invalid(m) => m.clone(),
        other => format!("{}: {other}", path.display()),
    }
}

/// Open a file. If it needs a password, `password` is tried first and then the
/// empty one; a file that opens with neither is an error when a password was
/// given, and is returned locked when none was (only `info` can say something
/// about it then).
fn open_pdf(path: &Path, password: &str) -> Result<Document, Failure> {
    Document::open_with_password(path, password).map_err(|e| open_error(path, &e))
}

/// Open a file and refuse it if it is still locked: operations need to read it.
fn open_unlocked(path: &Path, password: &str) -> Result<Document, Failure> {
    let doc = open_pdf(path, password)?;
    if doc.is_locked() {
        return Err(needs_password_message(path));
    }
    Ok(doc)
}

fn page_list(doc: &Document, path: &Path, list: &str) -> Result<Vec<usize>, Failure> {
    let count = doc.page_count().map_err(|e| op_error(path, &e))?;
    ops::parse_page_ranges(list, count).map_err(|e| op_error(path, &e))
}

/// Refuse to write over an input, or over an existing file without `--force`.
fn check_output(output: &Path, inputs: &[PathBuf], force: bool) -> Result<(), Failure> {
    if output.is_dir() {
        return Err(format!("{} is a folder, not a file name", output.display()));
    }
    if !output.exists() {
        return Ok(());
    }
    if let Ok(out) = fs::canonicalize(output) {
        for input in inputs {
            if fs::canonicalize(input).is_ok_and(|i| i == out) {
                return Err(format!(
                    "the output file {} is the same as an input file; choose another name",
                    output.display()
                ));
            }
        }
    }
    if !force {
        return Err(format!("{} already exists; use --force to overwrite it", output.display()));
    }
    Ok(())
}

fn print_warnings(warnings: &[Warning]) {
    for w in warnings {
        eprintln!("warning: {}", clean(&w.to_string()));
    }
}

/// Write `data` as the file `target` all at once: into a new file next to it,
/// which then takes its place (a rename, which replaces an existing file).
/// Nothing is ever written into the old file, so an input that is another name
/// for the same data (a hard link) is left as it was, and a failure part way
/// leaves no half-written output.
fn write_replacing(target: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "out".to_string());
    for attempt in 0..1000u32 {
        let temp = dir.join(format!(".{name}.qingpdf-{}-{attempt}.tmp", std::process::id()));
        let mut file = match fs::OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let written = file.write_all(data).and_then(|()| file.flush());
        drop(file);
        let done = written.and_then(|()| fs::rename(&temp, target));
        if done.is_err() {
            let _ = fs::remove_file(&temp);
        }
        return done;
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free name for a temporary file"))
}

fn write_file(output: &Path, data: &[u8]) -> Result<(), Failure> {
    write_replacing(output, data).map_err(|e| format!("cannot write {}: {e}", output.display()))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Write a finished file and report.
fn finish(output: &Path, out: &Output) -> Result<(), Failure> {
    write_file(output, &out.data)?;
    print_warnings(&out.warnings);
    say(&format!("wrote {} ({} page{})\n", output.display(), out.pages, plural(out.pages)));
    Ok(())
}

// --- commands ------------------------------------------------------------------------------

/// Screen text: like [`clean`], but the form feed that separates pages stays.
fn say_text(text: &str) {
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive('\u{c}') {
        match piece.strip_suffix('\u{c}') {
            Some(body) => {
                out.push_str(&clean(body));
                out.push('\u{c}');
            }
            None => out.push_str(&clean(piece)),
        }
    }
    let _ = std::io::stdout().write_all(out.as_bytes());
}

fn text_command(
    input: &Path,
    pages: Option<&str>,
    output: Option<&Path>,
    force: bool,
    password: &str,
) -> Result<(), Failure> {
    let doc = open_unlocked(input, password)?;
    text::check_extraction_allowed(&doc, password).map_err(|e| op_error(input, &e))?;
    let wanted: Vec<usize> = match pages {
        Some(list) => page_list(&doc, input, list)?,
        None => (0..doc.page_count().map_err(|e| op_error(input, &e))?).collect(),
    };
    if let Some(out) = output {
        check_output(out, std::slice::from_ref(&input.to_path_buf()), force)?;
    }
    let all = doc.pages().map_err(|e| op_error(input, &e))?;
    let mut extractor = text::TextExtractor::new(&doc);
    let mut result = String::new();
    for (k, &index) in wanted.iter().enumerate() {
        let Some(page) = all.get(index) else { continue };
        if k > 0 {
            result.push('\u{c}');
        }
        let page_text = extractor.page_text(page).map_err(|e| format!("page {}: {}", index + 1, op_error(input, &e)))?;
        result.push_str(&page_text);
    }
    for w in extractor.take_warnings() {
        eprintln!("warning: {}", clean(&w));
    }
    match output {
        Some(out) => {
            write_file(out, result.as_bytes())?;
            say(&format!("wrote {} ({} page{})\n", out.display(), wanted.len(), plural(wanted.len())));
        }
        None => say_text(&result),
    }
    Ok(())
}

fn render_command(input: &Path, pages: Option<&str>, dpi: f64, annots: bool, template: &str, force: bool, password: &str) -> Result<(), Failure> {
    let doc = open_unlocked(input, password)?;
    let wanted: Vec<usize> = match pages {
        Some(list) => page_list(&doc, input, list)?,
        None => (0..doc.page_count().map_err(|e| op_error(input, &e))?).collect(),
    };
    if wanted.len() > 1 && !template.contains("%d") {
        return Err("with more than one page the output name must contain %d (for example page_%d.png)".to_string());
    }
    // Work out and check every name before drawing anything.
    let names: Vec<PathBuf> = wanted.iter().map(|&i| PathBuf::from(template.replace("%d", &(i + 1).to_string()))).collect();
    for name in &names {
        check_output(name, std::slice::from_ref(&input.to_path_buf()), force)?;
    }
    let all = doc.pages().map_err(|e| op_error(input, &e))?;
    let mut renderer = render::Renderer::new(&doc);
    renderer.set_annotations(annots);
    // Each page is reported as soon as it is done, so that if a later page fails the pages written are known.
    for (&index, name) in wanted.iter().zip(&names) {
        let Some(page) = all.get(index) else { continue };
        let drawn = renderer.render_page(page, dpi);
        for w in renderer.take_warnings() {
            eprintln!("warning: page {}: {}", index + 1, clean(&w));
        }
        let bitmap = drawn.map_err(|e| format!("page {}: {}", index + 1, op_error(input, &e)))?;
        let png = bitmap.to_png().map_err(|e| format!("page {}: {}", index + 1, op_error(input, &e)))?;
        write_file(name, &png)?;
        say(&format!("wrote {} ({} x {} pixels)
", name.display(), bitmap.width, bitmap.height));
        if bitmap.boxed_characters > 0 {
            let [no_font, no_char, not_in_font] = bitmap.boxed_causes;
            eprintln!(
                "warning: page {}: {} characters are drawn as outline boxes (no font for them: {no_font}; code with no character: {no_char}; character not in the font: {not_in_font})",
                index + 1,
                bitmap.boxed_characters
            );
        }
    }
    Ok(())
}

fn merge_command(inputs: &[PathBuf], output: &Path, force: bool, password: &str) -> Result<(), Failure> {
    let docs: Vec<Document> = inputs.iter().map(|p| open_unlocked(p, password)).collect::<Result<_, _>>()?;
    let names: Vec<String> = inputs.iter().map(|p| p.display().to_string()).collect();
    check_output(output, inputs, force)?;
    let merge_inputs: Vec<Input<'_>> =
        docs.iter().zip(&names).map(|(doc, name)| Input { name: name.as_str(), doc, password }).collect();
    let out = ops::merge(&merge_inputs).map_err(|e| match &e {
        Error::Invalid(m) => m.clone(),
        other => format!("merge failed: {other}"),
    })?;
    finish(output, &out)
}

fn split_every_command(input: &Path, every: usize, template: &Path, force: bool, password: &str) -> Result<(), Failure> {
    let doc = open_unlocked(input, password)?;
    let count = doc.page_count().map_err(|e| op_error(input, &e))?;
    let template = template.to_str().ok_or("the output name is not valid text")?;
    // Work out and check every name before writing any file.
    let names: Vec<PathBuf> = (1..=ops::chunk_count(count, every))
        .map(|n| PathBuf::from(template.replace("%d", &n.to_string())))
        .collect();
    for name in &names {
        check_output(name, &[input.to_path_buf()], force)?;
    }
    let mut report: Vec<String> = Vec::new();
    // The files are written by a second thread while the next one is made; a
    // short queue keeps at most a few of them in memory.
    let (to_disk, queue) = std::sync::mpsc::sync_channel::<(PathBuf, Vec<u8>)>(2);
    let writer = std::thread::spawn(move || -> Result<(), Failure> {
        for (name, data) in queue {
            write_replacing(&name, &data).map_err(|e| format!("cannot write {}: {e}", name.display()))?;
        }
        Ok(())
    });
    let made = ops::split_every(&doc, every, &mut |n, out| {
        let name = n
            .checked_sub(1)
            .and_then(|i| names.get(i))
            .ok_or_else(|| Error::Invalid("more files than expected".to_string()))?;
        report.push(format!("wrote {} ({} page{})\n", name.display(), out.pages, plural(out.pages)));
        // A send fails only when the writer has stopped; its error is told below.
        to_disk
            .send((name.clone(), out.data))
            .map_err(|_| Error::Invalid("the output files could not be written".to_string()))
    });
    drop(to_disk);
    let written = writer.join().map_err(|_| "the thread writing the files failed".to_string())?;
    // If a file could not be written, say why that was, not that the rest were dropped.
    written?;
    let warnings = made.map_err(|e| op_error(input, &e))?;
    print_warnings(&warnings);
    for line in report {
        say(&line);
    }
    Ok(())
}

fn images_command(inputs: &[PathBuf], mode: PageMode, output: &Path, force: bool) -> Result<(), Failure> {
    check_output(output, inputs, force)?;
    // One image at a time: each file is read when its turn comes and dropped
    // before the next is read.
    let mut load = |i: usize| -> Result<LoadedImage<'static>, Error> {
        let path = inputs.get(i).ok_or_else(|| Error::Invalid("no such image".to_string()))?;
        let data =
            fs::read(path).map_err(|e| Error::Invalid(format!("cannot read {}: {e}", path.display())))?;
        Ok(LoadedImage { name: path.display().to_string(), data: Cow::Owned(data) })
    };
    let out = ops::images_to_pdf_with(inputs.len(), &mut load, mode).map_err(|e| match &e {
        Error::Invalid(m) => m.clone(),
        other => format!("cannot make the PDF: {other}"),
    })?;
    finish(output, &out)
}

// --- info ---------------------------------------------------------------------------------------

fn info_command(input: &Path, password: &str) -> Result<(), Failure> {
    let doc = open_pdf(input, password)?;
    let report = info::describe(&doc).map_err(|e| op_error(input, &e))?;
    say(&render_info(input, &report));
    Ok(())
}

/// A number with at most `decimals` decimals and no trailing zeros.
fn trimmed(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn render_info(path: &Path, r: &Report) -> String {
    let mut s = String::new();
    s.push_str(&format!("File:                {}\n", path.display()));
    s.push_str(&format!("PDF version:         {}.{}\n", r.version.0, r.version.1));
    match &r.pages_unavailable {
        Some(why) => s.push_str(&format!("Pages:               (not available: {why})\n")),
        None => s.push_str(&format!("Pages:               {}\n", r.pages.len())),
    }
    s.push_str(&format!("Encrypted:           {}\n", yes_no(r.encrypted)));
    if let Some(e) = &r.encryption {
        render_encryption(&mut s, e);
    }
    s.push_str(&format!("Cross-ref streams:   {}\n", yes_no(r.xref_streams)));
    s.push_str(&format!("Object streams:      {}\n", yes_no(r.object_streams)));
    s.push_str(&format!(
        "Repaired:            {}\n",
        if r.repaired { "yes (the cross-reference table was damaged and was rebuilt by scanning the file)" } else { "no" }
    ));
    if let Some(warning) = &r.warning {
        s.push_str(&format!("Warning:             {warning}\n"));
    }
    s.push_str("\nDocument info:\n");
    if let Some(why) = &r.info_unavailable {
        s.push_str(&format!("  (not available: {why})\n"));
    } else if r.info.is_empty() {
        s.push_str("  (none)\n");
    } else {
        for (key, value) in &r.info {
            s.push_str(&format!("  {key}: {value}\n"));
        }
    }
    s.push_str("\nPages (size as displayed, rotation):\n");
    let width = r.pages.len().to_string().len();
    for (i, page) in r.pages.iter().enumerate() {
        let size = match (page.size_pt, page.size_mm()) {
            (Some((w, h)), Some((mw, mh))) => {
                format!("{} x {} pt ({} x {} mm)", trimmed(w, 2), trimmed(h, 2), trimmed(mw, 1), trimmed(mh, 1))
            }
            _ => "size unknown".to_string(),
        };
        let rotation = if page.rotation != 0 { format!(", rotated {}", page.rotation) } else { String::new() };
        s.push_str(&format!("  {:>width$}: {size}{rotation}\n", i + 1));
    }
    s
}

/// The lines about how the file is encrypted: the method, whether a password
/// is needed and which one opened it, and what the author allows.
fn render_encryption(s: &mut String, e: &qingpdf_core::security::Encryption) {
    let metadata = if e.encrypt_metadata { "" } else { ", metadata not encrypted" };
    s.push_str(&format!("Encryption:          {} (V{}, R{}{metadata})\n", e.method_name(), e.version, e.revision));
    let method_note = if e.stream_method == Method::None && e.string_method == Method::None && e.file_method == Method::None
    {
        " (nothing is actually encrypted)"
    } else {
        ""
    };
    let password = match e.opened {
        None => "needed (none was given; use --password)".to_string(),
        Some(a) => {
            let which = match (a.kind, a.empty) {
                (PasswordKind::User, true) => "the empty user password",
                (PasswordKind::Owner, true) => "the empty password, which is also the owner password",
                (PasswordKind::User, false) => "the user password",
                (PasswordKind::Owner, false) => "the owner password",
            };
            if e.needs_password() {
                format!("needed (opened with {which})")
            } else if a.empty {
                format!("not needed (opened with {which})")
            } else {
                format!("not needed (the empty password opens it too; opened with {which})")
            }
        }
    };
    s.push_str(&format!("Password to open:    {password}{method_note}\n"));
    for (i, (what, allowed)) in e.permissions.list().iter().enumerate() {
        let label = if i == 0 { "Permissions:        " } else { "                    " };
        s.push_str(&format!("{label} {what}: {}\n", yes_no(*allowed)));
    }
    if !e.perms_valid && e.opened.is_none_or(|a| a.kind != PasswordKind::Owner) {
        s.push_str(
            "                     (the permission flags /P do not agree with the check value /Perms that only the file key can read: someone may have edited them, so nothing is allowed when the file is opened with the user password)\n",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_trimmed() {
        assert_eq!(trimmed(595.276, 2), "595.28");
        assert_eq!(trimmed(612.0, 2), "612");
        assert_eq!(trimmed(210.0, 1), "210");
        assert_eq!(trimmed(35.2778, 1), "35.3");
        assert_eq!(trimmed(0.0, 2), "0");
        assert_eq!(trimmed(100.0, 0), "100");
    }
}

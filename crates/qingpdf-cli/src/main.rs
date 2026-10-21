//! The `qingpdf` command line.

mod args;
mod help;
mod random;

use std::borrow::Cow;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use qingpdf_core::compress::{self, Preset};
use qingpdf_core::edit::{self, ContentOp};
use qingpdf_core::image::PageMode;
use qingpdf_core::info::{self, Report};
use qingpdf_core::security::{Method, PasswordKind, Permissions, Protection};
use qingpdf_core::ops::{self, Input, LoadedImage, Output};
use qingpdf_core::{Document, Error, Warning, docx, render, text, xlsx};

use args::{BookmarkAction, Parsed, PrintLevel, Protect, RedactArgs, Request, UsageError};

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
        Request::Encrypt { input, output, force, password, how } => encrypt_command(&input, &output, force, &password, &how),
        Request::Compress { input, output, force, password, preset, lossless } => compress_command(&input, &output, force, &password, preset, lossless),
        Request::Text { input, pages, output, force, password } => {
            text_command(&input, pages.as_deref(), output.as_deref(), force, &password)
        }
        Request::Render { input, pages, dpi, annots, output, force, password } => {
            render_command(&input, pages.as_deref(), dpi, annots, &output, force, &password)
        }
        Request::Img2pdf { inputs, mode, output, force } => images_command(&inputs, mode, &output, force),
        Request::Watermark { input, text, image, opacity, angle, tile, under, pages, output, force, password } => {
            let doc = open_unlocked(&input, &password)?;
            let wanted = match pages {
                Some(list) => page_list(&doc, &input, &list)?,
                None => (0..doc.page_count().map_err(|e| op_error(&input, &e))?).collect(),
            };
            check_output(&output, std::slice::from_ref(&input), force)?;
            let image = match &image {
                Some(path) => read_picture(path)?,
                None => Vec::new(),
            };
            let pages = wanted.iter().map(|&p| u32::try_from(p).unwrap_or(u32::MAX)).collect();
            let mark = edit::Watermark { text: text.unwrap_or_default(), image, opacity, angle, tile, under, pages };
            add_command(&doc, &password, &input, &output, &ContentOp::Watermark(mark))
        }
        Request::Pagenum { input, slots, size, color, margin, pages, start, mirror, skip_first, output, force, password } => {
            let doc = open_unlocked(&input, &password)?;
            let wanted = match pages {
                Some(list) => page_list(&doc, &input, &list)?,
                None => (0..doc.page_count().map_err(|e| op_error(&input, &e))?).collect(),
            };
            check_output(&output, std::slice::from_ref(&input), force)?;
            let shown = size.clamp(edit::MIN_FONT_SIZE, edit::MAX_FONT_SIZE);
            if shown != size {
                eprintln!("warning: the size {size} was changed to {shown} (the text is from {} to {} points high)", edit::MIN_FONT_SIZE, edit::MAX_FONT_SIZE);
            }
            let spec = edit::Pagination {
                slots: (*slots).map(Option::unwrap_or_default),
                size: shown as f32,
                color,
                margin: margin as f32,
                pages: wanted.iter().map(|&p| u32::try_from(p).unwrap_or(u32::MAX)).collect(),
                start,
                mirror,
                skip_first,
                file: input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                date: edit::today_utc(),
            };
            add_command(&doc, &password, &input, &output, &ContentOp::Pagination(spec))
        }
        Request::Stamp { input, image, page, at, width, output, force, password } => {
            let doc = open_unlocked(&input, &password)?;
            check_output(&output, std::slice::from_ref(&input), force)?;
            let image = read_picture(&image)?;
            add_command(&doc, &password, &input, &output, &ContentOp::Image { page: page - 1, at, width, image })
        }
        Request::Docx { input, pages, output, force, password } => docx_command(&input, pages.as_deref(), &output, force, &password),
        Request::Xlsx { input, pages, output, force, password } => xlsx_command(&input, pages.as_deref(), &output, force, &password),
        Request::Bookmarks { input, action, output, force, password } => bookmarks_command(&input, &action, output.as_deref(), force, &password),
        Request::Replace { input, find, with, pages, output, force, password } => replace_command(&input, &find, &with, pages.as_deref(), &output, force, &password),
        Request::Redact(args) => redact_command(&args),
        Request::Addtext { input, page, at, size, text, output, force, password } => {
            let doc = open_unlocked(&input, &password)?;
            check_output(&output, std::slice::from_ref(&input), force)?;
            add_command(&doc, &password, &input, &output, &ContentOp::Text { page: page - 1, at, size, text: text.replace("\\n", "\n"), color: [0, 0, 0] })
        }
    }
}

/// The passwords of `--password-file`: line 1 the user password, line 2 the owner password (a missing line, or an empty one, is no
/// password). A byte order mark and the line ends are not part of them.
fn read_password_file(path: &Path) -> Result<(String, String), Failure> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() > 4096 {
        return Err(format!("{} is too big for a password file (line 1 the user password, line 2 the owner password)", path.display()));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    // A line ends with CRLF, LF or a lone CR, and that one end is no part of the password.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = text.split('\n');
    let (user, owner) = (lines.next().unwrap_or_default().to_string(), lines.next().unwrap_or_default().to_string());
    if lines.any(|l| !l.is_empty()) {
        return Err(format!("{} has more than two lines (line 1 is the user password, line 2 the owner password)", path.display()));
    }
    // What is left is the password as it is, so a space (or a tab) at either end is not something the line end left behind by accident:
    // `echo secret > pw.txt` in cmd writes "secret " and then the line end. Refused rather than guessed at; a control character likewise.
    for (which, password) in [("user", &user), ("owner", &owner)] {
        let spaced = password.chars().next().is_some_and(char::is_whitespace) || password.chars().next_back().is_some_and(char::is_whitespace);
        if spaced || password.chars().any(char::is_control) {
            return Err(format!(
                "the {which} password in {} starts or ends with a space, or has a control character (cmd's `echo word > file` leaves a space before the line end); write it without",
                path.display()
            ));
        }
    }
    Ok((user, owner))
}

/// What the options of `encrypt` ask for.
fn protection_of(how: &Protect) -> Result<Protection, Failure> {
    let (user, owner) = match &how.password_file {
        Some(path) => read_password_file(path)?,
        None => (how.user_password.clone().unwrap_or_default(), how.owner_password.clone().unwrap_or_default()),
    };
    let permissions = Permissions {
        print: how.print != PrintLevel::None,
        print_high_quality: how.print == PrintLevel::High,
        modify: !how.no_modify,
        copy: !how.no_copy,
        annotate: !how.no_annotate,
        fill_forms: !how.no_forms,
        assemble: !how.no_assemble,
        accessibility: !how.no_accessibility,
    };
    if !permissions.allows_everything() && owner.is_empty() {
        return Err("a permission is turned off, so an owner password is needed (--owner-password, or line 2 of --password-file): it is what keeps anyone from turning the permission on again".to_string());
    }
    if user.is_empty() && owner.is_empty() {
        return Err("nothing would be protected: give --user-password or --owner-password (or --password-file), or turn a permission off with --print, --no-copy and the like".to_string());
    }
    Ok(Protection {
        method: if how.aes128 { Method::AesV2 } else { Method::AesV3 },
        user_password: user,
        owner_password: owner,
        permissions,
        encrypt_metadata: !how.cleartext_metadata,
    })
}

/// Write a copy of the input under a new encryption (AES-256, or AES-128 with --aes128).
fn encrypt_command(input: &Path, output: &Path, force: bool, password: &str, how: &Protect) -> Result<(), Failure> {
    let doc = open_unlocked(input, password)?;
    check_output(output, std::slice::from_ref(&input.to_path_buf()), force)?;
    let protection = protection_of(how)?;
    // The engine takes the random bytes from here and has no source of its own.
    let seed = random::seed()?;
    let out = ops::protect(&doc, password, &protection, &seed).map_err(|e| op_error(input, &e))?;
    finish(output, &out)?;
    let denied: Vec<&str> = protection.permissions.list().iter().filter(|(_, allowed)| !allowed).map(|(what, _)| *what).collect();
    say(&format!(
        "  {}; {}; {}\n",
        if how.aes128 { "AES-128" } else { "AES-256" },
        if protection.user_password.is_empty() { "opens without a password" } else { "a password is needed to open it" },
        if denied.is_empty() { "nothing is restricted".to_string() } else { format!("not allowed: {}", denied.join(", ")) }
    ));
    Ok(())
}

/// Digits with a comma between each three.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Write a smaller copy of the input; say how big both are. When it would not be smaller the output is a copy of the input.
fn compress_command(input: &Path, output: &Path, force: bool, password: &str, preset: Preset, lossless: bool) -> Result<(), Failure> {
    let doc = open_unlocked(input, password)?;
    check_output(output, std::slice::from_ref(&input.to_path_buf()), force)?;
    let how = compress::Options { preset, lossless_only: lossless, ..compress::Options::default() };
    let shown = std::cell::Cell::new(101u32);
    let progress = |percent: u32| {
        if std::io::stderr().is_terminal() && percent / 5 != shown.get() / 5 {
            shown.set(percent);
            eprint!("\r{percent:>3}%");
        }
    };
    let control = compress::Control { stop: None, progress: &progress };
    let packed = compress::compress(&doc, password, &how, &control).map_err(|e| op_error(input, &e));
    if shown.get() != 101 {
        eprint!("\r    \r");
    }
    let packed = packed?;
    print_warnings(&packed.warnings);
    let (before, after) = (packed.before, packed.after);
    match &packed.data {
        Some(data) => {
            write_file(output, data)?;
            say(&format!(
                "wrote {}: {} bytes -> {} bytes ({:.1}% smaller; {} picture{} made smaller, {} object{} merged)\n",
                output.display(),
                grouped(before),
                grouped(after),
                (before - after) as f64 * 100.0 / before.max(1) as f64,
                packed.pictures,
                plural(packed.pictures as usize),
                packed.merged,
                plural(packed.merged as usize)
            ));
        }
        None => {
            write_file(output, doc.data())?;
            say(&format!("wrote {}: no smaller than the input ({} bytes), so it is a copy of the input\n", output.display(), grouped(before)));
        }
    }
    Ok(())
}

/// Write the bookmarks of the input as text, or make them what a text file or the headings say (the input with the change appended).
fn bookmarks_command(input: &Path, action: &BookmarkAction, output: Option<&Path>, force: bool, password: &str) -> Result<(), Failure> {
    use qingpdf_core::bookmarks;
    let doc = open_unlocked(input, password)?;
    let input_path = input.to_path_buf();
    let inputs = std::slice::from_ref(&input_path);
    let (old, cut) = qingpdf_core::view::read_bookmarks(&doc).map_err(|e| op_error(input, &e))?;
    if cut {
        let (most, deep) = (qingpdf_core::view::MAX_OUTLINE_ITEMS, qingpdf_core::view::MAX_OUTLINE_DEPTH);
        let rest = if matches!(action, BookmarkAction::Export(_)) { "the rest are not in the text" } else { "all of them are replaced" };
        eprintln!("warning: the file has more bookmarks than are read (at most {most}, {deep} levels deep): {rest}");
    }
    let items = match action {
        BookmarkAction::Export(file) => {
            check_output(file, inputs, force)?;
            write_file(file, bookmarks::export_text(&old).as_bytes())?;
            say(&format!("wrote {} ({} bookmark{})\n", file.display(), old.len(), plural(old.len())));
            return Ok(());
        }
        BookmarkAction::Import(file) => {
            let size = fs::metadata(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?.len();
            if size > bookmarks::MAX_IMPORT_BYTES {
                return Err(format!("{} is too big ({size} bytes; at most {})", file.display(), bookmarks::MAX_IMPORT_BYTES));
            }
            let bytes = fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            let text = String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8 text", file.display()))?;
            let pages = doc.page_count().map_err(|e| op_error(input, &e))?;
            let items = bookmarks::parse_text(&text, pages).map_err(|e| format!("{}: {e}", file.display()))?;
            if items.is_empty() {
                return Err(format!("{} has no bookmarks in it", file.display()));
            }
            items
        }
        BookmarkAction::Auto => {
            let found = docx::headings(&doc, password, None, None).map_err(|e| op_error(input, &e))?;
            let (items, left_out) = bookmarks::from_headings(&found);
            if items.is_empty() {
                return Err(format!("no headings were found in {}", input.display()));
            }
            if left_out {
                eprintln!("warning: there are more headings than bookmarks may be ({}); the rest are left out", qingpdf_core::view::MAX_OUTLINE_ITEMS);
            }
            items
        }
    };
    let output = output.ok_or("-o <output file> is required")?;
    check_output(output, inputs, force)?;
    let count = items.len();
    let added = edit::set_outline(&doc, password, items).map_err(|e| op_error(input, &e))?;
    write_file(output, &added.data)?;
    for warning in &added.warnings {
        eprintln!("warning: {warning}");
    }
    if added.rewritten {
        eprintln!("warning: {} had damaged structure data, so it was written afresh as a whole; its original bytes are not kept", input.display());
    }
    let replaced = if old.is_empty() { String::new() } else { format!(", replacing {}", old.len()) };
    say(&format!("wrote {} ({count} bookmark{}{replaced})\n", output.display(), plural(count)));
    Ok(())
}

/// Make a Word file of the pages.
fn docx_command(input: &Path, pages: Option<&str>, output: &Path, force: bool, password: &str) -> Result<(), Failure> {
    let doc = open_unlocked(input, password)?;
    let wanted = match pages {
        Some(list) => Some(page_list(&doc, input, list)?),
        None => None,
    };
    check_output(output, std::slice::from_ref(&input.to_path_buf()), force)?;
    let report = docx::export_to_path(&doc, password, wanted.as_deref(), output, None).map_err(|e| op_error(input, &e))?;
    for w in &report.warnings {
        eprintln!("warning: {}", clean(w));
    }
    let c = report.counts;
    say(&format!(
        "wrote {} ({} page{}, {} paragraph{}, {} heading{}, {} table{}, {} picture{}, {} bytes)\n",
        output.display(),
        report.pages,
        plural(report.pages),
        c.paragraphs,
        plural(c.paragraphs),
        c.headings,
        plural(c.headings),
        c.tables,
        plural(c.tables),
        c.images,
        plural(c.images),
        report.bytes
    ));
    Ok(())
}

/// Make an Excel file of the tables of the pages.
fn xlsx_command(input: &Path, pages: Option<&str>, output: &Path, force: bool, password: &str) -> Result<(), Failure> {
    let doc = open_unlocked(input, password)?;
    let wanted = match pages {
        Some(list) => Some(page_list(&doc, input, list)?),
        None => None,
    };
    check_output(output, std::slice::from_ref(&input.to_path_buf()), force)?;
    let report = xlsx::export_to_path(&doc, password, wanted.as_deref(), output, None).map_err(|e| op_error(input, &e))?;
    for w in &report.warnings {
        eprintln!("warning: {}", clean(w));
    }
    say(&format!(
        "wrote {} ({} page{}, {} table{}, {} cell{}, {} bytes)
",
        output.display(),
        report.pages,
        plural(report.pages),
        report.tables,
        plural(report.tables),
        report.cells,
        plural(report.cells as usize),
        report.bytes
    ));
    Ok(())
}

/// Replace text in the pages and write the file: the input as it was, with the change appended to it.
fn replace_command(input: &Path, find: &str, with: &str, pages: Option<&str>, output: &Path, force: bool, password: &str) -> Result<(), Failure> {
    let doc = open_unlocked(input, password)?;
    let wanted = match pages {
        Some(list) => Some(page_list(&doc, input, list)?),
        None => None,
    };
    check_output(output, std::slice::from_ref(&input.to_path_buf()), force)?;
    let found = edit::replace_text(&doc, password, find, with, wanted.as_deref()).map_err(|e| op_error(input, &e))?;
    for warning in &found.warnings {
        eprintln!("warning: {warning}");
    }
    let skipped: usize = found.skipped.iter().map(|(_, n)| n).sum();
    for (why, n) in &found.skipped {
        eprintln!("warning: {n} place{} not changed: {why}", plural(*n));
    }
    if found.count == 0 {
        return Err(if skipped == 0 { format!("'{find}' was not found in {}", input.display()) } else { format!("'{find}' was found {skipped} time{} but could not be changed", plural(skipped)) });
    }
    write_file(output, &found.data)?;
    if found.rewritten {
        eprintln!("warning: {} had damaged structure data, so it was written afresh as a whole; its original bytes are not kept", input.display());
    }
    say(&format!("replaced {} place{} on {} page{}; wrote {}
", found.count, plural(found.count), found.pages, plural(found.pages), output.display()));
    Ok(())
}

/// Take what lies under the places out of the file and write it afresh as a whole.
fn redact_command(args: &RedactArgs) -> Result<(), Failure> {
    let input = args.input.as_path();
    let doc = open_unlocked(input, &args.password)?;
    check_output(&args.output, std::slice::from_ref(&args.input), args.force)?;
    let pages = doc.pages().map_err(|e| op_error(input, &e))?;
    let mut areas = Vec::with_capacity(args.areas.len());
    for &(page, [x, y, w, h]) in &args.areas {
        let Some(p) = pages.get(page) else { return Err(format!("{}: there is no page {} (the file has {})", input.display(), page + 1, pages.len())) };
        // From the lower left, y up, to the top left, y down.
        let (_, height) = text::Shown::of(p).size();
        areas.push(edit::Area { page, rect: [x as f32, (height - y - h) as f32, (x + w) as f32, (height - y) as f32] });
    }
    let spec = edit::RedactSpec {
        find: edit::FindSpec { words: args.finds.clone(), presets: args.patterns.clone() },
        areas,
        color: args.color,
        label: args.label.clone(),
        use_marks: true,
        clear_metadata: args.clear_metadata,
        remove_active: args.remove_attachments,
    };
    let done = edit::redact_file(&doc, &args.password, &spec).map_err(|e| op_error(input, &e))?;
    for warning in &done.warnings {
        eprintln!("warning: {warning}");
    }
    for what in &done.unfound {
        eprintln!("warning: '{what}' was not found");
    }
    write_file(&args.output, &done.data)?;
    let (mut glyphs, mut pictures, mut paths, mut annots) = (0, 0, 0, 0);
    for (page, c) in &done.pages {
        say(&format!(
            "page {}: {} character{}, {} picture{}, {} shape{}, {} annotation{} taken out
",
            page + 1,
            c.glyphs,
            plural(c.glyphs),
            c.images,
            plural(c.images),
            c.paths,
            plural(c.paths),
            c.annots,
            plural(c.annots)
        ));
        glyphs += c.glyphs;
        pictures += c.images;
        paths += c.paths;
        annots += c.annots;
    }
    say(&format!(
        "redacted {} place{} on {} page{} ({glyphs} character{}, {pictures} picture{}, {paths} shape{}, {annots} annotation{}); wrote {}
",
        done.marks,
        plural(done.marks),
        done.pages.len(),
        plural(done.pages.len()),
        plural(glyphs),
        plural(pictures),
        plural(paths),
        plural(annots),
        args.output.display()
    ));
    Ok(())
}

/// Read a picture file for a page (a size over the limit is refused before it is read).
fn read_picture(path: &Path) -> Result<Vec<u8>, Failure> {
    let size = fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?.len();
    if size > edit::MAX_STAMP_FILE_BYTES as u64 {
        return Err(format!("{} is bigger than {} MB, the most a picture file may be", path.display(), edit::MAX_STAMP_FILE_BYTES >> 20));
    }
    fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// Put what `op` says on the pages and write the file: the input as it was, with the change appended to it.
fn add_command(doc: &Document, password: &str, input: &Path, output: &Path, op: &ContentOp) -> Result<(), Failure> {
    let added = edit::add_content(doc, password, op).map_err(|e| op_error(input, &e))?;
    write_file(output, &added.data)?;
    for warning in &added.warnings {
        eprintln!("warning: {warning}");
    }
    if added.rewritten {
        eprintln!("warning: {} had damaged structure data, so it was written afresh as a whole; its original bytes are not kept", input.display());
    }
    let pages = doc.page_count().unwrap_or(0);
    say(&format!("wrote {} ({} page{})\n", output.display(), pages, plural(pages)));
    Ok(())
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
        // (On the disk before the rename: a crash must not leave the new name on a file that is not all there.)
        let written = file.write_all(data).and_then(|()| file.flush()).and_then(|()| file.sync_all());
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
    let mut renderer = render::Renderer::new(&doc, qingpdf_core::forms::painter());
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

//! The `qingpdf` command line.

mod args;
mod help;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use qingpdf_core::image::PageMode;
use qingpdf_core::info::{self, Report};
use qingpdf_core::ops::{self, ImageInput, Input, Output};
use qingpdf_core::{Document, Error, Warning};

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
                eprintln!("error: {message}");
                ExitCode::from(1)
            }
        },
        Err(UsageError { message, command }) => {
            eprintln!("qingpdf: {message}");
            match command {
                Some(c) => eprintln!("Try 'qingpdf {} --help' for usage.", c.name()),
                None => eprintln!("Try 'qingpdf --help' for usage."),
            }
            ExitCode::from(2)
        }
    }
}

/// Print to standard output without panicking if the pipe is closed.
fn say(text: &str) {
    let _ = std::io::stdout().write_all(text.as_bytes());
}

fn run(request: Request) -> Result<(), Failure> {
    match request {
        Request::Info { input } => info_command(&input),
        Request::Merge { inputs, output, force } => merge_command(&inputs, &output, force),
        Request::SplitPages { input, pages, output, force } => {
            let doc = open_plain(&input)?;
            let wanted = page_list(&doc, &input, &pages)?;
            check_output(&output, std::slice::from_ref(&input), force)?;
            let out = ops::extract_pages(&doc, &wanted).map_err(|e| op_error(&input, &e))?;
            finish(&output, &out)
        }
        Request::SplitEvery { input, every, output, force } => split_every_command(&input, every, &output, force),
        Request::Delete { input, pages, output, force } => {
            let doc = open_plain(&input)?;
            let gone = page_list(&doc, &input, &pages)?;
            check_output(&output, std::slice::from_ref(&input), force)?;
            let out = ops::delete_pages(&doc, &gone).map_err(|e| op_error(&input, &e))?;
            finish(&output, &out)
        }
        Request::Rotate { input, pages, angle, output, force } => {
            let doc = open_plain(&input)?;
            let turn = match pages {
                Some(list) => page_list(&doc, &input, &list)?,
                None => (0..doc.page_count().map_err(|e| op_error(&input, &e))?).collect(),
            };
            check_output(&output, std::slice::from_ref(&input), force)?;
            let out = ops::rotate_pages(&doc, &turn, angle).map_err(|e| op_error(&input, &e))?;
            finish(&output, &out)
        }
        Request::Img2pdf { inputs, mode, output, force } => images_command(&inputs, mode, &output, force),
    }
}

// --- reading and writing files -------------------------------------------------------

fn encrypted_message(path: &Path) -> String {
    format!("{} is encrypted. Encrypted PDF files are not supported yet.", path.display())
}

/// Say why a file could not be opened, for people.
fn open_error(path: &Path, e: &Error) -> String {
    match e {
        Error::Io(io) => format!("cannot read {}: {io}", path.display()),
        Error::Unsupported(m) if m == "encrypted PDF" => encrypted_message(path),
        Error::Unsupported(m) => format!("{}: not supported yet: {m}", path.display()),
        other => format!("{} is not a PDF file, or is too damaged to read ({other})", path.display()),
    }
}

/// Say why an operation on an open file failed.
fn op_error(path: &Path, e: &Error) -> String {
    match e {
        Error::Unsupported(m) if m.starts_with("encrypted PDF") => encrypted_message(path),
        Error::Unsupported(m) => format!("{}: not supported yet: {m}", path.display()),
        Error::Invalid(m) => m.clone(),
        other => format!("{}: {other}", path.display()),
    }
}

fn open_pdf(path: &Path) -> Result<Document, Failure> {
    Document::open(path).map_err(|e| open_error(path, &e))
}

/// Open a file and refuse it if it is encrypted.
fn open_plain(path: &Path) -> Result<Document, Failure> {
    let doc = open_pdf(path)?;
    if doc.is_encrypted() {
        return Err(encrypted_message(path));
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
        eprintln!("warning: {w}");
    }
}

fn write_file(output: &Path, data: &[u8]) -> Result<(), Failure> {
    fs::write(output, data).map_err(|e| format!("cannot write {}: {e}", output.display()))
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

fn merge_command(inputs: &[PathBuf], output: &Path, force: bool) -> Result<(), Failure> {
    let docs: Vec<Document> = inputs.iter().map(|p| open_plain(p)).collect::<Result<_, _>>()?;
    let names: Vec<String> = inputs.iter().map(|p| p.display().to_string()).collect();
    check_output(output, inputs, force)?;
    let merge_inputs: Vec<Input<'_>> =
        docs.iter().zip(&names).map(|(doc, name)| Input { name: name.as_str(), doc }).collect();
    let out = ops::merge(&merge_inputs).map_err(|e| match &e {
        Error::Invalid(m) => m.clone(),
        other => format!("merge failed: {other}"),
    })?;
    finish(output, &out)
}

fn split_every_command(input: &Path, every: usize, template: &Path, force: bool) -> Result<(), Failure> {
    let doc = open_plain(input)?;
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
    let warnings = ops::split_every(&doc, every, &mut |n, out| {
        let name = n
            .checked_sub(1)
            .and_then(|i| names.get(i))
            .ok_or_else(|| Error::Invalid("more files than expected".to_string()))?;
        fs::write(name, &out.data).map_err(|e| Error::Invalid(format!("cannot write {}: {e}", name.display())))?;
        report.push(format!("wrote {} ({} page{})\n", name.display(), out.pages, plural(out.pages)));
        Ok(())
    })
    .map_err(|e| op_error(input, &e))?;
    print_warnings(&warnings);
    for line in report {
        say(&line);
    }
    Ok(())
}

fn images_command(inputs: &[PathBuf], mode: PageMode, output: &Path, force: bool) -> Result<(), Failure> {
    check_output(output, inputs, force)?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(inputs.len());
    for path in inputs {
        let data = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        files.push((path.display().to_string(), data));
    }
    let images: Vec<ImageInput<'_>> =
        files.iter().map(|(name, data)| ImageInput { name: name.as_str(), data: data.as_slice() }).collect();
    let out = ops::images_to_pdf(&images, mode).map_err(|e| match &e {
        Error::Invalid(m) => m.clone(),
        other => format!("cannot make the PDF: {other}"),
    })?;
    finish(output, &out)
}

// --- info ---------------------------------------------------------------------------------------

fn info_command(input: &Path) -> Result<(), Failure> {
    let doc = open_pdf(input)?;
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
    s.push_str(&format!("Pages:               {}\n", r.pages.len()));
    s.push_str(&format!("Encrypted:           {}\n", yes_no(r.encrypted)));
    s.push_str(&format!("Cross-ref streams:   {}\n", yes_no(r.xref_streams)));
    s.push_str(&format!("Object streams:      {}\n", yes_no(r.object_streams)));
    s.push_str(&format!(
        "Repaired:            {}\n",
        if r.repaired { "yes (the cross-reference table was damaged and was rebuilt by scanning the file)" } else { "no" }
    ));
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

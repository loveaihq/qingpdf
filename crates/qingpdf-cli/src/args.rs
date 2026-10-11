//! Hand-written command-line parsing (no clap: size). Turns the arguments
//! into a [`Request`] or a usage error.

use std::ffi::OsString;
use std::path::PathBuf;

use qingpdf_core::image::PageMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Info,
    Merge,
    Split,
    Delete,
    Rotate,
    Decrypt,
    Text,
    Render,
    Img2pdf,
    Watermark,
    Stamp,
    Addtext,
}

impl Command {
    pub const ALL: [Command; 12] = [
        Command::Info,
        Command::Merge,
        Command::Split,
        Command::Delete,
        Command::Rotate,
        Command::Decrypt,
        Command::Text,
        Command::Render,
        Command::Img2pdf,
        Command::Watermark,
        Command::Stamp,
        Command::Addtext,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Command::Info => "info",
            Command::Merge => "merge",
            Command::Split => "split",
            Command::Delete => "delete",
            Command::Rotate => "rotate",
            Command::Decrypt => "decrypt",
            Command::Text => "text",
            Command::Render => "render",
            Command::Img2pdf => "img2pdf",
            Command::Watermark => "watermark",
            Command::Stamp => "stamp",
            Command::Addtext => "addtext",
        }
    }

    fn from_name(name: &str) -> Option<Command> {
        Command::ALL.into_iter().find(|c| c.name() == name)
    }
}

/// What the user asked for, checked for completeness.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Info { input: PathBuf, password: String },
    Merge { inputs: Vec<PathBuf>, output: PathBuf, force: bool, password: String },
    SplitPages { input: PathBuf, pages: String, output: PathBuf, force: bool, password: String },
    SplitEvery { input: PathBuf, every: usize, output: PathBuf, force: bool, password: String },
    Delete { input: PathBuf, pages: String, output: PathBuf, force: bool, password: String },
    Rotate { input: PathBuf, pages: Option<String>, angle: i64, output: PathBuf, force: bool, password: String },
    Decrypt { input: PathBuf, output: PathBuf, force: bool, password: String },
    Text { input: PathBuf, pages: Option<String>, output: Option<PathBuf>, force: bool, password: String },
    Render { input: PathBuf, pages: Option<String>, dpi: f64, annots: bool, output: String, force: bool, password: String },
    Img2pdf { inputs: Vec<PathBuf>, mode: PageMode, output: PathBuf, force: bool },
    /// `text` and `image` are not both empty. `pages`: a page list (all pages when none).
    Watermark {
        input: PathBuf,
        text: Option<String>,
        image: Option<PathBuf>,
        opacity: f32,
        angle: f32,
        tile: bool,
        under: bool,
        pages: Option<String>,
        output: PathBuf,
        force: bool,
        password: String,
    },
    /// `page` from 1, `at` in points from the lower left of the page as shown.
    Stamp { input: PathBuf, image: PathBuf, page: usize, at: [f64; 2], width: f64, output: PathBuf, force: bool, password: String },
    Addtext { input: PathBuf, page: usize, at: [f64; 2], size: f64, text: String, output: PathBuf, force: bool, password: String },
}

/// What `parse` found.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(Request),
    /// `--help`, for the whole tool or one command.
    Help(Option<Command>),
    Version,
}

/// The command line is wrong. Exit code 2.
#[derive(Debug, PartialEq)]
pub struct UsageError {
    pub message: String,
    /// The command to point the user at, if one was named.
    pub command: Option<Command>,
}

fn usage(command: Option<Command>, message: impl Into<String>) -> UsageError {
    UsageError { message: message.into(), command }
}

/// Options a command takes, besides `-h`.
struct Allowed {
    output: bool,
    force: bool,
    pages: bool,
    every: bool,
    angle: bool,
    page: bool,
    dpi: bool,
    /// `--no-annots`: `render` only.
    no_annots: bool,
    /// `--password`: every command that reads a PDF file.
    password: bool,
    /// The options that put things on pages: `--text`, `--image`, `--opacity`, `--tile`, `--under`, `--at`, `--width`, `--size`.
    mark: bool,
}

fn allowed(c: Command) -> Allowed {
    let none =
        Allowed { output: false, force: false, pages: false, every: false, angle: false, page: false, dpi: false, no_annots: false, password: false, mark: false };
    match c {
        Command::Info => Allowed { password: true, ..none },
        Command::Merge => Allowed { output: true, force: true, password: true, ..none },
        Command::Split => Allowed { output: true, force: true, pages: true, every: true, password: true, ..none },
        Command::Delete => Allowed { output: true, force: true, pages: true, password: true, ..none },
        Command::Rotate => Allowed { output: true, force: true, pages: true, angle: true, password: true, ..none },
        Command::Decrypt => Allowed { output: true, force: true, password: true, ..none },
        Command::Text => Allowed { output: true, force: true, pages: true, password: true, ..none },
        Command::Render => Allowed { output: true, force: true, pages: true, dpi: true, no_annots: true, password: true, ..none },
        Command::Img2pdf => Allowed { output: true, force: true, page: true, ..none },
        Command::Watermark => Allowed { output: true, force: true, pages: true, angle: true, password: true, mark: true, ..none },
        Command::Stamp | Command::Addtext => Allowed { output: true, force: true, page: true, password: true, mark: true, ..none },
    }
}

/// The options as written, before the command's rules are applied.
#[derive(Default)]
struct Collected {
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    force: bool,
    no_annots: bool,
    pages: Option<String>,
    every: Option<String>,
    angle: Option<String>,
    page: Option<String>,
    dpi: Option<String>,
    password: Option<String>,
    text: Option<String>,
    image: Option<PathBuf>,
    opacity: Option<String>,
    at: Option<String>,
    width: Option<String>,
    size: Option<String>,
    tile: bool,
    under: bool,
}

fn text(value: OsString, option: &str, command: Command) -> Result<String, UsageError> {
    value.into_string().map_err(|_| usage(Some(command), format!("the value of {option} is not valid text")))
}

/// Parse the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Parsed, UsageError> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Err(usage(None, "no command given"));
    };
    let first_text = first.to_string_lossy().into_owned();
    match first_text.as_str() {
        "-h" | "--help" => return Ok(Parsed::Help(None)),
        "-V" | "--version" => return Ok(Parsed::Version),
        "help" => {
            return match args.next() {
                None => Ok(Parsed::Help(None)),
                Some(name) => {
                    let name = name.to_string_lossy().into_owned();
                    match Command::from_name(&name) {
                        Some(c) => Ok(Parsed::Help(Some(c))),
                        None => Err(usage(None, format!("unknown command '{name}'"))),
                    }
                }
            };
        }
        _ => {}
    }
    let Some(command) = Command::from_name(&first_text) else {
        return Err(usage(None, format!("unknown command '{first_text}'")));
    };
    let ok = allowed(command);
    let c = Some(command);
    let mut got = Collected::default();
    let mut only_files = false;
    while let Some(arg) = args.next() {
        let lossy = arg.to_string_lossy().into_owned();
        if only_files || lossy == "-" || !lossy.starts_with('-') {
            got.inputs.push(PathBuf::from(arg));
            continue;
        }
        if lossy == "--" {
            only_files = true;
            continue;
        }
        if lossy == "-h" || lossy == "--help" {
            return Ok(Parsed::Help(Some(command)));
        }
        // `--name=value` and `--name value`.
        let (name, inline) = match lossy.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
            _ => (lossy.clone(), None),
        };
        match name.as_str() {
            "-o" | "--output" if ok.output => {}
            "--pages" if ok.pages => {}
            "--every" if ok.every => {}
            "--angle" if ok.angle => {}
            "--page" if ok.page => {}
            "--dpi" if ok.dpi => {}
            "--password" if ok.password => {}
            "--text" | "--image" | "--opacity" | "--at" | "--width" | "--size" if ok.mark && mark_option(command, &name) => {}
            "--tile" | "--under" if ok.mark && mark_option(command, &name) => {
                if inline.is_some() {
                    return Err(usage(c, format!("{name} does not take a value")));
                }
                if name == "--tile" {
                    got.tile = true;
                } else {
                    got.under = true;
                }
                continue;
            }
            "--force" if ok.force => {
                if inline.is_some() {
                    return Err(usage(c, "--force does not take a value"));
                }
                got.force = true;
                continue;
            }
            "--no-annots" if ok.no_annots => {
                if inline.is_some() {
                    return Err(usage(c, "--no-annots does not take a value"));
                }
                got.no_annots = true;
                continue;
            }
            _ => {
                // Show the option, never what was given with it: that may be a password.
                let shown = name.split('=').next().unwrap_or("");
                return Err(usage(c, format!("unknown option '{shown}' for '{}'", command.name())));
            }
        }
        let value = match inline {
            Some(v) => OsString::from(v),
            None => args.next().ok_or_else(|| usage(c, format!("{name} needs a value")))?,
        };
        let slot_taken = |what: &str| usage(c, format!("{what} given more than once"));
        match name.as_str() {
            "-o" | "--output" => {
                if got.output.replace(PathBuf::from(value)).is_some() {
                    return Err(slot_taken("the output file"));
                }
            }
            "--pages" => {
                if got.pages.replace(text(value, "--pages", command)?).is_some() {
                    return Err(slot_taken("--pages"));
                }
            }
            "--every" => {
                if got.every.replace(text(value, "--every", command)?).is_some() {
                    return Err(slot_taken("--every"));
                }
            }
            "--angle" => {
                if got.angle.replace(text(value, "--angle", command)?).is_some() {
                    return Err(slot_taken("--angle"));
                }
            }
            "--dpi" => {
                if got.dpi.replace(text(value, "--dpi", command)?).is_some() {
                    return Err(slot_taken("--dpi"));
                }
            }
            "--password" => {
                if got.password.replace(text(value, "--password", command)?).is_some() {
                    return Err(slot_taken("--password"));
                }
            }
            "--image" => {
                if got.image.replace(PathBuf::from(value)).is_some() {
                    return Err(slot_taken("--image"));
                }
            }
            "--text" | "--opacity" | "--at" | "--width" | "--size" => {
                let slot = match name.as_str() {
                    "--text" => &mut got.text,
                    "--opacity" => &mut got.opacity,
                    "--at" => &mut got.at,
                    "--width" => &mut got.width,
                    _ => &mut got.size,
                };
                if slot.replace(text(value, &name, command)?).is_some() {
                    return Err(slot_taken(&name));
                }
            }
            _ => {
                if got.page.replace(text(value, "--page", command)?).is_some() {
                    return Err(slot_taken("--page"));
                }
            }
        }
    }
    finish(command, got).map(Parsed::Run)
}

/// Which of the options that put things on pages a command takes.
fn mark_option(command: Command, name: &str) -> bool {
    match command {
        Command::Watermark => matches!(name, "--text" | "--image" | "--opacity" | "--tile" | "--under"),
        Command::Stamp => matches!(name, "--image" | "--at" | "--width"),
        _ => matches!(name, "--text" | "--at" | "--size"),
    }
}

/// A number option: `default` when it was not given (NaN: it must be given, and the caller says so when it is not more than 0).
fn number(c: Option<Command>, name: &str, value: Option<&str>, default: f64) -> Result<f64, UsageError> {
    match value {
        None => Ok(default),
        Some(v) => v.trim().parse::<f64>().ok().filter(|n| n.is_finite()).ok_or_else(|| usage(c, format!("{name} needs a number, not '{v}'"))),
    }
}

/// `--page N` (from 1), required.
fn page_number(c: Option<Command>, value: Option<&str>) -> Result<usize, UsageError> {
    let v = value.ok_or_else(|| usage(c, "--page <number> is required"))?;
    v.trim().parse::<usize>().ok().filter(|&n| n >= 1).ok_or_else(|| usage(c, format!("--page needs a page number from 1, not '{v}'")))
}

/// The biggest number a place, a width or an angle may be (what the engine takes as a number on a page), and the most degrees of
/// a watermark's angle.
const MAX_PLACE: f64 = 1.0e7;
const MAX_ANGLE: f64 = 36_000.0;

/// `--at X,Y`, required: points from the lower left corner of the page as it is shown.
fn place(c: Option<Command>, value: Option<&str>) -> Result<[f64; 2], UsageError> {
    let v = value.ok_or_else(|| usage(c, "--at <X,Y> is required"))?;
    let parsed = v.split_once(',').and_then(|(x, y)| Some([x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?])).filter(|p| p.iter().all(|n| n.is_finite()));
    let parsed = parsed.ok_or_else(|| usage(c, format!("--at needs two numbers like 72,720, not '{v}'")))?;
    if parsed.iter().any(|n| n.abs() > MAX_PLACE) {
        return Err(usage(c, format!("--at must be between -{MAX_PLACE} and {MAX_PLACE} points, not '{v}'")));
    }
    Ok(parsed)
}

fn finish(command: Command, got: Collected) -> Result<Request, UsageError> {
    let c = Some(command);
    let Collected { mut inputs, output, force, no_annots, pages, every, angle, page, dpi, password, text, image, opacity, at, width, size, tile, under } = got;
    let password = password.unwrap_or_default();
    let one_input = |inputs: &mut Vec<PathBuf>| -> Result<PathBuf, UsageError> {
        match inputs.len() {
            1 => Ok(inputs.remove(0)),
            0 => Err(usage(c, "no input file given")),
            _ => Err(usage(c, format!("'{}' takes one input file", command.name()))),
        }
    };
    let need_output = |output: Option<PathBuf>| output.ok_or_else(|| usage(c, "-o <output file> is required"));
    match command {
        Command::Info => Ok(Request::Info { input: one_input(&mut inputs)?, password }),
        Command::Merge => {
            if inputs.len() < 2 {
                return Err(usage(c, "merge needs at least two input files"));
            }
            Ok(Request::Merge { inputs, output: need_output(output)?, force, password })
        }
        Command::Split => {
            let input = one_input(&mut inputs)?;
            let output = need_output(output)?;
            match (pages, every) {
                (Some(_), Some(_)) => Err(usage(c, "use either --pages or --every, not both")),
                (None, None) => Err(usage(c, "split needs --pages <list> or --every <N>")),
                (Some(pages), None) => Ok(Request::SplitPages { input, pages, output, force, password }),
                (None, Some(n)) => {
                    let every = n
                        .parse::<usize>()
                        .ok()
                        .filter(|&n| n >= 1)
                        .ok_or_else(|| usage(c, format!("--every needs a whole number of at least 1, not '{n}'")))?;
                    if !output.to_str().is_some_and(|o| o.contains("%d")) {
                        return Err(usage(c, "with --every the output name must contain %d (for example out_%d.pdf)"));
                    }
                    Ok(Request::SplitEvery { input, every, output, force, password })
                }
            }
        }
        Command::Delete => {
            let input = one_input(&mut inputs)?;
            let output = need_output(output)?;
            let pages = pages.ok_or_else(|| usage(c, "delete needs --pages <list>"))?;
            Ok(Request::Delete { input, pages, output, force, password })
        }
        Command::Rotate => {
            let input = one_input(&mut inputs)?;
            let output = need_output(output)?;
            let angle = angle.ok_or_else(|| usage(c, "rotate needs --angle <degrees>"))?;
            let angle = angle
                .parse::<i64>()
                .ok()
                .filter(|a| a % 90 == 0)
                .ok_or_else(|| usage(c, format!("--angle must be a multiple of 90 (such as 90, 180, 270 or -90), not '{angle}'")))?;
            Ok(Request::Rotate { input, pages, angle, output, force, password })
        }
        Command::Decrypt => {
            let input = one_input(&mut inputs)?;
            Ok(Request::Decrypt { input, output: need_output(output)?, force, password })
        }
        Command::Text => {
            let input = one_input(&mut inputs)?;
            if force && output.is_none() {
                return Err(usage(c, "--force only goes with -o <output file>"));
            }
            Ok(Request::Text { input, pages, output, force, password })
        }
        Command::Render => {
            let input = one_input(&mut inputs)?;
            let output = need_output(output)?;
            let output = output.to_str().ok_or_else(|| usage(c, "the output name is not valid text"))?.to_string();
            let dpi = match dpi {
                None => 150.0,
                Some(d) => d
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && (qingpdf_core::render::MIN_DPI..=qingpdf_core::render::MAX_DPI).contains(v))
                    .ok_or_else(|| usage(c, format!("--dpi must be a number from 1 to 2400, not '{d}'")))?,
            };
            Ok(Request::Render { input, pages, dpi, annots: !no_annots, output, force, password })
        }
        Command::Watermark => {
            let input = one_input(&mut inputs)?;
            let output = need_output(output)?;
            let text = text.filter(|t| !t.trim().is_empty());
            if text.is_none() && image.is_none() {
                return Err(usage(c, "watermark needs --text <text> or --image <file>"));
            }
            let opacity = number(c, "--opacity", opacity.as_deref(), 0.3)?;
            if !(opacity > 0.0 && opacity <= 1.0) {
                return Err(usage(c, "--opacity must be more than 0 and at most 1"));
            }
            let degrees = number(c, "--angle", angle.as_deref(), 0.0)?;
            if degrees.abs() > MAX_ANGLE {
                return Err(usage(c, format!("--angle must be from -{MAX_ANGLE} to {MAX_ANGLE} degrees, not '{}'", angle.as_deref().unwrap_or_default())));
            }
            Ok(Request::Watermark { input, text, image, opacity: opacity as f32, angle: degrees as f32, tile, under, pages, output, force, password })
        }
        Command::Stamp => {
            let input = one_input(&mut inputs)?;
            let output = need_output(output)?;
            let image = image.ok_or_else(|| usage(c, "stamp needs --image <file>"))?;
            let points = number(c, "--width", width.as_deref(), f64::NAN)?;
            if points.is_nan() || points <= 0.0 {
                return Err(usage(c, "stamp needs --width <points>, more than 0"));
            }
            if points > MAX_PLACE {
                return Err(usage(c, format!("--width must be at most {MAX_PLACE} points, not '{}'", width.as_deref().unwrap_or_default())));
            }
            Ok(Request::Stamp { input, image, page: page_number(c, page.as_deref())?, at: place(c, at.as_deref())?, width: points, output, force, password })
        }
        Command::Addtext => {
            let input = one_input(&mut inputs)?;
            let output = need_output(output)?;
            let text = text.filter(|t| !t.trim().is_empty()).ok_or_else(|| usage(c, "addtext needs --text <text>"))?;
            let size = number(c, "--size", size.as_deref(), f64::NAN)?;
            if size.is_nan() || size <= 0.0 {
                return Err(usage(c, "addtext needs --size <points>, more than 0"));
            }
            Ok(Request::Addtext { input, page: page_number(c, page.as_deref())?, at: place(c, at.as_deref())?, size, text, output, force, password })
        }
        Command::Img2pdf => {
            if inputs.is_empty() {
                return Err(usage(c, "no image files given"));
            }
            let mode = match page.as_deref() {
                None | Some("a4") => PageMode::A4,
                Some("fit") => PageMode::Fit,
                Some(other) => return Err(usage(c, format!("--page must be a4 or fit, not '{other}'"))),
            };
            Ok(Request::Img2pdf { inputs, mode, output: need_output(output)?, force })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Parsed, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    fn run(args: &[&str]) -> Request {
        match p(args) {
            Ok(Parsed::Run(r)) => r,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    fn fails(args: &[&str]) -> String {
        match p(args) {
            Err(e) => e.message,
            other => panic!("{args:?} should be a usage error: {other:?}"),
        }
    }

    #[test]
    fn help_and_version() {
        assert_eq!(p(&["--help"]), Ok(Parsed::Help(None)));
        assert_eq!(p(&["-h"]), Ok(Parsed::Help(None)));
        assert_eq!(p(&["help"]), Ok(Parsed::Help(None)));
        assert_eq!(p(&["help", "split"]), Ok(Parsed::Help(Some(Command::Split))));
        assert_eq!(p(&["split", "--help"]), Ok(Parsed::Help(Some(Command::Split))));
        assert_eq!(p(&["merge", "a.pdf", "-h"]), Ok(Parsed::Help(Some(Command::Merge))));
        assert_eq!(p(&["--version"]), Ok(Parsed::Version));
        assert_eq!(p(&["-V"]), Ok(Parsed::Version));
        assert!(fails(&[]).contains("no command"));
        assert!(fails(&["frobnicate"]).contains("unknown command"));
        assert!(fails(&["help", "frobnicate"]).contains("unknown command"));
    }

    #[test]
    fn every_command_in_its_documented_form() {
        assert_eq!(run(&["info", "a.pdf"]), Request::Info { input: "a.pdf".into(), password: String::new() });
        assert_eq!(
            run(&["merge", "a.pdf", "b.pdf", "c.pdf", "-o", "out.pdf"]),
            Request::Merge {
                inputs: vec!["a.pdf".into(), "b.pdf".into(), "c.pdf".into()],
                output: "out.pdf".into(),
                force: false,
                password: String::new()
            }
        );
        assert_eq!(
            run(&["split", "a.pdf", "--pages", "1-3,5", "-o", "out.pdf"]),
            Request::SplitPages {
                input: "a.pdf".into(),
                pages: "1-3,5".into(),
                output: "out.pdf".into(),
                force: false,
                password: String::new()
            }
        );
        assert_eq!(
            run(&["split", "a.pdf", "--every", "10", "-o", "out_%d.pdf", "--force"]),
            Request::SplitEvery {
                input: "a.pdf".into(),
                every: 10,
                output: "out_%d.pdf".into(),
                force: true,
                password: String::new()
            }
        );
        assert_eq!(
            run(&["delete", "a.pdf", "--pages", "2,4-6", "-o", "out.pdf"]),
            Request::Delete {
                input: "a.pdf".into(),
                pages: "2,4-6".into(),
                output: "out.pdf".into(),
                force: false,
                password: String::new()
            }
        );
        assert_eq!(
            run(&["rotate", "a.pdf", "--pages", "1-3", "--angle", "90", "-o", "out.pdf"]),
            Request::Rotate {
                input: "a.pdf".into(),
                pages: Some("1-3".into()),
                angle: 90,
                output: "out.pdf".into(),
                force: false,
                password: String::new()
            }
        );
        assert_eq!(
            run(&["decrypt", "a.pdf", "-o", "out.pdf", "--password", "s3cret"]),
            Request::Decrypt { input: "a.pdf".into(), output: "out.pdf".into(), force: false, password: "s3cret".into() }
        );
        assert_eq!(
            run(&["img2pdf", "1.jpg", "2.png", "-o", "out.pdf"]),
            Request::Img2pdf { inputs: vec!["1.jpg".into(), "2.png".into()], mode: PageMode::A4, output: "out.pdf".into(), force: false }
        );
        assert!(matches!(run(&["img2pdf", "1.jpg", "--page", "fit", "-o", "o.pdf"]), Request::Img2pdf { mode: PageMode::Fit, .. }));
    }

    #[test]
    fn options_may_be_anywhere_and_take_equals() {
        assert_eq!(
            run(&["rotate", "--angle=-90", "a.pdf", "--output=o.pdf"]),
            Request::Rotate {
                input: "a.pdf".into(),
                pages: None,
                angle: -90,
                output: "o.pdf".into(),
                force: false,
                password: String::new()
            }
        );
        assert_eq!(
            run(&["delete", "--pages", "1", "-o", "o.pdf", "a.pdf"]),
            Request::Delete {
                input: "a.pdf".into(),
                pages: "1".into(),
                output: "o.pdf".into(),
                force: false,
                password: String::new()
            }
        );
        // A value that starts with '-' is still the value.
        assert!(matches!(run(&["rotate", "a.pdf", "--angle", "-90", "-o", "o.pdf"]), Request::Rotate { angle: -90, .. }));
        // `--` ends the options; `-` alone is a file name.
        assert_eq!(
            run(&["info", "--", "-weird.pdf"]),
            Request::Info { input: "-weird.pdf".into(), password: String::new() }
        );
    }

    #[test]
    fn the_password_is_an_option_of_every_command_that_reads_a_pdf() {
        for args in [
            &["info", "a.pdf", "--password", "pw"][..],
            &["merge", "a.pdf", "b.pdf", "-o", "o.pdf", "--password=pw"],
            &["split", "a.pdf", "--pages", "1", "-o", "o.pdf", "--password", "pw"],
            &["split", "a.pdf", "--every", "1", "-o", "o_%d.pdf", "--password", "pw"],
            &["delete", "a.pdf", "--pages", "1", "-o", "o.pdf", "--password", "pw"],
            &["rotate", "a.pdf", "--angle", "90", "-o", "o.pdf", "--password", "pw"],
            &["decrypt", "a.pdf", "-o", "o.pdf", "--password", "pw"],
        ] {
            match run(args) {
                Request::Info { password, .. }
                | Request::Merge { password, .. }
                | Request::SplitPages { password, .. }
                | Request::SplitEvery { password, .. }
                | Request::Delete { password, .. }
                | Request::Rotate { password, .. }
                | Request::Decrypt { password, .. } => assert_eq!(password, "pw", "{args:?}"),
                other => panic!("{other:?}"),
            }
        }
        // An empty password is allowed (it is what is tried when none is given); images have none.
        assert!(matches!(run(&["info", "a.pdf", "--password", ""]), Request::Info { password, .. } if password.is_empty()));
        assert!(fails(&["img2pdf", "a.png", "-o", "o.pdf", "--password", "x"]).contains("unknown option '--password'"));
        assert!(fails(&["info", "a.pdf", "--password"]).contains("needs a value"));
        assert!(fails(&["info", "a.pdf", "--password", "a", "--password", "b"]).contains("more than once"));
        assert!(fails(&["decrypt", "a.pdf"]).contains("-o"));
        // What is wrong with the command line is shown, a password given with it is not.
        let message = fails(&["info", "a.pdf", "-p=topsecret"]);
        assert!(!message.contains("topsecret"), "{message}");
        let message = fails(&["info", "a.pdf", "--passwrd=topsecret"]);
        assert!(message.contains("--passwrd") && !message.contains("topsecret"), "{message}");
    }

    #[test]
    fn render_draws_annotations_unless_told_not_to() {
        let annots = |args: &[&str]| match run(args) {
            Request::Render { annots, .. } => annots,
            other => panic!("{other:?}"),
        };
        assert!(annots(&["render", "a.pdf", "-o", "p.png"]));
        assert!(!annots(&["render", "a.pdf", "--no-annots", "-o", "p.png"]));
        assert!(fails(&["render", "a.pdf", "--no-annots=1", "-o", "p.png"]).contains("--no-annots"));
        assert!(fails(&["text", "a.pdf", "--no-annots"]).contains("unknown option '--no-annots'"));
    }

    #[test]
    fn mistakes_are_usage_errors() {
        assert!(fails(&["info"]).contains("no input"));
        assert!(fails(&["info", "a.pdf", "b.pdf"]).contains("one input"));
        assert!(fails(&["info", "a.pdf", "-o", "x"]).contains("unknown option '-o'"));
        assert!(fails(&["merge", "a.pdf", "-o", "x.pdf"]).contains("at least two"));
        assert!(fails(&["merge", "a.pdf", "b.pdf"]).contains("-o"));
        assert!(fails(&["merge", "a.pdf", "b.pdf", "-o"]).contains("needs a value"));
        assert!(fails(&["merge", "a.pdf", "b.pdf", "-o", "x", "-o", "y"]).contains("more than once"));
        assert!(fails(&["merge", "a.pdf", "b.pdf", "-o", "x", "--pages", "1"]).contains("unknown option '--pages'"));
        assert!(fails(&["merge", "a.pdf", "b.pdf", "-o", "x", "--bogus"]).contains("unknown option '--bogus'"));
        assert!(fails(&["merge", "a.pdf", "b.pdf", "-o", "x", "--force=yes"]).contains("--force"));
        assert!(fails(&["split", "a.pdf", "-o", "x.pdf"]).contains("--pages"));
        assert!(fails(&["split", "a.pdf", "-o", "x.pdf", "--pages", "1", "--every", "2"]).contains("not both"));
        assert!(fails(&["split", "a.pdf", "-o", "x.pdf", "--every", "0"]).contains("at least 1"));
        assert!(fails(&["split", "a.pdf", "-o", "x.pdf", "--every", "two"]).contains("whole number"));
        assert!(fails(&["split", "a.pdf", "-o", "x.pdf", "--every", "2"]).contains("%d"));
        assert!(fails(&["delete", "a.pdf", "-o", "x.pdf"]).contains("--pages"));
        assert!(fails(&["rotate", "a.pdf", "-o", "x.pdf"]).contains("--angle"));
        assert!(fails(&["rotate", "a.pdf", "-o", "x.pdf", "--angle", "45"]).contains("multiple of 90"));
        assert!(fails(&["rotate", "a.pdf", "-o", "x.pdf", "--angle", "x"]).contains("multiple of 90"));
        assert!(fails(&["img2pdf", "-o", "x.pdf"]).contains("no image"));
        assert!(fails(&["img2pdf", "a.png", "-o", "x.pdf", "--page", "letter"]).contains("a4 or fit"));
        // The error carries the command to point the user at.
        assert_eq!(p(&["rotate", "a.pdf"]).unwrap_err().command, Some(Command::Rotate));
        assert_eq!(p(&[]).unwrap_err().command, None);
    }
}

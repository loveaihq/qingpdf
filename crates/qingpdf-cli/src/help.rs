//! The text of `--help`.

use crate::args::Command;

const MAIN: &str = "\
qingpdf - small, fast, offline PDF tools

Usage:
  qingpdf <command> [options]

Commands:
  info      Show a PDF's version, pages, page sizes, document info and structure
  merge     Join PDF files in order
  split     Take pages out into a new file, or cut a PDF into files of N pages
  delete    Remove pages
  rotate    Turn pages by a multiple of 90 degrees
  img2pdf   Make a PDF from JPEG and PNG images, one image per page

Options:
  -h, --help       Show this help (also: qingpdf <command> --help)
  -V, --version    Show the version

Output files are never overwritten unless you add --force, and the output can
never be one of the input files. Encrypted PDFs are not supported yet.
Exit status: 0 success, 1 error, 2 wrong command line.
";

const PAGE_LIST: &str = "\
A page list is 1-based and comma separated: 1-3,5 is pages 1, 2, 3 and 5;
8- is page 8 to the last page. Pages come out in the order written.
";

const INFO: &str = "\
Usage:
  qingpdf info <file.pdf>

Show the PDF version, the number of pages, each page's size in points and
millimetres (as displayed, with its rotation), the document information
(title, author ...), whether the file is encrypted, whether it uses
cross-reference streams or object streams, and whether it had to be repaired
(its cross-reference table was damaged and was rebuilt by scanning the file).
";

const MERGE: &str = "\
Usage:
  qingpdf merge <a.pdf> <b.pdf> [<more.pdf> ...] -o <output.pdf> [--force]

Join the files in the order given. The first file is the base: its bookmarks,
form, named destinations and so on are kept. From the other files only the
pages (with their resources and annotations) are taken; a warning lists what
was left behind for each of them.

Options:
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
";

const SPLIT: &str = "\
Usage:
  qingpdf split <file.pdf> --pages <list> -o <output.pdf> [--force]
  qingpdf split <file.pdf> --every <N> -o <output_%d.pdf> [--force]

With --pages, write the listed pages to one new file. With --every, cut the
document into files of N pages each (the last may be shorter); the output
name must contain %d, which becomes 1, 2, 3, ... (for example out_%d.pdf gives
out_1.pdf, out_2.pdf, ...).

Options:
  --pages <list>        The pages to take
  --every <N>           Pages per file
  -o, --output <file>   The file to write (required)
  --force               Overwrite output files that exist

";

const DELETE: &str = "\
Usage:
  qingpdf delete <file.pdf> --pages <list> -o <output.pdf> [--force]

Write a copy of the document without the listed pages.

Options:
  --pages <list>        The pages to remove (required)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists

";

const ROTATE: &str = "\
Usage:
  qingpdf rotate <file.pdf> [--pages <list>] --angle <degrees> -o <output.pdf> [--force]

Turn pages clockwise by a multiple of 90 degrees (90, 180, 270; -90 turns the
other way). The angle is added to the rotation the page already has. Without
--pages every page is turned. Only the page's /Rotate entry changes.

Options:
  --pages <list>        The pages to turn (default: all)
  --angle <degrees>     A multiple of 90 (required)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists

";

const IMG2PDF: &str = "\
Usage:
  qingpdf img2pdf <1.jpg> <2.png> [...] -o <output.pdf> [--page a4|fit] [--force]

Make a PDF with one page per image, in the order given. JPEG files are
embedded as they are (not re-compressed) and honour the camera's rotation
(EXIF orientation). PNG files are stored losslessly; transparency is kept.
The file type is recognised from the file's contents, not its name.

Options:
  --page a4|fit         a4 (default): A4 paper, landscape for wide images; the
                        image is scaled to fit and centred.
                        fit: the page is exactly the size of the image at the
                        resolution stored in the file (96 dpi if none).
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
";

/// The help text for the tool or for one command.
pub fn text(command: Option<Command>) -> String {
    match command {
        None => MAIN.to_string(),
        Some(Command::Info) => INFO.to_string(),
        Some(Command::Merge) => MERGE.to_string(),
        Some(Command::Split) => format!("{SPLIT}{PAGE_LIST}"),
        Some(Command::Delete) => format!("{DELETE}{PAGE_LIST}"),
        Some(Command::Rotate) => format!("{ROTATE}{PAGE_LIST}"),
        Some(Command::Img2pdf) => IMG2PDF.to_string(),
    }
}

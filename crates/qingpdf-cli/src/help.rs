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
  decrypt   Write a copy of an encrypted PDF without the encryption
  img2pdf   Make a PDF from JPEG and PNG images, one image per page

Options:
  -h, --help       Show this help (also: qingpdf <command> --help)
  -V, --version    Show the version

Output files are never overwritten unless you add --force, and the output can
never be one of the input files.

Encrypted PDFs: a file that only restricts what may be done with it (printing,
copying ...) opens without a password. For one that needs a password, add
--password <password> to the command (it is used for every input that needs
one; the empty password is always tried first). Results keep the encryption of
the input; only 'decrypt' removes it.

Exit status: 0 success, 1 error, 2 wrong command line.
";

const PAGE_LIST: &str = "\
A page list is 1-based and comma separated: 1-3,5 is pages 1, 2, 3 and 5;
8- is page 8 to the last page. Pages come out in the order written.
";

const INFO: &str = "\
Usage:
  qingpdf info <file.pdf> [--password <password>]

Show the PDF version, the number of pages, each page's size in points and
millimetres (as displayed, with its rotation), the document information
(title, author ...), whether the file is encrypted, whether it uses
cross-reference streams or object streams, and whether it had to be repaired
(its cross-reference table was damaged and was rebuilt by scanning the file).

For an encrypted file it also says how it is encrypted (RC4 40-bit or 128-bit,
AES-128, AES-256), whether a password is needed to open it and which one
opened it, and what its author allows: printing, copying, modifying and so on.
Without the password only the encryption can be shown, not the document info.

Options:
  --password <password>  The password of an encrypted file
";

const MERGE: &str = "\
Usage:
  qingpdf merge <a.pdf> <b.pdf> [<more.pdf> ...] -o <output.pdf> [--force] [--password <password>]

Join the files in the order given. The first file is the base: its bookmarks,
form, named destinations and so on are kept. From the other files only the
pages (with their resources and annotations) are taken; a warning lists what
was left behind for each of them.

If the first file is encrypted, so is the result, with the first file's
passwords and permissions (encrypted files that follow are decrypted and
written under it). If the first file is not encrypted the result is not either,
and a warning names the later encrypted files.

Options:
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
  --password <password> The password of the encrypted input files
";

const SPLIT: &str = "\
Usage:
  qingpdf split <file.pdf> --pages <list> -o <output.pdf> [--force] [--password <password>]
  qingpdf split <file.pdf> --every <N> -o <output_%d.pdf> [--force] [--password <password>]

With --pages, write the listed pages to one new file. With --every, cut the
document into files of N pages each (the last may be shorter); the output
name must contain %d, which becomes 1, 2, 3, ... (for example out_%d.pdf gives
out_1.pdf, out_2.pdf, ...).

Options:
  --pages <list>        The pages to take
  --every <N>           Pages per file
  -o, --output <file>   The file to write (required)
  --force               Overwrite output files that exist
  --password <password> The password of an encrypted file (the pieces keep its
                        encryption)

";

const DELETE: &str = "\
Usage:
  qingpdf delete <file.pdf> --pages <list> -o <output.pdf> [--force] [--password <password>]

Write a copy of the document without the listed pages.

Options:
  --pages <list>        The pages to remove (required)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
  --password <password> The password of an encrypted file (the copy keeps its
                        encryption)

";

const ROTATE: &str = "\
Usage:
  qingpdf rotate <file.pdf> [--pages <list>] --angle <degrees> -o <output.pdf> [--force] [--password <password>]

Turn pages clockwise by a multiple of 90 degrees (90, 180, 270; -90 turns the
other way). The angle is added to the rotation the page already has. Without
--pages every page is turned. Only the page's /Rotate entry changes.

Options:
  --pages <list>        The pages to turn (default: all)
  --angle <degrees>     A multiple of 90 (required)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
  --password <password> The password of an encrypted file (the copy keeps its
                        encryption)

";

const DECRYPT: &str = "Usage:
  qingpdf decrypt <file.pdf> -o <output.pdf> [--password <password>] [--force]

Write a copy of an encrypted PDF without the encryption. This is only done
when the file was opened with its owner password, or when its permissions
allow everything (printing, copying, modifying, annotating, filling forms,
assembling); otherwise the author's restrictions would be taken off a file you
hold with limited rights, and the command refuses and says what is not allowed.
A file that is not encrypted is an error.

Options:
  --password <password> The owner password (or the user password, if the
                        file allows everything)
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
        Some(Command::Decrypt) => DECRYPT.to_string(),
        Some(Command::Img2pdf) => IMG2PDF.to_string(),
    }
}

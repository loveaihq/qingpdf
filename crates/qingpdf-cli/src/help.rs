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
  text      Extract the text of a PDF, in reading order
  render    Draw pages as PNG images (graphics and images; letters as boxes for now)
  img2pdf   Make a PDF from JPEG and PNG images, one image per page
  watermark Put a text or picture watermark on pages
  stamp     Put a picture (a signature, a logo) on a page
  addtext   Put text on a page
  replace   Find text in the pages and replace it with other text

Options:
  -h, --help       Show this help (also: qingpdf <command> --help)
  -V, --version    Show the version

Output files are never overwritten unless you add --force, and the output can
never be one of the input files.

Encrypted PDFs: a file that only restricts what may be done with it (printing,
copying ...) opens without a password. For one that needs a password, add
--password <password> to the command (it is used for every input that needs
one; a password that is given is tried first, then the empty one). Results keep
the encryption of the input; only 'decrypt' removes it.

watermark, stamp, addtext and replace write into the pages' own content, so every other
program and a printer show it too. The output is the input with the change
appended (the input's bytes are kept as they are, and signatures stay valid for
what they covered); they refuse a file that is signed, and an encrypted file whose
author did not allow changing it (give the owner password).

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
and a warning names the later encrypted files. A file that restricts what may
be done with it (it was not opened with its owner password, and its author did
not allow everything) can only be the first one; give its owner password to put
it later.

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

const TEXT: &str = "\
Usage:
  qingpdf text <file.pdf> [--pages <list>] [-o <output.txt>] [--force] [--password <password>]

Print the text of the PDF in reading order, as UTF-8, one line per text line.
Pages are separated by a form feed (the \\f character, as pdftotext does). With -o
the text goes to a file instead of the screen.

Chinese (and Japanese, Korean) text is read whether the fonts are embedded or
not, with or without a ToUnicode table; vertical text comes out one column per
line, right to left. Hidden text (the text layer of a scan) is included. A page
without a text layer gives nothing: scans are not recognized.

An encrypted file whose author did not allow copying or extracting text is
refused when it was opened with the user password; give the owner password.

Options:
  --pages <list>        The pages to read (default: all)
  -o, --output <file>   Write the text to this file
  --force               Overwrite the output file if it exists
  --password <password> The password of an encrypted file
";

const RENDER: &str = "Usage:
  qingpdf render <file.pdf> [--pages <list>] [--dpi <N>] [--no-annots] -o <page_%d.png> [--force] [--password <password>]

Draw pages as PNG images. The page is its crop box, turned by its /Rotate; one
file is written per page, and the output name must contain %d (which becomes the
page number) unless a single page is drawn.

What is drawn: paths (fill, stroke, dashes, clipping), colours (device, ICC
and Cal spaces as the device space, Indexed, Separation, DeviceN, Lab), images
(Flate, LZW, JPEG, JPEG 2000, CCITT fax, JBIG2; masks), forms, text (the font programs of the
PDF: TrueType, Type 1, CFF; fonts it does not carry come from the system fonts,
Chinese, Japanese and Korean ones included; a character whose glyph cannot be
had is drawn as an outline box), transparency (groups, soft masks, blend modes),
gradients (all seven shading types), tiling and shading patterns, and optional
content that is off (hidden layers). A page that asks for more work than is
allowed stops there with a warning and keeps what it has drawn. A grey block
stands in for a JPEG 2000 or JBIG2 image that cannot be decoded. Annotations
(notes, stamps, highlights, filled form fields) are drawn from their appearance
streams, on top of the page; hidden ones and pop-up windows are not. A form field
without an appearance stream is skipped with a warning.

Options:
  --pages <list>        The pages to draw (default: all)
  --dpi <N>             Resolution, 1 to 2400 (default 150); a page may have
                        at most 16 million pixels
  --no-annots           Do not draw the annotations
  -o, --output <file>   The file name pattern to write (required)
  --force               Overwrite output files that exist
  --password <password> The password of an encrypted file
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

const WATERMARK: &str = "\
Usage:
  qingpdf watermark <file.pdf> (--text <text> | --image <file>) [--opacity <0-1>] [--angle <degrees>]
                    [--tile] [--under] [--pages <list>] -o <output.pdf> [--force] [--password <password>]

Put a watermark on the pages: the text (Chinese too; it is embedded as a font
subset, so every reader shows it), a PNG or JPEG picture (a PNG's transparency
is kept), or both (the picture above the text). It is one mark in the middle of
the page, or with --tile repeated all over it. The mark is scaled to the page
and stays upright and centred on a page that has a /Rotate and on one whose box
does not start at 0.

Options:
  --text <text>         The text (at most 200 characters)
  --image <file>        A PNG or JPEG file (at most 16 million pixels, 12 MB)
  --opacity <number>    More than 0 and at most 1 (default 0.3)
  --angle <degrees>     Counterclockwise as the page is seen, -36000 to 36000 (default 0)
  --tile                Repeat the mark over the whole page
  --under               Put it under the page's own content (default: over it)
  --pages <list>        The pages (default: all; at most 20000 at once)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
  --password <password> The password of an encrypted file (the result keeps
                        its encryption)
";

const STAMP: &str = "\
Usage:
  qingpdf stamp <file.pdf> --image <file> --page <N> --at <X,Y> --width <W> -o <output.pdf> [--force] [--password <password>]

Put a PNG or JPEG picture (a signature, a logo; a PNG's transparency is kept)
on page N. X,Y is the picture's lower left corner and W its width, in points
(1/72 inch), from the lower left corner of the page as it is shown; the height
follows the picture's shape.

Options:
  --image <file>        The picture (required)
  --page <N>            The page, from 1 (required)
  --at <X,Y>            The lower left corner, in points (required; a place off the page is warned of)
  --width <W>           The width in points, up to 10000000 (required)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
  --password <password> The password of an encrypted file
";

const ADDTEXT: &str = "\
Usage:
  qingpdf addtext <file.pdf> --page <N> --at <X,Y> --size <S> --text <text> -o <output.pdf> [--force] [--password <password>]

Put text, in black, on page N, S points high. X,Y is where the first line's
baseline starts, in points from the lower left corner of the page as it is shown
(72 points are an inch). A backslash and an n in the text (\\n) start another
line below. The text is embedded as a font subset taken from the fonts of the
system, so Chinese is shown by every reader; a font that may not be embedded is
not used.

Options:
  --page <N>            The page, from 1 (required)
  --at <X,Y>            Where the text starts, in points (required; a place off the page is warned of)
  --size <S>            The size in points, 4 to 144; another size is changed to the nearest, with a warning (required)
  --text <text>         The text, at most 4000 characters (required)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
  --password <password> The password of an encrypted file
";

const REPLACE: &str = "Usage:
  qingpdf replace <file.pdf> --find <text> --replace <text> [--pages <list>] -o <output.pdf> [--force] [--password <password>]

Change text that is already on the pages. The text is found as `qingpdf text` gives it (one line at a
time; case as written; a line is not joined to the next), the old letters are taken out of the page's
drawing instructions (they are not covered over: copying the text, searching it and extracting it
give the new text only), and the new text is drawn where they were, in the same size, colour and
spacing. It uses the font of the old text when that font has every letter of the new text; the
others come from a subset of a system font that looks like it (Song, Hei, Kai or Fang for Chinese),
which is embedded in the file. Nothing is moved to make room: what follows a longer text in the same
line moves with it, and a warning says by how much the new texts are wider.

The output is the input with the change appended: the bytes of the input are kept as they are, so
the old text stays in the earlier part of the file (the pages no longer have it; to get rid of it
write the file again with 'qingpdf split out.pdf --pages 1- -o clean.pdf'). Text in a form that a
page draws more than once, or cut by a letter that stands for several characters, is not changed;
the number of such places is said. Exit status 1 when nothing was found or nothing could be
changed. Files that are signed are refused, and so are encrypted files whose author did not allow
changing them (give the owner password).

Options:
  --find <text>         The text to find (required; one line, at most 4000 characters)
  --replace <text>      The text to put there (required; may be empty to take the found text out)
  --pages <list>        Only these pages (default: all)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
  --password <password> The password of an encrypted file
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
        Some(Command::Text) => format!("{TEXT}{PAGE_LIST}"),
        Some(Command::Render) => format!("{RENDER}{PAGE_LIST}"),
        Some(Command::Img2pdf) => IMG2PDF.to_string(),
        Some(Command::Watermark) => format!("{WATERMARK}{PAGE_LIST}"),
        Some(Command::Stamp) => STAMP.to_string(),
        Some(Command::Addtext) => ADDTEXT.to_string(),
        Some(Command::Replace) => format!("{REPLACE}{PAGE_LIST}"),
    }
}

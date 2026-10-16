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
  encrypt   Write a copy with passwords and permissions (AES-256, or AES-128)
  text      Extract the text of a PDF, in reading order
  render    Draw pages as PNG images (graphics and images; letters as boxes for now)
  img2pdf   Make a PDF from JPEG and PNG images, one image per page
  watermark Put a text or picture watermark on pages
  pagenum   Put page numbers, headers and footers on pages
  stamp     Put a picture (a signature, a logo) on a page
  addtext   Put text on a page
  replace   Find text in the pages and replace it with other text
  docx      Make a Word file (.docx) of a PDF: text, headings, lists, tables, pictures
  xlsx      Make an Excel file (.xlsx) of the tables of a PDF, one sheet for each table

Options:
  -h, --help       Show this help (also: qingpdf <command> --help)
  -V, --version    Show the version

Output files are never overwritten unless you add --force, and the output can
never be one of the input files.

Encrypted PDFs: a file that only restricts what may be done with it (printing,
copying ...) opens without a password. For one that needs a password, add
--password <password> to the command (it is used for every input that needs
one; a password that is given is tried first, then the empty one). Results keep
the encryption of the input; only 'decrypt' removes it, and 'encrypt' sets or changes it.

watermark, pagenum, stamp, addtext and replace write into the pages' own content, so every other
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
A file that is not encrypted is an error, and so is a signed one (the whole file
is written again, which would break the signature).

Options:
  --password <password> The owner password (or the user password, if the
                        file allows everything)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
";

const ENCRYPT: &str = "Usage:
  qingpdf encrypt <file.pdf> -o <output.pdf> [--user-password <pw>] [--owner-password <pw>]
                  [--password-file <file>] [--aes128] [--print none|low|high] [--no-copy] [--no-modify]
                  [--no-annotate] [--no-forms] [--no-assemble] [--no-accessibility] [--cleartext-metadata]
                  [--password <password of the input>] [--force]

Write a copy of the PDF under a new encryption: AES-256 (the default; revision 6, read by current programs) or, with --aes128,
AES-128 (revision 4, for old programs). The user password is what opens the file (none: anyone can open it, with the
restrictions below). The owner password lifts the restrictions; it is required when a permission is turned off, and if only a
user password is given the owner password is the same. Give the passwords with --password-file instead of on the command line when
others can see your commands: line 1 of the file is the user password and line 2 the owner password (either may be empty).

Passwords: AES-256 takes any text, at most 127 bytes of UTF-8 (a Chinese character is 3). AES-128 keeps passwords in PDFDocEncoding,
at most 32 characters; one with Chinese in it is refused (use AES-256). Neither is ever printed or stored.

Permissions are all on unless turned off: --print none|low|high (high: the best quality; low: a poor one), --no-copy (copying text
and pictures), --no-modify (changing the content), --no-annotate (annotations), --no-forms (filling in forms), --no-assemble
(inserting, turning and deleting pages), --no-accessibility (extracting for screen readers). --cleartext-metadata leaves the
document's metadata unencrypted so that search programs can read it.

The whole file is written again (the pages, bookmarks, forms and the rest are kept; the bytes are not), so the output is a
new file: it may not be the input. An input that is encrypted needs --password and the owner's rights (the owner password, or a
file that allows everything), as 'decrypt' does. A signed file is refused: writing it again would break the signature.
To take the passwords off, use 'decrypt'.

Options:
  --user-password <pw>   The password that opens the file
  --owner-password <pw>  The password that lifts the restrictions
  --password-file <file> A text file with the user password on line 1 and the owner password on line 2
  --aes128               AES-128 instead of AES-256
  --print none|low|high  What printing is allowed (default: high)
  --no-copy, --no-modify, --no-annotate, --no-forms, --no-assemble, --no-accessibility
                         Turn that permission off
  --cleartext-metadata   Do not encrypt the metadata
  --password <password>  The password of the input, if it is encrypted
  -o, --output <file>    The file to write (required)
  --force                Overwrite the output file if it exists
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

const PAGENUM: &str = "\
Usage:
  qingpdf pagenum <file.pdf> [--header-left <t>] [--header-center <t>] [--header-right <t>]
                  [--footer-left <t>] [--footer-center <t>] [--footer-right <t>]
                  [--size <points>] [--color <rrggbb>] [--margin <mm>] [--pages <list>] [--start <N>]
                  [--mirror] [--skip-first] -o <output.pdf> [--force] [--password <password>]

Put page numbers, headers and footers on the pages: up to six pieces of text, at the left, middle and
right of the head and of the foot of each page. With no place named the number goes in the middle of the
foot (--footer-center '{page}'). The text is embedded as a font subset, so Chinese is shown by every
reader; it stays at the top and the bottom of the page as it is shown, upright, on a page that has a
/Rotate and on one whose box does not start at 0. It is marked as an artifact of the page (a header or a
footer, ISO 32000-1 14.8.2.2): text extraction, search and the export to Word and Excel leave it out.
Running the command again adds another set.

The text of a place may have fields:
  {page}    the number of the page
  {pages}   the number of the last page of the run (the number of pages, when the run is the whole file
            and starts at 1)
  {date}    today, as 2026-10-15 (UTC)
  {file}    the name of the file, without its folder
For example '- {page} -', '{page} / {pages}', 'Page {page} of {pages}' (Chinese text works too).

Options:
  --header-left, --header-center, --header-right, --footer-left, --footer-center, --footer-right <text>
                        The text of a place, at most 200 characters
  --size <points>       The size, 4 to 144 (default 10.5)
  --color <rrggbb>      The colour in hexadecimal (default 000000)
  --margin <mm>         The distance from the edge of the page, 0 to 500 (default 15)
  --pages <list>        The pages to number (default: all; at most 100000 at once). The first of them
                        is the first of the run: a later page has the start number plus its distance
                        from the first
  --start <N>           The number of the first page of the run, 0 to 1000000 (default 1); with
                        --pages 3- --start 1 page 3 reads 1
  --mirror              On the second, fourth ... page of the file the left places and the right ones
                        change sides (the number at the outer edge when the pages are printed on both sides)
  --skip-first          Nothing on the first page of the run (it is still counted)
  -o, --output <file>   The file to write (required)
  --force               Overwrite the output file if it exists
  --password <password> The password of an encrypted file (the result keeps its encryption)
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

const DOCX: &str = "Usage:
  qingpdf docx <file.pdf> [--pages <list>] -o <output.docx> [--force] [--password <password>]

Make a Word file of the pages, as text that can be edited: lines are joined into paragraphs (words
split by a hyphen at the end of a line are joined), big or bold lines become headings, bullets and
numbers become Word lists, two columns are read one after the other, tables that are drawn with
lines become Word tables with the text in the right cells, and the pictures are put in where they
are (JPEG as they are, the others as PNG). Running page headers, footers and page numbers are taken
out of the text and put in Word's header and footer. Chinese text keeps its kind of font (Song, Hei,
Kai, Fang) and full-width punctuation as it is; a page of vertical text becomes a section of vertical
text. A page that is only a picture (a scan) is a picture; the letters in it are not read (that is
what the next step will do). Nothing is moved to match the page exactly: this is a Word file to work
on, not a picture of the page.

Tables with no lines round them are found too, where the text lines up in rows and columns (a bank
statement, a financial report); text in columns that fills them, like the pages of a book, is not a
table.

Not done: text in the page that the file has as pictures or paths, pictures turned by other than
mirroring, and anything that is not text, a picture or a table (drawings, charts). A file whose author did not allow copying text
is refused unless the owner password is given. At most 5,000 pages, 8 million characters and 3,000
different pictures; choose some pages with --pages for more.

Options:
  --pages <list>        Only these pages (default: all)
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

const XLSX: &str = "Usage:
  qingpdf xlsx <file.pdf> [--pages <list>] -o <output.xlsx> [--force] [--password <password>]

Make an Excel file of the tables in the pages: each table is a sheet, named after its page and its
place on it (\"第3页-表1\"). Tables drawn with lines are found from the lines (cells joined across
lines that are missing stay merged cells); tables with no lines round them are found where the text
lines up in rows and columns, as in a bank statement or a financial report (a cell folded in two lines
is one cell, a heading across columns is a merged cell). A table that goes on from page to page (a
statement) is one sheet. Pages with no table give no sheet; a file with no table at all gets one
sheet with the text of its pages, a line to a row, and a warning.

Amounts, whole numbers, decimals with thousands separators (1,234.50), negative numbers (-12; (12)
and 12- when the column is made of amounts, else they are list numbers and stay text), percents and
amounts with a currency sign are stored as numbers in the same format, and dates (2024-05-01,
2024/5/1, 2024.05.01, 2024年5月1日) as dates. What a spreadsheet would spoil stays text:
identity numbers, phone numbers and card numbers (whole numbers of ten digits or more), codes that
begin with a zero (007), numbers of more than 15 digits, versions (1.2.3), ranges (3-5).

Not done: tables in vertical text, tables that are pictures or drawings, and fonts, colours or
borders of the cells. A file whose author did not allow copying text is refused unless the owner
password is given. At most 5,000 pages, 8 million characters, 1,000 tables and 32,767 characters in a
cell; choose some pages with --pages for more.

Options:
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
        Some(Command::Encrypt) => ENCRYPT.to_string(),
        Some(Command::Text) => format!("{TEXT}{PAGE_LIST}"),
        Some(Command::Render) => format!("{RENDER}{PAGE_LIST}"),
        Some(Command::Img2pdf) => IMG2PDF.to_string(),
        Some(Command::Watermark) => format!("{WATERMARK}{PAGE_LIST}"),
        Some(Command::Pagenum) => format!("{PAGENUM}{PAGE_LIST}"),
        Some(Command::Stamp) => STAMP.to_string(),
        Some(Command::Addtext) => ADDTEXT.to_string(),
        Some(Command::Replace) => format!("{REPLACE}{PAGE_LIST}"),
        Some(Command::Docx) => format!("{DOCX}{PAGE_LIST}"),
        Some(Command::Xlsx) => format!("{XLSX}{PAGE_LIST}"),
    }
}

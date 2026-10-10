//! Helpers for building small PDF files by hand in tests.

use std::collections::BTreeMap;

use miniz_oxide::deflate::compress_to_vec_zlib;

/// Appends objects to a byte buffer and remembers where each one starts.
pub struct PdfBuilder {
    buf: Vec<u8>,
    offsets: BTreeMap<u32, usize>,
}

impl PdfBuilder {
    /// A buffer holding just the header line and the binary comment (7.5.2).
    pub fn new() -> Self {
        Self::with_header("%PDF-1.4\n%\u{e2}\u{e3}\u{cf}\u{d3}\n")
    }

    pub fn with_header(header: &str) -> Self {
        // The binary comment bytes must be real bytes, not UTF-8 encoded.
        let mut buf = Vec::new();
        for ch in header.chars() {
            buf.push(u8::try_from(u32::from(ch)).unwrap_or(b'?'));
        }
        PdfBuilder { buf, offsets: BTreeMap::new() }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Offset of the most recent definition of object `num`.
    pub fn offset_of(&self, num: u32) -> usize {
        self.offsets[&num]
    }

    /// `num 0 obj body endobj`; returns the offset where it starts.
    pub fn obj(&mut self, num: u32, body: &str) -> usize {
        let at = self.buf.len();
        self.offsets.insert(num, at);
        self.buf.extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
        at
    }

    /// Append the text of an object that is written by hand (to produce a
    /// wrong `/Length`, say), recording where it starts as object `num`.
    pub fn raw_obj(&mut self, num: u32, text: &[u8]) -> usize {
        let at = self.buf.len();
        self.offsets.insert(num, at);
        self.buf.extend_from_slice(text);
        at
    }

    /// A stream object with a correct direct `/Length`; `dict_entries` are the
    /// other entries of the stream dictionary.
    pub fn stream_obj(&mut self, num: u32, dict_entries: &str, data: &[u8]) -> usize {
        let at = self.buf.len();
        self.offsets.insert(num, at);
        self.buf.extend_from_slice(
            format!("{num} 0 obj\n<< /Length {} {dict_entries} >>\nstream\n", data.len()).as_bytes(),
        );
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(b"\nendstream\nendobj\n");
        at
    }

    /// A Flate-compressed stream object.
    pub fn flate_stream_obj(&mut self, num: u32, dict_entries: &str, data: &[u8]) -> usize {
        let packed = compress_to_vec_zlib(data, 6);
        self.stream_obj(num, &format!("/Filter /FlateDecode {dict_entries}"), &packed)
    }

    /// A classic xref section with one entry per object `0..size`, free where
    /// no object was defined, then `trailer << /Size size {trailer_extra} >>`.
    /// Returns the offset of the `xref` keyword.
    pub fn classic_xref(&mut self, size: u32, trailer_extra: &str) -> usize {
        let at = self.buf.len();
        let mut s = format!("xref\n0 {size}\n");
        for num in 0..size {
            match self.offsets.get(&num) {
                Some(off) => s.push_str(&format!("{off:010} 00000 n \n")),
                None if num == 0 => s.push_str("0000000000 65535 f \n"),
                None => s.push_str("0000000000 00000 f \n"),
            }
        }
        s.push_str(&format!("trailer\n<< /Size {size} {trailer_extra} >>\n"));
        self.buf.extend_from_slice(s.as_bytes());
        at
    }

    pub fn startxref(&mut self, xref_offset: usize) {
        self.buf.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());
    }

    /// Write a classic xref section and `startxref`.
    pub fn finish_classic(mut self, size: u32, trailer_extra: &str) -> Vec<u8> {
        let at = self.classic_xref(size, trailer_extra);
        self.startxref(at);
        self.buf
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// A one-page document with a classic cross-reference table:
/// 1 catalog, 2 page tree, 3 page, 4 content stream.
pub fn sample_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << >> /Contents 4 0 R >>");
    b.stream_obj(4, "", b"0 0 m 100 100 l S");
    b.finish_classic(5, "/Root 1 0 R")
}

/// Rows of an xref-stream-like table, PNG "Up" filtered (tag 2) and Flate
/// compressed, the way real producers write cross-reference streams.
pub fn png_up_flate<const N: usize>(rows: &[[u8; N]]) -> Vec<u8> {
    let mut encoded = Vec::new();
    let mut prev = [0u8; N];
    for row in rows {
        encoded.push(2);
        for (x, p) in row.iter().zip(prev.iter()) {
            encoded.push(x.wrapping_sub(*p));
        }
        prev = *row;
    }
    compress_to_vec_zlib(&encoded, 6)
}

/// A PDF 1.5 style document: objects 1-3 (catalog, page tree, one 300x400
/// page) live in object stream 5, object 4 is the page's content stream,
/// object 6 is a Flate + PNG predictor cross-reference stream.
pub fn sample_objstm_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::with_header("%PDF-1.5\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
    let bodies = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 4 0 R >>",
    ];
    let mut header = String::new();
    let mut body = String::new();
    for (i, text) in bodies.iter().enumerate() {
        header.push_str(&format!("{} {} ", i + 1, body.len()));
        body.push_str(text);
        body.push('\n');
    }
    let o5 = b.flate_stream_obj(
        5,
        &format!("/Type /ObjStm /N 3 /First {}", header.len()),
        format!("{header}{body}").as_bytes(),
    );
    let o4 = b.stream_obj(4, "", b"0 0 m 1 1 l S");
    let x = b.len();
    let row = |t: u8, a: usize, g: u8| [t, (a >> 8) as u8, a as u8, g];
    let rows = [row(0, 0, 255), row(2, 5, 0), row(2, 5, 1), row(2, 5, 2), row(1, o4, 0), row(1, o5, 0), row(1, x, 0)];
    let packed = png_up_flate(&rows);
    b.stream_obj(
        6,
        "/Type /XRef /Size 7 /W [1 2 1] /Root 1 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4 >>",
        &packed,
    );
    b.startxref(x);
    b.finish()
}

/// A one-page file encrypted with RC4 and a 40-bit key (revision 2) and with no `/ID` in the trailer, which the key is made
/// without (7.6.3.3 takes the first string of the ID as empty). The user password is empty.
pub fn encrypted_rc4_without_id() -> Vec<u8> {
    const PAD: [u8; 32] = [
        0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C,
        0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
    ];
    let owner = [0x5Au8; 32];
    let p: i32 = -4;
    let mut input = PAD.to_vec();
    input.extend_from_slice(&owner);
    input.extend_from_slice(&p.to_le_bytes());
    let key = crate::cipher::md5(&input)[..5].to_vec();
    let mut user = PAD;
    crate::cipher::rc4(&key, &mut user);
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02X}")).collect::<String>();
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << >> >>");
    b.obj(10, &format!("<< /Filter /Standard /V 1 /R 2 /P -4 /O <{}> /U <{}> >>", hex(&owner), hex(&user)));
    b.finish_classic(11, "/Root 1 0 R /Encrypt 10 0 R")
}

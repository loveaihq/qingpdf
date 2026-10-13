//! Text into XML 1.0 (the parts of a .docx and a .xlsx).

/// Is `c` allowed in an XML 1.0 document (and not one of the control characters, noncharacters and C1 controls that Word does not
/// like even where XML would let them through)?
pub(crate) fn xml_ok(c: char) -> bool {
    matches!(u32::from(c), 0x9 | 0xA | 0xD | 0x20..=0x7E | 0xA0..=0xD7FF | 0xE000..=0xFDCF | 0xFDF0..=0xFFFD | 0x1_0000..=0x1_FFFD | 0x2_0000..=0x2_FFFD | 0x3_0000..=0x10_FFFD)
}

pub(crate) fn esc(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c if xml_ok(c) => out.push(c),
            _ => {}
        }
    }
}

/// The same for the value of an attribute in double quotes.
pub(crate) fn esc_attr(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '"' => out.push_str("&quot;"),
            c => esc(out, c.encode_utf8(&mut [0; 4])),
        }
    }
}

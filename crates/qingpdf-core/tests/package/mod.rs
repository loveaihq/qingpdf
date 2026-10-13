//! A reader for the packages (.docx, .xlsx) the writers make: the ZIP container, checked on the way, and the XML parts.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use qingpdf_core::render::export::crc32_update;

/// Every entry of a ZIP file with its name and contents; the sizes, the CRC and the headers are checked on the way.
pub fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
    let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let end = bytes.len() - 22;
    assert_eq!(u32_at(end), 0x0605_4B50, "end of central directory");
    let (count, size, start) = (u16_at(end + 10), u32_at(end + 12) as usize, u32_at(end + 16) as usize);
    assert_eq!(start + size, end, "the central directory runs up to the end record");
    let mut out = Vec::new();
    let mut at = start;
    for _ in 0..count {
        assert_eq!(u32_at(at), 0x0201_4B50, "central directory header");
        let (flags, method, crc, packed, unpacked) = (u16_at(at + 8), u16_at(at + 10), u32_at(at + 16), u32_at(at + 20) as usize, u32_at(at + 24) as usize);
        let (name_len, extra, comment, local) = (u16_at(at + 28), u16_at(at + 30), u16_at(at + 32), u32_at(at + 42) as usize);
        let name = std::str::from_utf8(&bytes[at + 46..at + 46 + name_len]).unwrap().to_string();
        assert_eq!(flags & 0x0800, 0x0800, "{name}: names are UTF-8");
        // The local header says the same.
        assert_eq!(u32_at(local), 0x0403_4B50, "{name}: local header");
        assert_eq!((u16_at(local + 8), u32_at(local + 14), u32_at(local + 18) as usize, u32_at(local + 22) as usize), (method, crc, packed, unpacked), "{name}");
        let data_at = local + 30 + u16_at(local + 26) + u16_at(local + 28);
        let data = &bytes[data_at..data_at + packed];
        let data = match method {
            0 => data.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec(data).expect("deflate"),
            m => panic!("method {m}"),
        };
        assert_eq!(data.len(), unpacked, "{name}");
        assert_eq!(crc32_update(0, &data), crc, "{name}: CRC");
        out.push((name, data));
        at += 46 + name_len + extra + comment;
    }
    out
}

/// Are the tags of an XML text opened and closed in order (and every attribute value quoted)?
pub fn well_formed(xml: &str) -> Result<(), String> {
    let mut stack: Vec<&str> = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let j = rest.find('>').ok_or("a tag that is never closed")?;
        let tag = &rest[..j];
        rest = &rest[j + 1..];
        if tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        if !tag.matches('"').count().is_multiple_of(2) {
            return Err(format!("an attribute value that is not closed: <{tag}>"));
        }
        if let Some(name) = tag.strip_prefix('/') {
            if stack.pop() != Some(name.trim()) {
                return Err(format!("</{name}> closes nothing"));
            }
        } else if !tag.ends_with('/') {
            stack.push(tag.split_whitespace().next().unwrap_or(""));
        }
    }
    if stack.is_empty() { Ok(()) } else { Err(format!("not closed: {stack:?}")) }
}

//! The container a .docx or .xlsx is: a ZIP archive (PKWARE APPNOTE 4.3: local file headers, the central directory, the end record),
//! written in one pass, to a sink.

use std::io::{self, Write};

use miniz_oxide::deflate::compress_to_vec;
use qingpdf_base::render::export::crc32_update;

/// `docProps/core.xml` (the creator and the dates) for a package made at `now` (seconds since 1970).
pub(crate) fn core_properties(now: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(now);
    let stamp = format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>
<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><dc:creator>qingpdf</dc:creator><cp:lastModifiedBy>qingpdf</cp:lastModifiedBy><dcterms:created xsi:type=\"dcterms:W3CDTF\">{stamp}</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">{stamp}</dcterms:modified></cp:coreProperties>"
    )
}

/// Run `make` on a file of its own in the folder of `path`; the file replaces `path` only when `make` is done and it is on disk (an
/// error or a cancel leaves `path` as it was, and no stray file).
pub(crate) fn write_file<T>(path: &std::path::Path, make: impl FnOnce(&mut io::BufWriter<std::fs::File>) -> crate::Result<T>) -> crate::Result<T> {
    let name = path.file_name().ok_or_else(|| crate::Error::Invalid("no file name".to_string()))?;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| std::path::Path::new("."));
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".qingpdf-{}.tmp", std::process::id()));
    let temp = dir.join(temp_name);
    let result = (|| {
        let mut sink = io::BufWriter::new(std::fs::File::create(&temp)?);
        let made = make(&mut sink)?;
        sink.flush()?;
        sink.into_inner().map_err(|e| crate::Error::Io(e.into_error()))?.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(made)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Seconds since 1970 to (year, month, day, hour, minute, second), UTC.
pub(crate) fn civil(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Days since 0000-03-01 (the algorithm of Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as u32;
    (year, month, day, (rest / 3600) as u32, (rest % 3600 / 60) as u32, (rest % 60) as u32)
}

struct Entry {
    name: String,
    crc: u32,
    packed: u32,
    size: u32,
    method: u16,
    offset: u32,
}

/// A ZIP archive written to `out` one entry at a time. No ZIP64: an archive over 4 GB, or with over 65,535 entries, is refused.
pub(crate) struct Zip<W: Write> {
    out: W,
    pos: u64,
    entries: Vec<Entry>,
    dos: (u16, u16),
}

fn too_big() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "the file would be over 4 GB (or have over 65,535 parts)")
}

impl<W: Write> Zip<W> {
    pub(crate) fn new(out: W, now: u64) -> Zip<W> {
        let (y, mo, d, h, mi, s) = civil(now);
        let date = ((y.clamp(1980, 2107) - 1980) << 9) | (mo << 5) | d;
        let time = (h << 11) | (mi << 5) | (s / 2);
        Zip { out, pos: 0, entries: Vec::new(), dos: (time as u16, date as u16) }
    }

    /// Bytes written so far.
    pub(crate) fn len(&self) -> u64 {
        self.pos
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    /// One entry, compressed with Deflate (for XML).
    pub(crate) fn add_deflated(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let packed = compress_to_vec(data, 6);
        self.add(name, data, &packed, 8)
    }

    /// One entry as it is (for images, which are compressed already).
    pub(crate) fn add_stored(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.add(name, data, data, 0)
    }

    fn add(&mut self, name: &str, data: &[u8], packed: &[u8], method: u16) -> io::Result<()> {
        let (size, csize) = (u32::try_from(data.len()).map_err(|_| too_big())?, u32::try_from(packed.len()).map_err(|_| too_big())?);
        let offset = u32::try_from(self.pos).map_err(|_| too_big())?;
        // (The central directory and the end record come after the entries: leave room for them.)
        if self.entries.len() >= 65_000 || self.pos + u64::from(csize) + 2 * name.len() as u64 + 200 + 100 * self.entries.len() as u64 >= u64::from(u32::MAX) {
            return Err(too_big());
        }
        let crc = crc32_update(0, data);
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes()); // version needed
        h.extend_from_slice(&0x0800u16.to_le_bytes()); // names are UTF-8
        h.extend_from_slice(&method.to_le_bytes());
        h.extend_from_slice(&self.dos.0.to_le_bytes());
        h.extend_from_slice(&self.dos.1.to_le_bytes());
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&csize.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&u16::try_from(name.len()).map_err(|_| too_big())?.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // no extra field
        h.extend_from_slice(name.as_bytes());
        self.put(&h)?;
        self.put(packed)?;
        self.entries.push(Entry { name: name.to_string(), crc, packed: csize, size, method, offset });
        Ok(())
    }

    /// The central directory and the end record; gives back the sink and the size of the archive.
    pub(crate) fn finish(mut self) -> io::Result<(W, u64)> {
        let start = u32::try_from(self.pos).map_err(|_| too_big())?;
        let entries = std::mem::take(&mut self.entries);
        let mut dir = Vec::new();
        for e in &entries {
            dir.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
            dir.extend_from_slice(&20u16.to_le_bytes()); // made by
            dir.extend_from_slice(&20u16.to_le_bytes()); // needed
            dir.extend_from_slice(&0x0800u16.to_le_bytes());
            dir.extend_from_slice(&e.method.to_le_bytes());
            dir.extend_from_slice(&self.dos.0.to_le_bytes());
            dir.extend_from_slice(&self.dos.1.to_le_bytes());
            dir.extend_from_slice(&e.crc.to_le_bytes());
            dir.extend_from_slice(&e.packed.to_le_bytes());
            dir.extend_from_slice(&e.size.to_le_bytes());
            dir.extend_from_slice(&u16::try_from(e.name.len()).map_err(|_| too_big())?.to_le_bytes());
            dir.extend_from_slice(&[0; 8]); // extra and comment lengths, disk, internal attributes
            dir.extend_from_slice(&0u32.to_le_bytes()); // external attributes
            dir.extend_from_slice(&e.offset.to_le_bytes());
            dir.extend_from_slice(e.name.as_bytes());
        }
        let size = u32::try_from(dir.len()).map_err(|_| too_big())?;
        if u64::from(start) + u64::from(size) + 22 > u64::from(u32::MAX) {
            return Err(too_big());
        }
        let n = u16::try_from(entries.len()).map_err(|_| too_big())?;
        dir.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
        dir.extend_from_slice(&[0; 4]); // disk numbers
        dir.extend_from_slice(&n.to_le_bytes());
        dir.extend_from_slice(&n.to_le_bytes());
        dir.extend_from_slice(&size.to_le_bytes());
        dir.extend_from_slice(&start.to_le_bytes());
        dir.extend_from_slice(&0u16.to_le_bytes()); // no comment
        self.put(&dir)?;
        self.out.flush()?;
        Ok((self.out, self.pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_that_would_pass_4_gb_or_65_thousand_parts_is_refused() {
        let mut z = Zip::new(std::io::sink(), 0);
        z.pos = u64::from(u32::MAX) - 100;
        assert!(z.add_stored("a.bin", b"hello").is_err());
        z.pos = 10;
        assert!(z.add_stored("a.bin", b"hello").is_ok());
        let mut many = Zip::new(std::io::sink(), 0);
        let results: Vec<bool> = (0..65_100).map(|i| many.add_stored(&format!("{i}"), b"").is_ok()).collect();
        assert!(results[64_999] && !results[65_000]);
    }

    #[test]
    fn the_central_directory_agrees_with_the_entries() {
        let mut z = Zip::new(Vec::new(), 1_000_000_000);
        z.add_deflated("word/\u{4e2d}.xml", b"<a/><a/><a/><a/>").unwrap();
        z.add_stored("b.bin", &[1, 2, 3]).unwrap();
        let (out, size) = z.finish().unwrap();
        assert_eq!(size as usize, out.len());
        let n = out.len();
        assert_eq!(&out[n - 22..n - 18], &[0x50, 0x4B, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([out[n - 12], out[n - 11]]), 2);
        // The name, in UTF-8, is in the central directory and in the local header.
        let name = "word/\u{4e2d}.xml".as_bytes();
        assert_eq!(out.windows(name.len()).filter(|w| *w == name).count(), 2);
    }

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(1_000_000_000), (2001, 9, 9, 1, 46, 40));
        assert_eq!(civil(1_709_164_800), (2024, 2, 29, 0, 0, 0));
    }
}

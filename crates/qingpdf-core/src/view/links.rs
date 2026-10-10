//! The links of a page (3d-2): the `/Link` annotations (12.5.6.5) with a destination in the file or an address, each with the
//! rectangle that is clickable on the page as it is shown.
//!
//! Only addresses that are safe to hand to the system are kept: see [`safe_uri`].

use std::collections::HashMap;

use crate::dests;
use crate::document::{Document, Page};
use crate::object::{Object, PdfString};
use crate::render::work::{Work, cost};
use crate::text::Shown;

use super::targets::{Names, target};

/// Most links one page has in the list; the rest are left out.
pub const MAX_LINKS: usize = 2_000;
/// Most entries of a page's `/Annots` looked at.
const MAX_ANNOTS_LOOKED_AT: usize = 20_000;
/// The longest address that is passed on.
pub const MAX_URI_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LinkKind {
    /// Leads to a place in this file (`page` and `y`).
    Page = 0,
    /// Leads to an address (`uri`): http, https or mailto, nothing else.
    Uri = 1,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// The clickable rectangle, `[left, top, right, bottom]` in points on the page as it is shown.
    pub rect: [f32; 4],
    pub kind: LinkKind,
    /// For [`LinkKind::Page`]: the page it leads to (from 0), and how far down it, if the destination says.
    pub page: u32,
    pub y: Option<f32>,
    /// For [`LinkKind::Uri`]: the address, checked by [`safe_uri`]. Empty for the other kind.
    pub uri: String,
}

/// An address from a file, if it is safe to show to the user and to hand to the system's browser or mail program: its
/// scheme is `http`, `https` or `mailto` (in any case), it is no longer than [`MAX_URI_BYTES`], and it is plain ASCII with no
/// controls, spaces or characters that could end a quoted argument or start a new one (`"` `<` `>` `\` `^` `` ` `` `{` `|`
/// `}`); a `%` only as the start of a percent-escape (`%` and two hexadecimal digits, RFC 3986 2.1), so that something like
/// `%USERNAME%` is never handed to a system that would expand it as an environment variable; an `http` or `https` address
/// has a host. The address is returned as it is, never changed.
pub fn safe_uri(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_URI_BYTES {
        return None;
    }
    if !bytes.iter().all(|&b| b.is_ascii_graphic() && !b"\"<>\\^`{|}".contains(&b)) {
        return None;
    }
    if bytes.iter().enumerate().any(|(i, &b)| b == b'%' && !bytes.get(i + 1..i + 3).is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit))) {
        return None;
    }
    let s = std::str::from_utf8(bytes).ok()?;
    let lower = s.to_ascii_lowercase();
    let after_scheme = if let Some(r) = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) {
        r
    } else {
        let r = lower.strip_prefix("mailto:")?;
        return (!r.is_empty()).then(|| s.to_string());
    };
    // A host: something before the first '/', '?' or '#'.
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    (!host.is_empty() && !host.starts_with('@')).then(|| s.to_string())
}

/// The links of `page`. `truncated`: there were more than [`MAX_LINKS`], or too many annotations, or the work ran out.
pub(super) fn read(doc: &Document, page: &Page, pages: &[Page], index: &HashMap<u32, u32>, names: &Names<'_>, work: &Work) -> (Vec<Link>, bool) {
    let mut links = Vec::new();
    let mut truncated = false;
    let Some(held) = page.dict.get("Annots").and_then(|a| work.read(doc, a)) else { return (links, false) };
    let Some(annots) = held.as_array() else { return (links, false) };
    let shown = Shown::of(page);
    for (looked, item) in annots.iter().enumerate() {
        if looked >= MAX_ANNOTS_LOOKED_AT || links.len() >= MAX_LINKS || !work.charge(cost::ANNOT_VISIT) {
            truncated = true;
            break;
        }
        let Ok(Some(held)) = work.resolve(doc, item) else { continue };
        let Some(d) = held.as_dict() else { continue };
        if !matches!(d.get("Subtype"), Some(Object::Name(n)) if n == "Link") {
            continue;
        }
        // Hidden (bit 2) and not-to-be-viewed (bit 6) annotations are not there for the reader.
        if d.get_int("F").is_some_and(|f| f & (2 | 32) != 0) {
            continue;
        }
        let Some([x0, y0, x1, y1]) = work.rectangle(doc, d.get("Rect")) else { continue };
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for (x, y) in [(x0, y0), (x0, y1), (x1, y0), (x1, y1)] {
            let (px, py) = shown.map(x, y);
            b = [b[0].min(px), b[1].min(py), b[2].max(px), b[3].max(py)];
        }
        if !b.iter().all(|v| v.is_finite()) || b[2] <= b[0] || b[3] <= b[1] {
            continue;
        }
        let rect = [b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32];
        // /Dest, else /A: a go-to action (a place in this file) or a URI action (an address).
        let mut link = None;
        if let Some(dest) = d.get("Dest") {
            let found = names.get(work).and_then(|n| dests::dest_array(doc, dest, n).ok().flatten()).and_then(|a| target(&a, pages, index));
            link = found.map(|t| Link { rect, kind: LinkKind::Page, page: t.page, y: t.y, uri: String::new() });
        } else if let Some(action) = d.get("A").and_then(|a| work.read(doc, a))
            && let Some(action) = action.as_dict()
        {
            match action.get("S") {
                Some(Object::Name(n)) if n == "GoTo" => {
                    let found = action
                        .get("D")
                        .and_then(|dest| names.get(work).and_then(|n| dests::dest_array(doc, dest, n).ok().flatten()))
                        .and_then(|a| target(&a, pages, index));
                    link = found.map(|t| Link { rect, kind: LinkKind::Page, page: t.page, y: t.y, uri: String::new() });
                }
                Some(Object::Name(n)) if n == "URI" => {
                    let uri = match action.get("URI").and_then(|u| work.read(doc, u)).as_deref() {
                        Some(Object::String(PdfString { bytes, .. })) => safe_uri(bytes),
                        _ => None,
                    };
                    link = uri.map(|uri| Link { rect, kind: LinkKind::Uri, page: 0, y: None, uri });
                }
                _ => {}
            }
        }
        links.extend(link);
    }
    (links, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_https_and_mailto_with_nothing_odd_are_safe() {
        assert_eq!(safe_uri(b"https://example.com/a?b=c#d").as_deref(), Some("https://example.com/a?b=c#d"));
        assert_eq!(safe_uri(b"HTTP://Example.com").as_deref(), Some("HTTP://Example.com"));
        assert_eq!(safe_uri(b"https://example.com/%e4%B8%ad%2F").as_deref(), Some("https://example.com/%e4%B8%ad%2F"));
        assert_eq!(safe_uri(b"mailto:someone@example.com?subject=Hi%20there").as_deref(), Some("mailto:someone@example.com?subject=Hi%20there"));
        for bad in [
            &b""[..],
            b"javascript:alert(1)",
            b"file:///C:/Windows/System32/calc.exe",
            b"ftp://example.com",
            b"ms-msdt:/id PCWDiagnostic",
            b"\\\\server\\share\\a.exe",
            b"http://",
            b"https:///path",
            b"https://?x",
            b"mailto:",
            b"example.com",
            b"https://exa mple.com",
            b"https://example.com/\"--flag",
            b"https://example.com/<x>",
            b"https://example.com/\x07",
            b"https://example.com/\n",
            "https://例子.com".as_bytes(),
            b"https://example.com/a^b",
            b"https://a.com/`x",
            // A '%' that does not begin a percent-escape: an environment variable of the system, or a cut-off escape.
            b"https://%USERNAME%.example.com/",
            b"https://example.com/%PATH%",
            b"https://example.com/%",
            b"https://example.com/%4",
            b"https://example.com/%4g",
            b"mailto:me@example.com?subject=%%41",
        ] {
            assert_eq!(safe_uri(bad), None, "{}", String::from_utf8_lossy(bad));
        }
        let mut long = b"https://example.com/".to_vec();
        long.resize(MAX_URI_BYTES, b'a');
        assert!(safe_uri(&long).is_some());
        long.push(b'a');
        assert_eq!(safe_uri(&long), None);
    }
}

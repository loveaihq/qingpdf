//! Changing the title, author, subject and keywords of a file (2026-10-21): the information dictionary (14.3.3), and the metadata stream (14.3.2) when the
//! file has one, so that the two say the same. Nothing else in either is touched: the other keys and the other properties of the XMP stay as they are,
//! and so do the dates (a program that wants the modification date changed says so itself).
//!
//! The Info dictionary is written where it is. A file that has none gets a new object for it; the trailer of the incremental update that writes the file
//! names it (`Document::set_pending_info`). The XMP stream is written again, uncompressed, with the four properties put right in its text; the
//! properties are `dc:title`, `dc:creator`, `dc:description` and `pdf:Keywords`. A file whose XMP cannot be put right (its text is not UTF-8 or has no
//! `rdf:Description` to put them in, or it is not encrypted like the rest) keeps it as it is, and the person is told.

use super::content::Added;
use super::outline::{added_by, title_string};
use super::signature::{self, Policy};
use super::{Ctx, EditClass};
use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dict, Object, Stream};
use crate::xml::{esc, esc_attr};

/// Most characters of one of the four.
pub const MAX_META_CHARS: usize = 2_000;

/// The properties to change; `None` leaves one as it is, an empty text takes it away.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
}

impl Meta {
    fn fields(&self) -> [(&'static str, Option<&str>); 4] {
        [("Title", self.title.as_deref()), ("Author", self.author.as_deref()), ("Subject", self.subject.as_deref()), ("Keywords", self.keywords.as_deref())]
    }
}

/// How an XMP property is written: its prefix and name, and the container of its value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `<rdf:Alt><rdf:li xml:lang="x-default">`
    Alt,
    /// `<rdf:Seq><rdf:li>`
    Seq,
    /// Just the text.
    Text,
}

/// The XMP property of each Info key.
fn property(key: &str) -> (&'static str, &'static str, Shape) {
    match key {
        "Title" => ("dc", "title", Shape::Alt),
        "Author" => ("dc", "creator", Shape::Seq),
        "Subject" => ("dc", "description", Shape::Alt),
        _ => ("pdf", "Keywords", Shape::Text),
    }
}

fn namespace(prefix: &str) -> &'static str {
    if prefix == "dc" { "http://purl.org/dc/elements/1.1/" } else { "http://ns.adobe.com/pdf/1.3/" }
}

/// The element for the property with `value`, with its namespace declared on it. (It is declared on every element that is written, whether the packet
/// declares the prefix somewhere or not: a packet like Acrobat's has one `rdf:Description` for each namespace, so a declaration that is somewhere in the
/// text may not be in scope where the element goes, and one that an earlier element had on itself goes when that element is replaced.)
fn element(key: &str, value: &str) -> String {
    let (prefix, name, shape) = property(key);
    let mut text = String::new();
    esc(&mut text, value);
    let ns = format!(" xmlns:{prefix}=\"{}\"", namespace(prefix));
    let inner = match shape {
        Shape::Alt => format!("<rdf:Alt><rdf:li xml:lang=\"x-default\">{text}</rdf:li></rdf:Alt>"),
        Shape::Seq => format!("<rdf:Seq><rdf:li>{text}</rdf:li></rdf:Seq>"),
        Shape::Text => text,
    };
    format!("<{prefix}:{name}{ns}>{inner}</{prefix}:{name}>")
}

/// The text an element says: what is between its tags, with the escapes of XML (`&amp;` and the like) read, without the white space at the ends.
fn text_of(element: &str) -> String {
    let (mut text, mut in_tag) = (String::new(), false);
    for c in element.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.trim().replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Where the element `<prefix:name ...>...</prefix:name>` (or `<prefix:name .../>`) of the document starts and ends.
fn find_element(xml: &str, prefix: &str, name: &str) -> Option<(usize, usize)> {
    let open = format!("<{prefix}:{name}");
    let mut from = 0;
    while let Some(at) = xml.get(from..)?.find(&open) {
        let start = from + at;
        let after = start + open.len();
        // (Not `<dc:titles`, say.)
        if xml.as_bytes().get(after).is_some_and(|b| !matches!(b, b' ' | b'>' | b'/' | b'\t' | b'\r' | b'\n')) {
            from = after;
            continue;
        }
        let tag_end = after + xml.get(after..)?.find('>')?;
        if xml.get(..tag_end)?.ends_with('/') {
            return Some((start, tag_end + 1));
        }
        let close = format!("</{prefix}:{name}>");
        let end = tag_end + xml.get(tag_end..)?.find(&close)? + close.len();
        return Some((start, end));
    }
    None
}

/// Where the attribute `prefix:name="..."` (or in single quotes) of a start tag starts and ends, with the space before it.
fn find_attribute(xml: &str, prefix: &str, name: &str) -> Option<(usize, usize)> {
    let head = format!("{prefix}:{name}=");
    let mut from = 0;
    while let Some(at) = xml.get(from..)?.find(&head) {
        let start = from + at;
        from = start + head.len();
        let quote = *xml.as_bytes().get(from)?;
        let before = xml.get(..start)?;
        // (An attribute is in a tag: the last `<` before it has no `>` after it, and a space comes before the name.)
        let in_tag = before.rfind('<').is_some_and(|lt| !before.get(lt..).unwrap_or("").contains('>'));
        if matches!(quote, b'"' | b'\'') && in_tag && before.ends_with(|c: char| c.is_ascii_whitespace()) {
            let end = from + 1 + xml.get(from + 1..)?.find(char::from(quote))? + 1;
            let space = before.trim_end_matches(|c: char| c.is_ascii_whitespace()).len();
            return Some((space, end));
        }
    }
    None
}

/// The text of the XMP packet `xml` with the properties of `meta` put right, or `None` when that cannot be done: it has no `rdf:Description` to put a
/// property that is not there in, or there is nothing to change. A property that is there is replaced where it is (as an element, or as an attribute
/// of a description); one that is not is put at the end of the first description; an empty text takes the property away.
#[inline(never)]
pub fn update_xmp(xml: &str, meta: &Meta) -> Option<String> {
    let mut out = xml.to_string();
    let mut changed = false;
    for (key, value) in meta.fields() {
        let Some(value) = value else { continue };
        let (prefix, name, _) = property(key);
        let value: String = value.chars().take(MAX_META_CHARS).collect();
        let value = value.trim();
        if let Some((a, b)) = find_element(&out, prefix, name) {
            // (The text it says already: nothing to write.)
            if !value.is_empty() && text_of(out.get(a..b)?) == value {
                continue;
            }
            let new = if value.is_empty() { String::new() } else { element(key, value) };
            out.replace_range(a..b, &new);
        } else if let Some((a, b)) = find_attribute(&out, prefix, name) {
            let mut new = String::new();
            if !value.is_empty() {
                new.push_str(&format!(" {prefix}:{name}=\""));
                esc_attr(&mut new, value);
                new.push('"');
            }
            out.replace_range(a..b, &new);
        } else if !value.is_empty() {
            let new = element(key, value);
            if let Some(end) = out.find("</rdf:Description>") {
                out.insert_str(end, &new);
            } else {
                // A description with no content, written as one tag.
                let start = out.find("<rdf:Description")?;
                let tag_end = start + out.get(start..)?.find('>')?;
                if !out.get(..tag_end)?.ends_with('/') {
                    return None;
                }
                out.replace_range(tag_end - 1..tag_end + 1, &format!(">{new}</rdf:Description>"));
            }
        } else {
            continue;
        }
        changed = true;
    }
    (changed && out != xml).then_some(out)
}

/// Work out the objects that make the properties `meta` says.
#[inline(never)]
pub(super) fn apply(ctx: &mut Ctx<'_>, meta: &Meta) -> Result<()> {
    let doc = ctx.doc;
    if doc.is_locked() {
        return Err(Error::PasswordRequired);
    }
    let from = ctx.objects.len();
    // The Info dictionary where it is, or in a new object.
    let (num, mut info) = match doc.info_entry() {
        Some(Object::Ref(r)) => match doc.get(r) {
            Ok(Object::Dict(d)) => (r.num, d),
            _ => (r.num, Dict::new()),
        },
        Some(Object::Dict(d)) => (ctx.alloc()?.num, d),
        _ => (ctx.alloc()?.num, Dict::new()),
    };
    for (key, value) in meta.fields() {
        let Some(value) = value else { continue };
        let value: String = value.chars().take(MAX_META_CHARS).collect();
        if value.trim().is_empty() {
            info.remove(key);
        } else {
            info.set(key, title_string(value.trim()));
        }
    }
    if doc.trailer().get("Info").and_then(Object::as_obj_ref).is_none_or(|r| r.num != num) {
        doc.set_pending_info(num);
    }
    ctx.objects.push((num, Object::Dict(info)));
    // The XMP stream, if there is one.
    if let Some(Object::Ref(r)) = doc.catalog()?.get("Metadata") {
        let r = *r;
        let left = match doc.get(r) {
            Ok(Object::Stream(s)) if !doc.encryption().is_some_and(|e| !e.encrypt_metadata) => match doc.decode_stream(&s).ok().and_then(|b| String::from_utf8(b).ok()).and_then(|x| update_xmp(&x, meta)) {
                Some(text) => {
                    let mut dict = s.dict;
                    for key in ["Filter", "DecodeParms", "Length"] {
                        dict.remove(key);
                    }
                    ctx.objects.push((r.num, Object::Stream(Stream { dict, data: text.into_bytes() })));
                    false
                }
                None => true,
            },
            _ => true,
        };
        if left {
            ctx.notes.push("the XMP metadata of the file was left as it is: it could not be put right".to_string());
        }
    }
    ctx.tally(from)
}

/// Why a file may not have its properties changed, if it may not: its permissions (bit 4, unless it was opened with the owner password) and its
/// signatures.
pub fn check_meta_allowed(doc: &Document, password: &str) -> Result<()> {
    if doc.is_locked() {
        return Err(Error::PasswordRequired);
    }
    if let Some(encryption) = doc.encryption()
        && !encryption.permissions.modify
        && !crate::ops::has_owner_rights(doc, &encryption, password)
    {
        return Err(Error::Invalid("the file's author did not allow changing it (permission bit 4); the owner password is needed (give it with --password)".to_string()));
    }
    match signature::look(doc).policy(EditClass::Meta) {
        Policy::Refuse(why) => Err(Error::Invalid(why.to_string())),
        _ => Ok(()),
    }
}

/// The file with its properties changed, as one incremental update behind the whole original.
pub fn set_meta(doc: &Document, password: &str, meta: &Meta) -> Result<Added> {
    check_meta_allowed(doc, password)?;
    added_by(doc, password, |ctx, _| apply(ctx, meta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{Edit, apply_any, commit, save_section};
    use crate::render::work::Work;
    use crate::testutil::PdfBuilder;

    const XMP: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" pdf:Producer=\"P\" pdf:Keywords=\"old, words\">\n<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Old title</rdf:li></rdf:Alt></dc:title>\n<dc:creator><rdf:Seq><rdf:li>Old author</rdf:li></rdf:Seq></dc:creator>\n</rdf:Description>\n</rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";

    /// What every element that is written declares of the prefix `dc`.
    const DC: &str = " xmlns:dc=\"http://purl.org/dc/elements/1.1/\"";

    /// Is `xml` well formed as far as tags and prefixes go (every tag closed, every prefix of an element or an attribute declared on it or on an element
    /// it is in)? What an XML parser would refuse for a packet written by hand.
    fn check_xml(xml: &str) -> std::result::Result<(), String> {
        let mut open: Vec<(String, Vec<String>)> = Vec::new();
        let mut at = 0;
        while let Some(lt) = xml[at..].find('<') {
            let start = at + lt;
            let rest = &xml[start..];
            if rest.starts_with("<?") || rest.starts_with("<!--") {
                let close = if rest.starts_with("<?") { "?>" } else { "-->" };
                at = start + rest.find(close).ok_or("a declaration is not closed")? + close.len();
                continue;
            }
            let end = start + rest.find('>').ok_or("a tag is not closed")?;
            let tag = &xml[start + 1..end];
            at = end + 1;
            if let Some(name) = tag.strip_prefix('/') {
                match open.pop() {
                    Some((n, _)) if n == name.trim() => {}
                    other => return Err(format!("</{name}> does not close {other:?}")),
                }
                continue;
            }
            let empty = tag.ends_with('/');
            let tag = tag.trim_end_matches('/');
            let name_end = tag.find(|c: char| c.is_whitespace()).unwrap_or(tag.len());
            let (name, mut attrs) = (&tag[..name_end], &tag[name_end..]);
            let (mut declared, mut used) = (Vec::new(), vec![name.to_string()]);
            while let Some(eq) = attrs.find('=') {
                let attr = attrs[..eq].trim();
                let quote = attrs[eq + 1..].chars().next().filter(|q| *q == '"' || *q == '\'').ok_or("an attribute has no quotes")?;
                let value_end = attrs[eq + 2..].find(quote).ok_or("an attribute value is not closed")? + eq + 2;
                match attr.strip_prefix("xmlns:") {
                    Some(p) => declared.push(p.to_string()),
                    None => used.push(attr.to_string()),
                }
                attrs = &attrs[value_end + 1..];
            }
            for q in &used {
                if let Some((prefix, _)) = q.split_once(':')
                    && prefix != "xml"
                    && !declared.iter().any(|d| d == prefix)
                    && !open.iter().any(|(_, ds)| ds.iter().any(|d| d == prefix))
                {
                    return Err(format!("the prefix {prefix} of {q} is not declared"));
                }
            }
            if !empty {
                open.push((name.to_string(), declared));
            }
        }
        if open.is_empty() { Ok(()) } else { Err(format!("not closed: {open:?}")) }
    }

    /// A packet the way Acrobat writes one: a description for each namespace, each with its own declaration.
    const ACROBAT: &str = "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n<rdf:Description rdf:about=\"\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\"><pdf:Producer>Acrobat</pdf:Producer></rdf:Description>\n<rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"><xmp:CreatorTool>Word</xmp:CreatorTool></rdf:Description>\n<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:format>application/pdf</dc:format><dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Old</rdf:li></rdf:Alt></dc:title></rdf:Description>\n</rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";

    #[test]
    fn a_packet_with_a_description_for_each_namespace_stays_well_formed_through_two_changes_in_a_row() {
        check_xml(ACROBAT).unwrap();
        // The first description declares `pdf` only: the new `dc` and `pdf` elements go into it and declare what they use themselves.
        let one = update_xmp(ACROBAT, &meta(None, Some("Me"), Some("About"), Some("k"))).unwrap();
        check_xml(&one).unwrap_or_else(|e| panic!("{e}\n{one}"));
        assert!(one.contains(&format!("<dc:creator{DC}>")) && one.contains(&format!("<dc:description{DC}>")) && one.contains("<pdf:Keywords xmlns:pdf="), "{one}");
        // The second change replaces elements the first wrote, and the one the packet had: the declarations they carried go with them, and the new ones carry their own.
        let two = update_xmp(&one, &meta(Some("New title"), Some("You"), Some("Other"), Some("k2"))).unwrap();
        check_xml(&two).unwrap_or_else(|e| panic!("{e}\n{two}"));
        for want in ["New title", "You", "Other", "k2", "Acrobat", "Word", "application/pdf"] {
            assert!(two.contains(want), "{want} in {two}");
        }
        for gone in ["Old", "Me", "About"] {
            assert!(!two.contains(&format!(">{gone}<")), "{gone} in {two}");
        }
        // And a third, which takes the properties away: still well formed.
        let three = update_xmp(&two, &meta(Some(""), Some(""), Some(""), Some(""))).unwrap();
        check_xml(&three).unwrap_or_else(|e| panic!("{e}\n{three}"));
        assert!(!three.contains("dc:title") && !three.contains("dc:creator") && !three.contains("Keywords"), "{three}");
        // The checker itself refuses what it should.
        assert!(check_xml("<a><b:c></a>").is_err() && check_xml("<a><b></a></b>").is_err() && check_xml("<a>").is_err());
    }

    fn meta(t: Option<&str>, a: Option<&str>, s: Option<&str>, k: Option<&str>) -> Meta {
        Meta { title: t.map(String::from), author: a.map(String::from), subject: s.map(String::from), keywords: k.map(String::from) }
    }

    #[test]
    fn properties_that_are_there_are_replaced_where_they_are_and_the_rest_stays() {
        let out = update_xmp(XMP, &meta(Some("New <title> & more"), Some("\u{4E2D}\u{6587}"), None, Some("a, b"))).unwrap();
        assert!(out.contains(&format!("<dc:title{DC}><rdf:Alt><rdf:li xml:lang=\"x-default\">New &lt;title&gt; &amp; more</rdf:li></rdf:Alt></dc:title>")), "{out}");
        assert!(out.contains(&format!("<dc:creator{DC}><rdf:Seq><rdf:li>\u{4E2D}\u{6587}</rdf:li></rdf:Seq></dc:creator>")));
        check_xml(&out).unwrap();
        // The keywords are an attribute of the description: replaced there, the other attributes stay.
        assert!(out.contains("pdf:Keywords=\"a, b\"") && out.contains("pdf:Producer=\"P\"") && !out.contains("old, words"), "{out}");
        assert!(out.starts_with("<?xpacket begin") && out.ends_with("<?xpacket end=\"w\"?>"));
        // Nothing asked: nothing changes. The same text: nothing changes.
        assert_eq!(update_xmp(XMP, &Meta::default()), None);
        assert_eq!(update_xmp(XMP, &meta(Some("Old title"), None, None, None)), None);
    }

    #[test]
    fn properties_that_are_not_there_are_added_to_the_first_description_and_empty_ones_are_taken_away() {
        let out = update_xmp(XMP, &meta(None, None, Some("About it"), None)).unwrap();
        assert!(out.contains(&format!("<dc:description{DC}><rdf:Alt><rdf:li xml:lang=\"x-default\">About it</rdf:li></rdf:Alt></dc:description></rdf:Description>")), "{out}");
        // (Each element declares its prefix, whether the packet does or not.)
        check_xml(&out).unwrap();
        let gone = update_xmp(&out, &meta(Some(""), Some("   "), None, Some(""))).unwrap();
        assert!(!gone.contains("dc:title") && !gone.contains("dc:creator") && !gone.contains("pdf:Keywords") && gone.contains("pdf:Producer=\"P\""), "{gone}");
        // A prefix the document does not declare is declared on the element; a description written as one tag is opened.
        let bare = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";
        let out = update_xmp(bare, &meta(Some("T"), None, None, Some("K"))).unwrap();
        assert!(out.contains("<dc:title xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><rdf:Alt>") && out.contains("<pdf:Keywords xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\">K</pdf:Keywords></rdf:Description>"), "{out}");
        // No description at all, or a name that only starts like the property's: left as it is.
        assert_eq!(update_xmp("<x:xmpmeta/>", &meta(Some("T"), None, None, None)), None);
        let titles = "<rdf:RDF xmlns:rdf=\"r\"><rdf:Description><dc:titles>x</dc:titles></rdf:Description></rdf:RDF>";
        assert!(update_xmp(titles, &meta(Some("T"), None, None, None)).unwrap().contains("<dc:titles>x</dc:titles><dc:title xmlns:dc"));
    }

    fn pdf(info: Option<&str>, xmp: Option<&str>) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R {} >>", if xmp.is_some() { "/Metadata 5 0 R" } else { "" }));
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
        if let Some(i) = info {
            b.obj(4, i);
        }
        if let Some(x) = xmp {
            b.stream_obj(5, "/Type /Metadata /Subtype /XML", x.as_bytes());
        }
        b.finish_classic(20, if info.is_some() { "/Root 1 0 R /Info 4 0 R" } else { "/Root 1 0 R" })
    }

    fn changed(base: &[u8], m: &Meta) -> (Document, Vec<String>) {
        let doc = Document::from_bytes(base.to_vec()).unwrap();
        let pages = doc.pages().unwrap();
        let (objects, notes) = apply_any(&doc, &pages, &Edit::Meta(Box::new(m.clone())), "", &Work::new(), 16 << 20).unwrap();
        commit(&doc, objects);
        let mut bytes = base.to_vec();
        bytes.extend_from_slice(&save_section(&doc).unwrap());
        (Document::from_bytes(bytes).unwrap(), notes)
    }

    fn text_of(doc: &Document, key: &str) -> Option<String> {
        match doc.info().unwrap()?.get(key) {
            Some(Object::String(s)) => Some(crate::info::decode_text_string(&s.bytes)),
            _ => None,
        }
    }

    #[test]
    fn the_info_dictionary_and_the_xmp_say_the_same_after_a_change() {
        let base = pdf(Some("<< /Title (Old) /Author (Me) /Creator (Writer) /CreationDate (D:20250101000000Z) >>"), Some(XMP));
        let (doc, notes) = changed(&base, &meta(Some("\u{65B0}\u{6807}\u{9898}"), None, Some("Subject"), Some("")));
        assert!(notes.is_empty());
        assert_eq!(text_of(&doc, "Title").as_deref(), Some("\u{65B0}\u{6807}\u{9898}"));
        // What was not asked for, and the dates and the other keys, are as they were; the emptied one is gone.
        assert_eq!((text_of(&doc, "Author").as_deref(), text_of(&doc, "Creator").as_deref(), text_of(&doc, "CreationDate").as_deref()), (Some("Me"), Some("Writer"), Some("D:20250101000000Z")));
        assert_eq!(text_of(&doc, "Subject").as_deref(), Some("Subject"));
        let Object::Stream(x) = doc.get(crate::object::ObjRef::new(5, 0)).unwrap() else { panic!("no XMP") };
        let xml = String::from_utf8(doc.decode_stream(&x).unwrap()).unwrap();
        assert!(xml.contains("\u{65B0}\u{6807}\u{9898}") && xml.contains("Subject") && !xml.contains("pdf:Keywords") && xml.contains("Old author"), "{xml}");
        assert_eq!(x.dict.get_name("Subtype").map(|n| n.as_bytes().to_vec()), Some(b"XML".to_vec()));
        // The original bytes are the start of the new file (an incremental update), and an XMP that cannot be put right is left, with a word about it.
        let (_, notes) = changed(&pdf(Some("<< /Title (T) >>"), Some("<x:xmpmeta/>")), &meta(Some("N"), None, None, None));
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn a_file_without_an_info_dictionary_gets_one_and_an_undo_takes_it_away_again() {
        let base = pdf(None, None);
        let (doc, _) = changed(&base, &meta(Some("Made"), Some("Writer"), None, None));
        assert_eq!((text_of(&doc, "Title").as_deref(), text_of(&doc, "Author").as_deref()), (Some("Made"), Some("Writer")));
        assert!(doc.trailer().get("Info").is_some());
        // In memory: the new dictionary is read, and when the change is taken back it is not.
        let doc = Document::from_bytes(base).unwrap();
        let pages = doc.pages().unwrap();
        let objects = apply_any(&doc, &pages, &Edit::Meta(Box::new(meta(Some("Made"), None, None, None))), "", &Work::new(), 16 << 20).unwrap().0;
        let changes = commit(&doc, objects);
        assert!(doc.info().unwrap().is_some());
        crate::edit::replay(&doc, &changes, false);
        assert!(doc.info().unwrap().is_none());
        assert!(!doc.has_overlay());
    }
}

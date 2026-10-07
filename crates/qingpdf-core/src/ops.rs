//! The operations behind the commands: copy, split, delete, rotate, merge and
//! images to PDF. Each returns the bytes of a new file and the warnings the
//! user should see.
//!
//! Copy, split, delete and rotate share one path ([`build`]): the whole
//! document is rewritten from its catalog (so bookmarks, named destinations,
//! forms and the like survive) and the page tree is rebuilt flat from the
//! selected pages, in order (7.7.3). Merge does the same with the first file
//! as the base and the pages of the other files added; the other files'
//! document-level items are not merged and a warning says which were lost.
//!
//! What was encrypted stays encrypted: the output has the first file's
//! encryption dictionary, key, passwords and permissions (as qpdf does), and
//! every string and stream is encrypted again under its new object number.
//! Only [`decrypt`] writes a file without it.

use std::cell::{Cell, OnceCell};
use std::collections::HashSet;

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::image::{self, Format, PageMode};
use crate::object::{Dict, Name, ObjRef, Object};
use crate::prune;
use crate::security::PasswordKind;
use crate::writer::{Builder, Warning};

/// A finished file and what to tell the user about it.
#[derive(Debug)]
pub struct Output {
    pub data: Vec<u8>,
    pub warnings: Vec<Warning>,
    /// How many pages the file has.
    pub pages: usize,
}

/// One input of a merge: the name to use in messages, and the document.
pub struct Input<'a> {
    pub name: &'a str,
    pub doc: &'a Document,
}

fn internal(what: &str) -> Error {
    Error::Invalid(format!("internal error: {what}"))
}

// --- page ranges ----------------------------------------------------------------

fn parse_page_number(text: &str, whole: &str) -> Result<usize> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Invalid(format!("\"{text}\" in the page list \"{whole}\" is not a page number")));
    }
    // Digits only, so a failure here is overflow: certainly out of range.
    text.parse::<usize>().map_err(|_| Error::Invalid(format!("page number {text} is out of range")))
}

/// Parse a page list such as `1-3,5,8-` for a document of `page_count` pages
/// into 0-based page indices, in the order written (a page named twice is
/// listed twice). Page numbers start at 1; `N-` runs to the last page; a
/// number past the last page, `0`, and a range that runs backwards are errors.
pub fn parse_page_ranges(spec: &str, page_count: usize) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(Error::Invalid(format!("empty entry in the page list \"{spec}\"")));
        }
        let (first, last) = match part.split_once('-') {
            None => {
                let n = parse_page_number(part, spec)?;
                (n, n)
            }
            Some((a, b)) => {
                let first = parse_page_number(a.trim(), spec)?;
                let b = b.trim();
                let last = if b.is_empty() { page_count } else { parse_page_number(b, spec)? };
                (first, last)
            }
        };
        for n in [first, last] {
            if n == 0 {
                return Err(Error::Invalid(format!("page numbers start at 1 (in \"{part}\")")));
            }
            if n > page_count {
                return Err(Error::Invalid(format!(
                    "page {n} is out of range: the document has {page_count} page{}",
                    if page_count == 1 { "" } else { "s" }
                )));
            }
        }
        if first > last {
            return Err(Error::Invalid(format!("the range \"{part}\" runs backwards")));
        }
        out.extend((first..=last).map(|n| n - 1));
    }
    Ok(out)
}

// --- rewriting a document -----------------------------------------------------------

/// One page of the output: page `page` of document `doc`, turned a further
/// `rotate_by` degrees.
struct Pick {
    doc: usize,
    page: usize,
    rotate_by: i64,
}

/// The pages of a document and their object numbers, read once.
struct Loaded<'a> {
    doc: &'a Document,
    pages: Vec<Page>,
    page_nums: HashSet<u32>,
    /// What the document-level structures say about the pages; read the first
    /// time a subset of the pages is cut out, then reused (splitting into many
    /// files cuts the same structures again and again).
    index: OnceCell<prune::Index>,
    /// The size of the file last written from this document: the next one is
    /// about as big (the pieces of a split), so the buffer is made that big.
    size_hint: Cell<usize>,
}

impl Loaded<'_> {
    fn index(&self) -> Result<&prune::Index> {
        if let Some(index) = self.index.get() {
            return Ok(index);
        }
        let built = prune::Index::build(self.doc, &self.pages, &self.page_nums)?;
        Ok(self.index.get_or_init(|| built))
    }
}

fn load(doc: &Document) -> Result<Loaded<'_>> {
    if doc.is_locked() {
        return Err(Error::PasswordRequired);
    }
    let pages = doc.pages()?;
    let page_nums = pages.iter().map(|p| p.obj_ref.num).collect();
    Ok(Loaded { doc, pages, page_nums, index: OnceCell::new(), size_hint: Cell::new(0) })
}

/// Catalog entries that name pages by position or hold page references that
/// cannot be repaired when pages go missing, with what to tell the user.
const LABELS: (&str, &str) = ("PageLabels", "page labels");
const THREADS: (&str, &str) = ("Threads", "article threads");

/// Write a new file whose pages are `picks`, in that order, from the
/// documents in `docs`; the catalog is the first document's. `encrypt` keeps the first document's
/// encryption (when it has any); without it the output is written in the clear.
///
/// When pages of the first document are left out, its bookmarks, forms and
/// named destinations are cut down to the pages that stay ([`prune`]). The
/// other documents bring only their pages: their named destinations are made
/// explicit and their structure tree links are dropped, because the things
/// they point into are not copied.
fn build(docs: &[Loaded<'_>], picks: &[Pick], encrypt: bool) -> Result<Output> {
    let base = docs.first().ok_or_else(|| internal("no documents"))?;
    let version = docs.iter().map(|d| d.doc.version()).max().unwrap_or((1, 4));
    // The pages of the first document that stay, by object number.
    let kept: HashSet<u32> =
        picks.iter().filter(|p| p.doc == 0).filter_map(|p| base.pages.get(p.page)).map(|p| p.obj_ref.num).collect();
    let subset = kept.len() < base.page_nums.len();

    let mut b = Builder::with_capacity(version, base.size_hint.get());
    let sources: Vec<usize> = docs.iter().map(|d| b.add_source(d.doc, &d.page_nums)).collect();
    let source_of = |i: usize| sources.get(i).copied().ok_or_else(|| internal("unknown document"));
    if encrypt {
        b.keep_encryption_of(source_of(0)?);
    }
    for &later in sources.iter().skip(1) {
        b.resolve_named_destinations(later);
        b.drop_structure_links(later);
    }

    let catalog_ref = b.reserve()?;
    let pages_ref = b.reserve()?;
    let mut warnings = Vec::new();
    let mut catalog = base.doc.catalog()?;
    catalog.remove("Pages");
    // `/EncryptMetadata false` leaves this stream, and only this one, unencrypted.
    if let Some(Object::Ref(metadata)) = catalog.get("Metadata") {
        b.mark_root_metadata(source_of(0)?, metadata.num);
    }
    if subset {
        let index = base.index()?;
        b.exclude_annotations(source_of(0)?, &index.annot_pages, &kept);
        index.apply(base.doc, &kept, &mut b, source_of(0)?, &mut catalog, &mut warnings)?;
    }

    // Give every output page its number first, so that a link on one page to
    // another resolves whatever the order of writing.
    let mut out_pages: Vec<ObjRef> = Vec::with_capacity(picks.len());
    for pick in picks {
        let page = docs.get(pick.doc).and_then(|d| d.pages.get(pick.page)).ok_or_else(|| internal("bad page"))?;
        let new = b.reserve()?;
        b.register_page(source_of(pick.doc)?, page.obj_ref, new);
        out_pages.push(new);
    }

    for (pick, &new) in picks.iter().zip(&out_pages) {
        let page = docs.get(pick.doc).and_then(|d| d.pages.get(pick.page)).ok_or_else(|| internal("bad page"))?;
        let source = source_of(pick.doc)?;
        let mut dict: Dict = (*page.dict).clone();
        // Article beads lead to threads at the document level (12.4.3); they
        // would pull the rest of the file back in.
        dict.remove("B");
        dict.set("Type", Object::from("Page"));
        // 7.7.3.3 Table 30: /MediaBox is required. A page with none (its own
        // or inherited) gets US Letter, which is what every reader assumes
        // for it anyway.
        if page.media_box().is_none() {
            dict.set("MediaBox", Object::Array([0, 0, 612, 792].into_iter().map(Object::Integer).collect()));
        }
        let Object::Dict(mut dict) = b.import(source, Object::Dict(dict))? else {
            return Err(internal("a page did not stay a dictionary"));
        };
        inherit(&mut b, source, page, &mut dict)?;
        if pick.rotate_by != 0 {
            // 7.7.3.3: a multiple of 90, so keep it in 0..360 and drop zero.
            let turned = page.rotate().saturating_add(pick.rotate_by).rem_euclid(360);
            dict.set("Rotate", if turned == 0 { Object::Null } else { Object::Integer(turned) });
        }
        dict.set("Parent", Object::Ref(pages_ref));
        b.put(new, &Object::Dict(dict))?;
    }

    // The page tree (7.7.3.2), flat: one node holding every page.
    let mut tree = Dict::new();
    tree.set("Type", Object::from("Pages"));
    tree.set("Kids", Object::Array(out_pages.iter().map(|r| Object::Ref(*r)).collect()));
    tree.set("Count", Object::Integer(i64::try_from(out_pages.len()).unwrap_or(i64::MAX)));
    b.put(pages_ref, &Object::Dict(tree))?;

    // The catalog (7.7.2) is the first file's, with the page tree swapped.
    // Page labels are indexed by page position: right only while every page
    // of the first file still sits where it was. Threads hold references to
    // pages, and a thread with pages missing is not a thread.
    let in_place = picks.iter().enumerate().all(|(i, p)| p.doc != 0 || p.page == i);
    let mut seen = vec![false; base.pages.len()];
    for p in picks.iter().filter(|p| p.doc == 0) {
        if let Some(slot) = seen.get_mut(p.page) {
            *slot = true;
        }
    }
    let all_kept = seen.iter().all(|&k| k);
    for (condition, (key, what)) in [(in_place, LABELS), (all_kept, THREADS)] {
        if !condition && catalog.remove(key).is_some() {
            warnings.push(Warning(format!("{what} were dropped because the pages no longer line up with them")));
        }
    }
    // 7.7.2: a catalog /Version supersedes the header's, so it must not be
    // older than the file.
    if catalog.contains_key("Version") {
        catalog.set("Version", Object::Name(Name::new(format!("{}.{}", version.0, version.1))));
    }
    let Object::Dict(mut catalog) = b.import(source_of(0)?, Object::Dict(catalog))? else {
        return Err(internal("the catalog did not stay a dictionary"));
    };
    catalog.set("Type", Object::from("Catalog"));
    catalog.set("Pages", Object::Ref(pages_ref));
    b.put(catalog_ref, &Object::Dict(catalog))?;

    // The information dictionary of the first file.
    let info_ref = match base.doc.trailer().get("Info") {
        Some(Object::Ref(r)) => b.import_ref(source_of(0)?, *r)?,
        Some(Object::Dict(d)) => {
            let Object::Dict(d) = b.import(source_of(0)?, Object::Dict(d.clone()))? else {
                return Err(internal("the information dictionary did not stay a dictionary"));
            };
            Some(b.add(&Object::Dict(d))?)
        }
        _ => None,
    };

    let (data, more) = b.finish(catalog_ref, info_ref)?;
    base.size_hint.set(data.len());
    warnings.extend(more);
    Ok(Output { data, warnings, pages: out_pages.len() })
}

/// Put what `page` inherits from the page tree (7.7.3.4) onto its output
/// dictionary `dict` (whose references are already output ones): a page taken
/// out of its tree must carry it. A direct `/Resources` that many pages share
/// is written once, as an object of its own, and each page refers to it; the
/// boxes and the rotation are small and are copied.
fn inherit(b: &mut Builder<'_>, source: usize, page: &Page, dict: &mut Dict) -> Result<()> {
    if let Some(shared) = page.inherited("Resources") {
        match shared.object {
            Object::Ref(r) => {
                if let Some(new) = b.import_ref(source, *r)? {
                    dict.set("Resources", Object::Ref(new));
                }
            }
            Object::Dict(_) => {
                let new = b.import_shared(source, shared.id, shared.object)?;
                dict.set("Resources", Object::Ref(new));
            }
            _ => {}
        }
    }
    // 7.7.3.3 Table 30: /Resources is required. An empty dictionary is what
    // every reader assumes for a page that has none.
    if !matches!(dict.get("Resources"), Some(Object::Dict(_) | Object::Ref(_))) {
        dict.set("Resources", Object::Dict(Dict::new()));
    }
    if !dict.contains_key("MediaBox") && page.media_box().is_some() {
        copy_inherited(page, dict, "MediaBox");
    }
    if !dict.contains_key("CropBox") && page.crop_box().is_some() {
        copy_inherited(page, dict, "CropBox");
    }
    if !dict.contains_key("Rotate") {
        copy_inherited(page, dict, "Rotate");
    }
    Ok(())
}

/// The value of an inherited box or rotation, references followed, if the
/// page gets one and it is a plausible one: four numbers, or a number.
fn copy_inherited(page: &Page, dict: &mut Dict, key: &str) {
    match page.inherited_resolved(key) {
        Some(value @ Object::Array(items)) if key != "Rotate" && items.len() == 4 => dict.set(key, value.clone()),
        Some(value @ (Object::Integer(_) | Object::Real(_))) if key == "Rotate" => dict.set(key, value.clone()),
        _ => {}
    }
}

fn all_picks(doc: usize, count: usize) -> impl Iterator<Item = Pick> {
    (0..count).map(move |page| Pick { doc, page, rotate_by: 0 })
}

/// Rewrite the whole document: the same pages, the same everything, written
/// afresh (classic cross-reference table, no object streams, renumbered, only
/// what is reachable).
pub fn copy_all(doc: &Document) -> Result<Output> {
    let d = load(doc)?;
    let picks: Vec<Pick> = all_picks(0, d.pages.len()).collect();
    build(&[d], &picks, true)
}

/// A new file with just the pages `pages` (0-based indices, as returned by
/// [`parse_page_ranges`]), in that order.
pub fn extract_pages(doc: &Document, pages: &[usize]) -> Result<Output> {
    if pages.is_empty() {
        return Err(Error::Invalid("no pages selected".to_string()));
    }
    let d = load(doc)?;
    let picks: Vec<Pick> = pages.iter().map(|&page| Pick { doc: 0, page, rotate_by: 0 }).collect();
    build(&[d], &picks, true)
}

/// A new file without the pages `pages` (0-based indices).
pub fn delete_pages(doc: &Document, pages: &[usize]) -> Result<Output> {
    let d = load(doc)?;
    let gone: HashSet<usize> = pages.iter().copied().collect();
    let picks: Vec<Pick> = all_picks(0, d.pages.len()).filter(|p| !gone.contains(&p.page)).collect();
    if picks.is_empty() {
        return Err(Error::Invalid("that would delete every page".to_string()));
    }
    build(&[d], &picks, true)
}

/// A new file in which the pages `pages` (0-based indices) are turned by
/// `angle` degrees clockwise, a multiple of 90, added to what they have.
pub fn rotate_pages(doc: &Document, pages: &[usize], angle: i64) -> Result<Output> {
    if angle % 90 != 0 {
        return Err(Error::Invalid(format!("the angle must be a multiple of 90, not {angle}")));
    }
    let d = load(doc)?;
    let turn: HashSet<usize> = pages.iter().copied().collect();
    let picks: Vec<Pick> = all_picks(0, d.pages.len())
        .map(|mut p| {
            if turn.contains(&p.page) {
                p.rotate_by = angle;
            }
            p
        })
        .collect();
    build(&[d], &picks, true)
}

/// How much of a document `split_every` keeps parsed while it works.
const SPLIT_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Turns the document's object cache on, and off again when it goes away.
struct ObjectCacheGuard<'a>(&'a Document);

impl<'a> ObjectCacheGuard<'a> {
    fn new(doc: &'a Document) -> Self {
        doc.cache_objects(SPLIT_CACHE_BYTES);
        ObjectCacheGuard(doc)
    }
}

impl Drop for ObjectCacheGuard<'_> {
    fn drop(&mut self) {
        self.0.cache_objects(0);
    }
}

/// How many files `split_every` makes for `page_count` pages.
pub fn chunk_count(page_count: usize, every: usize) -> usize {
    if every == 0 { 0 } else { page_count.div_ceil(every) }
}

/// Split into files of `every` pages (the last may be shorter). Each file is
/// handed to `sink` with its number (from 1) as soon as it is made. Returns
/// the warnings (also repeated in each file's [`Output`]), without repeats.
pub fn split_every(
    doc: &Document,
    every: usize,
    sink: &mut dyn FnMut(usize, Output) -> Result<()>,
) -> Result<Vec<Warning>> {
    if every == 0 {
        return Err(Error::Invalid("the number of pages per file must be at least 1".to_string()));
    }
    let loaded = [load(doc)?];
    let count = loaded.first().map_or(0, |d| d.pages.len());
    if count == 0 {
        return Err(Error::Invalid("the document has no pages".to_string()));
    }
    // Every file takes the shared fonts and resources again: parse each of
    // those objects once, not once per file.
    let _cache = ObjectCacheGuard::new(doc);
    let mut warnings: Vec<Warning> = Vec::new();
    for (i, start) in (0..count).step_by(every).enumerate() {
        let end = start.saturating_add(every).min(count);
        let picks: Vec<Pick> = (start..end).map(|page| Pick { doc: 0, page, rotate_by: 0 }).collect();
        let out = build(&loaded, &picks, true)?;
        for w in &out.warnings {
            if !warnings.contains(w) {
                warnings.push(w.clone());
            }
        }
        sink(i + 1, out)?;
    }
    Ok(warnings)
}

// --- merge --------------------------------------------------------------------------

/// What a merged-in file loses: the document-level items of its catalog that
/// are not carried over, named for a person.
fn lost_items(doc: &Document) -> Vec<String> {
    let Ok(catalog) = doc.catalog() else {
        return Vec::new();
    };
    let present = |key: &str| catalog.get(key).and_then(|o| doc.resolve(o).ok());
    let mut lost: Vec<String> = Vec::new();
    let mut note = |what: &str| {
        if !lost.iter().any(|l| l == what) {
            lost.push(what.to_string());
        }
    };
    if let Some(outlines) = present("Outlines")
        && outlines.as_dict().is_some_and(|d| d.contains_key("First"))
    {
        note("bookmarks");
    }
    if let Some(form) = present("AcroForm")
        && let Some(fields) = form.as_dict().and_then(|d| d.get("Fields")).and_then(|f| doc.resolve(f).ok())
        && fields.as_array().is_some_and(|a| !a.is_empty())
    {
        note("form fields");
    }
    if let Some(names) = present("Names") {
        for (key, _) in names.as_dict().map(|d| d.iter().collect::<Vec<_>>()).unwrap_or_default() {
            match key.as_bytes() {
                b"EmbeddedFiles" => note("embedded files"),
                b"Dests" => note("named destinations"),
                b"JavaScript" => note("JavaScript"),
                other => note(&format!("name tree /{}", String::from_utf8_lossy(other))),
            }
        }
    }
    for (key, what) in [
        ("Dests", "named destinations"),
        ("StructTreeRoot", "structure tree"),
        ("OCProperties", "layer (optional content) settings"),
        ("PageLabels", "page labels"),
        ("Threads", "article threads"),
        ("OpenAction", "open action"),
        ("AA", "document actions"),
        ("Collection", "portfolio settings"),
        ("Perms", "permissions"),
    ] {
        if present(key).is_some() {
            note(what);
        }
    }
    lost
}

/// Refuse a merge of more than `limit` pages in all.
fn check_total_pages(total: usize, limit: usize) -> Result<()> {
    if total > limit {
        return Err(Error::Limit(format!("the files together have {total} pages, more than the {limit} allowed")));
    }
    Ok(())
}

/// Join the files in order. The first file is the base: its catalog (bookmarks,
/// forms, names ...) is kept. The pages of the others are appended with their
/// resources and annotations; their own document-level items are not merged,
/// and one warning per file lists exactly which were dropped.
pub fn merge(inputs: &[Input<'_>]) -> Result<Output> {
    if inputs.is_empty() {
        return Err(Error::Invalid("nothing to merge".to_string()));
    }
    let mut docs = Vec::with_capacity(inputs.len());
    for input in inputs {
        if input.doc.is_locked() {
            return Err(Error::Invalid(format!("{}: the file is encrypted and needs a password", input.name)));
        }
        docs.push(load(input.doc)?);
    }
    // Each file is within the limit for pages; all of them together are too.
    let total = docs.iter().fold(0usize, |acc, d| acc.saturating_add(d.pages.len()));
    check_total_pages(total, crate::document::MAX_PAGES)?;
    let mut picks = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        picks.extend(all_picks(i, d.pages.len()));
    }
    let mut out = build(&docs, &picks, true)?;
    // The first file's encryption is the output's (qpdf does the same). If it
    // has none, the output has none, and the encrypted files that came after
    // it lose theirs.
    let first_is_encrypted = inputs.first().is_some_and(|i| i.doc.is_encrypted());
    if !first_is_encrypted {
        for input in inputs.iter().skip(1).filter(|i| i.doc.is_encrypted()) {
            out.warnings.push(Warning(format!(
                "{}: its encryption was not carried over; the output is not encrypted because the first file is not",
                input.name
            )));
        }
    }
    for input in inputs.iter().skip(1) {
        let lost = lost_items(input.doc);
        if !lost.is_empty() {
            out.warnings.push(Warning(format!(
                "{}: only the pages were merged; not carried over: {}",
                input.name,
                lost.join(", ")
            )));
        }
    }
    Ok(out)
}

// --- decrypt ----------------------------------------------------------------------------

/// A copy of the document without its encryption. Stricter than qpdf: only for
/// a file that was opened with the owner password, or whose permissions allow
/// everything; anything else would be taking the author's restrictions off a
/// file the user holds only with limited rights. `password` is the one the
/// user gave, if any (the empty password is always counted too): whether it is
/// the owner's is looked at here. The file must be encrypted and open
/// ([`Error::PasswordRequired`] otherwise).
pub fn decrypt(doc: &Document, password: &str) -> Result<Output> {
    let Some(encryption) = doc.encryption() else {
        return Err(Error::Invalid("the file is not encrypted".to_string()));
    };
    if doc.is_locked() {
        return Err(Error::PasswordRequired);
    }
    // The owner is whoever has the owner password: it opened the file, or it
    // is what was given, or it is the empty one (an owner with no password).
    let owner = encryption.opened.is_some_and(|a| a.kind == PasswordKind::Owner)
        || doc.security().is_some_and(|s| {
            (!password.is_empty() && s.is_owner_password(password)) || s.is_owner_password("")
        });
    if !owner && !encryption.permissions.allows_everything() {
        let denied: Vec<&str> =
            encryption.permissions.list().iter().filter(|(_, allowed)| !allowed).map(|(what, _)| *what).collect();
        return Err(Error::Invalid(format!(
            "the encryption is not removed: the file was opened with the user password and its author did not allow: {}. Give the owner password with --password to remove it",
            denied.join(", ")
        )));
    }
    let d = load(doc)?;
    let picks: Vec<Pick> = all_picks(0, d.pages.len()).collect();
    build(&[d], &picks, false)
}

// --- images to PDF ---------------------------------------------------------------------

/// One image file: its name (for messages) and bytes.
pub struct ImageInput<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

/// An image file as [`images_to_pdf_with`] asks for it: read when it is its
/// turn, and dropped before the next one is read.
pub struct LoadedImage<'a> {
    pub name: String,
    pub data: std::borrow::Cow<'a, [u8]>,
}

/// A number for a content stream: short, never an exponent, no negative zero.
fn content_number(v: f64) -> String {
    let rounded = (v * 100_000.0).round() / 100_000.0;
    if rounded == 0.0 { "0".to_string() } else { format!("{rounded}") }
}

/// One page per image, in order. JPEG files are embedded as they are
/// (`DCTDecode`); PNG files are decoded and stored with Flate, transparency as
/// a soft mask. See [`PageMode`] for the page size.
pub fn images_to_pdf(images: &[ImageInput<'_>], mode: PageMode) -> Result<Output> {
    images_to_pdf_with(
        images.len(),
        &mut |i| {
            let img = images.get(i).ok_or_else(|| internal("unknown image"))?;
            Ok(LoadedImage { name: img.name.to_string(), data: std::borrow::Cow::Borrowed(img.data) })
        },
        mode,
    )
}

/// [`images_to_pdf`] for `count` images that are fetched one at a time with
/// `load` (called with 0, 1, ... in order): only the image being worked on is
/// in memory next to the output.
pub fn images_to_pdf_with<'a>(
    count: usize,
    load: &mut dyn FnMut(usize) -> Result<LoadedImage<'a>>,
    mode: PageMode,
) -> Result<Output> {
    if count == 0 {
        return Err(Error::Invalid("no images given".to_string()));
    }
    // SMask needs PDF 1.4 (11.6.5.2).
    let mut b = Builder::new((1, 4));
    let catalog_ref = b.reserve()?;
    let pages_ref = b.reserve()?;
    let mut kids = Vec::with_capacity(count);
    for index in 0..count {
        let img = load(index)?;
        let named = |e: Error| match e {
            Error::Invalid(m) => Error::Invalid(format!("{}: {m}", img.name)),
            other => other,
        };
        let data: &[u8] = &img.data;
        let (xobject, placement) = match image::sniff(data) {
            Some(Format::Jpeg) => {
                let info = image::parse_jpeg(data).map_err(named)?;
                let xobject = image::add_jpeg(&mut b, data, &info)?;
                (xobject, image::place(info.width, info.height, info.orientation, info.dpi, mode))
            }
            Some(Format::Png) => {
                let png = image::decode_png(data).map_err(named)?;
                let (width, height, dpi) = (png.width, png.height, png.dpi);
                let xobject = image::add_png(&mut b, png)?;
                (xobject, image::place(width, height, 1, dpi, mode))
            }
            None => {
                return Err(Error::Invalid(format!("{}: not a JPEG or PNG file", img.name)));
            }
        };
        let m = placement.matrix.map(content_number).join(" ");
        let content = format!("q {m} cm /Im0 Do Q\n");
        let contents = b.add_flate_stream(Dict::new(), content.as_bytes())?;

        let mut xobjects = Dict::new();
        xobjects.set("Im0", Object::Ref(xobject));
        let mut resources = Dict::new();
        resources.set("XObject", Object::Dict(xobjects));
        let mut page = Dict::new();
        page.set("Type", Object::from("Page"));
        page.set("Parent", Object::Ref(pages_ref));
        page.set(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(placement.page_width),
                Object::Real(placement.page_height),
            ]),
        );
        page.set("Resources", Object::Dict(resources));
        page.set("Contents", Object::Ref(contents));
        kids.push(Object::Ref(b.add(&Object::Dict(page))?));
    }
    let mut tree = Dict::new();
    tree.set("Type", Object::from("Pages"));
    let kids_len = kids.len();
    tree.set("Kids", Object::Array(kids));
    tree.set("Count", Object::Integer(i64::try_from(kids_len).unwrap_or(i64::MAX)));
    b.put(pages_ref, &Object::Dict(tree))?;
    let mut catalog = Dict::new();
    catalog.set("Type", Object::from("Catalog"));
    catalog.set("Pages", Object::Ref(pages_ref));
    b.put(catalog_ref, &Object::Dict(catalog))?;
    let (data, warnings) = b.finish(catalog_ref, None)?;
    Ok(Output { data, warnings, pages: kids_len })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::find_bytes;
    use crate::testutil::*;

    fn open(bytes: Vec<u8>) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        find_bytes(haystack, needle.as_bytes(), 0).is_some()
    }

    /// Three pages with links, bookmarks, a label tree, a page-tree marker, an
    /// orphan object and a bead.
    ///   page 1 (obj 3): content "PAGE-ONE", links to page 3 (obj 8) and page 2 (obj 9), /B bead
    ///   page 2 (obj 4): content "PAGE-TWO"
    ///   page 3 (obj 5): content "PAGE-THREE-REMOVED"
    fn three_pages() -> Vec<u8> {
        let mut b = PdfBuilder::with_header("%PDF-1.5\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
        b.obj(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /PageLabels << /Nums [0 << /S /D >>] >> \
             /Dests << /Three [5 0 R /Fit] >> /Threads [20 0 R] /Lang (en) >>",
        );
        b.obj(
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 200 100] /Rotate 90 \
             /Resources 6 0 R /PageTreeMarker (PAGETREE-MARKER) >>",
        );
        b.obj(
            3,
            "<< /Type /Page /Parent 2 0 R /Contents 7 0 R /Annots [8 0 R 9 0 R] /B [21 0 R] >>",
        );
        b.obj(4, "<< /Type /Page /Parent 2 0 R /Contents 11 0 R /Rotate 0 >>");
        b.obj(5, "<< /Type /Page /Parent 2 0 R /Contents 12 0 R >>");
        b.obj(6, "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>");
        b.stream_obj(7, "", b"PAGE-ONE");
        b.obj(8, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /P 3 0 R /Dest [5 0 R /Fit] >>");
        b.obj(9, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /P 3 0 R /Dest [4 0 R /Fit] >>");
        b.obj(10, "<< /Type /Outlines /First 13 0 R /Last 14 0 R /Count 2 >>");
        b.stream_obj(11, "", b"PAGE-TWO");
        b.stream_obj(12, "", b"PAGE-THREE-REMOVED");
        b.obj(13, "<< /Title (To page 2) /Parent 10 0 R /Next 14 0 R /Dest [4 0 R /Fit] >>");
        b.obj(14, "<< /Title (To page 3) /Parent 10 0 R /Prev 13 0 R /Dest [5 0 R /Fit] >>");
        b.obj(15, "<< /Orphan (ORPHAN-MARKER) >>");
        b.obj(20, "<< /F 21 0 R >>");
        b.obj(21, "<< /T 20 0 R /N 21 0 R /V 21 0 R /P 3 0 R /R [0 0 5 5] >>");
        b.obj(30, "<< /Title (The Title) /Producer (test) >>");
        b.finish_classic(31, "/Root 1 0 R /Info 30 0 R")
    }

    fn obj(doc: &Document, r: ObjRef) -> Dict {
        doc.get(r).unwrap().as_dict().unwrap().clone()
    }

    fn page_dicts(doc: &Document) -> Vec<Dict> {
        doc.pages().unwrap().into_iter().map(|p| (*p.dict).clone()).collect()
    }

    // --- page ranges ------------------------------------------------------------

    #[test]
    fn page_ranges() {
        assert_eq!(parse_page_ranges("1-3,5", 10).unwrap(), vec![0, 1, 2, 4]);
        assert_eq!(parse_page_ranges("8-", 10).unwrap(), vec![7, 8, 9]);
        assert_eq!(parse_page_ranges("10", 10).unwrap(), vec![9]);
        assert_eq!(parse_page_ranges(" 2 , 4 - 5 ", 10).unwrap(), vec![1, 3, 4]);
        // Order and repeats are kept.
        assert_eq!(parse_page_ranges("3,1,3", 5).unwrap(), vec![2, 0, 2]);
        assert_eq!(parse_page_ranges("2-2", 5).unwrap(), vec![1]);
        for bad in [
            "", ",", "1,,2", "0", "0-3", "11", "1-11", "3-1", "x", "1-x", "-3", "1--3", "+2", "1.5", "99999999999999999999",
            "1-3,", "5-", // 5- on a 4-page document
        ] {
            let pages = if bad == "5-" { 4 } else { 10 };
            assert!(matches!(parse_page_ranges(bad, pages), Err(Error::Invalid(_))), "{bad:?}");
        }
        // Out of range names the page and the size.
        let Err(Error::Invalid(m)) = parse_page_ranges("12", 10) else { panic!() };
        assert!(m.contains("12") && m.contains("10 pages"), "{m}");
        let Err(Error::Invalid(m)) = parse_page_ranges("2", 1) else { panic!() };
        assert!(m.contains("1 page") && !m.contains("1 pages"), "{m}");
        assert!(parse_page_ranges("1", 0).is_err());
    }

    #[test]
    fn chunk_counts() {
        assert_eq!(chunk_count(10, 3), 4);
        assert_eq!(chunk_count(9, 3), 3);
        assert_eq!(chunk_count(1, 100), 1);
        assert_eq!(chunk_count(5, 0), 0);
    }

    // --- copy / split / delete / rotate -----------------------------------------------

    #[test]
    fn copy_rebuilds_a_flat_tree_and_keeps_the_catalog() {
        let src = open(three_pages());
        let out = copy_all(&src).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let doc = open(out.data.clone());
        assert!(!doc.was_repaired());
        assert_eq!(doc.version(), (1, 5));
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 3);

        // The tree is one node, with /Count, and every page points back to it.
        let catalog = doc.catalog().unwrap();
        let root = catalog.get("Pages").and_then(Object::as_obj_ref).unwrap();
        let tree = obj(&doc, root);
        assert_eq!(tree.get_int("Count"), Some(3));
        assert_eq!(tree.get("Kids").and_then(Object::as_array).unwrap().len(), 3);
        for p in &pages {
            let raw = obj(&doc, p.obj_ref);
            assert_eq!(raw.get("Parent"), Some(&Object::Ref(root)));
            assert_eq!(raw.get_name("Type").unwrap(), &Name::from("Page"));
        }
        // Inherited attributes were written onto the pages themselves.
        let first = obj(&doc, pages[0].obj_ref);
        assert!(first.contains_key("MediaBox") && first.contains_key("Resources") && first.contains_key("Rotate"));
        assert_eq!(pages[0].media_box(), Some([0.0, 0.0, 200.0, 100.0]));
        assert_eq!(pages[0].rotate(), 90);
        assert_eq!(pages[1].rotate(), 0); // its own /Rotate 0 beats the inherited 90
        // The page tree node's own entries are not copied anywhere.
        assert!(!contains(&out.data, "PAGETREE-MARKER"));
        // Unreferenced objects are gone; a page's /B is dropped.
        assert!(!contains(&out.data, "ORPHAN-MARKER"));
        assert!(!first.contains_key("B"));
        // Catalog items survive: outlines, page labels, named destinations, /Lang.
        for key in ["Outlines", "PageLabels", "Dests", "Lang", "Threads"] {
            assert!(catalog.contains_key(key), "{key}");
        }
        // The info dictionary too.
        let info = doc.info().unwrap().unwrap();
        assert_eq!(info.get("Title"), Some(&Object::String(crate::object::PdfString::literal("The Title"))));
        // Bookmarks point at the new pages.
        let outlines = doc.resolve(catalog.get("Outlines").unwrap()).unwrap();
        let first_item = obj(&doc, outlines.as_dict().unwrap().get("First").and_then(Object::as_obj_ref).unwrap());
        let dest = first_item.get("Dest").and_then(Object::as_array).unwrap();
        assert_eq!(dest.first(), Some(&Object::Ref(pages[1].obj_ref)));
        // The parent link inside the outline tree is still followed.
        assert_eq!(first_item.get("Parent"), catalog.get("Outlines"));
        // Content streams are byte for byte the same.
        for (old, new) in src.pages().unwrap().iter().zip(&pages) {
            assert_eq!(content_bytes(&src, old), content_bytes(&doc, new));
        }
    }

    fn content_bytes(doc: &Document, page: &Page) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(Object::Ref(r)) = page.dict.get("Contents")
            && let Object::Stream(s) = doc.get(*r).unwrap()
        {
            out.extend_from_slice(&s.data);
        }
        out
    }

    #[test]
    fn deleting_a_page_turns_links_to_it_into_null_and_drops_what_only_it_used() {
        let src = open(three_pages());
        let out = delete_pages(&src, &[2]).unwrap();
        // The removed page, its content and the old page tree are gone.
        assert!(!contains(&out.data, "PAGE-THREE-REMOVED"));
        assert!(!contains(&out.data, "PAGETREE-MARKER"));
        assert!(contains(&out.data, "PAGE-ONE") && contains(&out.data, "PAGE-TWO"));
        assert!(!contains(&out.data, "To page 3"));
        let doc = open(out.data.clone());
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 2);
        // A link to the removed page: the page slot is null; the other link is fine.
        let annots = pages[0].dict.get("Annots").and_then(Object::as_array).unwrap().to_vec();
        let dest_of = |i: usize| {
            let a = obj(&doc, annots[i].as_obj_ref().unwrap());
            a.get("Dest").and_then(Object::as_array).unwrap().to_vec()
        };
        assert_eq!(dest_of(0), vec![Object::Null, Object::from("Fit")]);
        assert_eq!(dest_of(1), vec![Object::Ref(pages[1].obj_ref), Object::from("Fit")]);
        // The annotation's /P still names the page it is on.
        let a = obj(&doc, annots[0].as_obj_ref().unwrap());
        assert_eq!(a.get("P"), Some(&Object::Ref(pages[0].obj_ref)));
        // The bookmark and the named destination for the removed page are gone;
        // the other bookmark stays, alone, with its links and count made right.
        let catalog = doc.catalog().unwrap();
        assert!(!catalog.contains_key("Dests"));
        let outlines = doc.resolve(catalog.get("Outlines").unwrap()).unwrap();
        let outlines = outlines.as_dict().unwrap();
        assert_eq!(outlines.get_int("Count"), Some(1));
        let first = outlines.get("First").and_then(Object::as_obj_ref).unwrap();
        assert_eq!(outlines.get("Last").and_then(Object::as_obj_ref), Some(first));
        let item = obj(&doc, first);
        assert_eq!(item.get("Dest"), Some(&Object::Array(vec![Object::Ref(pages[1].obj_ref), Object::from("Fit")])));
        assert!(!item.contains_key("Next") && !item.contains_key("Prev"));
        // Beads were dropped from the page; the thread list went with the page.
        assert!(!catalog.contains_key("Threads"));
        assert!(out.warnings.iter().any(|w| w.0.contains("article threads")), "{:?}", out.warnings);
    }

    #[test]
    fn page_labels_survive_only_while_the_first_pages_stay_in_place() {
        let src = open(three_pages());
        // Keeping pages 1-2: labels are still right.
        let out = extract_pages(&src, &[0, 1]).unwrap();
        assert!(open(out.data).catalog().unwrap().contains_key("PageLabels"));
        // Dropping page 1: they would be shifted.
        let out = delete_pages(&src, &[0]).unwrap();
        assert!(!open(out.data.clone()).catalog().unwrap().contains_key("PageLabels"));
        assert!(out.warnings.iter().any(|w| w.0.contains("page labels")));
        // Reordering too.
        let out = extract_pages(&src, &[1, 0, 2]).unwrap();
        assert!(!open(out.data).catalog().unwrap().contains_key("PageLabels"));
    }

    #[test]
    fn pages_without_the_required_resources_and_media_box_get_defaults() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /Resources 5 /MediaBox [0 0 (a) (b)] >>");
        let doc = open(b.finish_classic(5, "/Root 1 0 R"));
        let out = open(copy_all(&doc).unwrap().data);
        for page in out.pages().unwrap() {
            assert_eq!(page.media_box(), Some([0.0, 0.0, 612.0, 792.0]));
            assert_eq!(page.dict.get("Resources"), Some(&Object::Dict(Dict::new())));
        }
        // A page that has them (or inherits them) keeps its own.
        let src = open(three_pages());
        let out = open(copy_all(&src).unwrap().data);
        assert_eq!(out.pages().unwrap()[0].media_box(), Some([0.0, 0.0, 200.0, 100.0]));
    }

    #[test]
    fn a_document_without_pages_copies_to_one_without_pages() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let doc = open(b.finish_classic(3, "/Root 1 0 R"));
        let out = copy_all(&doc).unwrap();
        assert_eq!(out.pages, 0);
        assert_eq!(open(out.data).page_count().unwrap(), 0);
        assert!(extract_pages(&doc, &[]).is_err());
        assert!(rotate_pages(&doc, &[], 90).is_ok());
        assert!(delete_pages(&doc, &[0]).is_err());
    }

    #[test]
    fn extract_orders_and_repeats_pages() {
        let src = open(three_pages());
        let out = extract_pages(&src, &[2, 0, 0]).unwrap();
        let doc = open(out.data);
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 3);
        let contents: Vec<Vec<u8>> = pages.iter().map(|p| content_bytes(&doc, p)).collect();
        assert_eq!(contents[0], b"PAGE-THREE-REMOVED");
        assert_eq!(contents[1], b"PAGE-ONE");
        assert_eq!(contents[2], b"PAGE-ONE");
        // Distinct page objects, even for the repeated page.
        assert_ne!(pages[1].obj_ref, pages[2].obj_ref);
        assert!(matches!(extract_pages(&src, &[]), Err(Error::Invalid(_))));
        assert!(matches!(extract_pages(&src, &[7]), Err(Error::Invalid(_))));
    }

    #[test]
    fn delete_every_page_is_refused() {
        let src = open(three_pages());
        assert!(matches!(delete_pages(&src, &[0, 1, 2]), Err(Error::Invalid(_))));
        assert_eq!(open(delete_pages(&src, &[1, 1]).unwrap().data).page_count().unwrap(), 2);
    }

    #[test]
    fn rotation_adds_normalises_and_touches_only_the_chosen_pages() {
        let src = open(three_pages());
        // Page 1 inherits 90, page 2 has 0, page 3 inherits 90.
        let out = rotate_pages(&src, &[0, 1], 270).unwrap();
        let doc = open(out.data);
        let pages = doc.pages().unwrap();
        assert_eq!(pages[0].rotate(), 0); // 90 + 270
        assert_eq!(pages[1].rotate(), 270); // 0 + 270
        assert_eq!(pages[2].rotate(), 90); // untouched
        // Zero is written as no /Rotate at all, and the page then says 0.
        assert!(!page_dicts(&doc).first().unwrap().contains_key("Rotate"));
        // Negative angles and full turns.
        let doc = open(rotate_pages(&src, &[1], -90).unwrap().data);
        assert_eq!(doc.pages().unwrap()[1].rotate(), 270);
        let doc = open(rotate_pages(&src, &[1], 360).unwrap().data);
        assert_eq!(doc.pages().unwrap()[1].rotate(), 0);
        let doc = open(rotate_pages(&src, &[1], 180).unwrap().data);
        assert_eq!(doc.pages().unwrap()[1].rotate(), 180);
        let doc = open(rotate_pages(&src, &[0, 1, 2], 90).unwrap().data);
        let rots: Vec<i64> = doc.pages().unwrap().iter().map(Page::rotate).collect();
        assert_eq!(rots, vec![180, 90, 180]);
        // Only multiples of 90.
        for bad in [45, 1, -30, 91] {
            assert!(matches!(rotate_pages(&src, &[0], bad), Err(Error::Invalid(_))), "{bad}");
        }
        // Rotating by 0 is a plain copy.
        assert!(rotate_pages(&src, &[0], 0).is_ok());
    }

    #[test]
    fn split_every_makes_numbered_chunks() {
        let src = open(three_pages());
        let mut got: Vec<(usize, usize)> = Vec::new();
        let warnings = split_every(&src, 2, &mut |n, out| {
            assert_eq!(out.pages, open(out.data.clone()).page_count().unwrap());
            got.push((n, open(out.data).page_count().unwrap()));
            Ok(())
        })
        .unwrap();
        assert_eq!(got, vec![(1, 2), (2, 1)]);
        // The second file lost the labels (and, not having every page, the threads),
        // and the warning is listed once however many files had it.
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(split_every(&src, 0, &mut |_, _| Ok(())).is_err());
        // A sink error stops the run.
        let mut calls = 0;
        let r = split_every(&src, 1, &mut |_, _| {
            calls += 1;
            Err(Error::Invalid("stop".to_string()))
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn version_is_the_highest_input_version() {
        let src = open(three_pages());
        assert_eq!(open(copy_all(&src).unwrap().data).version(), (1, 5));
        let newer = {
            let mut b = PdfBuilder::with_header("%PDF-1.7\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
            b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
            b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
            b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 5 5] >>");
            open(b.finish_classic(4, "/Root 1 0 R"))
        };
        let a = Input { name: "a.pdf", doc: &src };
        let b = Input { name: "b.pdf", doc: &newer };
        assert_eq!(open(merge(&[a, b]).unwrap().data).version(), (1, 7));
        let a = Input { name: "a.pdf", doc: &src };
        let b = Input { name: "b.pdf", doc: &newer };
        assert_eq!(open(merge(&[b, a]).unwrap().data).version(), (1, 7));
    }

    #[test]
    fn a_catalog_version_is_not_left_older_than_the_header() {
        let mut b = PdfBuilder::with_header("%PDF-1.4\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Version /1.5 >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 5 5] >>");
        let base = open(b.finish_classic(4, "/Root 1 0 R"));
        let newer = {
            let mut b = PdfBuilder::with_header("%PDF-1.7\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
            b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
            b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
            b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 5 5] >>");
            open(b.finish_classic(4, "/Root 1 0 R"))
        };
        let out = merge(&[Input { name: "a", doc: &base }, Input { name: "b", doc: &newer }]).unwrap();
        let doc = open(out.data);
        assert_eq!(doc.version(), (1, 7));
        assert_eq!(doc.catalog().unwrap().get_name("Version").unwrap(), &Name::from("1.7"));
    }

    #[test]
    fn locked_input_needs_a_password() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 5 5] >>");
        b.obj(4, &format!("<< /Filter /Standard /V 1 /R 2 /O <{}> /U <{}> /P -4 >>", "00".repeat(32), "11".repeat(32)));
        let doc = open(b.finish_classic(5, "/Root 1 0 R /Encrypt 4 0 R /ID [<aa> <bb>]"));
        assert!(matches!(copy_all(&doc), Err(Error::PasswordRequired)));
        assert!(matches!(extract_pages(&doc, &[0]), Err(Error::PasswordRequired)));
        assert!(matches!(delete_pages(&doc, &[]), Err(Error::PasswordRequired)));
        assert!(matches!(rotate_pages(&doc, &[0], 90), Err(Error::PasswordRequired)));
        assert!(matches!(split_every(&doc, 1, &mut |_, _| Ok(())), Err(Error::PasswordRequired)));
        assert!(matches!(decrypt(&doc, ""), Err(Error::PasswordRequired)));
        let plain = open(three_pages());
        let r = merge(&[Input { name: "plain", doc: &plain }, Input { name: "locked.pdf", doc: &doc }]);
        assert!(matches!(r, Err(Error::Invalid(m)) if m.contains("locked.pdf") && m.contains("needs a password")));
    }

    #[test]
    fn a_damaged_object_becomes_null_with_a_warning_instead_of_failing() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 5 5] /Contents 4 0 R /Foo 5 0 R >>");
        b.raw_obj(4, b"4 0 obj\n<< /Length 5 /Broken [ ( unclosed\nendobj\n");
        b.obj(5, "(fine)");
        let doc = open(b.finish_classic(6, "/Root 1 0 R"));
        let out = copy_all(&doc).unwrap();
        assert!(out.warnings.iter().any(|w| w.0.contains("could not be read")), "{:?}", out.warnings);
        let copy = open(out.data);
        let page = copy.pages().unwrap().remove(0);
        assert!(!page.dict.contains_key("Contents"));
        assert!(page.dict.contains_key("Foo"));
    }

    // --- merge ----------------------------------------------------------------------

    /// A one-page file with everything a later merge input can lose.
    fn rich_second_file() -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(
            1,
            "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /AcroForm << /Fields [11 0 R] >> \
             /Names << /EmbeddedFiles << /Names [] >> /Dests << /Names [] >> >> /StructTreeRoot 12 0 R \
             /OCProperties << /OCGs [] >> /OpenAction [3 0 R /Fit] >>",
        );
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 300 300] >>");
        b.obj(
            3,
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots [5 0 R] /Resources << /XObject << /X 6 0 R >> >> >>",
        );
        b.stream_obj(4, "", b"SECOND-PAGE-CONTENT");
        b.obj(5, "<< /Type /Annot /Subtype /Link /Rect [0 0 9 9] /P 3 0 R /Dest [3 0 R /Fit] >>");
        b.stream_obj(6, "/Type /XObject /Subtype /Form /BBox [0 0 1 1]", b"SECOND-FORM");
        b.obj(10, "<< /Type /Outlines /First 13 0 R /Last 13 0 R /Count 1 >>");
        b.obj(11, "<< /FT /Tx /T (field) /Rect [0 0 1 1] >>");
        b.obj(12, "<< /Type /StructTreeRoot >>");
        b.obj(13, "<< /Title (Bookmark) /Parent 10 0 R /Dest [3 0 R /Fit] >>");
        b.finish_classic(14, "/Root 1 0 R")
    }

    #[test]
    fn merge_appends_pages_remaps_annotations_and_warns_about_what_is_dropped() {
        let first = open(three_pages());
        let second = open(rich_second_file());
        let out = merge(&[Input { name: "one.pdf", doc: &first }, Input { name: "two.pdf", doc: &second }]).unwrap();
        // Exactly one warning, for the second file, naming exactly what was lost.
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        let w = &out.warnings[0].0;
        assert!(w.starts_with("two.pdf:"), "{w}");
        for lost in ["bookmarks", "form fields", "embedded files", "named destinations", "structure tree", "layer", "open action"] {
            assert!(w.contains(lost), "{w} should mention {lost}");
        }
        assert!(!w.contains("page labels") && !w.contains("JavaScript"), "{w}");

        let doc = open(out.data.clone());
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 4);
        // The base keeps its catalog; the second file's items are not there.
        let catalog = doc.catalog().unwrap();
        assert!(catalog.contains_key("Outlines") && catalog.contains_key("PageLabels"));
        assert!(!catalog.contains_key("AcroForm") && !catalog.contains_key("StructTreeRoot"));
        assert!(!catalog.contains_key("OpenAction"));
        assert!(!contains(&out.data, "Bookmark"));
        // The appended page has its own box, content, resources and annotation.
        let last = &pages[3];
        assert_eq!(last.media_box(), Some([0.0, 0.0, 300.0, 300.0]));
        assert_eq!(content_bytes(&doc, last), b"SECOND-PAGE-CONTENT");
        let xobj = doc.resolve(last.dict.get("Resources").unwrap()).unwrap();
        let xobj = xobj.as_dict().unwrap().get("XObject").unwrap().clone();
        let form = xobj.as_dict().unwrap().get("X").and_then(Object::as_obj_ref).unwrap();
        let Object::Stream(s) = doc.get(form).unwrap() else { panic!("not a stream") };
        assert_eq!(s.data, b"SECOND-FORM");
        // The annotation's /P and its link destination point at the new page.
        let annot = obj(&doc, last.dict.get("Annots").and_then(Object::as_array).unwrap()[0].as_obj_ref().unwrap());
        assert_eq!(annot.get("P"), Some(&Object::Ref(last.obj_ref)));
        assert_eq!(
            annot.get("Dest"),
            Some(&Object::Array(vec![Object::Ref(last.obj_ref), Object::from("Fit")]))
        );
        // The first file's pages did not change.
        assert_eq!(content_bytes(&doc, &pages[0]), b"PAGE-ONE");
        // Page labels: the base's cover its own pages in place, so they stay.
        assert!(out.warnings.iter().all(|w| !w.0.contains("labels")));
    }

    #[test]
    fn merge_a_file_with_itself_copies_everything_separately() {
        let one = open(rich_second_file());
        let out = merge(&[Input { name: "a", doc: &one }, Input { name: "a again", doc: &one }]).unwrap();
        let doc = open(out.data);
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 2);
        assert_ne!(pages[0].obj_ref, pages[1].obj_ref);
        let annots: Vec<ObjRef> = pages
            .iter()
            .map(|p| p.dict.get("Annots").and_then(Object::as_array).unwrap()[0].as_obj_ref().unwrap())
            .collect();
        assert_ne!(annots[0], annots[1]);
        for (p, a) in pages.iter().zip(annots) {
            assert_eq!(obj(&doc, a).get("P"), Some(&Object::Ref(p.obj_ref)));
        }
    }

    #[test]
    fn merge_needs_input_and_a_clean_file_has_no_warnings() {
        assert!(merge(&[]).is_err());
        let a = open(sample_pdf());
        let b = open(sample_objstm_pdf());
        let out = merge(&[Input { name: "a", doc: &a }, Input { name: "b", doc: &b }]).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let doc = open(out.data);
        assert_eq!(doc.page_count().unwrap(), 2);
        assert_eq!(doc.version(), (1, 5));
        // A file read through object streams is written without them.
        assert!(!doc.uses_object_streams() && !doc.uses_xref_streams());
    }

    // --- images -----------------------------------------------------------------------

    fn tiny_jpeg(w: u16, h: u16, extra: &[u8]) -> Vec<u8> {
        // Header segments only plus a short scan: enough for the header parser and embedding.
        let mut out = vec![0xFF, 0xD8];
        out.extend_from_slice(extra);
        out.extend_from_slice(&[0xFF, 0xC0, 0x00, 17, 8]);
        out.extend_from_slice(&h.to_be_bytes());
        out.extend_from_slice(&w.to_be_bytes());
        out.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
        out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 1, 2, 3, 4, 0xFF, 0xD9]);
        out
    }

    fn exif_segment(orientation: u8) -> Vec<u8> {
        let mut seg = vec![0xFF, 0xE1, 0x00, 0x1E];
        seg.extend_from_slice(b"Exif\0\0MM\x00\x2A\x00\x00\x00\x08\x00\x01\x01\x12\x00\x03\x00\x00\x00\x01");
        seg.extend_from_slice(&[0, orientation, 0, 0]);
        seg
    }

    #[test]
    fn a_jpeg_is_embedded_untouched_on_an_a4_page() {
        let jpeg = tiny_jpeg(400, 300, &[]);
        let out = images_to_pdf(&[ImageInput { name: "photo.jpg", data: &jpeg }], PageMode::A4).unwrap();
        let doc = open(out.data);
        assert_eq!(doc.version(), (1, 4));
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 1);
        // Landscape A4 for a wide image.
        let mb = pages[0].media_box().unwrap();
        assert!((mb[2] - 841.89).abs() < 1e-9 && (mb[3] - 595.276).abs() < 1e-9, "{mb:?}");
        // The image: same bytes, DCTDecode, RGB.
        let res = doc.resolve(pages[0].dict.get("Resources").unwrap()).unwrap();
        let x = res.as_dict().unwrap().get("XObject").unwrap().as_dict().unwrap().get("Im0").unwrap().clone();
        let Object::Stream(s) = doc.resolve(&x).unwrap() else { panic!("not a stream") };
        assert_eq!(s.data, jpeg);
        assert_eq!(s.dict.get_name("Filter").unwrap(), &Name::from("DCTDecode"));
        assert_eq!(s.dict.get_name("ColorSpace").unwrap(), &Name::from("DeviceRGB"));
        assert_eq!((s.dict.get_int("Width"), s.dict.get_int("Height")), (Some(400), Some(300)));
        assert_eq!(s.dict.get_int("BitsPerComponent"), Some(8));
        assert!(!s.dict.contains_key("Decode"));
        // The content stream draws it filling the width, centred vertically.
        // The height is what limits: 400x300 on 841.89x595.276 scales by 595.276/300.
        let content = content_text(&doc, &pages[0]);
        let nums: Vec<f64> = content.split_whitespace().skip(1).take(6).map(|t| t.parse().unwrap()).collect();
        let scale = 595.276 / 300.0;
        for (got, want) in nums.iter().zip([400.0 * scale, 0.0, 0.0, 595.276, (841.89 - 400.0 * scale) / 2.0, 0.0]) {
            assert!((got - want).abs() < 1e-3, "{content}");
        }
        assert!(content.trim_end().ends_with("cm /Im0 Do Q"), "{content}");
    }

    fn content_text(doc: &Document, page: &Page) -> String {
        let r = page.dict.get("Contents").and_then(Object::as_obj_ref).unwrap();
        let Object::Stream(s) = doc.get(r).unwrap() else { panic!("not a stream") };
        String::from_utf8(doc.decode_stream(&s).unwrap()).unwrap()
    }

    #[test]
    fn exif_orientation_turns_the_image_on_the_page() {
        // Stored 400 wide, 300 high, orientation 6: upright it is 300 wide and 400 high,
        // so the page is portrait.
        let jpeg = tiny_jpeg(400, 300, &exif_segment(6));
        let out = images_to_pdf(&[ImageInput { name: "p.jpg", data: &jpeg }], PageMode::A4).unwrap();
        let doc = open(out.data);
        let page = doc.pages().unwrap().remove(0);
        let mb = page.media_box().unwrap();
        assert!((mb[2] - 595.276).abs() < 1e-9 && (mb[3] - 841.89).abs() < 1e-9, "{mb:?}");
        let content = content_text(&doc, &page);
        let nums: Vec<f64> = content
            .split_whitespace()
            .skip(1)
            .take(6)
            .map(|t| t.parse::<f64>().unwrap())
            .collect();
        // a = 0, d = 0: the matrix is a quarter turn.
        assert_eq!((nums[0], nums[3]), (0.0, 0.0));
        assert!(nums[1] < 0.0 && nums[2] > 0.0, "{nums:?}");
        // Without the Exif segment the same file is landscape.
        let plain = tiny_jpeg(400, 300, &[]);
        let out = images_to_pdf(&[ImageInput { name: "p.jpg", data: &plain }], PageMode::A4).unwrap();
        let mb = open(out.data).pages().unwrap().remove(0).media_box().unwrap();
        assert!(mb[2] > mb[3]);
    }

    #[test]
    fn fit_mode_uses_the_image_size_and_the_file_density() {
        let jpeg = tiny_jpeg(960, 480, &[]);
        let out = images_to_pdf(&[ImageInput { name: "a.jpg", data: &jpeg }], PageMode::Fit).unwrap();
        let mb = open(out.data).pages().unwrap().remove(0).media_box().unwrap();
        assert_eq!(mb, [0.0, 0.0, 720.0, 360.0]);
        // JFIF at 144 dpi: two thirds of that.
        let jfif = [0xFF, 0xE0, 0x00, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 1, 0, 144, 0, 144, 0, 0];
        let jpeg = tiny_jpeg(960, 480, &jfif);
        let out = images_to_pdf(&[ImageInput { name: "a.jpg", data: &jpeg }], PageMode::Fit).unwrap();
        let mb = open(out.data).pages().unwrap().remove(0).media_box().unwrap();
        assert_eq!(mb, [0.0, 0.0, 480.0, 240.0]);
    }

    #[test]
    fn inverted_cmyk_gets_a_decode_array() {
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xEE, 0x00, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 2]);
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 20, 8, 0, 4, 0, 4, 4]);
        for i in 1..=4u8 {
            jpeg.extend_from_slice(&[i, 0x11, 0]);
        }
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 1, 0xFF, 0xD9]);
        let out = images_to_pdf(&[ImageInput { name: "c.jpg", data: &jpeg }], PageMode::A4).unwrap();
        let doc = open(out.data);
        let page = doc.pages().unwrap().remove(0);
        let res = doc.resolve(page.dict.get("Resources").unwrap()).unwrap();
        let x = res.as_dict().unwrap().get("XObject").unwrap().as_dict().unwrap().get("Im0").unwrap().clone();
        let Object::Stream(s) = doc.resolve(&x).unwrap() else { panic!() };
        assert_eq!(s.dict.get_name("ColorSpace").unwrap(), &Name::from("DeviceCMYK"));
        let decode: Vec<i64> =
            s.dict.get("Decode").and_then(Object::as_array).unwrap().iter().map(|o| o.as_int().unwrap()).collect();
        assert_eq!(decode, vec![1, 0, 1, 0, 1, 0, 1, 0]);
    }

    fn make_png(w: u32, h: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(data).unwrap();
        }
        out
    }

    #[test]
    fn png_alpha_becomes_an_smask() {
        let png = make_png(2, 2, png::ColorType::Rgba, &[255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 9, 9, 9, 255]);
        let out = images_to_pdf(&[ImageInput { name: "a.png", data: &png }], PageMode::A4).unwrap();
        let doc = open(out.data);
        let page = doc.pages().unwrap().remove(0);
        let res = doc.resolve(page.dict.get("Resources").unwrap()).unwrap();
        let x = res.as_dict().unwrap().get("XObject").unwrap().as_dict().unwrap().get("Im0").unwrap().clone();
        let Object::Stream(img) = doc.resolve(&x).unwrap() else { panic!() };
        assert_eq!(img.dict.get_name("Filter").unwrap(), &Name::from("FlateDecode"));
        assert_eq!(img.dict.get_name("ColorSpace").unwrap(), &Name::from("DeviceRGB"));
        assert_eq!(doc.decode_stream(&img).unwrap(), vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]);
        let mask = img.dict.get("SMask").cloned().unwrap();
        let Object::Stream(mask) = doc.resolve(&mask).unwrap() else { panic!("SMask is not a stream") };
        assert_eq!(mask.dict.get_name("ColorSpace").unwrap(), &Name::from("DeviceGray"));
        assert_eq!(mask.dict.get_int("BitsPerComponent"), Some(8));
        assert_eq!((mask.dict.get_int("Width"), mask.dict.get_int("Height")), (Some(2), Some(2)));
        assert_eq!(doc.decode_stream(&mask).unwrap(), vec![255, 128, 0, 255]);
        // A PNG without transparency has no mask.
        let opaque = make_png(1, 1, png::ColorType::Grayscale, &[7]);
        let out = images_to_pdf(&[ImageInput { name: "g.png", data: &opaque }], PageMode::A4).unwrap();
        let doc = open(out.data);
        let page = doc.pages().unwrap().remove(0);
        let res = doc.resolve(page.dict.get("Resources").unwrap()).unwrap();
        let x = res.as_dict().unwrap().get("XObject").unwrap().as_dict().unwrap().get("Im0").unwrap().clone();
        let Object::Stream(img) = doc.resolve(&x).unwrap() else { panic!() };
        assert_eq!(img.dict.get_name("ColorSpace").unwrap(), &Name::from("DeviceGray"));
        assert!(!img.dict.contains_key("SMask"));
    }

    #[test]
    fn one_page_per_image_and_errors_name_the_file() {
        let jpeg = tiny_jpeg(10, 20, &[]);
        let png = make_png(1, 1, png::ColorType::Grayscale, &[0]);
        let out = images_to_pdf(
            &[ImageInput { name: "1.jpg", data: &jpeg }, ImageInput { name: "2.png", data: &png }],
            PageMode::A4,
        )
        .unwrap();
        assert_eq!(open(out.data).page_count().unwrap(), 2);
        let Err(Error::Invalid(m)) = images_to_pdf(&[ImageInput { name: "doc.txt", data: b"hello" }], PageMode::A4) else {
            panic!()
        };
        assert!(m.contains("doc.txt"), "{m}");
        let Err(Error::Invalid(m)) =
            images_to_pdf(&[ImageInput { name: "bad.jpg", data: &[0xFF, 0xD8, 0xFF, 0xD9] }], PageMode::A4)
        else {
            panic!()
        };
        assert!(m.contains("bad.jpg"), "{m}");
        let Err(Error::Invalid(m)) =
            images_to_pdf(&[ImageInput { name: "bad.png", data: b"\x89PNG\r\n\x1a\nxxxxxxxx" }], PageMode::A4)
        else {
            panic!()
        };
        assert!(m.contains("bad.png"), "{m}");
        assert!(images_to_pdf(&[], PageMode::A4).is_err());
    }

    // --- named destinations of later files, structure links, shared resources, /ID ----------

    /// A two-page file with a name tree entry `target` for its page `target_page`
    /// (1 or 2) and a link on page 1 for each way of naming a destination.
    fn linked(target_page: u32, with_links: bool, struct_parents: bool) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(
            1,
            &format!(
                "<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [(target) [{} 0 R /Fit]] >> >> \
                 /Dests << /Old [3 0 R /Fit] >> >>",
                2 + target_page
            ),
        );
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 9 9] >>");
        let annots = if with_links { "/Annots [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R]" } else { "" };
        let sp = if struct_parents { "/StructParents 5" } else { "" };
        b.obj(3, &format!("<< /Type /Page /Parent 2 0 R {annots} {sp} >>"));
        b.obj(4, "<< /Type /Page /Parent 2 0 R >>");
        let link = |extra: &str| format!("<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] {extra} /StructParent 7 >>");
        b.obj(5, &link("/Dest (target)"));
        b.obj(6, &link("/A << /S /GoTo /D (target) >>"));
        b.obj(7, &link("/Dest /Old"));
        b.obj(8, &link("/Dest (nothing)"));
        b.obj(9, &link("/A << /S /GoTo /D (nothing) >>"));
        b.obj(10, &link("/A << /S /URI /URI (http://example.com) >>"));
        b.finish_classic(11, "/Root 1 0 R")
    }

    #[test]
    fn named_destinations_of_a_later_file_are_resolved_through_its_own_tree() {
        // Both files call a destination `target`: in the first it is its page
        // 1, in the second its page 2. The second file's links must go to
        // the second file's page 2, not to the first file's page 1.
        let first = open(linked(1, false, false));
        let second = open(linked(2, true, false));
        let out = merge(&[Input { name: "a.pdf", doc: &first }, Input { name: "b.pdf", doc: &second }]).unwrap();
        let doc = open(out.data);
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 4);
        let annots = pages[2].dict.get("Annots").and_then(Object::as_array).unwrap().to_vec();
        assert_eq!(annots.len(), 6);
        let annot = |i: usize| obj(&doc, annots[i].as_obj_ref().unwrap());
        let fit = |page: usize| Object::Array(vec![Object::Ref(pages[page].obj_ref), Object::from("Fit")]);
        // /Dest given as a string, through the name tree.
        assert_eq!(annot(0).get("Dest"), Some(&fit(3)));
        // A go-to action.
        let action = doc.resolve(annot(1).get("A").unwrap()).unwrap();
        assert_eq!(action.as_dict().unwrap().get("D"), Some(&fit(3)));
        // /Dest given as a name, through the old dictionary: page 1 of the second file.
        assert_eq!(annot(2).get("Dest"), Some(&fit(2)));
        // A name nobody knows: no destination, and a go-to action with none is gone.
        assert!(!annot(3).contains_key("Dest"));
        assert!(!annot(4).contains_key("A"));
        // Other actions are left alone.
        let uri = doc.resolve(annot(5).get("A").unwrap()).unwrap();
        assert!(uri.as_dict().unwrap().contains_key("URI"));
        // The first file's own name tree is the output's, and still means its page 1.
        let names = crate::dests::NamedDests::load(&doc).unwrap();
        let target = names.lookup(&doc, b"target", false).unwrap().unwrap();
        assert_eq!(target.first(), Some(&Object::Ref(pages[0].obj_ref)));
        // Merging a file with itself: the second copy's links stay in the second copy.
        let one = open(linked(2, true, false));
        let out = merge(&[Input { name: "a", doc: &one }, Input { name: "a again", doc: &one }]).unwrap();
        let doc = open(out.data);
        let pages = doc.pages().unwrap();
        let second_annots = pages[2].dict.get("Annots").and_then(Object::as_array).unwrap().to_vec();
        let a = obj(&doc, second_annots[0].as_obj_ref().unwrap());
        assert_eq!(a.get("Dest"), Some(&Object::Array(vec![Object::Ref(pages[3].obj_ref), Object::from("Fit")])));
        // And the first copy's links (through the base's tree) are in the first copy.
        let first_annots = pages[0].dict.get("Annots").and_then(Object::as_array).unwrap().to_vec();
        let a = obj(&doc, first_annots[0].as_obj_ref().unwrap());
        assert!(matches!(a.get("Dest"), Some(Object::String(_))), "the base keeps its own, named, links");
    }

    #[test]
    fn structure_links_of_later_files_are_dropped_and_the_bases_are_kept() {
        let first = open(linked(1, true, true));
        let second = open(linked(2, true, true));
        let out = merge(&[Input { name: "a.pdf", doc: &first }, Input { name: "b.pdf", doc: &second }]).unwrap();
        let doc = open(out.data);
        let pages = doc.pages().unwrap();
        assert_eq!(pages[0].dict.get_int("StructParents"), Some(5));
        assert!(!pages[2].dict.contains_key("StructParents"));
        let first_annot = obj(&doc, pages[0].dict.get("Annots").and_then(Object::as_array).unwrap()[0].as_obj_ref().unwrap());
        assert_eq!(first_annot.get_int("StructParent"), Some(7));
        let second_annot = obj(&doc, pages[2].dict.get("Annots").and_then(Object::as_array).unwrap()[0].as_obj_ref().unwrap());
        assert!(!second_annot.contains_key("StructParent"));
    }

    #[test]
    fn a_resources_dictionary_shared_through_the_page_tree_is_written_once() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        let kids: String = (0..50).map(|i| format!("{} 0 R ", 3 + i)).collect();
        b.obj(
            2,
            &format!(
                "<< /Type /Pages /Kids [{kids}] /Count 50 /MediaBox [0 0 9 9] /CropBox [1 1 8 8] /Rotate 90 \
                 /Resources << /Marker (SHARED-RESOURCES-MARKER) /Font << /F1 100 0 R >> >> >>"
            ),
        );
        for i in 0..50 {
            b.obj(3 + i, "<< /Type /Page /Parent 2 0 R >>");
        }
        b.obj(100, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let src = open(b.finish_classic(101, "/Root 1 0 R"));
        for out in [copy_all(&src).unwrap(), rotate_pages(&src, &[0], 90).unwrap(), delete_pages(&src, &[3]).unwrap()] {
            assert_eq!(out.data.windows(24).filter(|w| *w == b"SHARED-RESOURCES-MARKER)").count(), 1);
            let doc = open(out.data);
            let pages = doc.pages().unwrap();
            let resources: Vec<&Object> = pages.iter().map(|p| p.dict.get("Resources").unwrap()).collect();
            assert!(matches!(resources[0], Object::Ref(_)));
            assert!(resources.iter().all(|r| *r == resources[0]), "every page refers to the same object");
            // The boxes and the rotation are still there, on every page.
            assert!(pages.iter().all(|p| p.media_box() == Some([0.0, 0.0, 9.0, 9.0]) && p.crop_box() == Some([1.0, 1.0, 8.0, 8.0])));
            assert_eq!(pages[1].rotate(), 90);
        }
    }

    #[test]
    fn every_output_has_a_file_identifier_that_is_the_same_for_the_same_output() {
        let src = open(three_pages());
        let a = copy_all(&src).unwrap();
        let b = copy_all(&src).unwrap();
        assert_eq!(a.data, b.data, "the same input gives the same bytes, identifier included");
        let doc = open(a.data.clone());
        let Some(Object::Array(id)) = doc.trailer().get("ID") else { panic!("no /ID") };
        assert_eq!(id.len(), 2);
        let (Object::String(first), Object::String(second)) = (&id[0], &id[1]) else { panic!() };
        assert_eq!(first.bytes.len(), 16);
        assert_eq!(first.bytes, second.bytes, "14.4: both are the same for a new file");
        // A different output has a different identifier.
        let other = extract_pages(&src, &[0]).unwrap();
        let doc2 = open(other.data);
        let Some(Object::Array(id2)) = doc2.trailer().get("ID") else { panic!() };
        assert_ne!(id2[0], id[0]);
        // Images and merged files get one too.
        let jpeg = tiny_jpeg(4, 4, &[]);
        let img = images_to_pdf(&[ImageInput { name: "a.jpg", data: &jpeg }], PageMode::A4).unwrap();
        assert!(open(img.data).trailer().contains_key("ID"));
    }

    #[test]
    fn a_merge_of_more_pages_than_one_document_may_have_is_refused() {
        assert!(check_total_pages(1000, 1000).is_ok());
        assert!(matches!(check_total_pages(1001, 1000), Err(Error::Limit(m)) if m.contains("1001")));
        // The real limit is the one for a document.
        assert!(check_total_pages(crate::document::MAX_PAGES, crate::document::MAX_PAGES).is_ok());
        assert!(check_total_pages(2 * crate::document::MAX_PAGES, crate::document::MAX_PAGES).is_err());
    }

    /// A file whose page 1 refers (through /Foo) to an object that lives in an object stream.
    fn page_with_an_object_in_an_object_stream() -> Vec<u8> {
        let mut b = PdfBuilder::with_header("%PDF-1.5\n%\u{e2}\u{e3}\u{cf}\u{d3}\n");
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 9 9] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /Foo 10 0 R >>");
        let header = "10 0 ";
        let at = b.flate_stream_obj(
            100,
            &format!("/Type /ObjStm /N 1 /First {}", header.len()),
            format!("{header}(FROM-THE-OBJECT-STREAM)").as_bytes(),
        );
        let x = b.len();
        let row = |t: u8, a: usize, g: u8| [t, (a >> 8) as u8, a as u8, g];
        let mut rows: Vec<[u8; 4]> = vec![row(0, 0, 0); 102];
        rows[0] = row(0, 0, 255);
        for n in 1..=3u32 {
            rows[n as usize] = row(1, b.offset_of(n), 0);
        }
        rows[10] = row(2, 100, 0);
        rows[100] = row(1, at, 0);
        rows[101] = row(1, x, 0);
        let packed = png_up_flate(&rows);
        b.stream_obj(
            101,
            "/Type /XRef /Size 102 /W [1 2 1] /Root 1 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4 >>",
            &packed,
        );
        b.startxref(x);
        b.finish()
    }

    #[test]
    fn running_out_of_decoding_budget_is_an_error_not_a_quietly_dropped_object() {
        let doc = open(page_with_an_object_in_an_object_stream());
        // With the budget it is all there.
        let out = copy_all(&doc).unwrap();
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert!(contains(&out.data, "FROM-THE-OBJECT-STREAM"));
        // Without it the object cannot be read, and that is not "damaged": the
        // whole operation says it hit a limit.
        doc.use_up_the_decoding_budget_for_test();
        match copy_all(&doc) {
            Err(Error::Limit(_)) => {}
            other => panic!("{:?}", other.map(|o| o.warnings)),
        }
    }

    /// A page whose /PieceInfo is an object with an array nested 300 deep.
    fn page_with_a_very_deep_object() -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 100 100] /Resources << >> >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /Contents 5 0 R /PieceInfo 6 0 R >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /Contents 5 0 R >>");
        b.stream_obj(5, "", b"0 0 m 10 10 l S");
        b.obj(6, &format!("<< /X {}{} >>", "[".repeat(300), "]".repeat(300)));
        b.finish_classic(7, "/Root 1 0 R")
    }

    #[test]
    fn one_object_nested_too_deeply_is_left_out_with_a_warning_and_nothing_else_is_lost() {
        let src = open(page_with_a_very_deep_object());
        let second = open(sample_pdf());
        let outputs = [
            copy_all(&src).unwrap(),
            rotate_pages(&src, &[0], 90).unwrap(),
            delete_pages(&src, &[1]).unwrap(),
            extract_pages(&src, &[0]).unwrap(),
            merge(&[Input { name: "a.pdf", doc: &src }, Input { name: "b.pdf", doc: &second }]).unwrap(),
            merge(&[Input { name: "b.pdf", doc: &second }, Input { name: "a.pdf", doc: &src }]).unwrap(),
        ];
        for out in outputs {
            assert!(
                out.warnings.iter().any(|w| w.0.contains("could not be read") && w.0.contains("nested too deeply")),
                "{:?}",
                out.warnings
            );
            let doc = open(out.data.clone());
            for page in doc.pages().unwrap() {
                assert!(!page.dict.contains_key("PieceInfo"));
            }
            assert!(contains(&out.data, "0 0 m 10 10 l S"), "the page content is still there");
        }
        // It is a damaged object, not a limit: reading it says so.
        assert!(matches!(src.get(ObjRef::new(6, 0)), Err(Error::TooDeep(_))));
    }

    #[test]
    fn references_to_huge_object_numbers_cannot_reach_what_the_program_made_up() {
        // A kept page names objects 4294967295, 4294967294, ... and 8388608:
        // numbers no file has. The name tree that a delete writes anew is made of
        // objects the program numbers from 4294967295 down; none of those may be
        // what the page's references come to.
        let mut b = PdfBuilder::new();
        let tree: String = (0..130).map(|i| format!("(d{i:03}) [{} 0 R /Fit] ", 3 + i % 3)).collect();
        b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [{tree}] >> >> >>"));
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 9 9] >>");
        b.obj(
            3,
            "<< /Type /Page /Parent 2 0 R /Foo [4294967295 0 R 4294967294 0 R 4294967293 0 R 8388608 0 R 8388607 0 R] \
             /Bar 4294967295 0 R >>",
        );
        b.obj(4, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(5, "<< /Type /Page /Parent 2 0 R >>");
        let src = open(b.finish_classic(6, "/Root 1 0 R"));
        for out in [delete_pages(&src, &[1]).unwrap(), extract_pages(&src, &[0, 2]).unwrap(), copy_all(&src).unwrap()] {
            let doc = open(out.data);
            let page = &doc.pages().unwrap()[0];
            let foo = page.dict.get("Foo").and_then(Object::as_array).unwrap();
            assert!(foo.iter().all(|o| matches!(o, Object::Null)), "{foo:?}");
            assert!(!page.dict.contains_key("Bar"));
        }
        // And the writer keeps made-up objects out of the file's own namespace:
        // asking for a reference to an object of the file above the limit gives nothing.
        let mut builder = Builder::new((1, 4));
        let pages = HashSet::new();
        let source = builder.add_source(&src, &pages);
        let made = builder.define_object(source, Object::Integer(7)).unwrap();
        assert!(made.num > crate::object::MAX_OBJECT_NUMBER);
        assert!(builder.import_ref(source, ObjRef::new(made.num - 5, 0)).unwrap().is_none());
        assert!(builder.import_ref(source, made).unwrap().is_some());
    }
}

//! Cutting the document-level structures of a file down to a subset of its
//! pages (split, delete): nothing that belongs to a page that is left out may
//! stay in the output.
//!
//! What follows a page around, and what is done about it:
//! - annotations (12.5): the ones listed only by removed pages are never
//!   imported (their references become null);
//! - the interactive form (12.7): fields all of whose widgets are on removed
//!   pages are dropped from `/Fields` and from their parents' `/Kids`, with
//!   their values and appearance streams;
//! - outlines (12.3.3): items that lead to removed pages go, the ancestors of
//!   items that stay remain, and the links and counts are made right again;
//! - named destinations (12.3.2.3): entries that name removed pages go;
//! - the structure tree (14.7): dropped as a whole, with a warning, because
//!   cutting it down properly is not done yet.
//!
//! The structures are read once ([`Index`]) and then cut for each subset, so
//! splitting a book into one file per page does not read its outline again
//! for every file.

use std::collections::{HashMap, HashSet};

use crate::dests::{self, NamedDests};
use crate::document::{Document, Page};
use crate::error::Result;
use crate::object::{Dict, ObjRef, Object};
use crate::writer::{Builder, Warning};

/// Most outline items and form fields looked at.
const MAX_ITEMS: usize = 2_000_000;

/// An outline item (12.3.3, Table 153).
struct Item {
    r: ObjRef,
    children: Vec<usize>,
    /// The page (object number) it leads to, if it leads to one of the pages.
    target: Option<u32>,
    open: bool,
}

/// The outline tree, parents before their children.
struct Outlines {
    items: Vec<Item>,
    /// The items of the first level, in order.
    top: Vec<usize>,
    /// Did the tree lead back to the root or to an item already seen? Those
    /// links are cut; the tree has to be written again for that to show.
    cycles: bool,
}

/// A field or widget of the form (12.7.3, 12.5.6.19).
struct Field {
    r: ObjRef,
    children: Vec<usize>,
    /// Its `/P` (the page it is on) if that is one of the pages.
    page: Option<u32>,
}

/// The field tree, parents before their children.
struct Form {
    fields: Vec<Field>,
    /// The entries of `/Fields`, in order.
    top: Vec<usize>,
}

/// What a document's structures say about its pages, read once.
pub(crate) struct Index {
    /// Annotation object number -> the pages that list it in `/Annots`.
    pub(crate) annot_pages: HashMap<u32, Vec<u32>>,
    names: NamedDests,
    tree_pages: Vec<Option<u32>>,
    dict_pages: Vec<Option<u32>>,
    outlines: Option<Outlines>,
    form: Option<Form>,
}

impl Index {
    /// Read the structures; `Limit` if the named destinations ask for more
    /// than they may.
    pub(crate) fn build(doc: &Document, pages: &[Page], page_nums: &HashSet<u32>) -> Result<Index> {
        let names = NamedDests::load(doc)?;
        let page_of = |entries: &[(Vec<u8>, Object)]| -> Result<Vec<Option<u32>>> {
            entries
                .iter()
                .map(|(_, value)| {
                    Ok(dests::explicit(doc, value, names.spend())?.and_then(|a| dests::array_page(&a, page_nums)))
                })
                .collect()
        };
        let tree_pages = page_of(names.tree())?;
        let dict_pages = page_of(names.dict())?;
        let catalog = doc.catalog().unwrap_or_default();

        // Which page lists which annotation.
        let mut annot_pages: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut seen_pages: HashSet<u32> = HashSet::new();
        for page in pages {
            if !seen_pages.insert(page.obj_ref.num) {
                continue;
            }
            let Some(Object::Array(annots)) = page.dict.get("Annots").and_then(|a| doc.resolve(a).ok()) else {
                continue;
            };
            for annot in &annots {
                if let Object::Ref(r) = annot {
                    let owners = annot_pages.entry(r.num).or_default();
                    if !owners.contains(&page.obj_ref.num) {
                        owners.push(page.obj_ref.num);
                    }
                }
            }
        }

        let outlines = match catalog.get("Outlines") {
            Some(o) => read_outlines(doc, o, &names, page_nums)?,
            None => None,
        };
        let form = catalog.get("AcroForm").and_then(|f| read_form(doc, f, page_nums));
        Ok(Index { annot_pages, names, tree_pages, dict_pages, outlines, form })
    }

    /// Cut the catalog and what hangs from it down to the pages `kept`: set
    /// `catalog` right, and tell the builder what to leave out of `source`
    /// and what to replace.
    pub(crate) fn apply(
        &self,
        doc: &Document,
        kept: &HashSet<u32>,
        b: &mut Builder<'_>,
        source: usize,
        catalog: &mut Dict,
        warnings: &mut Vec<Warning>,
    ) -> Result<()> {
        // The structure tree cannot follow a page subset (yet); what points
        // into it (parent tree numbers, an outline item's structure element)
        // goes with it.
        if catalog.remove("StructTreeRoot").is_some() {
            warnings.push(Warning(
                "the structure tree (tagged PDF) was dropped because it cannot follow a subset of the pages yet"
                    .to_string(),
            ));
            b.drop_structure_links(source);
            // 14.7.1: /Marked says the file conforms to tagged PDF; absent it is
            // false. A dictionary with nothing else in it goes altogether.
            if let Some(Object::Dict(mut mark)) = catalog.get("MarkInfo").and_then(|m| doc.resolve(m).ok()) {
                mark.remove("Marked");
                mark.remove("Type");
                catalog.set("MarkInfo", if mark.is_empty() { Object::Null } else { Object::Dict(mark) });
            }
        }
        self.cut_destinations(doc, kept, b, source, catalog)?;
        self.cut_outlines(doc, kept, b, source, catalog);
        self.cut_form(doc, kept, b, source, catalog, warnings);
        Ok(())
    }

    /// Named destinations that name a page that is gone.
    fn cut_destinations(
        &self,
        doc: &Document,
        kept: &HashSet<u32>,
        b: &mut Builder<'_>,
        source: usize,
        catalog: &mut Dict,
    ) -> Result<()> {
        let survive = |pages: &[Option<u32>], entries: &[(Vec<u8>, Object)]| -> Option<Vec<(Vec<u8>, Object)>> {
            let live: Vec<(Vec<u8>, Object)> = entries
                .iter()
                .zip(pages)
                .filter(|(_, page)| page.is_none_or(|p| kept.contains(&p)))
                .map(|(entry, _)| entry.clone())
                .collect();
            (live.len() < entries.len()).then_some(live)
        };
        // 12.3.2.3: the name tree in /Names ...
        if let Some(live) = survive(&self.tree_pages, self.names.tree())
            && let Some(Object::Dict(mut names)) = catalog.get("Names").and_then(|n| doc.resolve(n).ok())
        {
            if live.is_empty() {
                names.remove("Dests");
            } else {
                // Every node of the tree is an object of its own (Table 36).
                let root = dests::build_name_tree(live, &mut |node| b.define_object(source, Object::Dict(node)))?;
                names.set("Dests", Object::Ref(root));
            }
            let value = if names.is_empty() { Object::Null } else { Object::Dict(names) };
            put_catalog_entry(catalog, "Names", value, b, source);
        }
        // ... and the old dictionary in the catalog.
        if let Some(live) = survive(&self.dict_pages, self.names.dict()) {
            let value = if live.is_empty() { Object::Null } else { Object::Dict(dests::build_dests_dict(live)) };
            put_catalog_entry(catalog, "Dests", value, b, source);
        }
        Ok(())
    }

    /// Bookmarks that lead to a page that is gone.
    fn cut_outlines(&self, doc: &Document, kept: &HashSet<u32>, b: &mut Builder<'_>, source: usize, catalog: &mut Dict) {
        let Some(outlines) = &self.outlines else {
            return;
        };
        let items = &outlines.items;
        // An item stays if it leads to a kept page, or, when it leads nowhere
        // in this file (a web link, a title), if it has no children to speak
        // for it, or if any of its children stays.
        let mut alive = vec![false; items.len()];
        // How many items an open item shows below it; children come after
        // their parents, so a pass backwards sees the children first.
        let mut shown = vec![0i64; items.len()];
        for (i, item) in items.iter().enumerate().rev() {
            let any_child = item.children.iter().any(|&c| alive.get(c).copied().unwrap_or(false));
            let own = match item.target {
                Some(p) => kept.contains(&p),
                None => item.children.is_empty(),
            };
            if let Some(slot) = alive.get_mut(i) {
                *slot = own || any_child;
            }
            if own || any_child {
                let count = shown_below(items, &item.children, &alive, &shown);
                if let Some(slot) = shown.get_mut(i) {
                    *slot = count;
                }
            }
        }
        if alive.iter().all(|&a| a) && !outlines.cycles {
            return;
        }
        let live_top: Vec<usize> = outlines.top.iter().copied().filter(|&i| alive.get(i).copied().unwrap_or(false)).collect();
        let Some(root_obj) = catalog.get("Outlines").cloned() else {
            return;
        };
        let Some(Object::Dict(mut root)) = doc.resolve(&root_obj).ok() else {
            return;
        };
        if live_top.is_empty() {
            catalog.remove("Outlines");
            return;
        }

        // The links between the items that stay: each group of siblings
        // (the first level, or the children of one item) is a new list.
        let mut prev_next: HashMap<usize, (Option<ObjRef>, Option<ObjRef>)> = HashMap::new();
        let mut link_group = |group: &[usize]| {
            for (k, &i) in group.iter().enumerate() {
                let prev = k.checked_sub(1).and_then(|p| group.get(p)).and_then(|&p| items.get(p)).map(|it| it.r);
                let next = group.get(k + 1).and_then(|&n| items.get(n)).map(|it| it.r);
                prev_next.insert(i, (prev, next));
            }
        };
        link_group(&live_top);
        let live_children = |item: &Item| -> Vec<usize> {
            item.children.iter().copied().filter(|&c| alive.get(c).copied().unwrap_or(false)).collect()
        };
        for (i, item) in items.iter().enumerate() {
            if alive.get(i).copied().unwrap_or(false) {
                link_group(&live_children(item));
            }
        }

        for (i, item) in items.iter().enumerate() {
            if !alive.get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some(Object::Dict(mut d)) = doc.get(item.r).ok() else {
                continue;
            };
            let kids = live_children(item);
            set_ref(&mut d, "First", kids.first().and_then(|&c| items.get(c)).map(|it| it.r));
            set_ref(&mut d, "Last", kids.last().and_then(|&c| items.get(c)).map(|it| it.r));
            let (prev, next) = prev_next.get(&i).copied().unwrap_or((None, None));
            set_ref(&mut d, "Prev", prev);
            set_ref(&mut d, "Next", next);
            // 12.3.3: the count is negative when the item is closed.
            let count = shown.get(i).copied().unwrap_or(0);
            d.set("Count", if count == 0 { Object::Null } else { Object::Integer(if item.open { count } else { -count }) });
            b.replace(source, item.r.num, Object::Dict(d));
        }
        set_ref(&mut root, "First", live_top.first().and_then(|&c| items.get(c)).map(|it| it.r));
        set_ref(&mut root, "Last", live_top.last().and_then(|&c| items.get(c)).map(|it| it.r));
        let total = shown_below(items, &live_top, &alive, &shown);
        root.set("Count", Object::Integer(total));
        put_catalog_entry(catalog, "Outlines", Object::Dict(root), b, source);
    }

    /// The interactive form: fields whose widgets are all on removed pages.
    fn cut_form(
        &self,
        doc: &Document,
        kept: &HashSet<u32>,
        b: &mut Builder<'_>,
        source: usize,
        catalog: &mut Dict,
        warnings: &mut Vec<Warning>,
    ) {
        let Some(Object::Dict(mut acro)) = catalog.get("AcroForm").and_then(|f| doc.resolve(f).ok()) else {
            return;
        };
        let mut changed = false;
        // XFA form data (12.7.8) holds the values of all fields, wherever
        // they are; it cannot be cut down by page.
        if acro.remove("XFA").is_some() {
            warnings.push(Warning("the XFA form data was dropped because it cannot follow a subset of the pages".to_string()));
            changed = true;
        }
        if let Some(form) = &self.form {
            let fields = &form.fields;
            // A widget is dead if only removed pages list it (or, unlisted,
            // if its /P is a removed page); a field is dead if it has no
            // living widget or sub-field. A field with nothing to show
            // anywhere (a calculation field) lives.
            let widget_dead = |f: &Field| match self.annot_pages.get(&f.r.num) {
                Some(owners) => !owners.iter().any(|p| kept.contains(p)),
                None => f.page.is_some_and(|p| !kept.contains(&p)),
            };
            let mut alive = vec![false; fields.len()];
            for (i, f) in fields.iter().enumerate().rev() {
                let any_child = f.children.iter().any(|&c| alive.get(c).copied().unwrap_or(false));
                let live = if f.children.is_empty() { !widget_dead(f) } else { any_child };
                if let Some(slot) = alive.get_mut(i) {
                    *slot = live;
                }
            }
            if alive.iter().any(|&a| !a) {
                changed = true;
                // A dead field is not imported by anything that names it: not
                // by a parent's /Kids, and not by an action's /Fields either
                // (12.7.5.2: ResetForm, SubmitForm), which would bring its
                // value and appearance back.
                b.exclude_objects(
                    source,
                    fields.iter().zip(&alive).filter(|(_, a)| !**a).map(|(f, _)| f.r.num),
                );
                // Fields that lost a kid are written with the kids that stay.
                for (i, f) in fields.iter().enumerate() {
                    let live = alive.get(i).copied().unwrap_or(false);
                    if !live || f.children.iter().all(|&c| alive.get(c).copied().unwrap_or(false)) {
                        continue;
                    }
                    let Some(Object::Dict(mut d)) = doc.get(f.r).ok() else {
                        continue;
                    };
                    let kids: Vec<Object> = f
                        .children
                        .iter()
                        .filter(|&&c| alive.get(c).copied().unwrap_or(false))
                        .filter_map(|&c| fields.get(c))
                        .map(|k| Object::Ref(k.r))
                        .collect();
                    d.set("Kids", Object::Array(kids));
                    b.replace(source, f.r.num, Object::Dict(d));
                }
                let top: Vec<Object> = form
                    .top
                    .iter()
                    .filter(|&&i| alive.get(i).copied().unwrap_or(false))
                    .filter_map(|&i| fields.get(i))
                    .map(|f| Object::Ref(f.r))
                    .collect();
                // The calculation order names fields; the dead ones go.
                if let Some(Object::Array(order)) = acro.get("CO").and_then(|c| doc.resolve(c).ok()) {
                    let dead: HashSet<u32> =
                        fields.iter().zip(&alive).filter(|(_, a)| !**a).map(|(f, _)| f.r.num).collect();
                    let order: Vec<Object> = order
                        .into_iter()
                        .filter(|o| !matches!(o, Object::Ref(r) if dead.contains(&r.num)))
                        .collect();
                    acro.set("CO", Object::Array(order));
                }
                if top.is_empty() {
                    catalog.remove("AcroForm");
                    return;
                }
                acro.set("Fields", Object::Array(top));
            }
        }
        if changed {
            put_catalog_entry(catalog, "AcroForm", Object::Dict(acro), b, source);
        }
    }
}

/// How many items an open parent of `children` shows: each living child, and
/// below each open one what it shows.
fn shown_below(items: &[Item], children: &[usize], alive: &[bool], shown: &[i64]) -> i64 {
    let mut total = 0i64;
    for &c in children {
        if !alive.get(c).copied().unwrap_or(false) {
            continue;
        }
        total = total.saturating_add(1);
        if items.get(c).is_some_and(|it| it.open) {
            total = total.saturating_add(shown.get(c).copied().unwrap_or(0));
        }
    }
    total
}

/// Set `key` to a reference, or remove it.
fn set_ref(d: &mut Dict, key: &str, r: Option<ObjRef>) {
    d.set(key, r.map_or(Object::Null, Object::Ref));
}

/// Put `value` where the catalog's `key` is: into the object it refers to, or
/// into the catalog itself when the entry is direct. A null value removes it.
fn put_catalog_entry(catalog: &mut Dict, key: &str, value: Object, b: &mut Builder<'_>, source: usize) {
    if matches!(value, Object::Null) {
        catalog.remove(key);
        return;
    }
    match catalog.get(key) {
        Some(Object::Ref(r)) => b.replace(source, r.num, value),
        _ => catalog.set(key, value),
    }
}

/// The outline tree under the catalog's `/Outlines` (12.3.3), if there is one.
fn read_outlines(
    doc: &Document,
    entry: &Object,
    names: &NamedDests,
    pages: &HashSet<u32>,
) -> Result<Option<Outlines>> {
    let Some(Object::Dict(root)) = doc.resolve(entry).ok() else {
        return Ok(None);
    };
    let Some(Object::Ref(first)) = root.get("First") else {
        return Ok(None);
    };
    let mut items: Vec<Item> = Vec::new();
    let mut top: Vec<usize> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut cycles = false;
    // The root is not an item: a link that leads back to it is a cycle.
    if let Object::Ref(r) = entry {
        seen.insert(r.num);
    }
    // Chains of siblings still to walk: where the chain starts, and whose
    // children it is.
    let mut chains: Vec<(ObjRef, Option<usize>)> = vec![(*first, None)];
    while let Some((start, parent)) = chains.pop() {
        let mut current = Some(start);
        while let Some(r) = current {
            if items.len() >= MAX_ITEMS {
                break;
            }
            if !seen.insert(r.num) {
                cycles = true;
                break;
            }
            let Ok(Object::Dict(d)) = doc.get(r) else {
                break;
            };
            let index = items.len();
            let open = d.get("Count").and_then(Object::as_int).is_some_and(|c| c > 0);
            let target = dests::item_page(doc, &d, names, pages)?;
            items.push(Item { r, children: Vec::new(), target, open });
            match parent {
                None => top.push(index),
                Some(p) => {
                    if let Some(parent_item) = items.get_mut(p) {
                        parent_item.children.push(index);
                    }
                }
            }
            if let Some(Object::Ref(child)) = d.get("First") {
                chains.push((*child, Some(index)));
            }
            current = match d.get("Next") {
                Some(Object::Ref(next)) => Some(*next),
                _ => None,
            };
        }
    }
    Ok(Some(Outlines { items, top, cycles }))
}

/// The field tree under the catalog's `/AcroForm` `/Fields` (12.7.3).
fn read_form(doc: &Document, entry: &Object, pages: &HashSet<u32>) -> Option<Form> {
    let Object::Dict(acro) = doc.resolve(entry).ok()? else {
        return None;
    };
    let Object::Array(roots) = doc.resolve(acro.get("Fields")?).ok()? else {
        return None;
    };
    let mut fields: Vec<Field> = Vec::new();
    let mut top: Vec<usize> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    // Pushed in reverse so that they come out in the order of the file.
    let mut stack: Vec<(ObjRef, Option<usize>)> = Vec::new();
    for root in roots.iter().rev() {
        if let Object::Ref(r) = root {
            stack.push((*r, None));
        }
    }
    while let Some((r, parent)) = stack.pop() {
        if fields.len() >= MAX_ITEMS || !seen.insert(r.num) {
            continue;
        }
        let Ok(Object::Dict(d)) = doc.get(r) else {
            continue;
        };
        let index = fields.len();
        let page = match d.get("P") {
            Some(Object::Ref(p)) if pages.contains(&p.num) => Some(p.num),
            _ => None,
        };
        fields.push(Field { r, children: Vec::new(), page });
        match parent {
            None => top.push(index),
            Some(p) => {
                if let Some(parent_field) = fields.get_mut(p) {
                    parent_field.children.push(index);
                }
            }
        }
        if let Some(Object::Array(kids)) = d.get("Kids").and_then(|k| doc.resolve(k).ok()) {
            for kid in kids.iter().rev() {
                if let Object::Ref(k) = kid {
                    stack.push((*k, Some(index)));
                }
            }
        }
    }
    Some(Form { fields, top })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::find_bytes;
    use crate::ops;
    use crate::testutil::*;

    fn open(bytes: Vec<u8>) -> Document {
        Document::from_bytes(bytes).unwrap()
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        find_bytes(haystack, needle.as_bytes(), 0).is_some()
    }

    fn dict_of(doc: &Document, obj: &Object) -> Dict {
        doc.resolve(obj).unwrap().as_dict().unwrap().clone()
    }

    /// Two pages, a form with a merged field on each page and one field
    /// whose two widgets are one on each page, a calculation order, and
    /// appearance streams that say which page they belong to.
    fn two_page_form() -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 20 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 200] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /Annots [30 0 R 41 0 R] >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /Annots [42 0 R 32 0 R] >>");
        b.obj(20, "<< /Fields [30 0 R 31 0 R 32 0 R] /CO [30 0 R 32 0 R] /DA (/Helv 0 Tf) /XFA [(preamble) 60 0 R] >>");
        b.obj(30, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (Name) /V (ALICE) /Rect [0 0 9 9] /P 3 0 R >>");
        b.obj(31, "<< /FT /Btn /T (Choice) /V /Yes /Kids [41 0 R 42 0 R] >>");
        b.obj(41, "<< /Type /Annot /Subtype /Widget /Parent 31 0 R /P 3 0 R /Rect [0 0 9 9] /AP << /N 50 0 R >> >>");
        b.obj(42, "<< /Type /Annot /Subtype /Widget /Parent 31 0 R /P 4 0 R /Rect [0 0 9 9] /AP << /N 51 0 R >> >>");
        b.obj(32, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (SSN) /V (SECRET-123) /Rect [0 0 9 9] /P 4 0 R /AP << /N 52 0 R >> >>");
        b.stream_obj(50, "/Type /XObject /Subtype /Form /BBox [0 0 9 9]", b"KEPT-WIDGET-APPEARANCE");
        b.stream_obj(51, "/Type /XObject /Subtype /Form /BBox [0 0 9 9]", b"SECRET-WIDGET-APPEARANCE");
        b.stream_obj(52, "/Type /XObject /Subtype /Form /BBox [0 0 9 9]", b"SECRET-FIELD-APPEARANCE");
        b.stream_obj(60, "", b"SECRET-XFA-DATA");
        b.finish_classic(61, "/Root 1 0 R")
    }

    #[test]
    fn nothing_of_a_removed_page_survives_in_the_form() {
        let src = open(two_page_form());
        // Delete page 2 (index 1): its widgets, field values and appearances go.
        let out = ops::delete_pages(&src, &[1]).unwrap();
        assert!(!contains(&out.data, "SECRET"), "something of the removed page is still there");
        assert!(contains(&out.data, "ALICE") && contains(&out.data, "KEPT-WIDGET-APPEARANCE"));
        assert!(out.warnings.iter().any(|w| w.0.contains("XFA")), "{:?}", out.warnings);
        let doc = open(out.data);
        let catalog = doc.catalog().unwrap();
        let acro = dict_of(&doc, catalog.get("AcroForm").unwrap());
        assert!(!acro.contains_key("XFA"));
        // Two fields stay: the merged one and the one with a widget left.
        let fields = acro.get("Fields").and_then(Object::as_array).unwrap();
        assert_eq!(fields.len(), 2);
        let choice = dict_of(&doc, &fields[1]);
        assert_eq!(choice.get_name("FT").unwrap(), &crate::object::Name::from("Btn"));
        assert_eq!(choice.get("Kids").and_then(Object::as_array).unwrap().len(), 1);
        // The calculation order lost the dead field.
        assert_eq!(acro.get("CO").and_then(Object::as_array).unwrap().len(), 1);
        // The page keeps both of its annotations, and the widget still names its field.
        let pages = doc.pages().unwrap();
        assert_eq!(pages[0].dict.get("Annots").and_then(Object::as_array).unwrap().len(), 2);
        // Keeping page 2 instead turns it around.
        let out = ops::extract_pages(&src, &[1]).unwrap();
        assert!(!contains(&out.data, "ALICE") && !contains(&out.data, "KEPT-WIDGET-APPEARANCE"));
        assert!(contains(&out.data, "SECRET-FIELD-APPEARANCE") && contains(&out.data, "SECRET-123"));
        let doc = open(out.data);
        let acro = dict_of(&doc, doc.catalog().unwrap().get("AcroForm").unwrap());
        assert_eq!(acro.get("Fields").and_then(Object::as_array).unwrap().len(), 2);
    }

    #[test]
    fn a_form_whose_fields_are_all_on_removed_pages_is_dropped() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] >> >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 9 9] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /Annots [5 0 R] >>");
        b.obj(5, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (T) /V (GONE) /Rect [0 0 1 1] /P 4 0 R >>");
        let src = open(b.finish_classic(6, "/Root 1 0 R"));
        let out = ops::delete_pages(&src, &[1]).unwrap();
        assert!(!contains(&out.data, "GONE"));
        assert!(!open(out.data).catalog().unwrap().contains_key("AcroForm"));
        // A field that is on no page at all is not "on a removed page": it stays.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R] >> >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 9 9] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(5, "<< /FT /Tx /T (calc) /V (HIDDEN-VALUE) >>");
        let src = open(b.finish_classic(6, "/Root 1 0 R"));
        let out = ops::delete_pages(&src, &[1]).unwrap();
        assert!(contains(&out.data, "HIDDEN-VALUE"));
        assert!(open(out.data).catalog().unwrap().contains_key("AcroForm"));
    }

    /// Every `/Kids` entry of the name tree under `root`, all the way down, is an
    /// indirect reference.
    fn assert_all_kids_are_references(doc: &Document, root: &Object) {
        let mut stack = vec![root.clone()];
        let mut nodes = 0;
        while let Some(node) = stack.pop() {
            nodes += 1;
            let d = dict_of(doc, &node);
            if let Some(kids) = d.get("Kids") {
                let kids = doc.resolve(kids).unwrap();
                for kid in kids.as_array().unwrap() {
                    assert!(matches!(kid, Object::Ref(_)), "a kid is not an indirect reference: {kid:?}");
                    stack.push(kid.clone());
                }
            }
        }
        assert!(nodes > 1);
    }

    #[test]
    fn a_tree_of_many_levels_has_indirect_kids_at_every_level() {
        // 70,000 destinations need three levels of nodes; delete a third of them.
        let mut b = PdfBuilder::new();
        let tree: String = (0..70_000).map(|i| format!("(d{i:05}) [{} 0 R /Fit] ", 3 + i % 3)).collect();
        b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [{tree}] >> >> >>"));
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 9 9] >>");
        for n in 3..=5 {
            b.obj(n, "<< /Type /Page /Parent 2 0 R >>");
        }
        let src = open(b.finish_classic(6, "/Root 1 0 R"));
        let doc = open(ops::delete_pages(&src, &[1]).unwrap().data);
        let names = dict_of(&doc, doc.catalog().unwrap().get("Names").unwrap());
        assert_all_kids_are_references(&doc, names.get("Dests").unwrap());
        let loaded = NamedDests::load(&doc).unwrap();
        assert_eq!(loaded.tree().len(), (0..70_000).filter(|i| i % 3 != 1).count());
    }

    /// Page 2 has a non-terminal field with a value on the parent; page 1 has a
    /// ResetForm button, and an outline item has a SubmitForm action, both
    /// naming that field.
    fn actions_naming_a_field_of_another_page() -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 11 0 R] >> /Outlines 40 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 100 100] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /Annots [20 0 R] >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /Annots [21 0 R] >>");
        b.obj(10, "<< /T (btn) /FT /Btn /Ff 65536 /Kids [20 0 R] >>");
        b.obj(20, "<< /Type /Annot /Subtype /Widget /Parent 10 0 R /P 3 0 R /Rect [0 0 9 9] /A << /S /ResetForm /Fields [11 0 R] >> >>");
        b.obj(11, "<< /T (ssn) /FT /Tx /V (LEAK-PARENT-VALUE) /Kids [21 0 R] >>");
        b.obj(21, "<< /Type /Annot /Subtype /Widget /Parent 11 0 R /P 4 0 R /Rect [0 0 9 9] /AP << /N 30 0 R >> >>");
        b.stream_obj(30, "/Type /XObject /Subtype /Form /BBox [0 0 9 9]", b"LEAK-APPEARANCE");
        b.obj(40, "<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>");
        b.obj(41, "<< /Title (Submit) /Parent 40 0 R /A << /S /SubmitForm /F << /FS /URL /F (http://example.com) >> /Fields [11 0 R] >> >>");
        b.finish_classic(42, "/Root 1 0 R")
    }

    #[test]
    fn a_dead_field_named_by_an_action_does_not_come_back_through_it() {
        let src = open(actions_naming_a_field_of_another_page());
        let out = ops::delete_pages(&src, &[1]).unwrap();
        assert!(!contains(&out.data, "LEAK"), "the form data of the removed page is still in the file");
        let doc = open(out.data);
        // The button stays, its action stays, naming no field now.
        let pages = doc.pages().unwrap();
        let annots = pages[0].dict.get("Annots").and_then(Object::as_array).unwrap().to_vec();
        let button = dict_of(&doc, &annots[0]);
        let action = dict_of(&doc, button.get("A").unwrap());
        assert_eq!(action.get_name("S").unwrap(), &crate::object::Name::from("ResetForm"));
        assert_eq!(action.get("Fields"), Some(&Object::Array(vec![])));
        // The bookmark with the SubmitForm action stays too (it leads nowhere in the file).
        let outlines = dict_of(&doc, doc.catalog().unwrap().get("Outlines").unwrap());
        let item = dict_of(&doc, outlines.get("First").unwrap());
        let submit = dict_of(&doc, item.get("A").unwrap());
        assert_eq!(submit.get("Fields"), Some(&Object::Array(vec![])));
        // Keeping page 2 instead keeps the field, and the actions on page 1 are gone.
        let out = ops::extract_pages(&src, &[1]).unwrap();
        assert!(contains(&out.data, "LEAK-PARENT-VALUE") && contains(&out.data, "LEAK-APPEARANCE"));
    }

    #[test]
    fn outline_links_that_lead_back_to_the_root_or_to_an_item_seen_are_cut() {
        // A (page 1) has child A1 (page 2) whose /First is the outline root;
        // C (page 1) has a /First that is A and a /Next that is A again.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 100 100] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(10, "<< /Type /Outlines /First 11 0 R /Last 13 0 R /Count 5 >>");
        b.obj(11, "<< /Title (A-p1) /Parent 10 0 R /Next 12 0 R /Dest [3 0 R /Fit] /First 14 0 R /Last 14 0 R /Count 1 >>");
        b.obj(12, "<< /Title (B-p2) /Parent 10 0 R /Prev 11 0 R /Next 13 0 R /Dest [4 0 R /Fit] >>");
        b.obj(13, "<< /Title (C-p1) /Parent 10 0 R /Prev 12 0 R /Next 11 0 R /Dest [3 0 R /Fit] /First 11 0 R /Count 1 >>");
        b.obj(14, "<< /Title (A1-p2) /Parent 11 0 R /Next 12 0 R /Dest [4 0 R /Fit] /First 10 0 R >>");
        let src = open(b.finish_classic(15, "/Root 1 0 R"));
        let out = ops::delete_pages(&src, &[1]).unwrap();
        let doc = open(out.data);
        let root_ref = doc.catalog().unwrap().get("Outlines").and_then(Object::as_obj_ref).unwrap();
        // Walk the way a reader (qpdf) does: no item may be met twice, and none may lead to the root.
        let mut seen: HashSet<u32> = HashSet::new();
        seen.insert(root_ref.num);
        let mut titles: Vec<String> = Vec::new();
        let mut stack: Vec<ObjRef> = vec![dict_of(&doc, &Object::Ref(root_ref)).get("First").and_then(Object::as_obj_ref).unwrap()];
        while let Some(r) = stack.pop() {
            assert!(seen.insert(r.num), "outline item {} is met twice: a loop", r.num);
            let d = dict_of(&doc, &Object::Ref(r));
            let Some(Object::String(t)) = d.get("Title") else { panic!() };
            titles.push(String::from_utf8(t.bytes.clone()).unwrap());
            for key in ["Next", "First"] {
                if let Some(Object::Ref(next)) = d.get(key) {
                    stack.push(*next);
                }
            }
        }
        titles.sort();
        assert_eq!(titles, vec!["A-p1".to_string(), "C-p1".to_string()]);
        assert_eq!(dict_of(&doc, &Object::Ref(root_ref)).get_int("Count"), Some(2));
    }

    #[test]
    fn a_cycle_in_the_outline_is_cut_even_when_no_page_is_left_out() {
        // Same tree; extracting every page in another order cuts nothing by
        // page, but a delete that keeps everything that has a bookmark still
        // walks it, so check the walk itself: the item list has no repeat.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 100 100] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(10, "<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>");
        b.obj(11, "<< /Title (Loop) /Parent 10 0 R /Dest [3 0 R /Fit] /First 10 0 R >>");
        let src = open(b.finish_classic(12, "/Root 1 0 R"));
        let doc = Document::from_bytes(ops::delete_pages(&src, &[1]).unwrap().data).unwrap();
        let root = dict_of(&doc, doc.catalog().unwrap().get("Outlines").unwrap());
        let item = dict_of(&doc, root.get("First").unwrap());
        assert!(!item.contains_key("First"), "the link back to the root is cut");
    }

    /// Three pages; bookmarks:
    ///   A -> page 1
    ///   B (closed, no destination of its own)
    ///        B1 -> page 2
    ///        B2 -> page 1
    ///   C -> page 3, open, with child C1 -> page 3
    ///   D: a web link
    fn bookmarked() -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 9 9] >>");
        for n in 3..=5 {
            b.obj(n, "<< /Type /Page /Parent 2 0 R >>");
        }
        b.obj(10, "<< /Type /Outlines /First 11 0 R /Last 14 0 R /Count 7 >>");
        b.obj(11, "<< /Title (A) /Parent 10 0 R /Next 12 0 R /Dest [3 0 R /Fit] >>");
        b.obj(12, "<< /Title (B) /Parent 10 0 R /Prev 11 0 R /Next 13 0 R /First 20 0 R /Last 21 0 R /Count -2 >>");
        b.obj(13, "<< /Title (C) /Parent 10 0 R /Prev 12 0 R /Next 14 0 R /First 22 0 R /Last 22 0 R /Count 1 /A << /S /GoTo /D [5 0 R /Fit] >> >>");
        b.obj(14, "<< /Title (D) /Parent 10 0 R /Prev 13 0 R /A << /S /URI /URI (http://example.com) >> >>");
        b.obj(20, "<< /Title (B1) /Parent 12 0 R /Next 21 0 R /Dest [4 0 R /Fit] >>");
        b.obj(21, "<< /Title (B2) /Parent 12 0 R /Prev 20 0 R /Dest [3 0 R /Fit] >>");
        b.obj(22, "<< /Title (C1) /Parent 13 0 R /Dest [5 0 R /Fit] >>");
        b.finish_classic(23, "/Root 1 0 R")
    }

    fn titles(doc: &Document, root: &Object) -> Vec<(String, Option<i64>)> {
        // Walk First/Next, depth first; (title, count).
        let mut out = Vec::new();
        let mut stack: Vec<Object> = Vec::new();
        let root = dict_of(doc, root);
        if let Some(first) = root.get("First") {
            stack.push(first.clone());
        }
        while let Some(item) = stack.pop() {
            let d = dict_of(doc, &item);
            let Some(Object::String(t)) = d.get("Title") else { panic!() };
            out.push((String::from_utf8(t.bytes.clone()).unwrap(), d.get_int("Count")));
            if let Some(next) = d.get("Next") {
                stack.push(next.clone());
            }
            if let Some(first) = d.get("First") {
                stack.push(first.clone());
            }
        }
        out
    }

    #[test]
    fn bookmarks_to_removed_pages_go_and_their_ancestors_stay() {
        let src = open(bookmarked());
        // Keep page 1: A, B (for B2), D (a web link) stay; B1, C, C1 go.
        let out = ops::extract_pages(&src, &[0]).unwrap();
        let doc = open(out.data);
        let root_obj = doc.catalog().unwrap().get("Outlines").unwrap().clone();
        let root = dict_of(&doc, &root_obj);
        let list = titles(&doc, &root_obj);
        let names: Vec<&str> = list.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "B2", "D"]);
        // B is closed with one child: -1. The root shows A, B and D: 3.
        let b_item = list.iter().find(|(t, _)| t == "B").unwrap();
        assert_eq!(b_item.1, Some(-1));
        assert_eq!(root.get_int("Count"), Some(3));
        // The ends of the list are right.
        let last = dict_of(&doc, root.get("Last").unwrap());
        assert_eq!(last.get("Title"), Some(&Object::String(crate::object::PdfString::literal("D"))));
        assert!(!last.contains_key("Next"));
        let first = dict_of(&doc, root.get("First").unwrap());
        assert!(!first.contains_key("Prev"));
        // Keep pages 2 and 3: B (for B1), B2 goes, C (open, with C1) stays, A and D.
        let out = ops::delete_pages(&src, &[0]).unwrap();
        let doc = open(out.data);
        let root_obj = doc.catalog().unwrap().get("Outlines").unwrap().clone();
        let list = titles(&doc, &root_obj);
        let names: Vec<&str> = list.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(names, vec!["B", "B1", "C", "C1", "D"]);
        // C is open with one child: 1. The root shows B, C, C1 and D: 4.
        assert_eq!(list.iter().find(|(t, _)| t == "C").unwrap().1, Some(1));
        assert_eq!(dict_of(&doc, &root_obj).get_int("Count"), Some(4));
        // C's own destination (an action) leads to the page that is now second.
        let pages = doc.pages().unwrap();
        let c = {
            let root = dict_of(&doc, &root_obj);
            let mut item = dict_of(&doc, root.get("First").unwrap());
            item = dict_of(&doc, item.get("Next").unwrap());
            item
        };
        let action = dict_of(&doc, c.get("A").unwrap());
        assert_eq!(
            action.get("D"),
            Some(&Object::Array(vec![Object::Ref(pages[1].obj_ref), Object::from("Fit")]))
        );
    }

    #[test]
    fn all_bookmarks_for_removed_pages_drop_the_outline() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 9 9] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(10, "<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>");
        b.obj(11, "<< /Title (OnlyPageTwo) /Parent 10 0 R /Dest [4 0 R /Fit] >>");
        let src = open(b.finish_classic(12, "/Root 1 0 R"));
        let out = ops::extract_pages(&src, &[0]).unwrap();
        assert!(!contains(&out.data, "OnlyPageTwo"));
        assert!(!open(out.data).catalog().unwrap().contains_key("Outlines"));
        // Keeping everything leaves the outline as it was.
        let out = ops::rotate_pages(&src, &[0], 90).unwrap();
        assert!(contains(&out.data, "OnlyPageTwo"));
    }

    #[test]
    fn named_destinations_of_removed_pages_are_dropped_from_the_tree_and_the_old_dictionary() {
        let mut b = PdfBuilder::new();
        let tree: String = (0..130).map(|i| format!("(d{i:03}) [{} 0 R /Fit] ", 3 + i % 3)).collect();
        let old = "/Dests << /Old3 [3 0 R /Fit] /Old4 [4 0 R /Fit] /Odd [null /Fit] >>";
        b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [{tree}] >> >> {old} >>"));
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 9 9] >>");
        for n in 3..=5 {
            b.obj(n, "<< /Type /Page /Parent 2 0 R >>");
        }
        let src = open(b.finish_classic(6, "/Root 1 0 R"));
        // Keep pages 1 and 3: the entries for page 2 (every third) are gone.
        let out = ops::extract_pages(&src, &[0, 2]).unwrap();
        let doc = open(out.data);
        let names = NamedDests::load(&doc).unwrap();
        let expected = (0..130).filter(|i| i % 3 != 1).count();
        assert_eq!(names.tree().len(), expected);
        let pages = doc.pages().unwrap();
        let page_nums: HashSet<u32> = pages.iter().map(|p| p.obj_ref.num).collect();
        for (key, _) in names.tree() {
            let array = names.lookup(&doc, key, false).unwrap().unwrap();
            assert!(dests::array_page(&array, &page_nums).is_some(), "{}", String::from_utf8_lossy(key));
        }
        // The rebuilt tree is a real tree: more than one level, every node an
        // object of its own, and /Kids an array of indirect references (Table 36).
        let catalog = doc.catalog().unwrap();
        let names_dict = dict_of(&doc, catalog.get("Names").unwrap());
        let root_entry = names_dict.get("Dests").unwrap();
        assert!(matches!(root_entry, Object::Ref(_)), "the root is an indirect object");
        let root = dict_of(&doc, root_entry);
        assert!(root.contains_key("Kids") && !root.contains_key("Limits"));
        assert_all_kids_are_references(&doc, root_entry);
        // The old dictionary keeps what leads to kept pages, and what leads nowhere known.
        let old = dict_of(&doc, catalog.get("Dests").unwrap());
        assert!(old.contains_key("Old3") && old.contains_key("Odd") && !old.contains_key("Old4"));
        // Dropping every page the tree names removes the tree, and /Names with it.
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [(x) [4 0 R /Fit]] >> >> >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 9 9] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R >>");
        let src = open(b.finish_classic(5, "/Root 1 0 R"));
        let doc = open(ops::extract_pages(&src, &[0]).unwrap().data);
        assert!(!doc.catalog().unwrap().contains_key("Names"));
    }

    #[test]
    fn the_structure_tree_is_dropped_with_a_warning_and_what_points_into_it() {
        let mut b = PdfBuilder::new();
        b.obj(
            1,
            "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 30 0 R /MarkInfo << /Marked true /UserProperties true >> \
             /Outlines 10 0 R >>",
        );
        b.obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 9 9] >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /StructParents 0 /Annots [5 0 R] >>");
        b.obj(4, "<< /Type /Page /Parent 2 0 R /StructParents 1 >>");
        b.obj(5, "<< /Type /Annot /Subtype /Link /Rect [0 0 1 1] /StructParent 2 >>");
        b.obj(10, "<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>");
        b.obj(11, "<< /Title (T) /Parent 10 0 R /Dest [3 0 R /Fit] /SE 31 0 R >>");
        b.obj(30, "<< /Type /StructTreeRoot /K 31 0 R /ParentTree << /Nums [0 []] >> >>");
        b.obj(31, "<< /Type /StructElem /S /P /P 30 0 R /Pg 4 0 R /Alt (STRUCT-MARKER) >>");
        let src = open(b.finish_classic(32, "/Root 1 0 R"));
        let out = ops::extract_pages(&src, &[0]).unwrap();
        assert!(!contains(&out.data, "STRUCT-MARKER"), "the structure tree came back through an outline item");
        assert!(out.warnings.iter().any(|w| w.0.contains("structure tree")), "{:?}", out.warnings);
        let doc = open(out.data);
        let catalog = doc.catalog().unwrap();
        assert!(!catalog.contains_key("StructTreeRoot"));
        // No longer marked (the entry is gone), but what else it said stays.
        let mark = dict_of(&doc, catalog.get("MarkInfo").unwrap());
        assert!(!mark.contains_key("Marked"));
        assert_eq!(mark.get("UserProperties"), Some(&Object::Bool(true)));
        let page = &doc.pages().unwrap()[0];
        assert!(!page.dict.contains_key("StructParents"));
        let annot = dict_of(&doc, &page.dict.get("Annots").and_then(Object::as_array).unwrap()[0]);
        assert!(!annot.contains_key("StructParent"));
        // The outline item stays, without its structure element.
        let item = dict_of(&doc, dict_of(&doc, catalog.get("Outlines").unwrap()).get("First").unwrap());
        assert!(!item.contains_key("SE") && item.contains_key("Dest"));
        // Keeping every page keeps the tree.
        let out = ops::copy_all(&src).unwrap();
        assert!(contains(&out.data, "STRUCT-MARKER"));
        assert!(open(out.data).catalog().unwrap().contains_key("StructTreeRoot"));
    }

    #[test]
    fn splitting_into_many_files_cuts_the_structures_for_each() {
        let src = open(two_page_form());
        let mut seen: Vec<(usize, bool, bool)> = Vec::new();
        ops::split_every(&src, 1, &mut |n, out| {
            seen.push((n, contains(&out.data, "SECRET-FIELD-APPEARANCE"), contains(&out.data, "KEPT-WIDGET-APPEARANCE")));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, vec![(1, false, true), (2, true, false)]);
    }
}

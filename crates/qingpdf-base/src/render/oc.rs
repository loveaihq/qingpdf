//! Optional content (ISO 32000-1 8.11): which layers are shown by default. The default configuration
//! (`/OCProperties /D`, 8.11.4.3) says which optional content groups (OCGs) are on; a membership
//! dictionary (OCMD, 8.11.2.2) makes content depend on several groups, by a policy `/P` or by a visibility
//! expression `/VE`. Nothing here panics or loops on a hostile file: expressions nest at most
//! [`MAX_VE_DEPTH`] deep and cost at most [`MAX_VE_NODES`] steps, and an object that is not an OCG or
//! OCMD (an OCMD that names itself, say) simply counts for nothing.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::document::Document;
use crate::object::{Dict, ObjRef, Object};
use crate::text::interp::FxMap;

use super::work::{Work, cost};

/// Deepest nesting of a visibility expression followed.
const MAX_VE_DEPTH: usize = 8;
/// Most operands and operators one visibility expression may cost.
const MAX_VE_NODES: usize = 1000;
/// Most groups read from each of the lists of the configuration, and from the `/OCGs` of one membership dictionary
/// (past it the rest are not looked at). What a page may spend on all of them together is the work meter's.
const MAX_LIST: usize = 200_000;
/// Most results kept by object (they are kept for the whole document), and those of dictionaries written in a
/// resource dictionary or a content stream, which are kept for one page.
const MAX_CACHE: usize = 100_000;
const MAX_DIRECT: usize = 4096;
/// The longest dictionary written in a content stream that is remembered by its bytes.
const MAX_DIRECT_KEY: usize = 2048;

/// What names a membership dictionary that is not an object of its own: the resource dictionary it is written in
/// (numbered) and its name there, or its bytes in the content stream.
pub(crate) fn direct_key(resources: u32, name: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(5 + name.len());
    key.push(b'R');
    key.extend_from_slice(&resources.to_le_bytes());
    key.extend_from_slice(name);
    key
}

/// The default configuration of the document's optional content.
pub(crate) struct OcConfig {
    /// The document has `/OCProperties`; without them everything is shown.
    present: bool,
    base_on: bool,
    on: HashSet<ObjRef>,
    off: HashSet<ObjRef>,
    /// Groups an automatic state (`/AS` with the event `View`) sets, and to what.
    auto_view: HashMap<ObjRef, bool>,
    /// Whether a group or a membership dictionary is shown, by object: (it is a group, it is shown).
    states: RefCell<FxMap<ObjRef, (bool, bool)>>,
    /// The same for the membership dictionaries that are written in place (see [`direct_key`]), for the page being drawn.
    direct: RefCell<FxMap<Vec<u8>, bool>>,
}

fn refs_of(doc: &Document, obj: Option<&Object>) -> Vec<ObjRef> {
    let Some(Ok(Object::Array(items))) = obj.map(|o| doc.resolve(o)) else { return Vec::new() };
    items.iter().filter_map(Object::as_obj_ref).take(MAX_LIST).collect()
}

fn is_type(d: &Dict, name: &str) -> bool {
    d.get_name("Type").is_some_and(|t| t.as_bytes() == name.as_bytes())
}

impl OcConfig {
    pub fn load(doc: &Document) -> OcConfig {
        let mut config = OcConfig {
            present: false,
            base_on: true,
            on: HashSet::new(),
            off: HashSet::new(),
            auto_view: HashMap::new(),
            states: RefCell::new(FxMap::default()),
            direct: RefCell::new(FxMap::default()),
        };
        let Ok(catalog) = doc.catalog() else { return config };
        let Some(Ok(Object::Dict(props))) = catalog.get("OCProperties").map(|o| doc.resolve(o)) else { return config };
        config.present = true;
        let Some(Ok(Object::Dict(d))) = props.get("D").map(|o| doc.resolve(o)) else { return config };
        if let Some(Ok(Object::Name(n))) = d.get("BaseState").map(|o| doc.resolve(o)) {
            config.base_on = n.as_bytes() != b"OFF";
        }
        config.on = refs_of(doc, d.get("ON")).into_iter().collect();
        config.off = refs_of(doc, d.get("OFF")).into_iter().collect();
        // 8.11.4.4: an automatic state says that, for the event `View`, the groups of the listed categories follow
        // the `ViewState` of their usage dictionaries.
        if let Some(Ok(Object::Array(auto))) = d.get("AS").map(|o| doc.resolve(o)) {
            for entry in auto.iter().take(64) {
                let Ok(Object::Dict(entry)) = doc.resolve(entry) else { continue };
                let is_view = |key: &str| match entry.get(key).map(|o| doc.resolve(o)) {
                    Some(Ok(Object::Name(n))) => n.as_bytes() == b"View",
                    _ => false,
                };
                let categories_view = match entry.get("Category").map(|o| doc.resolve(o)) {
                    Some(Ok(Object::Array(c))) => c.iter().any(|o| matches!(doc.resolve(o), Ok(Object::Name(n)) if n.as_bytes() == b"View")),
                    _ => false,
                };
                if !is_view("Event") || !categories_view {
                    continue;
                }
                for r in refs_of(doc, entry.get("OCGs")) {
                    let Ok(Object::Dict(group)) = doc.get(r) else { continue };
                    let state = group
                        .get("Usage")
                        .and_then(|u| doc.resolve(u).ok())
                        .and_then(|u| u.as_dict().and_then(|u| u.get("View").and_then(|v| doc.resolve(v).ok())))
                        .and_then(|v| v.as_dict().and_then(|v| v.get("ViewState").and_then(|s| doc.resolve(s).ok())))
                        .and_then(|s| s.as_name().map(|n| n.as_bytes() != b"OFF"));
                    if let Some(on) = state {
                        config.auto_view.insert(r, on);
                    }
                }
            }
        }
        config
    }

    /// Does the document have optional content at all? If not, everything is shown.
    pub fn is_present(&self) -> bool {
        self.present
    }

    /// A page starts: what was remembered of the dictionaries written in place is dropped (their names mean something
    /// else on the next page).
    pub fn start_page(&self) {
        self.direct.borrow_mut().clear();
    }

    /// Is the content tied to `oc` (the value of an `/OC` entry, or a property list of `BDC /OC`: an OCG or an
    /// OCMD, by reference or written in place) shown? Whatever is not one of them shows. An answer is kept for the
    /// object, or for the page by `key` when `oc` is written in place (a [`direct_key`] or [`OcConfig::visible_inline`]).
    /// What it costs is charged to `work`; a page that has none left sees everything.
    pub fn visible(&self, doc: &Document, oc: &Object, key: Option<&[u8]>, work: &mut Work) -> bool {
        if !self.present {
            return true;
        }
        let r = oc.as_obj_ref();
        if let Some(r) = r
            && let Some((_, hit)) = self.states.borrow().get(&r)
        {
            return *hit;
        }
        let key = key.filter(|_| r.is_none());
        if let Some(key) = key
            && let Some(hit) = self.direct.borrow().get(key)
        {
            return *hit;
        }
        // (Read through the page's meter: a reference to something else than a group, a membership dictionary or a
        // dictionary is read once for the page, not for every annotation or operator that names it.)
        let held = work.read(doc, oc);
        let Some(Object::Dict(d)) = held.as_deref() else { return true };
        let (is_group, shown) = if is_type(d, "OCMD") {
            (false, self.membership(doc, d, work))
        } else if is_type(d, "OCG") {
            (true, self.group_on(r, d))
        } else {
            (false, true)
        };
        if work.is_over() {
            // The answer may be a guess: not kept.
            return shown;
        }
        if let Some(r) = r {
            let mut states = self.states.borrow_mut();
            if states.len() >= MAX_CACHE {
                states.clear();
            }
            states.insert(r, (is_group, shown));
        } else if let Some(key) = key {
            let mut direct = self.direct.borrow_mut();
            if direct.len() >= MAX_DIRECT {
                direct.clear();
            }
            direct.insert(key.to_vec(), shown);
        }
        shown
    }

    /// The same for a dictionary written in a content stream (`/OC <<...>> BDC`): `bytes` are the dictionary.
    pub fn visible_inline(&self, doc: &Document, bytes: &[u8], work: &mut Work) -> bool {
        if !self.present {
            return true;
        }
        let key = (bytes.len() <= MAX_DIRECT_KEY).then(|| {
            let mut key = Vec::with_capacity(1 + bytes.len());
            key.push(b'I');
            key.extend_from_slice(bytes);
            key
        });
        if let Some(key) = &key
            && let Some(hit) = self.direct.borrow().get(key)
        {
            return *hit;
        }
        let Ok(obj) = crate::parser::Parser::new(bytes, 0).parse_object() else { return true };
        self.visible(doc, &obj, key.as_deref(), work)
    }

    /// Is the group on in the default configuration (8.11.4.3)?
    fn group_on(&self, r: Option<ObjRef>, d: &Dict) -> bool {
        // A group whose intent is not `View` does not affect what is shown (8.11.2.1, Table 98).
        let for_view = match d.get("Intent") {
            Some(Object::Name(n)) => n.as_bytes() == b"View" || n.as_bytes() == b"All",
            Some(Object::Array(a)) => a.is_empty() || a.iter().any(|o| matches!(o, Object::Name(n) if n.as_bytes() == b"View" || n.as_bytes() == b"All")),
            _ => true,
        };
        if !for_view {
            return true;
        }
        let Some(r) = r else { return true };
        let mut on = self.base_on;
        if self.on.contains(&r) {
            on = true;
        }
        if self.off.contains(&r) {
            on = false;
        }
        if let Some(auto) = self.auto_view.get(&r) {
            on = *auto;
        }
        on
    }

    /// The state of an item in an OCMD's `/OCGs` or in an expression: `None` for anything that is not a group.
    fn group_state(&self, doc: &Document, obj: &Object, work: &mut Work) -> Option<bool> {
        // A group that was looked at before is answered without reading it again.
        if let Some(r) = obj.as_obj_ref()
            && let Some(&(is_group, shown)) = self.states.borrow().get(&r)
        {
            return is_group.then_some(shown);
        }
        let held = work.read(doc, obj);
        let Some(Object::Dict(d)) = held.as_deref() else { return None };
        is_type(d, "OCG").then(|| self.visible(doc, obj, None, work))
    }

    /// 8.11.2.2, Table 99.
    fn membership(&self, doc: &Document, d: &Dict, work: &mut Work) -> bool {
        if !work.charge(cost::OCMD) {
            return true;
        }
        // A visibility expression wins over `/OCGs` and `/P`.
        let ve_held = d.get("VE").and_then(|ve| work.read(doc, ve));
        if let Some(ve) = d.get("VE")
            && let Some(Object::Array(expr)) = ve_held.as_deref()
        {
            let mut nodes = MAX_VE_NODES;
            // An expression that is an object of its own may name itself: that counts for nothing.
            let mut path: Vec<ObjRef> = ve.as_obj_ref().into_iter().collect();
            let value = self.expression(doc, expr, 0, &mut nodes, &mut path, work);
            work.spend((MAX_VE_NODES - nodes) as f64 * cost::OC_NODE);
            return value.unwrap_or(true);
        }
        let states: Vec<bool> = match d.get("OCGs") {
            None => Vec::new(),
            Some(o) => match work.read(doc, o).as_deref() {
                Some(Object::Array(items)) => {
                    // The groups are paid for before they are looked at: a list of 200 thousand is dear however it ends.
                    if !work.charge(items.len().min(MAX_LIST) as f64 * cost::OC_NODE) {
                        return true;
                    }
                    items.iter().take(MAX_LIST).filter_map(|i| self.group_state(doc, i, work)).collect()
                }
                Some(_) => self.group_state(doc, o, work).into_iter().collect(),
                None => Vec::new(),
            },
        };
        if states.is_empty() {
            return true;
        }
        let policy = match d.get("P").and_then(|o| work.read(doc, o)).as_deref() {
            Some(Object::Name(n)) => n.as_bytes().to_vec(),
            _ => b"AnyOn".to_vec(),
        };
        match policy.as_slice() {
            b"AllOn" => states.iter().all(|&s| s),
            b"AnyOff" => states.iter().any(|&s| !s),
            b"AllOff" => states.iter().all(|&s| !s),
            _ => states.iter().any(|&s| s),
        }
    }

    /// 8.11.2.2: `[/And a b ...]`, `[/Or a b ...]`, `[/Not a]`; an operand is a group or another expression.
    fn expression(&self, doc: &Document, items: &[Object], depth: usize, nodes: &mut usize, path: &mut Vec<ObjRef>, work: &mut Work) -> Option<bool> {
        if depth > MAX_VE_DEPTH {
            return None;
        }
        *nodes = nodes.checked_sub(1)?;
        let op_held = work.read(doc, items.first()?)?;
        let Object::Name(op) = &*op_held else { return None };
        let mut operands = Vec::new();
        for item in items.iter().skip(1).take(MAX_LIST) {
            *nodes = nodes.checked_sub(1)?;
            let held = work.read(doc, item);
            let value = match held.as_deref() {
                Some(Object::Array(inner)) => match item.as_obj_ref() {
                    // An array that is being evaluated already: a loop.
                    Some(r) if path.contains(&r) => None,
                    Some(r) => {
                        path.push(r);
                        let v = self.expression(doc, inner, depth + 1, nodes, path, work);
                        path.pop();
                        v
                    }
                    None => self.expression(doc, inner, depth + 1, nodes, path, work),
                },
                Some(Object::Dict(_)) => self.group_state(doc, item, work),
                _ => None,
            };
            operands.extend(value);
        }
        match op.as_bytes() {
            b"And" => Some(operands.iter().all(|&v| v)),
            b"Or" => (!operands.is_empty()).then(|| operands.iter().any(|&v| v)),
            b"Not" => operands.first().map(|v| !v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;

    fn vis(c: &OcConfig, d: &Document, o: &Object) -> bool {
        c.visible(d, o, None, &mut Work::new())
    }

    fn doc(props: &str, objs: &[(u32, &str)]) -> Document {
        let mut b = PdfBuilder::new();
        b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /OCProperties {props} >>"));
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        let mut top = 2;
        for (n, body) in objs {
            b.obj(*n, body);
            top = top.max(*n);
        }
        Document::from_bytes(b.finish_classic(top + 1, "/Root 1 0 R")).expect("opens")
    }

    #[test]
    fn default_states_policies_and_expressions() {
        let d = doc(
            "<< /OCGs [3 0 R 4 0 R] /D << /BaseState /ON /OFF [4 0 R] >> >>",
            &[
                (3, "<< /Type /OCG /Name (on) >>"),
                (4, "<< /Type /OCG /Name (off) >>"),
                (5, "<< /Type /OCMD /OCGs [3 0 R 4 0 R] /P /AllOn >>"),
                (6, "<< /Type /OCMD /OCGs [3 0 R 4 0 R] >>"),
                (7, "<< /Type /OCMD /VE [/And 3 0 R [/Not 4 0 R]] >>"),
                (8, "<< /Type /OCMD /VE [/Or 4 0 R [/Not 3 0 R]] >>"),
                (9, "<< /Type /OCMD /OCGs [9 0 R] >>"),
                (10, "<< /Type /OCMD /VE [/And 10 0 R] >>"),
                (11, "[/And 11 0 R /Not 11 0 R]"),
                (12, "<< /Type /OCMD /VE 11 0 R >>"),
            ],
        );
        let c = OcConfig::load(&d);
        let r = |n: u32| Object::Ref(ObjRef::new(n, 0));
        assert!(vis(&c, &d, &r(3)));
        assert!(!vis(&c, &d, &r(4)));
        assert!(!vis(&c, &d, &r(5)));
        assert!(vis(&c, &d, &r(6)));
        assert!(vis(&c, &d, &r(7)));
        assert!(!vis(&c, &d, &r(8)));
        // An OCMD that names itself, an expression that names itself: counted for nothing, and no loop.
        assert!(vis(&c, &d, &r(9)));
        assert!(vis(&c, &d, &r(10)));
        assert!(vis(&c, &d, &r(12)));
    }

    #[test]
    fn base_state_off_and_automatic_view_state() {
        let d = doc(
            "<< /OCGs [3 0 R 4 0 R 5 0 R] /D << /BaseState /OFF /ON [3 0 R] /AS [<< /Event /View /Category [/View] /OCGs [3 0 R 4 0 R] >>] >> >>",
            &[
                (3, "<< /Type /OCG /Usage << /View << /ViewState /OFF >> >> >>"),
                (4, "<< /Type /OCG /Usage << /View << /ViewState /ON >> >> >>"),
                (5, "<< /Type /OCG /Intent /Design >>"),
            ],
        );
        let c = OcConfig::load(&d);
        let r = |n: u32| Object::Ref(ObjRef::new(n, 0));
        // 3 is on in the list but its usage says off for viewing; 4 is off by the base state but its usage says on.
        assert!(!vis(&c, &d, &r(3)));
        assert!(vis(&c, &d, &r(4)));
        // A group for another intent does not hide anything.
        assert!(vis(&c, &d, &r(5)));
    }

    #[test]
    fn no_properties_means_everything_shows() {
        let mut b = PdfBuilder::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
        b.obj(3, "<< /Type /OCG >>");
        let d = Document::from_bytes(b.finish_classic(4, "/Root 1 0 R")).expect("opens");
        assert!(vis(&OcConfig::load(&d), &d, &Object::Ref(ObjRef::new(3, 0))));
    }
}

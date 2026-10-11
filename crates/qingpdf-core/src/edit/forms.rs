//! Interactive forms (4b, ISO 32000-1 12.7): reading a field with the attributes it inherits, filling it in, and the appearance
//! streams that show the value.
//!
//! A field is found from its widget annotation: the widget and the fields above it (`/Parent`, at most [`MAX_PARENT_DEPTH`] levels,
//! a repeat ends the walk) give `/FT`, `/Ff`, `/V`, `/DA`, `/Q` and `/MaxLen` (12.7.3.1: the nearest wins; the `/AcroForm` has
//! the default `/DA` and `/Q`). The field itself is the nearest node with a `/T`. A field with several widgets shows one value in
//! all of them, so each gets an appearance of its own size and look (`/MK`, 12.5.6.19).
//!
//! What is written into the file always has its own font: a subset of a system font (`fontsub.rs`) is embedded for the characters
//! shown, so that a value typed in Chinese is drawn the same by every reader. Nothing in the form is executed: the `/AA` actions
//! of a field (format, keystroke, calculate, validate; JavaScript) do not run, and a value is kept exactly as it was typed.
//!
//! [`synthesize`] makes the appearance of a widget that has none, for drawing only (nothing is written): a form made by a program
//! that left the drawing to the reader (`NeedAppearances`, or just no `/AP`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use super::fontsub::{self, Look};
use super::layout::{self, Da, Face, Spec, Style};
use super::{Ctx, text_string};
use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dict, Name, ObjRef, Object, Stream};
use crate::render::glyf::TtFont;
use crate::render::sysfont;
use crate::render::work::{Held, Work};

/// Most levels of `/Parent` followed from a widget.
pub(crate) const MAX_PARENT_DEPTH: usize = 32;
/// Most widgets (or children) of one field looked at.
pub(crate) const MAX_KIDS: usize = 5_000;
/// Most options of a choice field read.
pub(crate) const MAX_OPTIONS: usize = 2_000;
/// Most characters of the value of a field.
pub const MAX_VALUE_CHARS: usize = 4_000;
/// Most options of a list box whose characters are put into the font (the ones that can be seen).
const MAX_LISTED_OPTIONS: usize = 200;
/// Widest border drawn, in points.
const MAX_BORDER: f64 = 10.0;
/// Most bytes of the texts of the options of one choice field (export values and shown texts together); the rest are left out.
pub const MAX_OPTION_BYTES: usize = 256 * 1024;
/// Most bytes of parsed `/DA` strings and option lists kept for the widgets that share them; when that is full they are let go of.
const CACHE_BYTES: usize = 4 << 20;
/// What a byte of a `/DA` string costs the work meter, and an option of a choice field (apart from its characters).
const DA_BYTE_COST: f64 = 8.0;
const OPTION_COST: f64 = 500.0;

/// What a field costs the work meter: looked up, and given an appearance (apart from its font and its characters).
const FIELD_COST: f64 = 20_000.0;
pub(crate) const APPEARANCE_COST: f64 = 200_000.0;
const CHAR_COST: f64 = 600.0;
/// What the appearance of a widget that has none costs to make for drawing (nothing is written), apart from what it inherits and
/// its characters: measured, about 20 microseconds with the line-up of the text.
const DRAW_COST: f64 = 30_000.0;

// Field flags (12.7.3.1, Tables 221, 226, 228, 230).
const FF_READ_ONLY: i64 = 1;
const FF_MULTILINE: i64 = 1 << 12;
const FF_PASSWORD: i64 = 1 << 13;
const FF_NO_TOGGLE_TO_OFF: i64 = 1 << 14;
const FF_RADIO: i64 = 1 << 15;
const FF_PUSHBUTTON: i64 = 1 << 16;
const FF_COMBO: i64 = 1 << 17;
const FF_EDIT: i64 = 1 << 18;
const FF_COMB: i64 = 1 << 24;
const FF_RADIOS_IN_UNISON: i64 = 1 << 25;

/// The kinds of field a reader fills in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FieldKind {
    Text = 0,
    Check = 1,
    Radio = 2,
    Push = 3,
    Combo = 4,
    List = 5,
    Signature = 6,
}

/// What is put into a field.
#[derive(Clone, Debug, PartialEq)]
pub enum FillValue {
    /// The text of a text field, or the choice of a combo box or list (an export value or what is shown).
    Text(String),
    /// A check box or radio button on or off.
    Checked(bool),
}

/// The defaults of the `/AcroForm` dictionary, and what is worked out once for the widgets that share it.
pub(crate) struct AcroForm {
    /// The default `/DA`, parsed once.
    pub da: Rc<Da>,
    pub q: i64,
    /// `/DR /Font`: the fonts the form's `/DA` strings name.
    pub fonts: Dict,
    #[allow(dead_code)]
    pub need_appearances: bool,
    cache: RefCell<Cache>,
}

/// Parsed `/DA` strings (by the object that has them) and option lists (by the object that has the `/Opt`): the widgets of a field,
/// and the fields of a form that inherit from one parent, share them instead of reading and parsing them again each.
#[derive(Default)]
struct Cache {
    das: HashMap<u32, Rc<Da>>,
    opts: HashMap<u32, Rc<Options>>,
    bytes: usize,
}

impl Cache {
    /// Make room for `bytes` more: what is kept is let go of when it would not fit (whoever holds a share of it keeps it).
    fn make_room(&mut self, bytes: usize) {
        if self.bytes.saturating_add(bytes) > CACHE_BYTES {
            self.das.clear();
            self.opts.clear();
            self.bytes = 0;
        }
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

/// What reading a form needs besides the field: the document, the form's defaults and caches, and the work meter that everything
/// read from the file is charged to.
#[derive(Clone, Copy)]
pub(crate) struct Env<'a> {
    pub doc: &'a Document,
    pub form: &'a AcroForm,
    pub work: &'a Work,
}

/// A `/DA` string parsed, charged by its length (`None`: the work is used up).
fn parse_charged(work: &Work, bytes: &[u8]) -> Option<Da> {
    work.charge(bytes.len() as f64 * DA_BYTE_COST).then(|| layout::parse_da(bytes))
}

fn res(doc: &Document, d: &Dict, key: &str) -> Option<Object> {
    let r = doc.resolve(d.get(key)?).ok()?;
    if matches!(r, Object::Null) { None } else { Some(r) }
}

fn res_int(doc: &Document, d: &Dict, key: &str) -> Option<i64> {
    res(doc, d, key)?.as_f64().filter(|v| v.is_finite()).map(|v| v as i64)
}

fn res_numbers(doc: &Document, d: &Dict, key: &str) -> Vec<f64> {
    match res(doc, d, key) {
        Some(Object::Array(a)) => a.iter().take(8).filter_map(|o| doc.resolve(o).ok()?.as_f64()).collect(),
        _ => Vec::new(),
    }
}

fn res_dict(doc: &Document, d: &Dict, key: &str) -> Option<Dict> {
    match res(doc, d, key)? {
        Object::Dict(d) => Some(d),
        _ => None,
    }
}

/// The form of the file, `None` for a file that has none. The default `/DA` is parsed here, once, and charged to `work`.
pub(crate) fn acroform(doc: &Document, work: &Work) -> Option<AcroForm> {
    let catalog = doc.catalog().ok()?;
    let form = res_dict(doc, &catalog, "AcroForm")?;
    let da = match res(doc, &form, "DA") {
        Some(Object::String(s)) => parse_charged(work, &s.bytes),
        _ => None,
    };
    let fonts = res_dict(doc, &form, "DR").and_then(|dr| res_dict(doc, &dr, "Font")).unwrap_or_default();
    Some(AcroForm {
        da: Rc::new(da.unwrap_or_else(|| layout::parse_da(b""))),
        q: res_int(doc, &form, "Q").unwrap_or(0),
        fonts,
        need_appearances: matches!(res(doc, &form, "NeedAppearances"), Some(Object::Bool(true))),
        cache: RefCell::new(Cache::default()),
    })
}

/// A dictionary of the field tree that is shared by whoever reads the widgets below it (it is read once, by `Work::resolve`).
#[derive(Clone)]
pub(crate) struct Node(Arc<Object>);

impl Node {
    fn of(d: Dict) -> Node {
        Node(Arc::new(Object::Dict(d)))
    }

    pub(crate) fn dict(&self) -> &Dict {
        static NONE: OnceLock<Dict> = OnceLock::new();
        self.0.as_dict().unwrap_or_else(|| NONE.get_or_init(Dict::new))
    }
}

/// A field as seen from one of its widgets.
pub(crate) struct Field {
    pub kind: FieldKind,
    pub flags: i64,
    /// The field node (the nearest one with a `/T`); `node_num` 0 when it is not an object of its own.
    pub node_num: u32,
    pub node: Node,
    pub widget_num: u32,
    pub widget: Node,
    pub da: Rc<Da>,
    pub q: i64,
    pub max_len: Option<usize>,
    pub value: Option<Arc<Object>>,
    /// The nearest node that has an `/Opt` (and its object number), read when the options are wanted.
    opt_at: Option<(u32, Node)>,
    pub top_index: usize,
}

impl Field {
    pub(crate) fn read_only(&self) -> bool {
        self.flags & FF_READ_ONLY != 0
    }

    pub(crate) fn multiline(&self) -> bool {
        self.kind == FieldKind::Text && self.flags & FF_MULTILINE != 0
    }

    pub(crate) fn password(&self) -> bool {
        self.kind == FieldKind::Text && self.flags & FF_PASSWORD != 0
    }

    pub(crate) fn comb(&self) -> Option<usize> {
        let n = self.max_len?;
        (self.kind == FieldKind::Text && self.flags & FF_COMB != 0 && !self.multiline() && !self.password() && n > 0 && n <= 200).then_some(n)
    }

    pub(crate) fn editable_combo(&self) -> bool {
        self.kind == FieldKind::Combo && self.flags & FF_EDIT != 0
    }
}

/// A value of the file, kept: shared with every other reader of the same object when it is a reference (`Work::resolve` reads it
/// once for the page and charges it by its size), `None` for null or for something that cannot be read.
fn kept(env: Env<'_>, o: &Object) -> Option<Arc<Object>> {
    let held = env.work.read(env.doc, o)?;
    if matches!(&*held, Object::Null) {
        return None;
    }
    Some(match held {
        Held::Shared(a) => a,
        Held::Direct(d) => Arc::new(d.clone()),
    })
}

fn scalar(env: Env<'_>, d: &Dict, key: &str) -> Option<i64> {
    env.work.read(env.doc, d.get(key)?)?.as_f64().filter(|v| v.is_finite()).map(|v| v as i64)
}

fn name_of(env: Env<'_>, d: &Dict, key: &str) -> Option<Vec<u8>> {
    match &*env.work.read(env.doc, d.get(key)?)? {
        Object::Name(n) => Some(n.as_bytes().to_vec()),
        _ => None,
    }
}

/// The parsed `/DA` that the string `o` of the object `at` is: parsed once for all the widgets that inherit it, and charged by its
/// length. `None` when `o` is no string (or the work is used up).
fn da_of(env: Env<'_>, at: u32, o: &Object) -> Option<Rc<Da>> {
    let hit = if at != 0 { env.form.cache.borrow().das.get(&at).cloned() } else { None };
    if hit.is_some() {
        return hit;
    }
    let held = env.work.read(env.doc, o)?;
    let Object::String(s) = &*held else { return None };
    let da = Rc::new(parse_charged(env.work, &s.bytes)?);
    if at != 0 {
        let mut cache = env.form.cache.borrow_mut();
        cache.make_room(256);
        cache.das.insert(at, da.clone());
    }
    Some(da)
}

/// The field a widget belongs to. `None` for something that is no field this reader fills in (no `/FT`, an unknown one, a
/// widget that is no dictionary), or when the work is used up. What the widget inherits is read through the page's meter
/// (`Work::resolve`: each parent is read from the file once, charged by its size, then shared), never copied for each widget.
pub(crate) fn resolve_field(env: Env<'_>, widget_num: u32, widget: Dict) -> Option<Field> {
    #[derive(Default)]
    struct Inherited {
        ft: Option<Vec<u8>>,
        ff: Option<i64>,
        value: Option<Arc<Object>>,
        da: Option<Rc<Da>>,
        q: Option<i64>,
        max_len: Option<usize>,
        opt_at: Option<(u32, Node)>,
        ti: Option<i64>,
    }
    let Inherited { mut ft, mut ff, mut value, mut da, mut q, mut max_len, mut opt_at, mut ti } = Inherited::default();
    let widget = Node::of(widget);
    let mut node: Option<(u32, Node)> = None;
    let mut seen: Vec<u32> = Vec::new();
    let (mut at, mut current) = (widget_num, widget.clone());
    for _ in 0..=MAX_PARENT_DEPTH {
        let d = current.dict();
        if ft.is_none() {
            ft = name_of(env, d, "FT");
        }
        if ff.is_none() {
            ff = scalar(env, d, "Ff");
        }
        if value.is_none()
            && let Some(o) = d.get("V")
        {
            value = kept(env, o);
        }
        if da.is_none()
            && let Some(o) = d.get("DA")
        {
            da = da_of(env, at, o);
        }
        if q.is_none() {
            q = scalar(env, d, "Q");
        }
        if max_len.is_none() {
            max_len = scalar(env, d, "MaxLen").and_then(|n| usize::try_from(n).ok()).filter(|&n| n > 0);
        }
        if opt_at.is_none() && d.contains_key("Opt") {
            opt_at = Some((at, current.clone()));
        }
        if ti.is_none() {
            ti = scalar(env, d, "TI");
        }
        if node.is_none() && d.contains_key("T") {
            node = Some((at, current.clone()));
        }
        let Some(parent) = d.get("Parent").and_then(Object::as_obj_ref) else { break };
        if seen.contains(&parent.num) || parent.num == widget_num {
            break;
        }
        seen.push(parent.num);
        let Some(Held::Shared(next)) = env.work.read(env.doc, &Object::Ref(parent)) else { break };
        if !matches!(&*next, Object::Dict(_)) {
            break;
        }
        (at, current) = (parent.num, Node(next));
    }
    let flags = ff.unwrap_or(0);
    let kind = match ft.as_deref()? {
        b"Tx" => FieldKind::Text,
        b"Sig" => FieldKind::Signature,
        b"Ch" if flags & FF_COMBO != 0 => FieldKind::Combo,
        b"Ch" => FieldKind::List,
        b"Btn" if flags & FF_PUSHBUTTON != 0 => FieldKind::Push,
        b"Btn" if flags & FF_RADIO != 0 => FieldKind::Radio,
        b"Btn" => FieldKind::Check,
        _ => return None,
    };
    let (node_num, node) = node.unwrap_or((widget_num, widget.clone()));
    Some(Field {
        kind,
        flags,
        node_num,
        node,
        widget_num,
        widget,
        da: da.unwrap_or_else(|| env.form.da.clone()),
        q: q.unwrap_or(env.form.q).clamp(0, 2),
        max_len,
        value,
        opt_at,
        top_index: ti.and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
    })
}

fn decode_text(bytes: &[u8]) -> String {
    // (Only what can be kept is turned into text: four bytes a character is the most any of the encodings below uses.)
    let bytes = bytes.get(..MAX_VALUE_CHARS * 4 + 4).unwrap_or(bytes);
    let utf16 = |rest: &[u8], big: bool| -> String {
        let units = rest.as_chunks::<2>().0.iter().map(|p| if big { u16::from_be_bytes(*p) } else { u16::from_le_bytes(*p) });
        char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect()
    };
    let text: String = if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, true)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, false)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| if matches!(b, b'\r' | b'\n') { char::from(b) } else { crate::info::pdf_doc_char(b) }).collect()
    };
    text.replace("\r\n", "\n").replace('\r', "\n").chars().map(|c| if c.is_control() && c != '\n' { ' ' } else { c }).take(MAX_VALUE_CHARS).collect()
}

/// The text a value stands for (a string, or a stream of text; a name is its letters).
pub(crate) fn text_of(doc: &Document, o: &Object) -> String {
    match o {
        Object::String(s) => decode_text(&s.bytes),
        Object::Name(n) => String::from_utf8_lossy(n.as_bytes()).into_owned(),
        Object::Stream(s) => doc.decode_stream(s).map(|b| decode_text(&b)).unwrap_or_default(),
        _ => String::new(),
    }
}

/// The options of a choice field: (export value, what is shown), and whether some were left out ([`MAX_OPTIONS`] of them, their
/// texts together [`MAX_OPTION_BYTES`], or the work ran out).
#[derive(Default)]
pub(crate) struct Options {
    pub items: Vec<(String, String)>,
    pub truncated: bool,
    bytes: usize,
}

/// The text of an option: a string or a name.
fn option_text(o: &Object) -> Option<String> {
    match o {
        Object::String(s) => Some(decode_text(&s.bytes)),
        Object::Name(n) => Some(String::from_utf8_lossy(n.as_bytes()).into_owned()),
        _ => None,
    }
}

/// The options of the node `holder`, read through the meter (each item charged by its size and its characters).
fn read_options(env: Env<'_>, holder: &Node) -> Options {
    let mut out = Options::default();
    let Some(held) = holder.dict().get("Opt").and_then(|o| env.work.read(env.doc, o)) else { return out };
    let Object::Array(items) = &*held else { return out };
    for item in items {
        if out.items.len() >= MAX_OPTIONS || !env.work.charge(OPTION_COST) {
            out.truncated = true;
            break;
        }
        let Some(item) = env.work.read(env.doc, item) else { continue };
        let (export, shown) = match &*item {
            Object::Array(pair) => {
                let Some(export) = pair.first().and_then(|o| env.work.read(env.doc, o)).and_then(|o| option_text(&o)) else { continue };
                let shown = pair.get(1).and_then(|o| env.work.read(env.doc, o)).and_then(|o| option_text(&o)).unwrap_or_else(|| export.clone());
                (export, shown)
            }
            other => {
                let t = option_text(other).unwrap_or_default();
                (t.clone(), t)
            }
        };
        let size = export.len() + shown.len() + 48;
        if out.bytes.saturating_add(size) > MAX_OPTION_BYTES || !env.work.charge(CHAR_COST * (export.chars().count() + shown.chars().count()) as f64) {
            out.truncated = true;
            break;
        }
        out.bytes += size;
        out.items.push((export, shown));
    }
    out
}

/// The options of a choice field, shared by all the widgets (and fields) that inherit the same `/Opt`: read once.
pub(crate) fn options(env: Env<'_>, field: &Field) -> Rc<Options> {
    let Some((at, holder)) = &field.opt_at else { return Rc::new(Options::default()) };
    let hit = if *at != 0 { env.form.cache.borrow().opts.get(at).cloned() } else { None };
    if let Some(hit) = hit {
        return hit;
    }
    let options = Rc::new(read_options(env, holder));
    if *at != 0 {
        let mut cache = env.form.cache.borrow_mut();
        cache.make_room(options.bytes + 64);
        cache.opts.insert(*at, options.clone());
    }
    options
}

/// The value of a text or choice field as text (a choice shows what its export value is called).
pub(crate) fn value_text(env: Env<'_>, field: &Field) -> String {
    let raw = match field.value.as_deref() {
        Some(Object::Array(items)) => items.first().and_then(|o| env.work.read(env.doc, o)).map(|o| text_of(env.doc, &o)).unwrap_or_default(),
        Some(o) => text_of(env.doc, o),
        None => String::new(),
    };
    if matches!(field.kind, FieldKind::Combo | FieldKind::List)
        && let Some((_, shown)) = options(env, field).items.iter().find(|(export, _)| *export == raw)
    {
        return shown.clone();
    }
    raw
}

// --- how a widget looks -------------------------------------------------------------------------------------------

/// The size of a widget (its `/Rect`), `None` for one without an area.
pub(crate) fn widget_size(doc: &Document, widget: &Dict) -> Option<(f64, f64)> {
    let r = res_numbers(doc, widget, "Rect");
    let [a, b, c, d] = <[f64; 4]>::try_from(r.as_slice()).ok()?;
    let (w, h) = ((c - a).abs(), (d - b).abs());
    (w.is_finite() && h.is_finite() && w >= 1.0 && h >= 1.0 && w <= 1.0e5 && h <= 1.0e5).then_some((w, h))
}

/// The `/MK` and border of a widget: how it is drawn apart from its contents, and how it is turned (0, 90, 180, 270).
pub(crate) fn style_of(doc: &Document, widget: &Dict) -> (Style, i64) {
    let mk = res_dict(doc, widget, "MK").unwrap_or_default();
    let mut style = Style {
        bg: layout::color_op(&res_numbers(doc, &mk, "BG"), false),
        bc: layout::color_op(&res_numbers(doc, &mk, "BC"), true),
        width: 1.0,
        ..Style::default()
    };
    if let Some(Object::String(s)) = res(doc, &mk, "CA") {
        style.caption = decode_text(&s.bytes);
    }
    // /BS (12.5.4): width and style; without it the border array's third number is the width.
    if let Some(bs) = res_dict(doc, widget, "BS") {
        if let Some(w) = res(doc, &bs, "W").and_then(|w| w.as_f64()) {
            style.width = w;
        }
        match res(doc, &bs, "S") {
            Some(Object::Name(n)) if n == "D" => style.dashed = true,
            Some(Object::Name(n)) if n == "U" => style.underline = true,
            _ => {}
        }
    } else if let Some(w) = res_numbers(doc, widget, "Border").get(2) {
        style.width = *w;
    }
    style.width = if style.width.is_finite() { style.width.clamp(0.0, MAX_BORDER) } else { 1.0 };
    let rotate = match res_int(doc, &mk, "R").unwrap_or(0).rem_euclid(360) {
        90 => 90,
        180 => 180,
        270 => 270,
        _ => 0,
    };
    (style, rotate)
}

/// The drawing space of a widget of `w` by `h` turned by `rotate` (`/MK /R`): its size and the matrix that puts it on the widget.
pub(crate) fn local_frame(w: f64, h: f64, rotate: i64) -> (f64, f64, [f64; 6]) {
    match rotate {
        90 => (h, w, [0.0, 1.0, -1.0, 0.0, w, 0.0]),
        180 => (w, h, [-1.0, 0.0, 0.0, -1.0, w, h]),
        270 => (h, w, [0.0, -1.0, 1.0, 0.0, 0.0, h]),
        _ => (w, h, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
    }
}

/// The text a field shows: its value, or stars for a password.
fn shown_text(field: &Field, text: &str) -> String {
    if field.password() { text.chars().filter(|&c| c != '\n').map(|_| '*').collect() } else { text.to_string() }
}

/// The content stream of a text, combo or list widget, laid out with `face` (the font is `/F0` of the form): `shown` is the text of a
/// text or combo field, a list shows `options` (those `chosen` says so of marked). The flag says that lines were left out.
#[allow(clippy::too_many_arguments)]
fn field_content(field: &Field, style: &Style, face: &dyn Face, lw: f64, lh: f64, shown: &str, options: &[(String, String)], chosen: &dyn Fn(&str, &str) -> bool) -> (String, bool) {
    let inset = if style.bc.is_some() { style.width } else { 0.0 };
    let mut s = layout::frame(style, lw, lh, false);
    let mut cut = false;
    if field.kind == FieldKind::List {
        s.push_str(&layout::list_content(face, options, chosen, field.top_index, &field.da, lw, lh, inset, "F0"));
    } else {
        let spec = Spec { text: shown, multiline: field.multiline(), q: field.q, comb: field.comb() };
        let (body, lines_cut) = layout::text_content(face, &spec, &field.da, lw, lh, inset, "F0");
        s.push_str(&body);
        cut = lines_cut;
    }
    (s, cut)
}

// --- reading the form for a reader's window -------------------------------------------------------------------------

/// What the window needs to know to let a person fill a field in.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldInfo {
    /// The widget's place in the page's `/Annots`, and its object number (what [`crate::view::Engine::fill_field`] asks for).
    pub index: u32,
    pub num: u32,
    pub kind: FieldKind,
    /// `[left, top, right, bottom]` in points on the page as it is shown.
    pub rect: [f32; 4],
    pub read_only: bool,
    pub multiline: bool,
    pub password: bool,
    pub comb: bool,
    pub max_len: u32,
    /// The text (text fields, combo boxes, lists) or whether it is on (check boxes and radio buttons).
    pub value: String,
    pub checked: bool,
    /// A combo box the person may type in.
    pub editable: bool,
    /// The name of the field (its `/T`).
    pub name: String,
}

/// Does the widget say it is on? (`/AS`, else the value of the field, 12.7.4.2.3.)
pub(crate) fn is_on(doc: &Document, field: &Field) -> bool {
    match res(doc, field.widget.dict(), "AS") {
        Some(Object::Name(n)) => n != "Off",
        _ => match field.value.as_deref() {
            Some(Object::Name(n)) => n != "Off" && field.node_num == field.widget_num,
            _ => false,
        },
    }
}

/// The information on one widget, `None` for a widget that is no field the reader fills in (signatures are listed, so that
/// the reader can say they are not filled in here). The options of a choice are not in it ([`options_of`] gives them).
pub(crate) fn describe(env: Env<'_>, widget_num: u32, widget: &Dict, rect: [f32; 4], index: u32) -> Option<FieldInfo> {
    let doc = env.doc;
    let field = resolve_field(env, widget_num, widget.clone())?;
    let name = match res(doc, field.node.dict(), "T") {
        Some(Object::String(s)) => decode_text(&s.bytes),
        _ => String::new(),
    };
    let value = match field.kind {
        FieldKind::Text | FieldKind::Combo | FieldKind::List => value_text(env, &field),
        _ => String::new(),
    };
    Some(FieldInfo {
        index,
        num: widget_num,
        kind: field.kind,
        rect,
        read_only: field.read_only(),
        multiline: field.multiline(),
        password: field.password(),
        comb: field.comb().is_some(),
        max_len: field.max_len.map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
        value,
        checked: matches!(field.kind, FieldKind::Check | FieldKind::Radio) && is_on(doc, &field),
        editable: field.editable_combo(),
        name,
    })
}

/// The options of the choice field that widget `widget_num` belongs to, for a person who is about to choose: (export value, shown
/// text), at most [`MAX_OPTION_BYTES`] of text (the flag says some were left out). Empty for a field that is no choice.
pub(crate) fn options_of(env: Env<'_>, widget_num: u32, widget: &Dict) -> (Vec<(String, String)>, bool) {
    let Some(field) = resolve_field(env, widget_num, widget.clone()) else { return (Vec::new(), false) };
    if !matches!(field.kind, FieldKind::Combo | FieldKind::List) {
        return (Vec::new(), false);
    }
    let options = options(env, &field);
    (options.items.clone(), options.truncated)
}

// --- filling in ---------------------------------------------------------------------------------------------------

/// The widgets of a field: its children that are widgets, or the field itself when it is its own widget. The widget the person
/// used is always among them.
fn widgets_of(ctx: &Ctx<'_>, field: &Field) -> Result<Vec<(u32, Dict)>> {
    let mut out: Vec<(u32, Dict)> = Vec::new();
    if let Some(Object::Array(kids)) = res(ctx.doc, field.node.dict(), "Kids") {
        for kid in kids.iter().take(MAX_KIDS) {
            if !ctx.work.charge(FIELD_COST) {
                return Err(Error::Limit("the form took too much work".to_string()));
            }
            let Object::Ref(r) = kid else { continue };
            let Ok(Object::Dict(d)) = ctx.doc.get(*r) else { continue };
            if d.contains_key("Rect") && !d.contains_key("T") {
                out.push((r.num, d));
            }
        }
    }
    if out.is_empty() || !out.iter().any(|(n, _)| *n == field.widget_num) {
        out.retain(|(n, _)| *n != field.widget_num);
        out.push((field.widget_num, field.widget.dict().clone()));
    }
    Ok(out)
}

/// Make a form XObject with `content`, the matrix and resources given.
fn add_form(ctx: &mut Ctx<'_>, bbox: [f64; 4], matrix: [f64; 6], content: &str, resources: Dict) -> Result<ObjRef> {
    let r = ctx.alloc()?;
    let mut dict = Dict::new();
    dict.set("Type", Object::from("XObject"));
    dict.set("Subtype", Object::from("Form"));
    dict.set("BBox", super::reals(&bbox));
    dict.set("Matrix", super::reals(&matrix));
    dict.set("Resources", Object::Dict(resources));
    dict.set("Filter", Object::from("FlateDecode"));
    let data = miniz_oxide::deflate::compress_to_vec_zlib(content.as_bytes(), 6);
    ctx.objects.push((r.num, Object::Stream(Stream { dict, data })));
    Ok(r)
}

fn set_normal_appearance(widget: &mut Dict, doc: &Document, n: Object) {
    let mut ap = res_dict(doc, widget, "AP").unwrap_or_default();
    ap.remove("D");
    ap.remove("R");
    ap.set("N", n);
    widget.set("AP", Object::Dict(ap));
}

/// A widget list, as dictionaries to write by object number (a field that is its own widget is one entry).
struct Changes {
    dicts: BTreeMap<u32, Dict>,
}

impl Changes {
    fn new() -> Changes {
        Changes { dicts: BTreeMap::new() }
    }

    fn get(&mut self, num: u32, original: &Dict) -> &mut Dict {
        self.dicts.entry(num).or_insert_with(|| original.clone())
    }

    fn write(self, ctx: &mut Ctx<'_>) {
        ctx.objects.extend(self.dicts.into_iter().map(|(n, d)| (n, Object::Dict(d))));
    }
}

/// Fill in the widget `widget` (the `index`-th entry of the page's `/Annots`, which must be object `num`) with `value`.
pub(super) fn fill(ctx: &mut Ctx<'_>, widget_ref: ObjRef, widget: Dict, value: &FillValue) -> Result<()> {
    let (doc, work) = (ctx.doc, ctx.work);
    let form = acroform(doc, work).ok_or_else(|| Error::Invalid("the file has no form".to_string()))?;
    if !work.charge(FIELD_COST) {
        return Err(Error::Limit("the form took too much work".to_string()));
    }
    let env = Env { doc, form: &form, work };
    let Some(field) = resolve_field(env, widget_ref.num, widget) else {
        return Err(if work.is_over() { Error::Limit("the form took too much work".to_string()) } else { Error::Invalid("this is not a form field this reader fills in".to_string()) });
    };
    if field.read_only() {
        return Err(Error::Invalid("the field is read-only".to_string()));
    }
    if field.node_num == 0 {
        return Err(Error::Invalid("the field is not an object of its own".to_string()));
    }
    match (field.kind, value) {
        (FieldKind::Text | FieldKind::Combo | FieldKind::List, FillValue::Text(t)) => fill_text(ctx, env, &field, t),
        (FieldKind::Check | FieldKind::Radio, FillValue::Checked(on)) => fill_button(ctx, &field, *on),
        (FieldKind::Push | FieldKind::Signature, _) => Err(Error::Invalid("this kind of field is not filled in".to_string())),
        _ => Err(Error::Invalid("this value does not suit the field".to_string())),
    }
}

fn fill_text(ctx: &mut Ctx<'_>, env: Env<'_>, field: &Field, text: &str) -> Result<()> {
    // The text as the field takes it: line ends only in a multi-line field, no other control characters.
    let multiline = field.multiline();
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut clean = String::new();
    for c in normalized.chars() {
        match c {
            '\n' if multiline => clean.push('\n'),
            '\n' | '\t' => clean.push(' '),
            c if c.is_control() => {}
            c => clean.push(c),
        }
        if clean.chars().count() > MAX_VALUE_CHARS {
            return Err(Error::Limit(format!("a field takes at most {MAX_VALUE_CHARS} characters")));
        }
    }
    if let Some(max) = field.max_len.filter(|_| field.kind == FieldKind::Text)
        && clean.chars().count() > max
    {
        return Err(Error::Invalid(format!("the field takes at most {max} characters")));
    }
    // What is stored (the export value) and what is shown.
    let opts: Rc<Options> = if field.kind == FieldKind::Text { Rc::new(Options::default()) } else { options(env, field) };
    let (stored, shown, index) = match field.kind {
        FieldKind::Text => (clean.clone(), shown_text(field, &clean), None),
        _ => match opts.items.iter().position(|(export, _)| *export == clean).or_else(|| opts.items.iter().position(|(_, display)| *display == clean)) {
            Some(i) => {
                let (export, display) = opts.items.get(i).cloned().unwrap_or_default();
                (export, display, Some(i))
            }
            None if field.editable_combo() => (clean.clone(), clean.clone(), None),
            None => return Err(Error::Invalid("that is not one of the choices".to_string())),
        },
    };

    // The font: a subset of a system font, for the characters shown.
    let mut chars: BTreeSet<char> = shown.chars().filter(|&c| c != '\n').collect();
    if field.kind == FieldKind::List {
        // A list shows its options, not just the one chosen.
        for (_, display) in opts.items.iter().skip(field.top_index).take(MAX_LISTED_OPTIONS) {
            chars.extend(display.chars().filter(|c| !c.is_control()));
        }
    }
    let embedded = if chars.is_empty() {
        None
    } else {
        let font = fontsub::choose(&chars, Look::of_name(&field.da.font))?;
        Some(fontsub::embed(&font, &chars, ctx.work)?)
    };
    let font_ref = match &embedded {
        Some(e) => Some(fontsub::add_objects(ctx, e)?),
        None => None,
    };

    let mut changes = Changes::new();
    {
        let node = changes.get(field.node_num, field.node.dict());
        // (A rich text value, `/RV`, would show the old text in readers that prefer it.)
        node.remove("RV");
        node.set("V",if stored.is_empty() && field.kind == FieldKind::Text { text_string("") } else { text_string(&stored.replace('\n', "\r")) });
        match index {
            Some(i) if field.kind == FieldKind::List => node.set("I", Object::Array(vec![Object::Integer(i64::try_from(i).unwrap_or(0))])),
            _ => {
                node.remove("I");
            }
        }
    }
    for (num, wdict) in widgets_of(ctx, field)? {
        let (w, h) = widget_size(ctx.doc, &wdict).ok_or_else(|| Error::Invalid("a widget of the field has no area".to_string()))?;
        if !ctx.work.charge(APPEARANCE_COST + CHAR_COST * shown.chars().count() as f64) {
            return Err(Error::Limit("the form took too much work".to_string()));
        }
        let (style, rotate) = style_of(ctx.doc, &wdict);
        let (lw, lh, matrix) = local_frame(w, h, rotate);
        let content = match &embedded {
            Some(e) => {
                let (content, cut) = field_content(field, &style, e, lw, lh, &shown, &opts.items, &|_, display| display == shown);
                if cut {
                    return Err(Error::Limit(format!("the text takes more than {} lines to show", layout::MAX_LINES)));
                }
                content
            }
            None => format!("{}/Tx BMC\nq\nQ\nEMC\n", layout::frame(&style, lw, lh, false)),
        };
        let mut resources = Dict::new();
        if let Some(f) = font_ref {
            let mut fonts = Dict::new();
            fonts.set("F0", Object::Ref(f));
            resources.set("Font", Object::Dict(fonts));
        }
        let ap = add_form(ctx, [0.0, 0.0, lw, lh], matrix, &content, resources)?;
        let target = if num == field.node_num { changes.get(field.node_num, field.node.dict()) } else { changes.get(num, &wdict) };
        set_normal_appearance(target, ctx.doc, Object::Ref(ap));
    }
    changes.write(ctx);
    Ok(())
}



/// The name of the state of a check box or radio button that is on: the one in its normal appearance that is not `Off`.
fn on_state(doc: &Document, widget: &Dict) -> Option<Vec<u8>> {
    let ap = res_dict(doc, widget, "AP")?;
    let Some(Object::Dict(states)) = res(doc, &ap, "N") else { return None };
    states.iter().map(|(k, _)| k.as_bytes()).find(|k| *k != b"Off").map(<[u8]>::to_vec)
}

/// An `/AP` for a check box or radio button that has none: the states `Off` and `on`.
fn button_ap(ctx: &mut Ctx<'_>, field: &Field, widget: &Dict, on: &str, radio: bool) -> Result<Object> {
    let (w, h) = widget_size(ctx.doc, widget).ok_or_else(|| Error::Invalid("a widget of the field has no area".to_string()))?;
    if !ctx.work.charge(2.0 * APPEARANCE_COST) {
        return Err(Error::Limit("the form took too much work".to_string()));
    }
    let (style, rotate) = style_of(ctx.doc, widget);
    let (lw, lh, matrix) = local_frame(w, h, rotate);
    let frame = layout::frame(&style, lw, lh, radio);
    let on_content = format!("{frame}{}", layout::mark(&style, &field.da.color, lw, lh, radio));
    let off = add_form(ctx, [0.0, 0.0, lw, lh], matrix, &frame, Dict::new())?;
    let on_ref = add_form(ctx, [0.0, 0.0, lw, lh], matrix, &on_content, Dict::new())?;
    let mut states = Dict::new();
    states.set("Off", Object::Ref(off));
    states.set(Name::from(on), Object::Ref(on_ref));
    let mut ap = Dict::new();
    ap.set("N", Object::Dict(states));
    Ok(Object::Dict(ap))
}

fn fill_button(ctx: &mut Ctx<'_>, field: &Field, on: bool) -> Result<()> {
    let radio = field.kind == FieldKind::Radio;
    let mut changes = Changes::new();
    let own = field.node_num == field.widget_num;
    // The widgets of a radio group; a check box or a radio button that is its own widget is a group of one.
    let group: Vec<(u32, Dict)> = if radio && !own { widgets_of(ctx, field)? } else { vec![(field.widget_num, field.widget.dict().clone())] };
    let mut names: Vec<(u32, Vec<u8>)> = Vec::new();
    for (i, (num, wdict)) in group.iter().enumerate() {
        let name = match on_state(ctx.doc, wdict) {
            Some(n) => n,
            None => {
                // No appearance for the on state: made here, with a name for the state (the position in the group for a radio button).
                let name = if radio { i.to_string() } else { "Yes".to_string() };
                let ap = button_ap(ctx, field, wdict, &name, radio)?;
                changes.get(*num, wdict).set("AP", ap);
                name.into_bytes()
            }
        };
        names.push((*num, name));
    }
    let chosen = names.iter().find(|(n, _)| *n == field.widget_num).map(|(_, name)| name.clone()).unwrap_or_default();
    if !on && radio && field.flags & FF_NO_TOGGLE_TO_OFF != 0 {
        return Err(Error::Invalid("a radio button of this group cannot be turned off".to_string()));
    }
    let unison = field.flags & FF_RADIOS_IN_UNISON != 0;
    for (num, name) in &names {
        let is_on = on && (*num == field.widget_num || (unison && *name == chosen));
        let wdict = group.iter().find(|(n, _)| n == num).map(|(_, d)| d).unwrap_or_else(|| field.widget.dict());
        let state = if is_on { name.clone() } else { b"Off".to_vec() };
        changes.get(*num, wdict).set("AS", Object::Name(Name::new(state)));
    }
    let value = if on { chosen } else { b"Off".to_vec() };
    changes.get(field.node_num, field.node.dict()).set("V", Object::Name(Name::new(value)));
    changes.write(ctx);
    Ok(())
}

// --- an appearance for drawing only -----------------------------------------------------------------------------------

/// The widths of a system font, and the encoding that goes with a simple font of the form (WinAnsi).
struct SystemFace {
    font: Option<Arc<TtFont>>,
}

/// The byte a character has in WinAnsiEncoding (Annex D), `None` for one the encoding lacks.
fn win_ansi(c: char) -> Option<u8> {
    const HIGH: [char; 32] = [
        '€', '\u{FFFD}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{FFFD}', 'Ž', '\u{FFFD}', '\u{FFFD}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{FFFD}', 'ž', 'Ÿ',
    ];
    match c as u32 {
        0x20..=0x7E | 0xA0..=0xFF => u8::try_from(c as u32).ok(),
        _ => HIGH.iter().position(|&h| h == c && h != '\u{FFFD}').and_then(|p| u8::try_from(0x80 + p).ok()),
    }
}

impl Face for SystemFace {
    fn width(&self, c: char) -> f64 {
        self.font
            .as_ref()
            .and_then(|f| f.cmap.as_ref()?.unicode(c as u32).and_then(|g| f.advance(g)))
            .map_or(500.0, |a| f64::from(a) * 1000.0)
    }

    fn ascent_descent(&self) -> (f64, f64) {
        match &self.font {
            Some(f) => {
                let (a, d) = f.ascender_descender();
                let em = f.units_per_em();
                if a == 0 && d == 0 { (0.8, -0.2) } else { (f64::from(a) / em, f64::from(d) / em) }
            }
            None => (0.8, -0.2),
        }
    }

    fn encode(&self, text: &str) -> String {
        let mut s = String::from("<");
        for c in text.chars() {
            let _ = write!(s, "{:02X}", win_ansi(c).unwrap_or(b'?'));
        }
        s.push('>');
        s
    }
}

/// A system Chinese font used without being embedded (for drawing a field that has no appearance): the text is spelt in CIDs of the
/// Adobe-GB1 collection, which is what a font that is not embedded but says it is of that collection is drawn from.
struct CjkFace {
    font: Option<Arc<TtFont>>,
    /// The CID of each character and its width in thousandths of an em.
    cids: std::collections::BTreeMap<char, (u32, u16)>,
}

impl CjkFace {
    fn new(text: &str) -> CjkFace {
        let font = sysfont::load(sysfont::Request { kind: sysfont::Kind::Cjk(sysfont::Script::Gb1, sysfont::Cjk::Song), bold: false, italic: false });
        let gb1 = crate::text::data::Ordering::Gb1;
        let mut cids = std::collections::BTreeMap::new();
        for c in text.chars().filter(|&c| c != '\n').take(MAX_VALUE_CHARS * 2) {
            let cid = gb1.unicode_to_cid(c as u32).or_else(|| gb1.unicode_to_cid('?' as u32)).unwrap_or(1);
            let width = font
                .as_ref()
                .and_then(|f| f.cmap.as_ref()?.unicode(c as u32).and_then(|g| f.advance(g)))
                .map_or(if c as u32 >= 0x2E80 { 1000.0 } else { 500.0 }, |a| f64::from(a) * 1000.0);
            cids.insert(c, (cid, width.round().clamp(0.0, 3000.0) as u16));
        }
        CjkFace { font, cids }
    }

    /// The font, as a font of the form would be: a Type 0 font with `Identity-H`, the widths of the characters used.
    fn font_dict(&self) -> Object {
        let mut info = Dict::new();
        info.set("Registry", Object::String(crate::object::PdfString::literal(b"Adobe".to_vec())));
        info.set("Ordering", Object::String(crate::object::PdfString::literal(b"GB1".to_vec())));
        info.set("Supplement", Object::Integer(5));
        let mut cid_font = Dict::new();
        cid_font.set("Type", Object::from("Font"));
        cid_font.set("Subtype", Object::from("CIDFontType2"));
        cid_font.set("BaseFont", Object::from("SimSun"));
        cid_font.set("CIDSystemInfo", Object::Dict(info));
        cid_font.set("DW", Object::Integer(1000));
        let mut widths: Vec<(u32, u16)> = self.cids.values().copied().collect();
        widths.sort_unstable();
        widths.dedup_by_key(|w| w.0);
        cid_font.set("W", Object::Array(widths.iter().flat_map(|&(cid, w)| [Object::Integer(i64::from(cid)), Object::Array(vec![Object::Integer(i64::from(w))])]).collect()));
        let mut font = Dict::new();
        font.set("Type", Object::from("Font"));
        font.set("Subtype", Object::from("Type0"));
        font.set("BaseFont", Object::from("SimSun"));
        font.set("Encoding", Object::from("Identity-H"));
        font.set("DescendantFonts", Object::Array(vec![Object::Dict(cid_font)]));
        Object::Dict(font)
    }
}

impl Face for CjkFace {
    fn width(&self, c: char) -> f64 {
        self.cids.get(&c).map_or(500.0, |&(_, w)| f64::from(w))
    }

    fn ascent_descent(&self) -> (f64, f64) {
        SystemFace { font: self.font.clone() }.ascent_descent()
    }

    fn encode(&self, text: &str) -> String {
        let mut s = String::from("<");
        for c in text.chars() {
            let _ = write!(s, "{:04X}", self.cids.get(&c).map_or(1, |&(cid, _)| cid));
        }
        s.push('>');
        s
    }
}

/// The appearance a widget would have if it had one, for drawing (nothing is written into the file): a form XObject made as the
/// `/DA`, `/MK` and value say, with the font the `/DA` names in the form's `/DR` (Helvetica when it names none). `None` for
/// a field this does not draw (signatures, fields of no kind) or a widget without an area. The flag says that the text has more
/// lines than are laid out, and the rest are left out.
pub(crate) fn synthesize(doc: &Document, form: &AcroForm, widget_num: u32, widget: &Dict, work: &Work) -> Option<(Stream, bool)> {
    if !work.charge(FIELD_COST + DRAW_COST) {
        return None;
    }
    let env = Env { doc, form, work };
    let field = resolve_field(env, widget_num, widget.clone())?;
    let (w, h) = widget_size(doc, widget)?;
    let (style, rotate) = style_of(doc, widget);
    let (lw, lh, matrix) = local_frame(w, h, rotate);
    let da = field.da.clone();
    // The font of the form the /DA names, and one like it to measure with.
    let font_obj = (!da.font.is_empty()).then(|| form.fonts.get(&String::from_utf8_lossy(&da.font))).flatten().cloned();
    let base: Vec<u8> = match font_obj.as_ref().and_then(|o| doc.resolve(o).ok()) {
        Some(Object::Dict(d)) if d.get_name("Subtype").is_some_and(|s| s != "Type0") => d.get_name("BaseFont").map_or_else(|| da.font.clone(), |n| n.as_bytes().to_vec()),
        _ => da.font.clone(),
    };
    let usable = font_obj.as_ref().and_then(|o| doc.resolve(o).ok()).is_some_and(|o| matches!(&o, Object::Dict(d) if d.get_name("Subtype").is_some_and(|s| s != "Type0")));
    // What is going to be drawn: a text that WinAnsiEncoding cannot spell (Chinese, say) is drawn with a system font that is not
    // embedded, as a font of the Adobe-GB1 collection (its character codes are CIDs, which the renderer maps to Unicode again).
    let choice = matches!(field.kind, FieldKind::Combo | FieldKind::List);
    let opts: Rc<Options> = if choice { options(env, &field) } else { Rc::new(Options::default()) };
    let mut shown = String::new();
    match field.kind {
        FieldKind::Text | FieldKind::Combo => shown.push_str(&shown_text(&field, &value_text(env, &field))),
        // (A list is drawn from its options that can be seen.)
        FieldKind::List => opts.items.iter().skip(field.top_index).take(MAX_LISTED_OPTIONS).for_each(|(_, display)| shown.push_str(display)),
        FieldKind::Push => shown.push_str(&style.caption),
        _ => {}
    }
    let (face, font): (Box<dyn Face>, Object) = if shown.chars().any(|c| c != '\n' && win_ansi(c).is_none()) {
        let face = CjkFace::new(&shown);
        let font = face.font_dict();
        (Box::new(face), font)
    } else {
        let request = sysfont::request_for(&base, 32, 0.0, 0.0, None);
        let font = if usable {
            font_obj.unwrap_or(Object::Null)
        } else {
            let mut d = Dict::new();
            d.set("Type", Object::from("Font"));
            d.set("Subtype", Object::from("Type1"));
            d.set("BaseFont", Object::from("Helvetica"));
            d.set("Encoding", Object::from("WinAnsiEncoding"));
            Object::Dict(d)
        };
        (Box::new(SystemFace { font: sysfont::load(request) }), font)
    };
    let face = &*face;
    let mut cut = false;
    let body = match field.kind {
        FieldKind::Text | FieldKind::Combo | FieldKind::List => {
            if !work.charge(CHAR_COST * shown.chars().count() as f64) {
                return None;
            }
            let chosen: Vec<String> = match field.value.as_deref() {
                Some(Object::Array(items)) if field.kind == FieldKind::List => items.iter().take(MAX_OPTIONS).filter_map(|o| work.read(doc, o)).map(|o| text_of(doc, &o)).collect(),
                Some(o) if field.kind == FieldKind::List => vec![text_of(doc, o)],
                _ => Vec::new(),
            };
            let (content, lines_cut) = field_content(&field, &style, face, lw, lh, &shown, &opts.items, &|export, _| chosen.iter().any(|c| c == export));
            cut = lines_cut;
            content
        }
        FieldKind::Check | FieldKind::Radio => {
            let radio = field.kind == FieldKind::Radio;
            let mut s = layout::frame(&style, lw, lh, radio);
            if is_on(doc, &field) {
                s.push_str(&layout::mark(&style, &da.color, lw, lh, radio));
            }
            s
        }
        FieldKind::Push => {
            let mut s = layout::frame(&style, lw, lh, false);
            if !style.caption.is_empty() {
                let spec = Spec { text: &style.caption, multiline: false, q: 1, comb: None };
                let inset = if style.bc.is_some() { style.width } else { 0.0 };
                s.push_str(&layout::text_content(face, &spec, &da, lw, lh, inset, "F0").0);
            }
            s
        }
        FieldKind::Signature => return None,
    };
    let mut fonts = Dict::new();
    fonts.set("F0", font);
    let mut resources = Dict::new();
    resources.set("Font", Object::Dict(fonts));
    let mut dict = Dict::new();
    dict.set("Type", Object::from("XObject"));
    dict.set("Subtype", Object::from("Form"));
    dict.set("BBox", super::reals(&[0.0, 0.0, lw, lh]));
    dict.set("Matrix", super::reals(&matrix));
    dict.set("Resources", Object::Dict(resources));
    Some((Stream { dict, data: body.into_bytes() }, cut))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;

    #[test]
    fn win_ansi_has_latin_one_and_the_extras_of_windows() {
        assert_eq!(win_ansi('A'), Some(b'A'));
        assert_eq!(win_ansi('é'), Some(0xE9));
        assert_eq!(win_ansi('€'), Some(0x80));
        assert_eq!(win_ansi('“'), Some(0x93));
        assert_eq!(win_ansi('中'), None);
        assert_eq!(win_ansi('\u{81}'), None);
    }

    #[test]
    fn text_values_keep_their_line_ends_and_both_encodings() {
        assert_eq!(decode_text(b"a\rb\r\nc"), "a\nb\nc");
        assert_eq!(decode_text(&[0xFE, 0xFF, 0x4E, 0x2D, 0x00, 0x0A, 0x00, 0x61]), "\u{4E2D}\na");
        assert_eq!(decode_text(&[0xEF, 0xBB, 0xBF, 0xE4, 0xB8, 0xAD]), "\u{4E2D}");
        assert_eq!(decode_text(b"a\x07b"), "a b");
    }

    #[test]
    fn rotated_widgets_draw_in_a_turned_space() {
        assert_eq!(local_frame(100.0, 20.0, 0), (100.0, 20.0, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]));
        let (lw, lh, m) = local_frame(100.0, 20.0, 90);
        assert_eq!((lw, lh), (20.0, 100.0));
        // The far corner of the space (20, 100) lands on the far corner of the widget's box: x = -100 + 100, y = 20.
        assert_eq!((m[0] * 20.0 + m[2] * 100.0 + m[4], m[1] * 20.0 + m[3] * 100.0 + m[5]), (0.0, 20.0));
    }

    fn form_pdf(fields: &str, acro: &str) -> Document {
        let mut b = PdfBuilder::new();
        b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [10 0 R 11 0 R 12 0 R] /DA (/Helv 0 Tf 0 g) {acro} >> >>"));
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << >> /Annots [10 0 R 11 0 R 12 0 R 13 0 R 14 0 R] >>");
        b.obj(10, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (a) /V (hello) /Rect [10 150 110 170] /Ff 4096 /MaxLen 5 >>");
        b.obj(11, "<< /FT /Btn /Ff 32768 /T (g) /Kids [13 0 R 14 0 R] /V /x >>");
        b.obj(12, "<< /Type /Annot /Subtype /Widget /FT /Ch /Ff 131072 /T (c) /Rect [10 100 110 120] /Opt [[(e1) (One)] (Two)] /V (e1) >>");
        b.obj(13, "<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [10 50 30 70] /AS /x /AP << /N << /x 20 0 R /Off 20 0 R >> >> >>");
        b.obj(14, "<< /Type /Annot /Subtype /Widget /Parent 11 0 R /Rect [50 50 70 70] /AS /Off >>");
        b.obj(20, "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] >>");
        let _ = fields;
        Document::from_bytes(b.finish_classic(30, "/Root 1 0 R")).unwrap()
    }

    #[test]
    fn a_field_inherits_what_its_parents_have_and_a_repeat_ends_the_walk() {
        let doc = form_pdf("", "");
        let (work, form) = (Work::new(), acroform(&doc, &Work::new()).unwrap());
        let env = Env { doc: &doc, form: &form, work: &work };
        let text = resolve_field(env, 10, doc.get(ObjRef::new(10, 0)).unwrap().as_dict().unwrap().clone()).unwrap();
        assert_eq!((text.kind, text.max_len, text.da.font.as_slice(), text.da.color.as_str()), (FieldKind::Text, Some(5), b"Helv".as_slice(), "0 g"));
        assert!(text.multiline() && !text.password() && text.comb().is_none());
        // A radio button's kid has the type, the flags and the value of its parent.
        let kid = resolve_field(env, 13, doc.get(ObjRef::new(13, 0)).unwrap().as_dict().unwrap().clone()).unwrap();
        assert_eq!((kid.kind, kid.node_num, kid.value.as_deref().cloned()), (FieldKind::Radio, 11, Some(Object::Name(Name::from("x")))));
        assert!(is_on(&doc, &kid));
        let off = resolve_field(env, 14, doc.get(ObjRef::new(14, 0)).unwrap().as_dict().unwrap().clone()).unwrap();
        assert!(!is_on(&doc, &off));
        // A choice has its options, shown by their display text.
        let choice = resolve_field(env, 12, doc.get(ObjRef::new(12, 0)).unwrap().as_dict().unwrap().clone()).unwrap();
        assert_eq!(options(env, &choice).items, vec![("e1".to_string(), "One".to_string()), ("Two".to_string(), "Two".to_string())]);
        assert_eq!(value_text(env, &choice), "One");
        // A widget whose /Parent is itself ends there.
        let mut looped = Dict::new();
        looped.set("FT", Object::from("Tx"));
        looped.set("Parent", Object::Ref(ObjRef::new(10, 0)));
        assert!(resolve_field(env, 10, looped).is_some());
    }

    #[test]
    fn describing_a_widget_gives_what_the_window_needs() {
        let doc = form_pdf("", "");
        let (work, form) = (Work::new(), acroform(&doc, &Work::new()).unwrap());
        let env = Env { doc: &doc, form: &form, work: &work };
        let get = |n: u32| doc.get(ObjRef::new(n, 0)).unwrap().as_dict().unwrap().clone();
        let info = describe(env, 10, &get(10), [0.0; 4], 0).unwrap();
        assert_eq!((info.kind, info.value.as_str(), info.max_len, info.multiline, info.name.as_str()), (FieldKind::Text, "hello", 5, true, "a"));
        let info = describe(env, 13, &get(13), [0.0; 4], 3).unwrap();
        assert_eq!((info.kind, info.checked, info.index), (FieldKind::Radio, true, 3));
        let info = describe(env, 12, &get(12), [0.0; 4], 2).unwrap();
        assert_eq!((info.kind, info.value.as_str()), (FieldKind::Combo, "One"));
        // The options are asked for on their own.
        let (items, truncated) = options_of(env, 12, &get(12));
        assert_eq!((items.len(), truncated), (2, false));
        assert!(options_of(env, 10, &get(10)).0.is_empty());
    }
}

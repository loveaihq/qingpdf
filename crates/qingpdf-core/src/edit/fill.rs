//! Filling in a form field (4b): the new value and the appearance that shows it, written as objects of the edit.
//! What is read of the form (fields, their inherited values, the options, the look of a control) is in `crate::forms`.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::fontsub::{self, Look};
use super::{Ctx, text_string};
use crate::document::Document;
use crate::error::{Error, Result};
use crate::forms::*;
use crate::layout;
use crate::object::{Dict, Name, ObjRef, Object, Stream};

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


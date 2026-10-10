//! Signed and certified files (12.8): what an edit may do to a file that carries a signature without making the signature
//! show the file as changed, or the certifier's rule (`/Perms /DocMDP`, 12.8.2.2) broken.
//!
//! The reader adds annotations only. A signature over a file stays valid for what it covered when the update is appended
//! (that is what an incremental update is for), but a reader checking it later tells the person the file was changed after
//! signing; a certified file says which changes it allows (`/P`: 1 none, 2 filling in forms and signing, 3 and annotations).

use std::collections::HashSet;

use crate::document::Document;
use crate::object::{Dict, Object};

/// Most fields of the form looked at, and how deep.
const MAX_FIELDS: usize = 20_000;
const MAX_DEPTH: usize = 32;

/// What the signatures of a file leave to an edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Policy {
    /// No signature: edit as you like.
    Free,
    /// Signed, with no rule against annotations: allowed, and the person is told once that the signature will show the file as
    /// changed afterwards.
    Warn,
    /// Certified with a rule that does not allow annotations: refused (the reason, for the person).
    Refuse(&'static str),
}

/// Look at the file's signatures and certification.
pub(crate) fn policy(doc: &Document) -> Policy {
    let Ok(catalog) = doc.catalog() else { return Policy::Free };
    match certification(doc, &catalog) {
        Some(1) => return Policy::Refuse("the file is certified and allows no changes at all (DocMDP 1); adding annotations would break the certification"),
        Some(2) => return Policy::Refuse("the file is certified and allows only filling in forms and signing (DocMDP 2); adding annotations would break the certification"),
        Some(_) => return Policy::Warn,
        None => {}
    }
    if has_signed_field(doc, &catalog) { Policy::Warn } else { Policy::Free }
}

fn dict_of(doc: &Document, o: Option<&Object>) -> Option<Dict> {
    match doc.resolve(o?).ok()? {
        Object::Dict(d) => Some(d),
        Object::Stream(s) => Some(s.dict),
        _ => None,
    }
}

/// The `/P` of the certifying signature (`/Perms /DocMDP`), if there is one: 1, 2 or 3 (a value it should not have is read as 2,
/// the default of 12.8.2.2).
fn certification(doc: &Document, catalog: &Dict) -> Option<i64> {
    let perms = dict_of(doc, catalog.get("Perms"))?;
    let entry = perms.get("DocMDP")?;
    if matches!(doc.resolve(entry), Ok(Object::Null)) {
        return None;
    }
    let level = dict_of(doc, Some(entry))
        .and_then(|sig| doc.resolve(sig.get("Reference")?).ok())
        .and_then(|refs| {
            refs.as_array()?.iter().find_map(|r| {
                let r = dict_of(doc, Some(r))?;
                if !r.get("TransformMethod").is_some_and(|m| matches!(doc.resolve(m), Ok(Object::Name(n)) if n == "DocMDP")) {
                    return None;
                }
                let params = dict_of(doc, r.get("TransformParams"))?;
                doc.resolve(params.get("P")?).ok()?.as_int()
            })
        })
        .unwrap_or(2);
    Some(if (1..=3).contains(&level) { level } else { 2 })
}

/// Does a signature field of the form have a signature in it (`/FT /Sig` with a `/V`, 12.7.4.5)?
fn has_signed_field(doc: &Document, catalog: &Dict) -> bool {
    let Some(form) = dict_of(doc, catalog.get("AcroForm")) else { return false };
    let Some(fields) = form.get("Fields").and_then(|f| doc.resolve(f).ok()) else { return false };
    let Some(fields) = fields.as_array() else { return false };
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack: Vec<(Object, usize, bool)> = fields.iter().map(|f| (f.clone(), 0, false)).collect();
    let mut looked_at = 0usize;
    while let Some((item, depth, inherited_sig)) = stack.pop() {
        looked_at += 1;
        if looked_at > MAX_FIELDS {
            // Too big a form to tell: taken as signed (the warning costs a line; a wrong "unsigned" costs a signature).
            return true;
        }
        if let Object::Ref(r) = &item
            && !seen.insert(r.num)
        {
            continue;
        }
        let Some(d) = dict_of(doc, Some(&item)) else { continue };
        let is_sig = match d.get("FT").and_then(|t| doc.resolve(t).ok()) {
            Some(Object::Name(n)) => n == "Sig",
            _ => inherited_sig,
        };
        if is_sig && d.get("V").and_then(|v| doc.resolve(v).ok()).is_some_and(|v| matches!(v, Object::Dict(_) | Object::Stream(_))) {
            return true;
        }
        if depth < MAX_DEPTH
            && let Some(kids) = d.get("Kids").and_then(|k| doc.resolve(k).ok())
            && let Some(kids) = kids.as_array()
        {
            stack.extend(kids.iter().map(|k| (k.clone(), depth + 1, is_sig)));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::PdfBuilder;

    fn file(catalog_extra: &str, extra: impl FnOnce(&mut PdfBuilder)) -> Document {
        let mut b = PdfBuilder::new();
        b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>"));
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << >> >>");
        extra(&mut b);
        Document::from_bytes(b.finish_classic(20, "/Root 1 0 R")).unwrap()
    }

    #[test]
    fn a_file_with_no_signature_is_free_and_a_signed_one_is_warned_about() {
        assert_eq!(policy(&file("", |_| {})), Policy::Free);
        // A signature field with nothing in it is not a signature.
        let empty = file("/AcroForm << /Fields [5 0 R] /SigFlags 1 >>", |b| {
            b.obj(5, "<< /FT /Sig /T (s) >>");
        });
        assert_eq!(policy(&empty), Policy::Free);
        let signed = file("/AcroForm << /Fields [5 0 R] /SigFlags 3 >>", |b| {
            b.obj(5, "<< /FT /Sig /T (s) /V 6 0 R >>");
            b.obj(6, "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>");
        });
        assert_eq!(policy(&signed), Policy::Warn);
        // The type and the value may be on the parent and the kid.
        let kids = file("/AcroForm << /Fields [5 0 R] >>", |b| {
            b.obj(5, "<< /FT /Sig /T (s) /Kids [7 0 R] >>");
            b.obj(7, "<< /Parent 5 0 R /V 6 0 R >>");
            b.obj(6, "<< /Type /Sig >>");
        });
        assert_eq!(policy(&kids), Policy::Warn);
    }

    #[test]
    fn a_certified_file_says_what_is_allowed() {
        let certified = |p: &str| {
            file("/Perms << /DocMDP 5 0 R >>", |b| {
                b.obj(5, &format!("<< /Type /Sig /Reference [ << /Type /SigRef /TransformMethod /DocMDP /TransformParams << /Type /TransformParams /P {p} /V /1.2 >> >> ] >>"));
            })
        };
        assert!(matches!(policy(&certified("1")), Policy::Refuse(_)));
        assert!(matches!(policy(&certified("2")), Policy::Refuse(_)));
        assert_eq!(policy(&certified("3")), Policy::Warn);
        // No /P: the default is 2. A /P that is not 1, 2 or 3: the same.
        let no_p = file("/Perms << /DocMDP 5 0 R >>", |b| {
            b.obj(5, "<< /Type /Sig /Reference [ << /TransformMethod /DocMDP /TransformParams << >> >> ] >>");
        });
        assert!(matches!(policy(&no_p), Policy::Refuse(_)));
        assert!(matches!(policy(&certified("9")), Policy::Refuse(_)));
        // A usage-rights signature (/UR3) is not a certification.
        assert_eq!(
            policy(&file("/Perms << /UR3 5 0 R >>", |b| {
                b.obj(5, "<< /Type /Sig >>");
            })),
            Policy::Free
        );
    }
}

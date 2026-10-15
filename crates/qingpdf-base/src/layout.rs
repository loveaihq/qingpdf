//! Numbers and arrays for a content stream, as the appearances of form fields, the text boxes and the outlines are written (the
//! laying out of the text of a form field, in `qingpdf-core`, is built on them).

use crate::object::Object;

/// A number for a content stream: four decimals at most, no exponent, no trailing zeros.
pub fn num(v: f64) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let mut s = format!("{v:.4}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".to_string() } else { s }
}

/// An array of reals.
pub fn reals(values: &[f64]) -> Object {
    Object::Array(values.iter().map(|&v| Object::Real(v)).collect())
}

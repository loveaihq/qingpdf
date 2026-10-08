//! Functions (ISO 32000-1 7.10): sampled (type 0), exponential interpolation (2),
//! stitching (3) and PostScript calculator (4). Used for the tint transforms of
//! Separation and DeviceN colour spaces.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::document::Document;
use crate::object::{Dict, ObjRef, Object};

/// Most nesting of stitching functions followed.
const MAX_DEPTH: usize = 8;
/// Most samples of a type 0 function table.
const MAX_SAMPLES: usize = 4 * 1024 * 1024;
/// Most distinct function objects one colour space may load (a stitching function may name 256 others, each
/// of which may do the same), and the most samples plus program bytes in all of them.
const MAX_NODES: usize = 1024;
const MAX_WEIGHT: usize = MAX_SAMPLES;
/// Most operators one type 4 function may run per call, and the deepest stack (7.10.5: 100).
const MAX_PS_STEPS: usize = 100_000;
const MAX_PS_STACK: usize = 100;
/// Most inputs and outputs of a function.
const MAX_ARITY: usize = 32;

/// The work a page may spend running functions, counted in steps (one per table lookup, per
/// operator of a type 4 function, and so on). Shared by the threads that draw one image.
#[derive(Clone, Debug)]
pub(crate) struct Meter(Arc<MeterState>);

#[derive(Debug)]
struct MeterState {
    left: AtomicU64,
    /// Some call was refused because nothing was left.
    refused: AtomicBool,
}

impl Meter {
    pub fn new(steps: u64) -> Meter {
        Meter(Arc::new(MeterState { left: AtomicU64::new(steps), refused: AtomicBool::new(false) }))
    }

    /// Spend `steps`. `false` (and nothing spent) when nothing is left; the call that takes the last
    /// steps may overshoot.
    fn charge(&self, steps: u64) -> bool {
        let ok = self.0.left.try_update(Ordering::Relaxed, Ordering::Relaxed, |l| (l > 0).then(|| l.saturating_sub(steps))).is_ok();
        if !ok {
            self.0.refused.store(true, Ordering::Relaxed);
        }
        ok
    }

    /// Did a function not run for lack of steps?
    pub fn was_refused(&self) -> bool {
        self.0.refused.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub(crate) struct Function {
    domain: Vec<(f64, f64)>,
    range: Vec<(f64, f64)>,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Sampled { size: Vec<usize>, encode: Vec<(f64, f64)>, decode: Vec<(f64, f64)>, outputs: usize, samples: Vec<f32> },
    Exponential { c0: Vec<f64>, c1: Vec<f64>, n: f64 },
    Stitching { functions: Vec<Arc<Function>>, bounds: Vec<f64>, encode: Vec<(f64, f64)> },
    PostScript { program: Vec<PsOp>, outputs: usize },
}

fn pairs(doc: &Document, dict: &Dict, key: &str) -> Option<Vec<(f64, f64)>> {
    let obj = doc.resolve(dict.get(key)?).ok()?;
    let items = obj.as_array()?;
    let nums: Vec<f64> = items.iter().map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64()).filter(|v| v.is_finite())).collect::<Option<_>>()?;
    Some(nums.chunks_exact(2).filter_map(|c| Some((*c.first()?, *c.get(1)?))).collect())
}

fn numbers(doc: &Document, dict: &Dict, key: &str) -> Option<Vec<f64>> {
    let obj = doc.resolve(dict.get(key)?).ok()?;
    obj.as_array()?.iter().map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64()).filter(|v| v.is_finite())).collect()
}

fn clamp(v: f64, (lo, hi): (f64, f64)) -> f64 {
    if lo <= hi { v.max(lo).min(hi) } else { v.max(hi).min(lo) }
}

/// Reads function objects. A function named by reference is read once however many functions name
/// it (7.10.4 lets a stitching function name the same one over and over, and nesting that makes a
/// tree of exponential size), and the number of function objects and their size are bounded.
struct Loader<'a> {
    doc: &'a Document,
    memo: HashMap<ObjRef, Option<Arc<Function>>>,
    nodes_left: usize,
    weight_left: usize,
}

impl Loader<'_> {
    fn load_at(&mut self, obj: &Object, depth: usize) -> Option<Arc<Function>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let Object::Ref(r) = obj else { return self.load_object(obj, depth).map(Arc::new) };
        if let Some(hit) = self.memo.get(r) {
            return hit.clone();
        }
        // While it loads, a reference to itself finds nothing.
        self.memo.insert(*r, None);
        let loaded = self.load_object(obj, depth).map(Arc::new);
        self.memo.insert(*r, loaded.clone());
        loaded
    }

    fn load_object(&mut self, obj: &Object, depth: usize) -> Option<Function> {
        let doc = self.doc;
        self.nodes_left = self.nodes_left.checked_sub(1)?;
        let obj = doc.resolve(obj).ok()?;
        let dict = obj.as_dict()?;
        let domain = pairs(doc, dict, "Domain")?;
        if domain.is_empty() || domain.len() > MAX_ARITY {
            return None;
        }
        let range = pairs(doc, dict, "Range").unwrap_or_default();
        let kind = match doc.resolve(dict.get("FunctionType")?).ok()?.as_int()? {
            0 => {
                let Object::Stream(stream) = &obj else { return None };
                if range.is_empty() || range.len() > MAX_ARITY {
                    return None;
                }
                let m = domain.len();
                let n = range.len();
                let size: Vec<usize> = numbers(doc, dict, "Size")?.iter().map(|&v| if v >= 1.0 { v as usize } else { 0 }).collect();
                if size.len() != m || size.contains(&0) {
                    return None;
                }
                let bps = doc.resolve(dict.get("BitsPerSample")?).ok()?.as_int()?;
                if !matches!(bps, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) {
                    return None;
                }
                let bps = bps as usize;
                let mut total = n;
                for &s in &size {
                    total = total.checked_mul(s)?;
                    if total > MAX_SAMPLES {
                        return None;
                    }
                }
                self.weight_left = self.weight_left.checked_sub(total)?;
                let encode = pairs(doc, dict, "Encode").filter(|e| e.len() == m).unwrap_or_else(|| size.iter().map(|&s| (0.0, (s - 1) as f64)).collect());
                let decode = pairs(doc, dict, "Decode").filter(|d| d.len() == n).unwrap_or_else(|| range.clone());
                let data = doc.decode_stream_limited(stream, MAX_SAMPLES * 4).ok()?;
                let max = if bps == 32 { u32::MAX as f64 } else { ((1u64 << bps) - 1) as f64 };
                let mut samples = Vec::with_capacity(total);
                let mut bit = 0usize;
                for _ in 0..total {
                    let mut v: u64 = 0;
                    for _ in 0..bps {
                        let byte = data.get(bit / 8).copied().unwrap_or(0);
                        v = (v << 1) | u64::from((byte >> (7 - bit % 8)) & 1);
                        bit += 1;
                    }
                    samples.push((v as f64 / max) as f32);
                }
                Kind::Sampled { size, encode, decode, outputs: n, samples }
            }
            2 => {
                let c0 = numbers(doc, dict, "C0").unwrap_or_else(|| vec![0.0]);
                let c1 = numbers(doc, dict, "C1").unwrap_or_else(|| vec![1.0]);
                if c0.len() != c1.len() || c0.is_empty() || c0.len() > MAX_ARITY {
                    return None;
                }
                let n = doc.resolve(dict.get("N")?).ok()?.as_f64()?;
                Kind::Exponential { c0, c1, n }
            }
            3 => {
                let Object::Array(items) = doc.resolve(dict.get("Functions")?).ok()? else { return None };
                if items.is_empty() || items.len() > 256 {
                    return None;
                }
                let functions: Vec<Arc<Function>> = items.iter().map(|o| self.load_at(o, depth + 1)).collect::<Option<_>>()?;
                let bounds = numbers(doc, dict, "Bounds").unwrap_or_default();
                let encode = pairs(doc, dict, "Encode")?;
                if bounds.len() + 1 != functions.len() || encode.len() < functions.len() {
                    return None;
                }
                Kind::Stitching { functions, bounds, encode }
            }
            4 => {
                let Object::Stream(stream) = &obj else { return None };
                if range.is_empty() || range.len() > MAX_ARITY {
                    return None;
                }
                let code = doc.decode_stream_limited(stream, 1 << 20).ok()?;
                self.weight_left = self.weight_left.checked_sub(code.len())?;
                let program = parse_ps(&code)?;
                Kind::PostScript { program, outputs: range.len() }
            }
            _ => return None,
        };
        Some(Function { domain, range, kind })
    }
}

impl Function {
    /// Read a function object. `None` when it is not a function we can run.
    pub fn load(doc: &Document, obj: &Object) -> Option<Arc<Function>> {
        let mut loader = Loader { doc, memo: HashMap::new(), nodes_left: MAX_NODES, weight_left: MAX_WEIGHT };
        loader.load_at(obj, 0)
    }

    pub fn inputs(&self) -> usize {
        self.domain.len()
    }

    /// Run the function. `out` is cleared and receives the outputs. `false` (and no outputs) when the
    /// page has used up its steps ([`Meter`]); the caller then makes do without.
    pub fn eval(&self, input: &[f64], out: &mut Vec<f64>, meter: &Meter) -> bool {
        out.clear();
        if !meter.charge(1) {
            return false;
        }
        let x: Vec<f64> = self.domain.iter().zip(input.iter().chain(std::iter::repeat(&0.0))).map(|(d, &v)| clamp(v, *d)).collect();
        match &self.kind {
            Kind::Sampled { size, encode, decode, outputs, samples } => {
                // 7.10.2: encode each input to a position in the table, interpolate between the neighbours.
                let m = x.len();
                let mut idx0: Vec<usize> = Vec::with_capacity(m);
                let mut frac: Vec<f64> = Vec::with_capacity(m);
                for (((&v, d), e), &s) in x.iter().zip(&self.domain).zip(encode).zip(size) {
                    let span = d.1 - d.0;
                    let t = if span.abs() > 0.0 { e.0 + (v - d.0) * (e.1 - e.0) / span } else { e.0 };
                    let t = clamp(t, (0.0, (s - 1) as f64));
                    let i = (t.floor() as usize).min(s.saturating_sub(1));
                    idx0.push(i);
                    frac.push(t - i as f64);
                }
                let mut strides = Vec::with_capacity(m);
                let mut stride = *outputs;
                for &s in size {
                    strides.push(stride);
                    stride = stride.saturating_mul(s);
                }
                let corners = 1usize << m.min(8);
                meter.charge((corners * outputs) as u64);
                for j in 0..*outputs {
                    let mut acc = 0.0;
                    for corner in 0..corners {
                        let mut weight = 1.0;
                        let mut offset = j;
                        for k in 0..m {
                            let hi = k < 8 && (corner >> k) & 1 == 1;
                            let f = frac.get(k).copied().unwrap_or(0.0);
                            let i0 = idx0.get(k).copied().unwrap_or(0);
                            let s = size.get(k).copied().unwrap_or(1);
                            let st = strides.get(k).copied().unwrap_or(0);
                            if hi {
                                weight *= f;
                                offset += (i0 + 1).min(s - 1) * st;
                            } else {
                                weight *= 1.0 - f;
                                offset += i0 * st;
                            }
                        }
                        if weight != 0.0 {
                            acc += weight * f64::from(samples.get(offset).copied().unwrap_or(0.0));
                        }
                    }
                    let d = decode.get(j).copied().unwrap_or((0.0, 1.0));
                    out.push(d.0 + acc * (d.1 - d.0));
                }
            }
            Kind::Exponential { c0, c1, n } => {
                let t = x.first().copied().unwrap_or(0.0);
                let p = if *n == 1.0 { t } else { t.powf(*n) };
                let p = if p.is_finite() { p } else { 0.0 };
                for (a, b) in c0.iter().zip(c1) {
                    out.push(a + p * (b - a));
                }
            }
            Kind::Stitching { functions, bounds, encode } => {
                let t = x.first().copied().unwrap_or(0.0);
                let d = self.domain.first().copied().unwrap_or((0.0, 1.0));
                let k = bounds.iter().take_while(|&&b| t >= b).count().min(functions.len().saturating_sub(1));
                let lo = if k == 0 { d.0 } else { bounds.get(k - 1).copied().unwrap_or(d.0) };
                let hi = bounds.get(k).copied().unwrap_or(d.1);
                let e = encode.get(k).copied().unwrap_or((0.0, 1.0));
                let v = if hi != lo { e.0 + (t - lo) * (e.1 - e.0) / (hi - lo) } else { e.0 };
                if let Some(f) = functions.get(k)
                    && !f.eval(&[v], out, meter)
                {
                    return false;
                }
            }
            Kind::PostScript { program, outputs } => {
                let mut stack: Vec<Ps> = x.iter().map(|&v| Ps::R(v)).collect();
                let mut steps = 0usize;
                if run_ps(program, &mut stack, &mut steps).is_none() {
                    stack.clear();
                }
                meter.charge(steps as u64);
                let skip = stack.len().saturating_sub(*outputs);
                out.extend(stack.iter().skip(skip).map(Ps::num));
                while out.len() < *outputs {
                    out.insert(0, 0.0);
                }
            }
        }
        if !self.range.is_empty() {
            for (v, r) in out.iter_mut().zip(&self.range) {
                *v = clamp(*v, *r);
            }
        }
        for v in out.iter_mut() {
            if !v.is_finite() {
                *v = 0.0;
            }
        }
        true
    }
}

// --- type 4: PostScript calculator (7.10.5) -----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ps {
    I(i64),
    R(f64),
    B(bool),
}

impl Ps {
    fn num(&self) -> f64 {
        match *self {
            Ps::I(i) => i as f64,
            Ps::R(r) => r,
            Ps::B(b) => f64::from(u8::from(b)),
        }
    }
}

#[derive(Debug, Clone)]
enum PsOp {
    Push(Ps),
    Op(&'static str),
    If(Vec<PsOp>),
    IfElse(Vec<PsOp>, Vec<PsOp>),
}

const PS_OPERATORS: &[&str] = &[
    "abs", "add", "and", "atan", "bitshift", "ceiling", "copy", "cos", "cvi", "cvr", "div", "dup", "eq", "exch", "exp", "false", "floor", "ge",
    "gt", "idiv", "index", "le", "ln", "log", "lt", "mod", "mul", "ne", "neg", "not", "or", "pop", "roll", "round", "sin", "sqrt", "sub", "true",
    "truncate", "xor",
];

/// Parse `{ ... }` into operators; `None` for anything outside the language.
fn parse_ps(code: &[u8]) -> Option<Vec<PsOp>> {
    let text = String::from_utf8_lossy(code);
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        match ch {
            '{' | '}' => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
                tokens.push(ch.to_string());
            }
            c if c.is_whitespace() || c == '\0' => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
        if tokens.len() > 200_000 {
            return None;
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    let mut pos = 0usize;
    if tokens.get(pos).map(String::as_str) != Some("{") {
        return None;
    }
    pos += 1;
    ps_block(&tokens, &mut pos, 0)
}

/// After `{`: operators up to the matching `}`.
fn ps_block(tokens: &[String], pos: &mut usize, depth: usize) -> Option<Vec<PsOp>> {
    if depth > 32 {
        return None;
    }
    let mut ops: Vec<PsOp> = Vec::new();
    let mut pending: Vec<Vec<PsOp>> = Vec::new();
    loop {
        let tok = tokens.get(*pos)?;
        *pos += 1;
        match tok.as_str() {
            "}" => return Some(ops),
            "{" => {
                let inner = ps_block(tokens, pos, depth + 1)?;
                pending.push(inner);
                if pending.len() > 2 {
                    return None;
                }
            }
            "if" => {
                let body = pending.pop()?;
                if !pending.is_empty() {
                    return None;
                }
                ops.push(PsOp::If(body));
            }
            "ifelse" => {
                let no = pending.pop()?;
                let yes = pending.pop()?;
                ops.push(PsOp::IfElse(yes, no));
            }
            other => {
                if !pending.is_empty() {
                    return None;
                }
                if let Some(name) = PS_OPERATORS.iter().find(|n| **n == other) {
                    ops.push(PsOp::Op(name));
                } else if let Some((radix, digits)) = other.split_once('#') {
                    let radix: u32 = radix.parse().ok().filter(|r| (2..=36).contains(r))?;
                    ops.push(PsOp::Push(Ps::I(i64::from_str_radix(digits, radix).ok()?)));
                } else if let Ok(i) = other.parse::<i64>() {
                    ops.push(PsOp::Push(Ps::I(i)));
                } else {
                    let r: f64 = other.parse().ok()?;
                    ops.push(PsOp::Push(Ps::R(r)));
                }
            }
        }
    }
}

fn run_ps(program: &[PsOp], stack: &mut Vec<Ps>, steps: &mut usize) -> Option<()> {
    for op in program {
        *steps += 1;
        if *steps > MAX_PS_STEPS || stack.len() > MAX_PS_STACK {
            return None;
        }
        match op {
            PsOp::Push(v) => stack.push(*v),
            PsOp::If(body) => {
                if let Ps::B(true) = stack.pop()? {
                    run_ps(body, stack, steps)?;
                }
            }
            PsOp::IfElse(yes, no) => {
                let Ps::B(c) = stack.pop()? else { return None };
                run_ps(if c { yes } else { no }, stack, steps)?;
            }
            PsOp::Op(name) => ps_operator(name, stack)?,
        }
    }
    Some(())
}

fn pop_num(stack: &mut Vec<Ps>) -> Option<Ps> {
    match stack.pop()? {
        Ps::B(_) => None,
        n => Some(n),
    }
}

fn as_int(v: Ps) -> Option<i64> {
    match v {
        Ps::I(i) => Some(i),
        Ps::R(r) if r.is_finite() => Some(r as i64),
        _ => None,
    }
}

fn ps_operator(name: &str, stack: &mut Vec<Ps>) -> Option<()> {
    match name {
        "add" | "sub" | "mul" => {
            let b = pop_num(stack)?;
            let a = pop_num(stack)?;
            let v = match (a, b) {
                (Ps::I(x), Ps::I(y)) => {
                    let r = match name {
                        "add" => x.checked_add(y),
                        "sub" => x.checked_sub(y),
                        _ => x.checked_mul(y),
                    };
                    match r {
                        Some(i) => Ps::I(i),
                        None => Ps::R(match name {
                            "add" => x as f64 + y as f64,
                            "sub" => x as f64 - y as f64,
                            _ => x as f64 * y as f64,
                        }),
                    }
                }
                (a, b) => {
                    let (x, y) = (a.num(), b.num());
                    Ps::R(match name {
                        "add" => x + y,
                        "sub" => x - y,
                        _ => x * y,
                    })
                }
            };
            stack.push(v);
        }
        "div" => {
            let b = pop_num(stack)?.num();
            let a = pop_num(stack)?.num();
            if b == 0.0 {
                return None;
            }
            stack.push(Ps::R(a / b));
        }
        "idiv" | "mod" => {
            let b = as_int(pop_num(stack)?)?;
            let a = as_int(pop_num(stack)?)?;
            if b == 0 {
                return None;
            }
            stack.push(Ps::I(if name == "idiv" { a.checked_div(b)? } else { a.checked_rem(b)? }));
        }
        "neg" => {
            let v = pop_num(stack)?;
            stack.push(match v {
                Ps::I(i) => i.checked_neg().map_or(Ps::R(-(i as f64)), Ps::I),
                other => Ps::R(-other.num()),
            });
        }
        "abs" => {
            let v = pop_num(stack)?;
            stack.push(match v {
                Ps::I(i) => i.checked_abs().map_or(Ps::R((i as f64).abs()), Ps::I),
                other => Ps::R(other.num().abs()),
            });
        }
        "ceiling" | "floor" | "round" | "truncate" => {
            let v = pop_num(stack)?;
            stack.push(match v {
                Ps::I(i) => Ps::I(i),
                other => {
                    let r = other.num();
                    Ps::R(match name {
                        "ceiling" => r.ceil(),
                        "floor" => r.floor(),
                        "round" => (r + 0.5).floor(),
                        _ => r.trunc(),
                    })
                }
            });
        }
        "sqrt" | "sin" | "cos" | "ln" | "log" => {
            let r = pop_num(stack)?.num();
            let v = match name {
                "sqrt" => {
                    if r < 0.0 {
                        return None;
                    }
                    r.sqrt()
                }
                "sin" => r.to_radians().sin(),
                "cos" => r.to_radians().cos(),
                "ln" => r.ln(),
                _ => r.log10(),
            };
            stack.push(Ps::R(v));
        }
        "exp" => {
            let e = pop_num(stack)?.num();
            let b = pop_num(stack)?.num();
            stack.push(Ps::R(b.powf(e)));
        }
        "atan" => {
            let den = pop_num(stack)?.num();
            let num = pop_num(stack)?.num();
            let mut deg = num.atan2(den).to_degrees();
            if deg < 0.0 {
                deg += 360.0;
            }
            stack.push(Ps::R(deg));
        }
        "cvi" => {
            let v = pop_num(stack)?;
            stack.push(Ps::I(as_int(v)?));
        }
        "cvr" => {
            let v = pop_num(stack)?;
            stack.push(Ps::R(v.num()));
        }
        "eq" | "ne" => {
            let b = stack.pop()?;
            let a = stack.pop()?;
            let same = match (a, b) {
                (Ps::B(x), Ps::B(y)) => x == y,
                (Ps::B(_), _) | (_, Ps::B(_)) => false,
                (x, y) => x.num() == y.num(),
            };
            stack.push(Ps::B(same == (name == "eq")));
        }
        "gt" | "ge" | "lt" | "le" => {
            let b = pop_num(stack)?.num();
            let a = pop_num(stack)?.num();
            stack.push(Ps::B(match name {
                "gt" => a > b,
                "ge" => a >= b,
                "lt" => a < b,
                _ => a <= b,
            }));
        }
        "and" | "or" | "xor" => {
            let b = stack.pop()?;
            let a = stack.pop()?;
            stack.push(match (a, b) {
                (Ps::B(x), Ps::B(y)) => Ps::B(match name {
                    "and" => x & y,
                    "or" => x | y,
                    _ => x ^ y,
                }),
                (Ps::I(x), Ps::I(y)) => Ps::I(match name {
                    "and" => x & y,
                    "or" => x | y,
                    _ => x ^ y,
                }),
                _ => return None,
            });
        }
        "not" => {
            let v = stack.pop()?;
            stack.push(match v {
                Ps::B(x) => Ps::B(!x),
                Ps::I(x) => Ps::I(!x),
                Ps::R(_) => return None,
            });
        }
        "bitshift" => {
            let shift = as_int(pop_num(stack)?)?;
            let v = as_int(pop_num(stack)?)?;
            let r = if shift >= 0 { v.checked_shl(u32::try_from(shift).ok()?)? } else { v >> u32::try_from(-shift).ok()?.min(63) };
            stack.push(Ps::I(r));
        }
        "true" => stack.push(Ps::B(true)),
        "false" => stack.push(Ps::B(false)),
        "pop" => {
            stack.pop()?;
        }
        "exch" => {
            let b = stack.pop()?;
            let a = stack.pop()?;
            stack.push(b);
            stack.push(a);
        }
        "dup" => {
            let v = *stack.last()?;
            stack.push(v);
        }
        "copy" => {
            let n = usize::try_from(as_int(pop_num(stack)?)?).ok()?;
            if n > stack.len() || stack.len() + n > MAX_PS_STACK {
                return None;
            }
            let from = stack.len() - n;
            let tail: Vec<Ps> = stack.get(from..)?.to_vec();
            stack.extend(tail);
        }
        "index" => {
            let n = usize::try_from(as_int(pop_num(stack)?)?).ok()?;
            let at = stack.len().checked_sub(n.checked_add(1)?)?;
            let v = *stack.get(at)?;
            stack.push(v);
        }
        "roll" => {
            let j = as_int(pop_num(stack)?)?;
            let n = usize::try_from(as_int(pop_num(stack)?)?).ok()?;
            if n > stack.len() {
                return None;
            }
            if n > 0 {
                let from = stack.len() - n;
                let part = stack.get_mut(from..)?;
                let shift = j.rem_euclid(i64::try_from(n).ok()?);
                part.rotate_right(usize::try_from(shift).ok()?);
            }
        }
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(code: &str, input: &[f64], outputs: usize) -> Vec<f64> {
        let program = parse_ps(code.as_bytes()).expect("parses");
        let f = Function {
            domain: input.iter().map(|_| (-1000.0, 1000.0)).collect(),
            range: vec![(-1000.0, 1000.0); outputs],
            kind: Kind::PostScript { program, outputs },
        };
        let mut out = Vec::new();
        f.eval(input, &mut out, &Meter::new(u64::MAX));
        out
    }

    #[test]
    fn postscript_calculator() {
        assert_eq!(run("{ 2 mul }", &[3.0], 1), vec![6.0]);
        assert_eq!(run("{ dup 0.5 gt { 1 exch sub } { 2 mul } ifelse }", &[0.75], 1), vec![0.25]);
        assert_eq!(run("{ dup 0.5 gt { 1 exch sub } { 2 mul } ifelse }", &[0.25], 1), vec![0.5]);
        assert_eq!(run("{ 3 1 roll }", &[1.0, 2.0, 3.0], 3), vec![3.0, 1.0, 2.0]);
        assert_eq!(run("{ 7 3 idiv 7 3 mod }", &[], 2).len(), 2);
        // An operator that fails (division by zero) gives zeros, not a panic.
        assert_eq!(run("{ 0 div }", &[1.0], 1), vec![0.0]);
        assert!(parse_ps(b"{ foo }").is_none());
        assert!(parse_ps(b"{ 1 { 2 }").is_none());
    }

    #[test]
    fn exponential_and_stitching() {
        let f = Function { domain: vec![(0.0, 1.0)], range: vec![], kind: Kind::Exponential { c0: vec![0.0, 1.0], c1: vec![1.0, 0.0], n: 1.0 } };
        let mut out = Vec::new();
        f.eval(&[0.25], &mut out, &Meter::new(u64::MAX));
        assert_eq!(out, vec![0.25, 0.75]);
        let g = Function {
            domain: vec![(0.0, 1.0)],
            range: vec![],
            kind: Kind::Stitching { functions: vec![Arc::new(f), Arc::new(Function { domain: vec![(0.0, 1.0)], range: vec![], kind: Kind::Exponential { c0: vec![5.0, 5.0], c1: vec![5.0, 5.0], n: 1.0 } })], bounds: vec![0.5], encode: vec![(0.0, 1.0), (0.0, 1.0)] },
        };
        g.eval(&[0.25], &mut out, &Meter::new(u64::MAX));
        assert_eq!(out, vec![0.5, 0.5]);
        g.eval(&[0.75], &mut out, &Meter::new(u64::MAX));
        assert_eq!(out, vec![5.0, 5.0]);
    }
}

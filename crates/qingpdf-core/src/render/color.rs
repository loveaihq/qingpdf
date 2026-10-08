//! Colour spaces (ISO 32000-1 8.6) turned into screen RGB. The device spaces are
//! converted directly; CalGray, CalRGB and ICCBased are read as the device space with
//! the same number of channels (no colour management); Lab, Indexed, Separation and
//! DeviceN are converted through their base or alternate space.

use std::sync::Arc;

use crate::document::Document;
use crate::object::Object;

use super::func::{Function, Meter};

/// Nesting of base and alternate spaces followed.
const MAX_DEPTH: usize = 4;
/// Most components of a DeviceN space.
const MAX_COMPONENTS: usize = 32;

#[derive(Debug)]
pub(crate) enum ColorSpace {
    Gray,
    Rgb,
    Cmyk,
    Lab { range: [f64; 4] },
    Indexed { hival: usize, table: Vec<[f32; 3]> },
    /// Separation (one component) and DeviceN. `none`: every colorant is /None, nothing is painted.
    Tint { n: usize, alt: Arc<ColorSpace>, func: Arc<Function>, none: bool },
    /// A pattern space; the colour of an uncoloured pattern is in the base space.
    Pattern,
}

impl ColorSpace {
    pub fn components(&self) -> usize {
        match self {
            ColorSpace::Gray | ColorSpace::Indexed { .. } => 1,
            ColorSpace::Rgb | ColorSpace::Lab { .. } => 3,
            ColorSpace::Cmyk => 4,
            ColorSpace::Tint { n, .. } => *n,
            ColorSpace::Pattern => 0,
        }
    }

    /// The colour a space starts with (8.6.5 to 8.6.6).
    pub fn initial(&self) -> Vec<f64> {
        match self {
            ColorSpace::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            ColorSpace::Tint { n, .. } => vec![1.0; *n],
            other => vec![0.0; other.components()],
        }
    }

    /// The `/Decode` array an image in this space gets when it has none (8.9.5.2, Table 90).
    pub fn default_decode(&self, bpc: u32) -> Vec<f64> {
        match self {
            ColorSpace::Indexed { .. } => vec![0.0, f64::from((1u32 << bpc.min(16)) - 1)],
            ColorSpace::Lab { range, .. } => vec![0.0, 100.0, range[0], range[1], range[2], range[3]],
            other => (0..other.components()).flat_map(|_| [0.0, 1.0]).collect(),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, ColorSpace::Tint { none: true, .. })
    }

    /// RGB (0 to 1) of a colour given in this space's components. The tint functions of Separation and
    /// DeviceN run on the page's `meter`; when it has nothing left the tint is shown as a grey instead.
    pub fn to_rgb(&self, c: &[f64], meter: &Meter) -> [f32; 3] {
        let g = |i: usize| c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        match self {
            ColorSpace::Gray => {
                let v = g(0) as f32;
                [v, v, v]
            }
            ColorSpace::Rgb => [g(0) as f32, g(1) as f32, g(2) as f32],
            ColorSpace::Cmyk => cmyk_to_rgb(g(0), g(1), g(2), g(3)),
            ColorSpace::Lab { range } => {
                // (`f64::clamp` panics when min > max; a range read from a file is made sane at load, and this
                // is safe whatever it is.)
                let within = |v: f64, lo: f64, hi: f64| v.max(lo.min(hi)).min(hi.max(lo));
                let l = c.first().copied().unwrap_or(0.0).clamp(0.0, 100.0);
                let a = within(c.get(1).copied().unwrap_or(0.0), range[0], range[1]);
                let b = within(c.get(2).copied().unwrap_or(0.0), range[2], range[3]);
                lab_to_rgb(l, a, b)
            }
            ColorSpace::Indexed { hival, table } => {
                let i = c.first().copied().unwrap_or(0.0).round().max(0.0) as usize;
                table.get(i.min(*hival)).copied().unwrap_or([0.0; 3])
            }
            ColorSpace::Tint { alt, func, .. } => {
                let mut out = Vec::new();
                if func.eval(c, &mut out, meter) {
                    alt.to_rgb(&out, meter)
                } else {
                    // Out of steps: the strongest ink, as a grey (full tint is dark).
                    let t = c.iter().fold(0.0f64, |m, &v| m.max(v)).clamp(0.0, 1.0) as f32;
                    [1.0 - t; 3]
                }
            }
            ColorSpace::Pattern => [0.0; 3],
        }
    }

    /// Read a colour space object. `lookup` finds a name in the resources' `/ColorSpace`.
    pub fn load(doc: &Document, obj: &Object, lookup: &dyn Fn(&[u8]) -> Option<Object>, meter: &Meter) -> Option<Arc<ColorSpace>> {
        ColorSpace::load_at(doc, obj, lookup, meter, 0)
    }

    fn load_at(doc: &Document, obj: &Object, lookup: &dyn Fn(&[u8]) -> Option<Object>, meter: &Meter, depth: usize) -> Option<Arc<ColorSpace>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let obj = doc.resolve(obj).ok()?;
        match &obj {
            Object::Name(n) => {
                let space = match n.as_bytes() {
                    b"DeviceGray" | b"G" | b"CalGray" => ColorSpace::Gray,
                    b"DeviceRGB" | b"RGB" | b"CalRGB" => ColorSpace::Rgb,
                    b"DeviceCMYK" | b"CMYK" => ColorSpace::Cmyk,
                    b"Pattern" => ColorSpace::Pattern,
                    name => {
                        // A name from the resources (it must not name itself).
                        let found = lookup(name)?;
                        if matches!(&found, Object::Name(m) if m.as_bytes() == name) {
                            return None;
                        }
                        return ColorSpace::load_at(doc, &found, lookup, meter, depth + 1);
                    }
                };
                Some(Arc::new(space))
            }
            Object::Array(items) => {
                let family = doc.resolve(items.first()?).ok()?;
                let family = family.as_name()?.as_bytes().to_vec();
                match family.as_slice() {
                    b"DeviceGray" | b"G" | b"CalGray" => Some(Arc::new(ColorSpace::Gray)),
                    b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(Arc::new(ColorSpace::Rgb)),
                    b"DeviceCMYK" | b"CMYK" => Some(Arc::new(ColorSpace::Cmyk)),
                    b"Lab" => {
                        let dict = doc.resolve(items.get(1)?).ok()?;
                        let dict = dict.as_dict()?;
                        let nums = |key: &str, n: usize, default: &[f64]| -> Vec<f64> {
                            let v: Option<Vec<f64>> = dict
                                .get(key)
                                .and_then(|o| doc.resolve(o).ok())
                                .and_then(|o| o.as_array().map(<[Object]>::to_vec))
                                .and_then(|a| a.iter().map(|x| doc.resolve(x).ok().and_then(|x| x.as_f64())).collect());
                            v.filter(|v| v.len() == n && v.iter().all(|x| x.is_finite())).unwrap_or_else(|| default.to_vec())
                        };
                        let default = [-100.0, 100.0, -100.0, 100.0];
                        let range: [f64; 4] = nums("Range", 4, &default).try_into().unwrap_or(default);
                        // 8.6.5.4: each range runs from its minimum to its maximum; a reversed one is no range.
                        let range = if range[0] <= range[1] && range[2] <= range[3] { range } else { default };
                        Some(Arc::new(ColorSpace::Lab { range }))
                    }
                    b"ICCBased" => {
                        let stream = doc.resolve(items.get(1)?).ok()?;
                        let n = stream.as_dict()?.get("N").and_then(|o| doc.resolve(o).ok()).and_then(|o| o.as_int())?;
                        Some(Arc::new(match n {
                            1 => ColorSpace::Gray,
                            3 => ColorSpace::Rgb,
                            4 => ColorSpace::Cmyk,
                            _ => return None,
                        }))
                    }
                    b"Indexed" | b"I" => {
                        let base = ColorSpace::load_at(doc, items.get(1)?, lookup, meter, depth + 1)?;
                        if matches!(*base, ColorSpace::Pattern | ColorSpace::Indexed { .. }) {
                            return None;
                        }
                        let hival = doc.resolve(items.get(2)?).ok()?.as_int()?.clamp(0, 255) as usize;
                        let lookup_obj = doc.resolve(items.get(3)?).ok()?;
                        let bytes: Vec<u8> = match &lookup_obj {
                            Object::String(s) => s.bytes.clone(),
                            Object::Stream(s) => doc.decode_stream_limited(s, 1 << 20).ok()?,
                            _ => return None,
                        };
                        let n = base.components();
                        let decode = base.default_decode(8);
                        let mut table = Vec::with_capacity(hival + 1);
                        for entry in 0..=hival {
                            let comps: Vec<f64> = (0..n)
                                .map(|k| {
                                    let byte = bytes.get(entry * n + k).copied().unwrap_or(0);
                                    let lo = decode.get(2 * k).copied().unwrap_or(0.0);
                                    let hi = decode.get(2 * k + 1).copied().unwrap_or(1.0);
                                    lo + f64::from(byte) / 255.0 * (hi - lo)
                                })
                                .collect();
                            table.push(base.to_rgb(&comps, meter));
                        }
                        Some(Arc::new(ColorSpace::Indexed { hival, table }))
                    }
                    b"Separation" | b"DeviceN" => {
                        let separation = family == b"Separation";
                        let names = doc.resolve(items.get(1)?).ok()?;
                        let (n, none) = if separation {
                            (1, names.as_name().is_some_and(|n| n.as_bytes() == b"None"))
                        } else {
                            let list = names.as_array()?;
                            let all_none = !list.is_empty()
                                && list.iter().all(|o| doc.resolve(o).ok().and_then(|o| o.as_name().map(|n| n.as_bytes() == b"None")).unwrap_or(false));
                            (list.len(), all_none)
                        };
                        if n == 0 || n > MAX_COMPONENTS {
                            return None;
                        }
                        let alt = ColorSpace::load_at(doc, items.get(2)?, lookup, meter, depth + 1)?;
                        if matches!(*alt, ColorSpace::Pattern) {
                            return None;
                        }
                        let func = Function::load(doc, items.get(3)?)?;
                        if func.inputs() != n {
                            return None;
                        }
                        Some(Arc::new(ColorSpace::Tint { n, alt, func, none }))
                    }
                    b"Pattern" => Some(Arc::new(ColorSpace::Pattern)),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// DeviceCMYK to RGB, a polynomial fitted to Adobe's SWOP conversion (the one viewers use,
/// not the plain `(1 - c) (1 - k)`, which is far more saturated).
pub(crate) fn cmyk_to_rgb(c: f64, m: f64, y: f64, k: f64) -> [f32; 3] {
    let r = 255.0
        + c * (-4.387_332_384_609_988 * c + 54.486_151_941_891_76 * m + 18.822_905_021_653_02 * y + 212.256_624_516_395_85 * k - 285.233_102_613_700_4)
        + m * (1.714_976_347_736_213_4 * m - 5.609_673_690_404_731_5 * y - 17.873_870_861_415_444 * k - 5.497_006_427_196_366)
        + y * (-2.521_734_013_168_303_3 * y - 21.248_923_337_353_073 * k + 17.511_927_084_181_3)
        + k * (-21.861_221_474_636_05 * k - 189.481_808_359_227_47);
    let g = 255.0
        + c * (8.841_041_422_036_149 * c + 60.118_027_045_597_366 * m + 6.871_425_592_049_007 * y + 31.159_100_130_055_922 * k - 79.297_084_481_654_8)
        + m * (-15.310_361_306_967_817 * m + 17.575_251_261_109_482 * y + 131.352_509_124_939_76 * k - 190.945_330_258_895_1)
        + y * (4.444_339_102_852_739 * y + 9.863_286_149_340_5 * k - 24.867_415_825_558_78)
        + k * (-20.737_325_471_181_034 * k - 187.804_537_097_197_2);
    let b = 255.0
        + c * (0.884_252_243_000_329_6 * c + 8.078_677_503_112_928 * m + 30.899_783_097_037_29 * y - 0.238_832_386_891_789_34 * k - 14.183_576_799_673_286)
        + m * (10.495_932_734_320_72 * m + 63.023_784_947_540_52 * y + 50.606_957_656_360_734 * k - 112.238_842_537_192_53)
        + y * (0.032_960_411_148_732_17 * y + 115.603_844_496_466_41 * k - 193.582_093_568_615_05)
        + k * (-22.338_168_073_098_86 * k - 180.126_139_747_083_67);
    [(r / 255.0).clamp(0.0, 1.0) as f32, (g / 255.0).clamp(0.0, 1.0) as f32, (b / 255.0).clamp(0.0, 1.0) as f32]
}

/// CIE L*a*b* (8.6.5.4) to sRGB, taking the white point as D50 whatever `/WhitePoint` says: to XYZ, to linear sRGB, gamma.
fn lab_to_rgb(l: f64, a: f64, b: f64) -> [f32; 3] {
    let m = (l + 16.0) / 116.0;
    let g = |x: f64| if x >= 6.0 / 29.0 { x * x * x } else { 108.0 / 841.0 * (x - 4.0 / 29.0) };
    let x = g(m + a / 500.0) * 0.9642;
    let y = g(m);
    let z = g(m - b / 200.0) * 0.8249;
    let lin = [
        3.133_856_1 * x - 1.616_866_7 * y - 0.490_614_6 * z,
        -0.978_768_4 * x + 1.916_141_5 * y + 0.033_454_0 * z,
        0.071_945_3 * x - 0.228_991_4 * y + 1.405_242_7 * z,
    ];
    let enc = |v: f64| {
        let v = v.clamp(0.0, 1.0);
        (if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }) as f32
    };
    [enc(lin[0]), enc(lin[1]), enc(lin[2])]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_spaces() {
        assert_eq!(ColorSpace::Gray.to_rgb(&[0.5], &Meter::new(u64::MAX)), [0.5, 0.5, 0.5]);
        assert_eq!(ColorSpace::Rgb.to_rgb(&[1.0, 0.0, 0.5], &Meter::new(u64::MAX)), [1.0, 0.0, 0.5]);
        // Black ink is black, no ink is white.
        let k = ColorSpace::Cmyk.to_rgb(&[0.0, 0.0, 0.0, 1.0], &Meter::new(u64::MAX));
        assert!(k.iter().all(|&v| v < 0.25), "{k:?}");
        let w = ColorSpace::Cmyk.to_rgb(&[0.0, 0.0, 0.0, 0.0], &Meter::new(u64::MAX));
        assert_eq!(w, [1.0, 1.0, 1.0]);
        // Missing components are zero, out of range ones are clamped.
        assert_eq!(ColorSpace::Rgb.to_rgb(&[2.0], &Meter::new(u64::MAX)), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn lab_white_and_black() {
        let lab = ColorSpace::Lab { range: [-128.0, 127.0, -128.0, 127.0] };
        let w = lab.to_rgb(&[100.0, 0.0, 0.0], &Meter::new(u64::MAX));
        assert!(w.iter().all(|&v| v > 0.98), "{w:?}");
        let k = lab.to_rgb(&[0.0, 0.0, 0.0], &Meter::new(u64::MAX));
        assert!(k.iter().all(|&v| v < 0.02), "{k:?}");
        // A reversed or NaN range is no reason to panic.
        for range in [[100.0, -100.0, -100.0, 100.0], [f64::NAN, 1.0, 5.0, -5.0]] {
            let bad = ColorSpace::Lab { range };
            let _ = bad.to_rgb(&[50.0, 0.0, 0.0], &Meter::new(u64::MAX));
        }
    }
}

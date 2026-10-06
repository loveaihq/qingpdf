//! Stream filters (ISO 32000-1 7.4). Layer 1 decodes `FlateDecode` only, with
//! the predictors of 7.4.4.4; every other filter is reported as
//! [`Error::Unsupported`] and the caller keeps the raw bytes.

use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};

use crate::error::{Error, Result};
use crate::object::{Dict, Name, Object};

/// Largest decoded stream we are willing to build (a guard against
/// compression bombs).
pub const MAX_DECODED_SIZE: usize = 256 * 1024 * 1024;

/// Upper bounds for predictor parameters; the spec sets none, these only keep
/// hostile values from overflowing the row arithmetic.
const MAX_COLORS: i64 = 4096;
const MAX_COLUMNS: i64 = 1 << 28;

type Resolver<'a> = &'a dyn Fn(&Object) -> Result<Object>;

/// Apply all filters of a stream dictionary to its raw data (7.3.8.2, 7.4.1).
/// `resolve` follows indirect references found in `/Filter` and
/// `/DecodeParms`.
pub fn decode(dict: &Dict, data: &[u8], resolve: Resolver<'_>) -> Result<Vec<u8>> {
    decode_with_limit(dict, data, resolve, MAX_DECODED_SIZE)
}

/// Like [`decode`] when the dictionary is known to hold only direct objects
/// (cross-reference streams, 7.5.8.2).
pub fn decode_direct(dict: &Dict, data: &[u8]) -> Result<Vec<u8>> {
    decode(dict, data, &|o| Ok(o.clone()))
}

fn decode_with_limit(dict: &Dict, data: &[u8], resolve: Resolver<'_>, limit: usize) -> Result<Vec<u8>> {
    let filters = filter_names(dict, resolve)?;
    if filters.is_empty() {
        return Ok(data.to_vec());
    }
    let parms = decode_parms(dict, resolve)?;
    let mut current: Option<Vec<u8>> = None;
    for (i, name) in filters.iter().enumerate() {
        let input: &[u8] = current.as_deref().unwrap_or(data);
        let parm = parms.get(i).and_then(|p| p.as_ref());
        let output = match name.as_bytes() {
            b"FlateDecode" => {
                let inflated = inflate_zlib(input, limit)?;
                apply_predictor(inflated, parm, resolve)?
            }
            // 7.4.10: the Identity crypt filter (the default) leaves data as is.
            b"Crypt" => {
                let identity = match parm.and_then(|p| p.get("Name")) {
                    None => true,
                    Some(obj) => matches!(resolve(obj)?, Object::Name(n) if n == "Identity"),
                };
                if !identity {
                    return Err(Error::Unsupported("Crypt filter".to_string()));
                }
                input.to_vec()
            }
            other => {
                return Err(Error::Unsupported(format!("{} filter", String::from_utf8_lossy(other))));
            }
        };
        current = Some(output);
    }
    Ok(current.unwrap_or_default())
}

/// The `/Filter` entry as a list of names (7.3.8.2, Table 5).
fn filter_names(dict: &Dict, resolve: Resolver<'_>) -> Result<Vec<Name>> {
    let Some(entry) = dict.get("Filter") else {
        return Ok(Vec::new());
    };
    match resolve(entry)? {
        Object::Null => Ok(Vec::new()),
        Object::Name(n) => Ok(vec![n]),
        Object::Array(items) => {
            let mut names = Vec::with_capacity(items.len());
            for item in &items {
                match resolve(item)? {
                    Object::Name(n) => names.push(n),
                    Object::Null => {}
                    _ => return Err(Error::syntax(None, "/Filter array holds something other than names")),
                }
            }
            Ok(names)
        }
        _ => Err(Error::syntax(None, "/Filter is neither a name nor an array")),
    }
}

/// The `/DecodeParms` entry as one optional dictionary per filter.
fn decode_parms(dict: &Dict, resolve: Resolver<'_>) -> Result<Vec<Option<Dict>>> {
    let Some(entry) = dict.get("DecodeParms") else {
        return Ok(Vec::new());
    };
    match resolve(entry)? {
        Object::Dict(d) => Ok(vec![Some(d)]),
        Object::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in &items {
                out.push(match resolve(item)? {
                    Object::Dict(d) => Some(d),
                    _ => None,
                });
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

/// zlib/deflate decoding (RFC 1950, 1951). Truncated data and a wrong
/// checksum are tolerated and give what was decoded; corrupt data is an error.
fn inflate_zlib(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    match inflate_once(input, limit) {
        Err(Error::Syntax { .. }) => {
            // A stray end-of-line before the zlib header is a common defect.
            let trimmed = input.iter().position(|&b| b != b'\r' && b != b'\n').and_then(|p| input.get(p..));
            match trimmed {
                Some(rest) if rest.len() != input.len() => inflate_once(rest, limit),
                _ => Err(Error::syntax(None, "corrupt FlateDecode data")),
            }
        }
        other => other,
    }
}

fn inflate_once(input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let mut out = vec![0u8; input.len().saturating_mul(4).max(1024).min(limit.max(1))];
    let mut state = Box::<DecompressorOxide>::default();
    let mut in_pos = 0usize;
    let mut out_pos = 0usize;
    loop {
        let rest = input.get(in_pos..).unwrap_or(&[]);
        let (status, consumed, produced) = decompress(&mut state, rest, &mut out, out_pos, flags);
        in_pos = in_pos.saturating_add(consumed);
        out_pos = out_pos.saturating_add(produced).min(out.len());
        match status {
            TINFLStatus::Done | TINFLStatus::Adler32Mismatch => {
                out.truncate(out_pos);
                return Ok(out);
            }
            TINFLStatus::HasMoreOutput => {
                if out.len() >= limit {
                    return Err(Error::Limit(format!("decoded stream larger than {} bytes", limit)));
                }
                let new_len = out.len().saturating_mul(2).min(limit);
                out.resize(new_len, 0);
            }
            // Input ended before the end of the compressed data: keep what we have.
            TINFLStatus::FailedCannotMakeProgress | TINFLStatus::NeedsMoreInput => {
                out.truncate(out_pos);
                return Ok(out);
            }
            _ => return Err(Error::syntax(None, "corrupt FlateDecode data")),
        }
    }
}

/// Undo the predictor named in `/DecodeParms` (7.4.4.3, 7.4.4.4).
fn apply_predictor(data: Vec<u8>, parms: Option<&Dict>, resolve: Resolver<'_>) -> Result<Vec<u8>> {
    let Some(parms) = parms else {
        return Ok(data);
    };
    let predictor = int_param(parms, "Predictor", 1, resolve)?;
    if predictor == 1 {
        return Ok(data);
    }
    let colors = int_param(parms, "Colors", 1, resolve)?;
    let bpc = int_param(parms, "BitsPerComponent", 8, resolve)?;
    let columns = int_param(parms, "Columns", 1, resolve)?;
    if !(1..=MAX_COLORS).contains(&colors) {
        return Err(Error::syntax(None, format!("/Colors {colors} is out of range")));
    }
    if !matches!(bpc, 1 | 2 | 4 | 8 | 16) {
        return Err(Error::syntax(None, format!("/BitsPerComponent {bpc} is not valid")));
    }
    if !(1..=MAX_COLUMNS).contains(&columns) {
        return Err(Error::syntax(None, format!("/Columns {columns} is out of range")));
    }
    // All three are positive and bounded, so none of this overflows (7.4.4.4:
    // a row occupies a whole number of bytes).
    let row_bits = u64::try_from(colors * bpc * columns).unwrap_or(u64::MAX);
    let row_bytes = usize::try_from(row_bits.div_ceil(8)).unwrap_or(usize::MAX - 1);
    let colors = usize::try_from(colors).unwrap_or(1);
    let bpc = usize::try_from(bpc).unwrap_or(8);
    let columns = usize::try_from(columns).unwrap_or(1);
    match predictor {
        2 => Ok(tiff_unpredict(data, row_bytes, colors, bpc, columns)),
        10..=15 => {
            // 7.4.4.4: PNG predicts byte by byte; the "previous pixel" is
            // `bpp` bytes back, at least one byte.
            let bpp = (colors * bpc).div_ceil(8).max(1);
            png_unpredict(&data, row_bytes, bpp)
        }
        other => Err(Error::syntax(None, format!("invalid /Predictor {other}"))),
    }
}

fn int_param(parms: &Dict, key: &str, default: i64, resolve: Resolver<'_>) -> Result<i64> {
    let Some(entry) = parms.get(key) else {
        return Ok(default);
    };
    Ok(match resolve(entry)? {
        Object::Integer(i) => i,
        Object::Real(r) if r.is_finite() && r.fract() == 0.0 && r.abs() < 1e15 => r as i64,
        _ => default,
    })
}

/// PNG filter types (RFC 2083 section 6): None, Sub, Up, Average, Paeth. Every
/// row starts with its filter-type byte.
fn png_unpredict(data: &[u8], row_bytes: usize, bpp: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev: Vec<u8> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    // The last row may be cut short; it is decoded as far as it goes.
    for chunk in data.chunks(row_bytes.saturating_add(1)) {
        let Some((&tag, raw)) = chunk.split_first() else {
            break;
        };
        cur.clear();
        for (i, &x) in raw.iter().enumerate() {
            let left = i.checked_sub(bpp).and_then(|j| cur.get(j)).copied().unwrap_or(0);
            let up = prev.get(i).copied().unwrap_or(0);
            let up_left = i.checked_sub(bpp).and_then(|j| prev.get(j)).copied().unwrap_or(0);
            let value = match tag {
                0 => x,
                1 => x.wrapping_add(left),
                2 => x.wrapping_add(up),
                3 => {
                    let avg = (u16::from(left) + u16::from(up)) / 2;
                    x.wrapping_add(u8::try_from(avg).unwrap_or(0))
                }
                4 => x.wrapping_add(paeth(left, up, up_left)),
                _ => {
                    return Err(Error::syntax(None, format!("invalid PNG filter type {tag}")));
                }
            };
            cur.push(value);
        }
        out.extend_from_slice(&cur);
        std::mem::swap(&mut prev, &mut cur);
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i32::from(a), i32::from(b), i32::from(c));
    let p = ia + ib - ic;
    let pa = (p - ia).abs();
    let pb = (p - ib).abs();
    let pc = (p - ic).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// TIFF Predictor 2 (7.4.4.4): each colour component is stored as the
/// difference from the same component of the sample to its left. Samples
/// outside the image are 0, so every row starts afresh.
fn tiff_unpredict(mut data: Vec<u8>, row_bytes: usize, colors: usize, bpc: usize, columns: usize) -> Vec<u8> {
    let mut left = vec![0u16; colors];
    for row in data.chunks_mut(row_bytes.max(1)) {
        left.fill(0);
        match bpc {
            8 => {
                for (i, byte) in row.iter_mut().enumerate() {
                    if let Some(l) = left.get_mut(i % colors) {
                        let v = (*byte).wrapping_add(u8::try_from(*l).unwrap_or(0));
                        *l = u16::from(v);
                        *byte = v;
                    }
                }
            }
            16 => {
                for (i, [hi, lo]) in row.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    if let Some(l) = left.get_mut(i % colors) {
                        let v = u16::from_be_bytes([*hi, *lo]).wrapping_add(*l);
                        *l = v;
                        [*hi, *lo] = v.to_be_bytes();
                    }
                }
            }
            _ => {
                // 1, 2 or 4 bits per component, packed high bit first.
                let mask = (1u8 << bpc) - 1;
                for j in 0..columns.saturating_mul(colors) {
                    let bit = j * bpc;
                    let shift = 8 - bpc - bit % 8;
                    let Some(byte) = row.get_mut(bit / 8) else {
                        break;
                    };
                    let Some(l) = left.get_mut(j % colors) else {
                        break;
                    };
                    let v = ((*byte >> shift) & mask).wrapping_add(u8::try_from(*l).unwrap_or(0)) & mask;
                    *l = u16::from(v);
                    *byte = (*byte & !(mask << shift)) | (v << shift);
                }
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniz_oxide::deflate::compress_to_vec_zlib;

    fn dict(entries: Vec<(&str, Object)>) -> Dict {
        entries.into_iter().map(|(k, v)| (Name::from(k), v)).collect()
    }

    fn parms(predictor: i64, colors: i64, bpc: i64, columns: i64) -> Object {
        Object::Dict(dict(vec![
            ("Predictor", Object::Integer(predictor)),
            ("Colors", Object::Integer(colors)),
            ("BitsPerComponent", Object::Integer(bpc)),
            ("Columns", Object::Integer(columns)),
        ]))
    }

    fn flate_dict(parms: Option<Object>) -> Dict {
        let mut entries = vec![("Filter", Object::from("FlateDecode"))];
        if let Some(p) = parms {
            entries.push(("DecodeParms", p));
        }
        dict(entries)
    }

    #[test]
    fn no_filter_returns_the_data() {
        assert_eq!(decode_direct(&Dict::new(), b"abc").unwrap(), b"abc");
        let empty_array = dict(vec![("Filter", Object::Array(vec![]))]);
        assert_eq!(decode_direct(&empty_array, b"abc").unwrap(), b"abc");
    }

    #[test]
    fn flate_round_trip() {
        let original: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let packed = compress_to_vec_zlib(&original, 6);
        assert_eq!(decode_direct(&flate_dict(None), &packed).unwrap(), original);
        assert_eq!(decode_direct(&flate_dict(None), &compress_to_vec_zlib(b"", 6)).unwrap(), b"");
        assert_eq!(decode_direct(&flate_dict(None), b"").unwrap(), b"");
    }

    #[test]
    fn flate_in_filter_array_twice() {
        let original = b"hello hello hello hello".to_vec();
        let twice = compress_to_vec_zlib(&compress_to_vec_zlib(&original, 6), 6);
        let d = dict(vec![("Filter", Object::Array(vec![Object::from("FlateDecode"), Object::from("FlateDecode")]))]);
        assert_eq!(decode_direct(&d, &twice).unwrap(), original);
    }

    #[test]
    fn flate_tolerates_truncation_and_bad_checksum() {
        let original: Vec<u8> = (0..20_000u32).map(|i| (i * 7 % 256) as u8).collect();
        let packed = compress_to_vec_zlib(&original, 6);
        // Truncated: we get a prefix.
        let cut = packed.get(..packed.len() / 2).unwrap();
        let out = decode_direct(&flate_dict(None), cut).unwrap();
        assert!(!out.is_empty() && out.len() < original.len());
        assert_eq!(&original[..out.len()], &out[..]);
        // Wrong Adler-32: data still returned in full.
        let mut bad = packed.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        assert_eq!(decode_direct(&flate_dict(None), &bad).unwrap(), original);
    }

    #[test]
    fn flate_with_leading_newline() {
        let packed = compress_to_vec_zlib(b"payload", 6);
        let mut data = b"\r\n".to_vec();
        data.extend_from_slice(&packed);
        assert_eq!(decode_direct(&flate_dict(None), &data).unwrap(), b"payload");
    }

    #[test]
    fn flate_garbage_is_an_error() {
        let r = decode_direct(&flate_dict(None), b"this is not zlib data at all");
        assert!(matches!(r, Err(Error::Syntax { .. })), "{r:?}");
    }

    #[test]
    fn flate_output_limit() {
        let packed = compress_to_vec_zlib(&vec![0u8; 100_000], 6);
        let d = flate_dict(None);
        let r = decode_with_limit(&d, &packed, &|o| Ok(o.clone()), 10_000);
        assert!(matches!(r, Err(Error::Limit(_))), "{r:?}");
        // The same data under a big enough limit decodes.
        let ok = decode_with_limit(&d, &packed, &|o| Ok(o.clone()), 100_000).unwrap();
        assert_eq!(ok.len(), 100_000);
        let ok = decode_with_limit(&d, &packed, &|o| Ok(o.clone()), 1 << 20).unwrap();
        assert_eq!(ok.len(), 100_000);
    }

    #[test]
    fn other_filters_are_unsupported() {
        for name in [
            "ASCIIHexDecode",
            "ASCII85Decode",
            "LZWDecode",
            "DCTDecode",
            "RunLengthDecode",
            "CCITTFaxDecode",
            "JBIG2Decode",
            "JPXDecode",
            "Bogus",
        ] {
            let d = dict(vec![("Filter", Object::from(name))]);
            assert!(matches!(decode_direct(&d, b"x"), Err(Error::Unsupported(_))), "{name}");
        }
        // Flate first, then something we cannot decode: the pipeline fails as a whole.
        let d = dict(vec![("Filter", Object::Array(vec![Object::from("FlateDecode"), Object::from("DCTDecode")]))]);
        let packed = compress_to_vec_zlib(b"jpeg bytes", 6);
        assert!(matches!(decode_direct(&d, &packed), Err(Error::Unsupported(_))));
    }

    #[test]
    fn crypt_identity_is_a_no_op() {
        let d = dict(vec![("Filter", Object::from("Crypt"))]);
        assert_eq!(decode_direct(&d, b"abc").unwrap(), b"abc");
        let d = dict(vec![
            ("Filter", Object::from("Crypt")),
            ("DecodeParms", Object::Dict(dict(vec![("Name", Object::from("Identity"))]))),
        ]);
        assert_eq!(decode_direct(&d, b"abc").unwrap(), b"abc");
        let d = dict(vec![
            ("Filter", Object::from("Crypt")),
            ("DecodeParms", Object::Dict(dict(vec![("Name", Object::from("StdCF"))]))),
        ]);
        assert!(matches!(decode_direct(&d, b"abc"), Err(Error::Unsupported(_))));
    }

    #[test]
    fn indirect_filter_and_parms_go_through_the_resolver() {
        use crate::object::ObjRef;
        let original = vec![1u8, 2, 3, 4, 5, 6];
        // Columns 3: two rows, PNG Up on both.
        let filtered = [2u8, 1, 2, 3, 2, 3, 3, 3];
        let packed = compress_to_vec_zlib(&filtered, 6);
        let d = dict(vec![
            ("Filter", Object::Ref(ObjRef::new(1, 0))),
            ("DecodeParms", Object::Array(vec![Object::Ref(ObjRef::new(2, 0))])),
        ]);
        let resolve = |o: &Object| -> Result<Object> {
            Ok(match o {
                Object::Ref(r) if r.num == 1 => Object::Array(vec![Object::Ref(ObjRef::new(3, 0))]),
                Object::Ref(r) if r.num == 2 => parms(12, 1, 8, 3),
                Object::Ref(r) if r.num == 3 => Object::from("FlateDecode"),
                other => other.clone(),
            })
        };
        assert_eq!(decode(&d, &packed, &resolve).unwrap(), original);
    }

    // --- predictors ---------------------------------------------------------

    fn unpredict(predictor: i64, colors: i64, bpc: i64, columns: i64, encoded: &[u8]) -> Result<Vec<u8>> {
        let packed = compress_to_vec_zlib(encoded, 6);
        decode_direct(&flate_dict(Some(parms(predictor, colors, bpc, columns))), &packed)
    }

    #[test]
    fn png_all_filter_types() {
        // Hand-computed from RFC 2083: 3 one-byte samples per row.
        //   Sub:     10 5 5   -> 10 15 20
        //   Up:      1 1 1    -> 11 16 21
        //   Average: 0 0 0    -> 5 10 15
        //   Paeth:   1 1 1    -> 6 11 16
        //   None:    7 8 9    -> 7 8 9
        let encoded = [1, 10, 5, 5, 2, 1, 1, 1, 3, 0, 0, 0, 4, 1, 1, 1, 0, 7, 8, 9];
        let expected = [10, 15, 20, 11, 16, 21, 5, 10, 15, 6, 11, 16, 7, 8, 9];
        for predictor in 10..=15 {
            assert_eq!(unpredict(predictor, 1, 8, 3, &encoded).unwrap(), expected, "predictor {predictor}");
        }
    }

    #[test]
    fn png_up_as_used_by_xref_streams() {
        // Three rows of [type, offset hi, offset lo, gen] with PNG Up (tag 2).
        let rows = [[1u8, 0x00, 0x10, 0], [1, 0x00, 0x50, 0], [1, 0x01, 0x20, 0]];
        let mut encoded = Vec::new();
        let mut prev = [0u8; 4];
        for row in &rows {
            encoded.push(2);
            for (x, p) in row.iter().zip(prev.iter()) {
                encoded.push(x.wrapping_sub(*p));
            }
            prev = *row;
        }
        let expected: Vec<u8> = rows.iter().flatten().copied().collect();
        assert_eq!(unpredict(12, 1, 8, 4, &encoded).unwrap(), expected);
    }

    #[test]
    fn png_with_multi_byte_pixels() {
        // RGB: bpp = 3. Sub filter must look 3 bytes back.
        let encoded = [1u8, 10, 20, 30, 1, 2, 3];
        assert_eq!(unpredict(11, 3, 8, 2, &encoded).unwrap(), [10, 20, 30, 11, 22, 33]);
        // 16 bits per component, one component: bpp = 2.
        let encoded = [1u8, 0, 1, 0, 1];
        assert_eq!(unpredict(11, 1, 16, 2, &encoded).unwrap(), [0, 1, 0, 2]);
        // 1 bit per pixel: bpp rounds up to 1.
        let encoded = [1u8, 0b1010_0000, 0b0101_0000];
        assert_eq!(unpredict(11, 1, 1, 16, &encoded).unwrap(), [0b1010_0000, 0b1111_0000]);
    }

    #[test]
    fn png_partial_last_row_and_bad_tag() {
        // Last row cut short: decoded as far as it goes.
        let encoded = [2u8, 1, 2, 3, 2, 1];
        assert_eq!(unpredict(12, 1, 8, 3, &encoded).unwrap(), [1, 2, 3, 2]);
        // A trailing lone tag byte adds nothing.
        let encoded = [0u8, 1, 2, 3, 0];
        assert_eq!(unpredict(10, 1, 8, 3, &encoded).unwrap(), [1, 2, 3]);
        // Unknown filter type.
        let encoded = [9u8, 1, 2, 3];
        assert!(unpredict(10, 1, 8, 3, &encoded).is_err());
    }

    #[test]
    fn tiff_predictor_8_bit() {
        let encoded = [1u8, 2, 3, 1, 1, 1, 10, 20, 30, 1, 2, 3];
        assert_eq!(unpredict(2, 3, 8, 2, &encoded).unwrap(), [1, 2, 3, 2, 3, 4, 10, 20, 30, 11, 22, 33]);
        // Wrap-around at 256.
        assert_eq!(unpredict(2, 1, 8, 2, &[200, 100]).unwrap(), [200, 44]);
    }

    #[test]
    fn tiff_predictor_16_bit() {
        let encoded = [0x00, 0xFF, 0x00, 0x01, 0xFF, 0xFF];
        assert_eq!(unpredict(2, 1, 16, 3, &encoded).unwrap(), [0x00, 0xFF, 0x01, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn tiff_predictor_sub_byte_depths() {
        // 4 bits: nibbles 1 2 3 4 -> 1 3 6 10
        assert_eq!(unpredict(2, 1, 4, 4, &[0x12, 0x34]).unwrap(), [0x13, 0x6A]);
        // Modulo 16.
        assert_eq!(unpredict(2, 1, 4, 2, &[0xF2]).unwrap(), [0xF1]);
        // 1 bit: running parity.
        assert_eq!(unpredict(2, 1, 1, 8, &[0xFF]).unwrap(), [0xAA]);
        // 2 bits, two colours: the left neighbour of a component is two samples back.
        // samples (c0 c1 c0 c1) = 1 1 1 2 -> 1 1 2 3 -> 0b01_01_10_11
        assert_eq!(unpredict(2, 2, 2, 2, &[0b01_01_01_10]).unwrap(), [0b01_01_10_11]);
        // Rows are byte aligned: 3 columns of 4 bits is 2 bytes with 4 padding bits.
        assert_eq!(unpredict(2, 1, 4, 3, &[0x11, 0x10, 0x22, 0x20]).unwrap(), [0x12, 0x30, 0x24, 0x60]);
    }

    #[test]
    fn predictor_one_and_missing_parms_do_nothing() {
        let packed = compress_to_vec_zlib(b"abc", 6);
        let d = flate_dict(Some(parms(1, 1, 8, 1)));
        assert_eq!(decode_direct(&d, &packed).unwrap(), b"abc");
        let d = flate_dict(Some(Object::Dict(Dict::new())));
        assert_eq!(decode_direct(&d, &packed).unwrap(), b"abc");
    }

    #[test]
    fn invalid_predictor_parameters_are_errors() {
        for (p, c, b, w) in [
            (7, 1, 8, 1),
            (12, 0, 8, 1),
            (12, 1, 3, 1),
            (12, 1, 8, 0),
            (12, 1, 8, -4),
            (12, i64::MAX, 8, 1),
            (12, 1, 8, i64::MAX),
        ] {
            assert!(unpredict(p, c, b, w, b"xxxx").is_err(), "{p} {c} {b} {w}");
        }
    }

    #[test]
    fn huge_columns_with_small_data_do_not_allocate_rows() {
        // Columns near the cap, data far shorter than one row.
        let out = unpredict(12, 4096, 16, 1 << 28, &[2, 1, 2, 3]).unwrap();
        assert_eq!(out, [1, 2, 3]);
        let out = unpredict(2, 4096, 16, 1 << 28, &[1, 2, 3]).unwrap();
        assert_eq!(out.len(), 3);
    }
}

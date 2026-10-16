//! Making a file smaller: the pieces that work on the bytes of its streams and pictures. `qingpdf-core::compress` decides what to do
//! to which object; this is how a stream's data is coded again ([`tidy_stream`]) and how a picture is made smaller ([`image`], with
//! the JPEG encoder of [`jpeg`]).

pub mod image;

pub use qingpdf_base::squeeze::{average, jpeg};

use miniz_oxide::deflate::compress_to_vec_zlib;

use crate::error::Result;
use crate::filter;
use crate::object::{Dict, Name, Object};
use crate::writer::BEST_FLATE_LEVEL;

/// Streams of fewer bytes than this are left as they are: nothing to gain.
const MIN_TIDY_BYTES: usize = 48;

/// Is `filter` (the entry of a dictionary) Flate alone: the name, or an array of that one name? `false` for anything else, an indirect
/// entry too.
fn is_flate_alone(filter: Option<&Object>) -> bool {
    match filter {
        Some(Object::Name(n)) => n == "FlateDecode",
        Some(Object::Array(items)) => matches!(items.as_slice(), [Object::Name(n)] if n == "FlateDecode"),
        _ => false,
    }
}

/// Does a program keep the data of this stream as it is: the metadata (a reader of the file's text may look for the XML in the bytes), and
/// a stream of the file's own structure?
fn kept_as_it_is(dict: &Dict) -> bool {
    ["Metadata", "XRef", "ObjStm"].iter().any(|t| dict.get_name("Type").is_some_and(|n| n == t)) || dict.contains_key("F")
}

/// Code the data of a stream again so that it is smaller, without changing what it decodes to (the dictionary's other entries stay
/// true): data that is Flate-coded is inflated and deflated again at the best level, data that is not coded at all is Flate-coded. The
/// data stays as it was when the new one is not smaller, or when the stream is coded in a way not done here, or when it inflates to more
/// than `limit` bytes. `charge` is told how many bytes are about to be deflated (the work is paid for before it is done: an error from it
/// ends this). `true` when it changed.
pub fn tidy_stream(dict: &mut Dict, data: &mut Vec<u8>, limit: usize, charge: &mut dyn FnMut(usize) -> Result<()>) -> Result<bool> {
    if data.len() < MIN_TIDY_BYTES || kept_as_it_is(dict) {
        return Ok(false);
    }
    let packed = if is_flate_alone(dict.get("Filter")) {
        // (A predictor in /DecodeParms was applied before the data was deflated, and stays true for the new deflation of the same bytes.)
        let Ok(raw) = filter::inflate(data, limit) else { return Ok(false) };
        charge(raw.len())?;
        compress_to_vec_zlib(&raw, BEST_FLATE_LEVEL)
    } else if dict.get("Filter").is_none() && !dict.contains_key("DecodeParms") {
        charge(data.len())?;
        compress_to_vec_zlib(data, BEST_FLATE_LEVEL)
    } else {
        return Ok(false);
    };
    if packed.len() >= data.len() {
        return Ok(false);
    }
    if dict.get("Filter").is_none() {
        dict.set("Filter", Object::Name(Name::from("FlateDecode")));
    }
    *data = packed;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_dict(filter: Option<&str>) -> Dict {
        let mut d = Dict::new();
        if let Some(f) = filter {
            d.set("Filter", Object::from(f));
        }
        d
    }

    #[test]
    fn data_deflated_at_a_low_level_is_deflated_again_and_decodes_the_same() {
        let text: Vec<u8> = (0..4000).flat_map(|i| format!("{} {} m {} l S\n", i % 97, i % 31, i % 13).into_bytes()).collect();
        let mut data = miniz_oxide::deflate::compress_to_vec_zlib(&text, 1);
        let before = data.len();
        let mut dict = stream_dict(Some("FlateDecode"));
        assert!(tidy_stream(&mut dict, &mut data, 1 << 24, &mut |_| Ok(())).unwrap());
        assert!(data.len() < before);
        assert_eq!(filter::inflate(&data, 1 << 24).unwrap(), text);
        // Already at the best: the second time nothing changes.
        let again = data.clone();
        assert!(!tidy_stream(&mut dict, &mut data, 1 << 24, &mut |_| Ok(())).unwrap());
        assert_eq!(data, again);
    }

    #[test]
    fn data_not_coded_is_deflated_and_named_but_metadata_and_odd_filters_are_left() {
        let text = b"BT /F1 12 Tf (hello) Tj ET\n".repeat(50);
        let mut data = text.clone();
        let mut dict = Dict::new();
        assert!(tidy_stream(&mut dict, &mut data, 1 << 24, &mut |_| Ok(())).unwrap());
        assert_eq!(dict.get_name("Filter").unwrap(), &Name::from("FlateDecode"));
        assert_eq!(filter::inflate(&data, 1 << 20).unwrap(), text);
        // Metadata stays readable as it is; a filter chain it does not know, and too little data, are left.
        let mut meta = Dict::from_iter([(Name::from("Type"), Object::from("Metadata"))]);
        let mut xml = b"<x:xmpmeta/>".repeat(40);
        assert!(!tidy_stream(&mut meta, &mut xml, 1 << 24, &mut |_| Ok(())).unwrap() && xml.len() == 12 * 40);
        let mut chain = Dict::from_iter([(Name::from("Filter"), Object::Array(vec![Object::from("ASCII85Decode"), Object::from("FlateDecode")]))]);
        let mut d = text.clone();
        assert!(!tidy_stream(&mut chain, &mut d, 1 << 24, &mut |_| Ok(())).unwrap() && d == text);
        let mut small = b"q Q".to_vec();
        assert!(!tidy_stream(&mut Dict::new(), &mut small, 1 << 24, &mut |_| Ok(())).unwrap());
        // Garbage that claims to be Flate stays as it is.
        let mut junk = vec![7u8; 200];
        let mut dict = stream_dict(Some("FlateDecode"));
        assert!(!tidy_stream(&mut dict, &mut junk, 1 << 24, &mut |_| Ok(())).unwrap() && junk == vec![7u8; 200]);
    }
    #[test]
    fn a_stream_that_inflates_to_more_than_the_limit_is_left_and_what_is_deflated_is_charged_first() {
        let text = vec![b'a'; 1 << 20];
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&text, 1);
        // More than the limit once inflated: left as it is, and nothing was charged (nothing was done).
        let mut data = packed.clone();
        let mut dict = stream_dict(Some("FlateDecode"));
        let mut charged = 0usize;
        assert!(!tidy_stream(&mut dict, &mut data, 1 << 19, &mut |n| {
            charged += n;
            Ok(())
        })
        .unwrap());
        assert_eq!((data == packed, charged), (true, 0));
        // Within the limit: the inflated size is what is charged, before the deflating.
        let mut charged = 0usize;
        let mut dict = stream_dict(Some("FlateDecode"));
        let _ = tidy_stream(&mut dict, &mut data, 1 << 21, &mut |n| {
            charged += n;
            Ok(())
        })
        .unwrap();
        assert_eq!(charged, 1 << 20);
        // Not coded: its own length. A refusal of the charge ends it with that error, and the data is as it was.
        let mut raw = b"BT /F1 12 Tf (hello) Tj ET
".repeat(50);
        let before = raw.clone();
        let mut dict = Dict::new();
        let e = tidy_stream(&mut dict, &mut raw, 1 << 24, &mut |n| {
            assert_eq!(n, before.len());
            Err(crate::Error::Cancelled)
        })
        .unwrap_err();
        assert!(matches!(e, crate::Error::Cancelled) && raw == before && dict.get("Filter").is_none());
    }
}

//! Hard rules 4 and 6 for encrypted files: what a hostile file can make the engine
//! do, in time and memory, however its encryption is written.
//!
//! - an `/Encrypt` dictionary that points sixty-four times at a 32 MiB string (the
//!   reader used to copy every target in full and look at the total afterwards:
//!   2 GB and six seconds for a 33 MB file);
//! - a file whose objects are spread over two object streams of 50 MiB each in an order
//!   that makes every object a re-read of the stream the cache has just let go (the
//!   re-reads copy, and for an encrypted file decrypt, all of it: minutes of work).
//!
//! The time limits are for the optimised build (`cargo test --release`).
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use md5::{Digest, Md5};
use qingpdf_core::{Document, Error, Object, ops, writer};

/// Generous: these must end in a limit error rather than run for minutes, also on a throttled laptop.
fn limit() -> Duration {
    if cfg!(debug_assertions) { Duration::from_secs(60) } else { Duration::from_secs(15) }
}

fn generated(part: &str) -> common::Encrypted {
    common::generated().into_iter().find(|r| r.path.to_string_lossy().contains(part)).expect("a generated file")
}

// --- /Encrypt pointing at huge objects ---------------------------------------------------------

/// The file with `/Junk [7 0 R ... x64]` added to its encryption dictionary and an object
/// 900 that is a string of `size` bytes, which the junk points at.
fn with_junk(file: &[u8], target_size: usize) -> Vec<u8> {
    let mut bytes = file.to_vec();
    let at = bytes.windows(17).position(|w| w == b"/Filter /Standard").expect("the encryption dictionary");
    let junk = std::iter::repeat_n("900 0 R", 64).collect::<Vec<_>>().join(" ");
    let insert = format!("/Junk [{junk}] ");
    bytes.splice(at..at, insert.bytes());
    let xref = (0..bytes.len()).rev().find(|&i| bytes[i..].starts_with(b"\nxref\n")).expect("an xref table") + 1;
    let mut object = b"900 0 obj\n(".to_vec();
    object.resize(object.len() + target_size, b'A');
    object.extend_from_slice(b")\nendobj\n");
    bytes.splice(xref..xref, object);
    common::with_fresh_xref(&bytes)
}

#[test]
fn an_encrypt_dictionary_that_points_at_huge_objects_is_a_limit_not_two_gigabytes() {
    let row = generated("utf8.r3-128-rc4-empty-print-none");
    let file = std::fs::read(&row.path).unwrap();
    // Sixty-four references to a 32 MiB string: refused after the first look, fast.
    let hostile = with_junk(&file, 32 << 20);
    assert!(hostile.len() > 32 << 20);
    let started = Instant::now();
    let result = Document::from_bytes(hostile.clone());
    let took = started.elapsed();
    assert!(matches!(&result, Err(Error::Limit(m)) if m.contains("encryption dictionary")), "{:?}", result.map(|_| ()));
    assert!(took < limit() / 4, "opening took {took:?}");
    // The same as a direct value of the dictionary and as a longer chain: no matter how it is written.
    let started = Instant::now();
    let with_password = Document::from_bytes_with_password(hostile, "password");
    assert!(matches!(with_password, Err(Error::Limit(_))));
    assert!(started.elapsed() < limit() / 4);
    // Repeats are counted: sixty-four references to a 100 KB string is 6.4 MB.
    let repeated = with_junk(&file, 100 * 1000);
    assert!(matches!(Document::from_bytes(repeated), Err(Error::Limit(_))));
    // What is legitimate still opens: sixty-four references to a 1 KB string (64 KB all told).
    let small = with_junk(&file, 1000);
    let doc = Document::from_bytes(small).expect("a small dictionary of references opens");
    assert!(!doc.is_locked() && doc.page_count().is_ok());
    // And one reference to a string just under the limit.
    let one = {
        let mut bytes = file.clone();
        let at = bytes.windows(17).position(|w| w == b"/Filter /Standard").unwrap();
        bytes.splice(at..at, b"/Junk 900 0 R ".iter().copied());
        let xref = (0..bytes.len()).rev().find(|&i| bytes[i..].starts_with(b"\nxref\n")).unwrap() + 1;
        let mut object = b"900 0 obj\n(".to_vec();
        object.resize(object.len() + 900_000, b'A');
        object.extend_from_slice(b")\nendobj\n");
        bytes.splice(xref..xref, object);
        common::with_fresh_xref(&bytes)
    };
    assert!(Document::from_bytes(one).is_ok());
}

// --- re-reading object streams -----------------------------------------------------------------

fn rc4(key: &[u8], data: &mut [u8]) {
    let mut s: Vec<u8> = (0..=255u8).collect();
    let mut j = 0u8;
    for i in 0..256usize {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    for byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[usize::from(i)]);
        s.swap(usize::from(i), usize::from(j));
        *byte ^= s[usize::from(s[usize::from(i)].wrapping_add(s[usize::from(j)]))];
    }
}

fn aes128_cbc(key: &[u8], iv: [u8; 16], data: &[u8]) -> Vec<u8> {
    use aes::Aes128;
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    let cipher = Aes128::new_from_slice(key).unwrap();
    let mut out = iv.to_vec();
    let mut padded = data.to_vec();
    let pad = 16 - data.len() % 16;
    padded.extend(std::iter::repeat_n(pad as u8, pad));
    let mut previous = iv;
    for chunk in padded.chunks(16) {
        let mut block = [0u8; 16];
        for (i, b) in block.iter_mut().enumerate() {
            *b = chunk[i] ^ previous[i];
        }
        let mut array = aes::cipher::Array::from(block);
        cipher.encrypt_block(&mut array);
        previous.copy_from_slice(&array);
        out.extend_from_slice(&previous);
    }
    out
}

/// How the object streams of the file are encrypted: the key and the method of an
/// open generated file whose encryption dictionary the new file takes over.
enum Crypt<'a> {
    None,
    Rc4(&'a Document),
    Aes128(&'a Document),
}

/// A file with `items` outline items in two object streams, the items alternating between
/// them (so that following `/Next` goes from one stream to the other and back), each stream
/// made big with `pad` bytes of a trailing comment. The object streams have no filter. The
/// catalog and the page tree are ordinary objects. Encrypted like `crypt`, if it says so.
fn thrash_file(items: usize, pad: usize, crypt: &Crypt<'_>) -> Vec<u8> {
    let first = 20usize;
    let stream_nums = [first + items, first + items + 1];
    let xref_num = first + items + 2;
    let mut buf = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets: Vec<(usize, usize)> = Vec::new(); // (object number, offset)
    let mut put = |buf: &mut Vec<u8>, num: usize, body: &[u8]| {
        offsets.push((num, buf.len()));
        buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        buf.extend_from_slice(body);
        buf.extend_from_slice(b"\nendobj\n");
    };
    put(&mut buf, 1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 3 0 R >>");
    put(&mut buf, 2, b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>");
    put(&mut buf, 3, format!("<< /Type /Outlines /First {first} 0 R /Last {} 0 R /Count {items} >>", first + items - 1).as_bytes());
    put(&mut buf, 4, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>");
    let (encrypt_text, id0, key) = match crypt {
        Crypt::None => (None, Vec::new(), Vec::new()),
        Crypt::Rc4(doc) | Crypt::Aes128(doc) => {
            let dict = doc.resolve(doc.trailer().get("Encrypt").unwrap()).unwrap();
            let mut text = Vec::new();
            writer::write_object(&mut text, &dict).unwrap();
            let id0 = match doc.resolve(doc.trailer().get("ID").unwrap()).unwrap() {
                Object::Array(a) => match &a[0] {
                    Object::String(s) => s.bytes.clone(),
                    other => panic!("{other:?}"),
                },
                other => panic!("{other:?}"),
            };
            (Some(text), id0, doc.security().unwrap().file_key().unwrap().to_vec())
        }
    };
    if let Some(text) = &encrypt_text {
        put(&mut buf, 5, text);
    }
    let mut placement: Vec<(usize, usize, usize)> = Vec::new(); // (item number, stream, index)
    for (s, &stream_num) in stream_nums.iter().enumerate() {
        let mut header = String::new();
        let mut body = Vec::new();
        let mut count = 0usize;
        for i in (0..items).filter(|i| i % 2 == s) {
            let num = first + i;
            header.push_str(&format!("{num} {} ", body.len()));
            let next = if i + 1 < items { format!("/Next {} 0 R ", num + 1) } else { String::new() };
            let prev = if i > 0 { format!("/Prev {} 0 R ", num - 1) } else { String::new() };
            body.extend_from_slice(format!("<< /Title (bm {i:06}) /Parent 3 0 R {next}{prev}/Dest [4 0 R /Fit] >>\n").as_bytes());
            placement.push((num, s, count));
            count += 1;
        }
        body.push(b'%');
        body.resize(body.len() + pad, b' ');
        let mut data = header.clone().into_bytes();
        data.extend_from_slice(&body);
        let data = match crypt {
            Crypt::None => data,
            Crypt::Rc4(_) | Crypt::Aes128(_) => {
                let aes = matches!(crypt, Crypt::Aes128(_));
                let mut input = key.clone();
                let n = stream_num as u32;
                input.extend_from_slice(&n.to_le_bytes()[..3]);
                input.extend_from_slice(&[0, 0]);
                if aes {
                    input.extend_from_slice(b"sAlT");
                }
                let object_key = Md5::digest(&input)[..(key.len() + 5).min(16)].to_vec();
                if aes {
                    aes128_cbc(&object_key, [stream_num as u8; 16], &data)
                } else {
                    let mut data = data;
                    rc4(&object_key, &mut data);
                    data
                }
            }
        };
        offsets.push((stream_num, buf.len()));
        buf.extend_from_slice(
            format!("{stream_num} 0 obj\n<< /Type /ObjStm /N {count} /First {} /Length {} >>\nstream\n", header.len(), data.len()).as_bytes(),
        );
        buf.extend_from_slice(&data);
        buf.extend_from_slice(b"\nendstream\nendobj\n");
    }
    // The cross-reference stream, not filtered.
    offsets.push((xref_num, buf.len()));
    let size = xref_num + 1;
    let mut rows = vec![[0u8; 7]; size];
    rows[0] = [0, 0, 0, 0, 0, 0xFF, 0xFF];
    for &(num, offset) in &offsets {
        let o = offset as u32;
        rows[num] = [1, (o >> 24) as u8, (o >> 16) as u8, (o >> 8) as u8, o as u8, 0, 0];
    }
    for &(num, stream, index) in &placement {
        rows[num] = [2, 0, 0, 0, stream_nums[stream] as u8, (index >> 8) as u8, index as u8];
        // The stream number may not fit one byte.
        let sn = stream_nums[stream] as u32;
        rows[num][1..5].copy_from_slice(&sn.to_be_bytes());
    }
    let data: Vec<u8> = rows.concat();
    let encrypt = if encrypt_text.is_some() {
        format!("/Encrypt 5 0 R /ID [<{0}> <{0}>] ", id0.iter().map(|b| format!("{b:02x}")).collect::<String>())
    } else {
        String::new()
    };
    buf.extend_from_slice(
        format!("{xref_num} 0 obj\n<< /Type /XRef /Size {size} /W [1 4 2] /Root 1 0 R {encrypt}/Length {} >>\nstream\n", data.len()).as_bytes(),
    );
    buf.extend_from_slice(&data);
    buf.extend_from_slice(b"\nendstream\nendobj\n");
    buf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", offsets.last().unwrap().1).as_bytes());
    buf
}

/// The cache of decoded object streams holds 96 MiB: two streams of 50 MiB cannot both
/// stay in it, so every outline item is a re-read, and a re-read copies (and, in an
/// encrypted file, decrypts) 50 MiB. That ended, after minutes, in a success. It ends in a
/// limit error, in seconds, because the re-reads are charged to the decoding budget.
#[test]
fn rereading_big_object_streams_ends_in_a_limit_within_seconds() {
    let pad = 50 << 20;
    let rc4_source = generated("utf8.r3-128-rc4-empty-print-none");
    let aes_source = generated("objstm.r4-aes128-empty-print-none");
    let rc4_doc = Document::open(&rc4_source.path).unwrap();
    let aes_doc = Document::open(&aes_source.path).unwrap();
    for (what, crypt) in [("plain", Crypt::None), ("RC4", Crypt::Rc4(&rc4_doc)), ("AES-128", Crypt::Aes128(&aes_doc))] {
        let file = thrash_file(200, pad, &crypt);
        let doc = Document::from_bytes(file).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert!(!doc.is_locked(), "{what}");
        let started = Instant::now();
        let result = ops::extract_pages(&doc, &[0]);
        let took = started.elapsed();
        assert!(matches!(&result, Err(Error::Limit(m)) if m.contains("decoding work")), "{what}: {:?}", result.map(|o| o.pages));
        assert!(took < limit(), "{what}: took {took:?}");
        println!("{what}: a limit error after {took:?}");
    }
}

/// The charge is for re-reads only: a file whose object streams fit the cache is read once
/// each, however many objects are taken from them, and costs nothing.
#[test]
fn object_streams_that_fit_the_cache_are_read_once_and_free() {
    let doc = Document::from_bytes(thrash_file(2000, 1 << 20, &Crypt::None)).unwrap();
    let started = Instant::now();
    let out = ops::extract_pages(&doc, &[0]).expect("two small object streams are no problem");
    assert_eq!(out.pages, 1);
    assert!(started.elapsed() < limit());
}

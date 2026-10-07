//! Layer 1.5: encrypted files. qpdf is the reference answer: the file key we
//! find, every string and stream we decrypt (compared byte for byte with what
//! `qpdf --decrypt` writes), and what qpdf thinks of the encrypted files we
//! write. Without qpdf the qpdf-based checks say so and do nothing.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use qingpdf_core::info;
use qingpdf_core::ops::{self, Input};
use qingpdf_core::security::{Method, PasswordKind};
use qingpdf_core::{Document, Error, Object};

// --- the files and their passwords --------------------------------------------------

use common::{Encrypted as Row, KNOWN, generated};

fn corpus_path(rel: &str) -> PathBuf {
    common::corpus_root().join(rel)
}

fn open(path: &Path, password: &str) -> Document {
    Document::open_with_password(path, password).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn name(path: &Path) -> String {
    common::corpus_name(path)
}

// --- comparing objects ----------------------------------------------------------------

/// Every string in the object (inside arrays and dictionaries too), as bytes,
/// sorted: how a string is written (hexadecimal or literal) and the order of
/// the keys of a dictionary do not matter, and qpdf's QDF output adds things
/// of its own (a `/MediaBox` where one is missing) that are not what is being
/// compared.
fn strings_of(obj: &Object) -> Vec<Vec<u8>> {
    fn collect(obj: &Object, out: &mut Vec<Vec<u8>>) {
        match obj {
            Object::String(s) => out.push(s.bytes.clone()),
            Object::Array(items) => items.iter().for_each(|i| collect(i, out)),
            Object::Dict(d) => d.iter().for_each(|(_, v)| collect(v, out)),
            Object::Stream(s) => s.dict.iter().for_each(|(_, v)| collect(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    collect(obj, &mut out);
    out.sort();
    out
}

/// Is `ours` what qpdf made of the same object: the same strings, and for a
/// stream the same data (the bytes after decryption, before any filter).
fn same(ours: &Object, theirs: &Object, at: &str) -> Result<(), String> {
    if strings_of(ours) != strings_of(theirs) {
        let show = |o: &Object| strings_of(o).iter().map(|s| String::from_utf8_lossy(&s[..s.len().min(40)]).into_owned()).collect::<Vec<_>>();
        return Err(format!("{at}: the strings differ: {:?} and {:?}", show(ours), show(theirs)));
    }
    match (ours, theirs) {
        (Object::Stream(p), Object::Stream(q)) => {
            if p.data == q.data {
                Ok(())
            } else {
                let first = p.data.iter().zip(&q.data).position(|(m, n)| m != n);
                Err(format!("{at}: stream data differs ({} and {} bytes, first difference at {first:?})", p.data.len(), q.data.len()))
            }
        }
        (Object::Stream(_), _) | (_, Object::Stream(_)) => Err(format!("{at}: only one of them is a stream")),
        _ => Ok(()),
    }
}

/// Compare every object of `doc` (opened with `password`) with qpdf's
/// decrypted version of `file`. Returns how many objects were compared.
fn compare_with_qpdf(qpdf: &Path, file: &Path, password: &str, doc: &Document, out_dir: &Path, tag: &str) -> usize {
    let decrypted = out_dir.join(format!("{tag}.qpdf-decrypted.pdf"));
    assert!(common::qpdf_decrypt(qpdf, file, password, &decrypted), "{}: qpdf could not decrypt it", file.display());
    let bytes = std::fs::read(&decrypted).unwrap();
    let numbers = common::qdf_original_numbers(&bytes);
    let theirs = Document::from_bytes(bytes).unwrap_or_else(|e| panic!("{}: cannot read what qpdf wrote: {e}", file.display()));
    assert!(!theirs.is_encrypted(), "{}: qpdf's output is still encrypted", file.display());
    let mut compared = 0usize;
    let mut problems: Vec<String> = Vec::new();
    for r in doc.object_refs() {
        let ours = match doc.get(r) {
            Ok(o) => o,
            Err(e) => {
                problems.push(format!("object {}: {e}", r.num));
                continue;
            }
        };
        // Structure that qpdf rewrites: cross-reference and object streams, and
        // the encryption dictionary itself.
        let kind = ours.as_dict().and_then(|d| d.get_name("Type")).map(|n| n.0.clone());
        if matches!(kind.as_deref(), Some(b"XRef" | b"ObjStm"))
            || ours.as_dict().is_some_and(|d| d.contains_key("Filter") && d.contains_key("O") && d.contains_key("U"))
        {
            continue;
        }
        let Some(&there) = numbers.get(&r.num) else {
            continue; // qpdf does not write an object nothing refers to
        };
        let other = theirs.get(qingpdf_core::ObjRef::new(there, 0)).unwrap_or(Object::Null);
        compared += 1;
        if let Err(why) = same(&ours, &other, &format!("object {}", r.num)) {
            problems.push(why);
        }
    }
    assert!(problems.is_empty(), "{}: {} problem(s) against qpdf:
{}", file.display(), problems.len(), problems.join("
"));
    compared
}

fn page_contents(doc: &Document) -> Vec<Vec<u8>> {
    let mut all = Vec::new();
    for page in doc.pages().unwrap() {
        let mut bytes = Vec::new();
        let streams: Vec<Object> = match page.dict.get("Contents").map(|c| doc.resolve(c).unwrap()) {
            Some(Object::Array(items)) => items.iter().map(|i| doc.resolve(i).unwrap()).collect(),
            Some(other) => vec![other],
            None => Vec::new(),
        };
        for s in streams {
            if let Object::Stream(s) = s {
                bytes.extend(doc.decode_stream(&s).unwrap());
            }
        }
        all.push(bytes);
    }
    all
}

fn info_of(doc: &Document) -> Vec<(String, String)> {
    info::describe(doc).unwrap().info
}

// --- opening ---------------------------------------------------------------------------

#[test]
fn generated_files_open_with_the_right_passwords_and_nothing_else() {
    let qpdf = common::find_qpdf();
    for row in generated() {
        let label = name(&row.path);
        // The empty password opens exactly the files that have no user password.
        let plain = Document::open(&row.path).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert!(plain.is_encrypted(), "{label}");
        assert_eq!(plain.is_locked(), !row.user.is_empty(), "{label}");
        if row.user.is_empty() {
            let enc = plain.encryption().unwrap();
            assert!(!enc.needs_password(), "{label}");
            assert_eq!(enc.opened.map(|a| (a.kind, a.empty)), Some((PasswordKind::User, true)), "{label}");
        } else {
            assert!(matches!(plain.pages(), Err(Error::PasswordRequired) | Ok(_)), "{label}");
            assert!(plain.encryption().unwrap().needs_password());
            assert!(matches!(plain.info(), Err(Error::PasswordRequired)) || plain.trailer().get("Info").is_none(), "{label}");
        }
        // The user and the owner password, and the alternatives.
        let as_user = open(&row.path, &row.user);
        let as_owner = open(&row.path, &row.owner);
        assert!(!as_user.is_locked() && !as_owner.is_locked(), "{label}");
        if !row.user.is_empty() {
            let u = as_user.encryption().unwrap().opened.unwrap();
            assert_eq!((u.kind, u.empty), (PasswordKind::User, false), "{label}");
        }
        // The empty password is tried first, and opens a file without a user
        // password as the user's; whoever gives the owner password is the owner
        // all the same.
        let o = as_owner.encryption().unwrap().opened.unwrap();
        let expected = if row.user.is_empty() { (PasswordKind::User, true) } else { (PasswordKind::Owner, false) };
        assert_eq!((o.kind, o.empty), expected, "{label}");
        assert!(as_owner.security().unwrap().is_owner_password(&row.owner), "{label}");
        assert!(as_user.security().unwrap().is_owner_password(&row.owner), "{label}");
        assert!(!as_user.security().unwrap().is_owner_password("wrong"), "{label}");
        assert!(row.user == row.owner || !as_user.security().unwrap().is_owner_password(&row.user), "{label}: the user password is not the owner's");
        assert_eq!(as_user.security().unwrap().file_key(), as_owner.security().unwrap().file_key(), "{label}: both passwords give one key");
        for alt in &row.alternatives {
            assert!(!open(&row.path, alt).is_locked(), "{label}: {alt}");
        }
        // Wrong passwords are refused, whatever they look like.
        for wrong in ["wrong", "x", &"y".repeat(300), "密码错误", "\u{0}"] {
            if wrong == row.user || wrong == row.owner {
                continue;
            }
            let result = Document::open_with_password(&row.path, wrong);
            if row.user.is_empty() {
                // The empty password has already opened it: a wrong one is not looked at.
                assert!(result.is_ok(), "{label}: {wrong:?}");
            } else {
                assert!(matches!(result, Err(Error::WrongPassword)), "{label}: {wrong:?}: {:?}", result.map(|_| ()));
            }
        }
        // qpdf finds the same key and the same permissions.
        let Some(qpdf) = &qpdf else { continue };
        let theirs = common::qpdf_encryption(qpdf, &row.path, &row.user).unwrap_or_else(|| panic!("{label}: qpdf refuses the user password"));
        let security = as_user.security().unwrap();
        let hex: String = security.file_key().unwrap().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, theirs.key, "{label}: the file key");
        let enc = as_user.encryption().unwrap();
        assert_eq!(i64::from(security_flags(&as_user)), theirs.permissions & 0xFFFF_FFFF, "{label}: P");
        assert_eq!(enc.revision, theirs.revision, "{label}: R");
        let method = |m: Method| match m {
            Method::None => "none",
            Method::Rc4 => "RC4",
            Method::AesV2 => "AESv2",
            Method::AesV3 => "AESv3",
        };
        if !theirs.methods.is_empty() {
            assert_eq!(method(enc.stream_method), theirs.methods[0], "{label}: stream method");
            assert_eq!(method(enc.string_method), theirs.methods[1], "{label}: string method");
        }
        let owner_theirs = common::qpdf_encryption(qpdf, &row.path, &row.owner).unwrap();
        assert_eq!(owner_theirs.key, theirs.key, "{label}: qpdf's key from the owner password");
        assert!(owner_theirs.owner_matched, "{label}");
    }
}

/// The flags `/P` as the file stores them (low 32 bits).
fn security_flags(doc: &Document) -> u32 {
    let dict = doc.trailer().get("Encrypt").map(|e| doc.resolve(e).unwrap()).unwrap();
    u32::try_from(dict.as_dict().unwrap().get_int("P").unwrap() & 0xFFFF_FFFF).unwrap()
}

#[test]
fn corpus_files_open_with_their_passwords() {
    for known in KNOWN {
        let path = corpus_path(known.rel);
        if !path.is_file() {
            assert!(!known.required, "{} is missing", known.rel);
            continue;
        }
        let label = known.rel;
        let user = open(&path, known.user);
        assert!(!user.is_locked(), "{label}");
        assert!(user.page_count().is_ok(), "{label}: {:?}", user.page_count().err());
        let enc = user.encryption().unwrap();
        // The password may be the owner's, or both (pr6531_1 has one password
        // for both): qpdf says which.
        let kind = enc.opened.unwrap().kind;
        let owner_rights = user.security().unwrap().is_owner_password(known.user);
        match common::find_qpdf().and_then(|q| common::qpdf_encryption(&q, &path, known.user)) {
            Some(theirs) => {
                assert_eq!(owner_rights, theirs.owner_matched, "{label}: qpdf says owner={}", theirs.owner_matched);
                let expected = if theirs.user_matched { PasswordKind::User } else { PasswordKind::Owner };
                assert_eq!(kind, expected, "{label}");
            }
            None if known.owner == Some(known.user) => assert!(owner_rights, "{label}"),
            None => {}
        }
        assert!(!user.is_locked());
        if let Some(owner) = known.owner {
            let doc = open(&path, owner);
            if owner != known.user {
                assert_eq!(doc.encryption().unwrap().opened.unwrap().kind, PasswordKind::Owner, "{label}");
            }
            assert!(doc.security().unwrap().is_owner_password(owner), "{label}");
            assert_eq!(doc.security().unwrap().file_key(), user.security().unwrap().file_key(), "{label}");
        }
        if !known.user.is_empty() {
            let locked = Document::open(&path).unwrap();
            assert!(locked.is_locked(), "{label}");
            assert!(matches!(Document::open_with_password(&path, "not it"), Err(Error::WrongPassword)), "{label}");
            assert!(matches!(ops::copy_all(&locked), Err(Error::PasswordRequired)), "{label}");
        }
        // Whatever the first layer can do, it can do on the opened file.
        let out = ops::copy_all(&user).unwrap_or_else(|e| panic!("{label}: copy: {e}"));
        assert_eq!(out.pages, user.page_count().unwrap(), "{label}");
        let again = Document::from_bytes_with_password(out.data, known.user).unwrap();
        assert_eq!(page_contents(&again), page_contents(&user), "{label}");
        assert_eq!(info_of(&again), info_of(&user), "{label}");
    }
}

#[test]
fn the_files_with_a_damaged_owner_entry_are_refused_without_a_fuss() {
    for rel in ["public/encrypted/encrypted_hello_world_r2_bad_okey.pdf", "public/encrypted/encrypted_hello_world_r3_bad_okey.pdf"] {
        let path = corpus_path(rel);
        let locked = Document::open(&path).unwrap();
        assert!(locked.is_locked(), "{rel}");
        for pw in ["a", "hôtel", "âge", "1234"] {
            assert!(matches!(Document::open_with_password(&path, pw), Err(Error::WrongPassword)), "{rel}: {pw}");
        }
        assert!(locked.info().is_err() || locked.trailer().get("Info").is_none());
    }
}

#[test]
fn the_light_password_preparation_is_what_makes_the_full_width_password_work() {
    let row = generated().into_iter().find(|r| !r.alternatives.is_empty()).expect("a file with an alternative password");
    // The file was made with the plain password; the full-width one only works
    // by the preparation, and a half-way different one does not.
    assert_eq!(row.user, "ABC123");
    assert!(!open(&row.path, "ＡＢＣ１２３").is_locked());
    assert!(matches!(Document::open_with_password(&row.path, "ＡＢＣ１２４"), Err(Error::WrongPassword)));
    // Zero-width characters and a soft hyphen are dropped, ideographic spaces become spaces.
    assert!(!open(&row.path, "AB\u{200B}C1\u{00AD}23").is_locked());
}

#[test]
fn passwords_of_revision_4_are_tried_as_pdfdoc_text_and_as_utf8() {
    // PDFium's files are made with the Latin-1 form of "hôtel"; typed as text
    // it is PDFDocEncoding that finds it. Chinese passwords have no
    // PDFDocEncoding: the UTF-8 bytes are used.
    let r2 = corpus_path("public/encrypted/encrypted_hello_world_r2.pdf");
    assert!(!open(&r2, "hôtel").is_locked());
    assert!(!open(&r2, "âge").is_locked());
    let chinese = generated().into_iter().find(|r| r.path.to_string_lossy().contains("r4-aes128-user-chinese")).unwrap();
    assert!(!open(&chinese.path, "密码").is_locked());
    assert!(!open(&chinese.path, "主人").is_locked());
    // R4 uses only the first 32 bytes of a password (Algorithm 2 step a): the long
    // one is opened by its first 32 bytes, and is no longer needed in full.
    let long = generated().into_iter().find(|r| r.path.to_string_lossy().contains("r4-aes128-user-long")).unwrap();
    assert!(!open(&long.path, &long.user).is_locked());
    assert!(!open(&long.path, &long.user[..32]).is_locked());
}

// --- decrypting, against qpdf -------------------------------------------------------------

#[test]
fn every_string_and_stream_decrypts_like_qpdf() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found");
        return;
    };
    let out_dir = common::fresh_out_dir("encryption-decrypt");
    let mut total = 0usize;
    let mut files = 0usize;
    for (i, row) in generated().iter().enumerate() {
        let doc = open(&row.path, &row.user);
        total += compare_with_qpdf(&qpdf, &row.path, &row.user, &doc, &out_dir, &format!("g{i}"));
        files += 1;
    }
    for (i, known) in KNOWN.iter().enumerate() {
        let path = corpus_path(known.rel);
        if !path.is_file() {
            continue;
        }
        // qpdf must accept the password too (it does for these).
        if common::qpdf_encryption(&qpdf, &path, known.user).is_none() {
            println!("qpdf does not open {}; not compared", known.rel);
            continue;
        }
        let doc = open(&path, known.user);
        total += compare_with_qpdf(&qpdf, &path, known.user, &doc, &out_dir, &format!("k{i}"));
        files += 1;
    }
    println!("{total} objects of {files} files are byte for byte what qpdf decrypts");
    assert!(total > 150, "only {total} objects were compared");
}

#[test]
fn the_owner_password_decrypts_the_same_way() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found");
        return;
    };
    let out_dir = common::fresh_out_dir("encryption-owner");
    for (i, row) in generated().iter().enumerate().filter(|(i, _)| i % 3 == 0) {
        let doc = open(&row.path, &row.owner);
        assert!(doc.security().unwrap().is_owner_password(&row.owner));
        compare_with_qpdf(&qpdf, &row.path, &row.owner, &doc, &out_dir, &format!("o{i}"));
    }
}

// --- writing: the encryption is kept -----------------------------------------------------------

/// What an output has to be, judged by qpdf and by reading it again: qpdf
/// accepts it and shows the same revision, permissions, method and key as the
/// input; our reader reads it back with the same pages and the same text; and
/// qpdf's decryption of it is what our reader sees.
fn check_encrypted_output(
    qpdf: &Path,
    data: &[u8],
    input: &Path,
    password: &str,
    out_dir: &Path,
    tag: &str,
    expect_pages: usize,
) -> Document {
    let output = out_dir.join(format!("{tag}.pdf"));
    std::fs::write(&output, data).unwrap();
    common::qpdf_accepts(qpdf, &output, password, &[(input, password)], tag);
    let (before, after) = (common::qpdf_encryption(qpdf, input, password).unwrap(), common::qpdf_encryption(qpdf, &output, password));
    let after = after.unwrap_or_else(|| panic!("{tag}: qpdf does not open the output with {password:?}"));
    assert_eq!(after.revision, before.revision, "{tag}: R");
    assert_eq!(after.permissions, before.permissions, "{tag}: P");
    assert_eq!(after.methods, before.methods, "{tag}: methods");
    assert_eq!(after.key, before.key, "{tag}: the file key");
    let doc = Document::from_bytes_with_password(data.to_vec(), password).unwrap_or_else(|e| panic!("{tag}: {e}"));
    assert!(!doc.was_repaired(), "{tag}: the output needed repair");
    assert_eq!(doc.page_count().unwrap(), expect_pages, "{tag}");
    compare_with_qpdf(qpdf, &output, password, &doc, out_dir, tag);
    doc
}

#[test]
fn every_operation_keeps_the_encryption_and_qpdf_agrees() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found");
        return;
    };
    let out_dir = common::fresh_out_dir("encryption-outputs");
    let mut checked = 0usize;
    for (i, row) in generated().iter().enumerate() {
        let label = name(&row.path);
        let doc = open(&row.path, &row.user);
        let pages = doc.page_count().unwrap();
        let contents = page_contents(&doc);
        let text = info_of(&doc);
        let mut outputs: Vec<(String, Vec<u8>, usize)> = Vec::new();
        let copy = ops::copy_all(&doc).unwrap();
        outputs.push(("copy".into(), copy.data, pages));
        let first = ops::extract_pages(&doc, &[0]).unwrap();
        outputs.push(("split".into(), first.data, 1));
        let turned = ops::rotate_pages(&doc, &(0..pages).collect::<Vec<_>>(), 90).unwrap();
        outputs.push(("rotate".into(), turned.data, pages));
        if pages > 1 {
            let rest = ops::delete_pages(&doc, &[0]).unwrap();
            outputs.push(("delete".into(), rest.data, pages - 1));
        }
        let twice = ops::merge(&[Input { name: "a", doc: &doc }, Input { name: "b", doc: &doc }]).unwrap();
        outputs.push(("merge with itself".into(), twice.data, pages * 2));
        let mut every = Vec::new();
        ops::split_every(&doc, 1, &mut |n, out| {
            every.push((n, out.data));
            Ok(())
        })
        .unwrap();
        for (n, data) in every.into_iter().take(2) {
            outputs.push((format!("split-every-{n}"), data, 1));
        }
        for (what, data, expect_pages) in outputs {
            let tag = format!("o{i}-{}", what.replace(' ', "-"));
            let out = check_encrypted_output(&qpdf, &data, &row.path, &row.user, &out_dir, &tag, expect_pages);
            // The first page's text and the document information are what they were.
            if what == "copy" || what == "rotate" {
                assert_eq!(page_contents(&out), contents, "{label}: {what}");
                assert_eq!(info_of(&out), text, "{label}: {what}");
            }
            checked += 1;
        }
    }
    println!("{checked} encrypted outputs agree with qpdf");
    assert!(checked > 100);
}

#[test]
fn merging_follows_the_first_files_encryption() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found");
        return;
    };
    let out_dir = common::fresh_out_dir("encryption-merge");
    let rows = generated();
    let find = |part: &str| rows.iter().find(|r| r.path.to_string_lossy().contains(part)).unwrap().clone();
    let aes128 = find("bookmarks.r4-aes128-user-assemble-n");
    let aes256 = find("pdf20utf8.r6-aes256-user-modify-none");
    let rc4 = find("rects.r3-128-rc4-empty-extract-n");
    let plain_path = common::corpus_root().join("public").join("xref-classic").join("hello_world_2_pages.pdf");
    let plain = Document::open(&plain_path).unwrap();
    let (d128, d256, drc4) = (open(&aes128.path, &aes128.user), open(&aes256.path, &aes256.user), open(&rc4.path, &rc4.user));

    // Encrypted first, plain second: the output is encrypted like the first.
    let out = ops::merge(&[Input { name: "first", doc: &d128 }, Input { name: "plain", doc: &plain }]).unwrap();
    assert!(out.warnings.iter().all(|w| !w.0.contains("encryption")), "{:?}", out.warnings);
    let total = d128.page_count().unwrap() + plain.page_count().unwrap();
    let merged = check_encrypted_output(&qpdf, &out.data, &aes128.path, &aes128.user, &out_dir, "enc-plain", total);
    assert!(merged.is_encrypted());

    // Two files with different encryption: the second is decrypted with its key
    // and written under the first's; its text survives.
    let out = ops::merge(&[Input { name: "first", doc: &d128 }, Input { name: "second", doc: &d256 }, Input { name: "third", doc: &drc4 }]).unwrap();
    assert!(out.warnings.iter().all(|w| !w.0.contains("not carried over; the output is not encrypted")), "{:?}", out.warnings);
    let total = d128.page_count().unwrap() + d256.page_count().unwrap() + drc4.page_count().unwrap();
    let merged = check_encrypted_output(&qpdf, &out.data, &aes128.path, &aes128.user, &out_dir, "enc-enc-enc", total);
    let expected: Vec<Vec<u8>> =
        [&d128, &d256, &drc4].iter().flat_map(|d| page_contents(d)).collect();
    assert_eq!(page_contents(&merged), expected);

    // Plain first, encrypted second: the output is not encrypted, and says so.
    let out = ops::merge(&[Input { name: "plain.pdf", doc: &plain }, Input { name: "secret.pdf", doc: &d256 }]).unwrap();
    let warning = out.warnings.iter().find(|w| w.0.contains("secret.pdf") && w.0.contains("encryption")).expect("a warning that names the later file");
    assert!(warning.0.contains("not encrypted"), "{}", warning.0);
    assert!(out.warnings.iter().all(|w| !w.0.contains("plain.pdf:")));
    let merged = Document::from_bytes(out.data.clone()).unwrap();
    assert!(!merged.is_encrypted());
    let expected: Vec<Vec<u8>> = [&plain, &d256].iter().flat_map(|d| page_contents(d)).collect();
    assert_eq!(page_contents(&merged), expected);
    let written = out_dir.join("plain-enc.pdf");
    std::fs::write(&written, &out.data).unwrap();
    common::qpdf_accepts(&qpdf, &written, "", &[(&plain_path, ""), (&aes256.path, &aes256.user)], "plain + encrypted");
    let shown = std::process::Command::new(&qpdf).arg("--show-encryption").arg(&written).output().unwrap();
    assert!(String::from_utf8_lossy(&shown.stdout).contains("not encrypted"));
}

#[test]
fn decrypt_needs_the_owner_password_or_every_permission() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found");
        return;
    };
    let out_dir = common::fresh_out_dir("encryption-decrypt-command");
    let mut allowed = 0usize;
    let mut refused = 0usize;
    for (i, row) in generated().iter().enumerate() {
        let label = name(&row.path);
        let as_user = open(&row.path, &row.user);
        let everything = as_user.encryption().unwrap().permissions.allows_everything();
        let by_owner = open(&row.path, &row.owner);
        let result = ops::decrypt(&as_user, &row.user);
        if everything || row.owner.is_empty() {
            assert!(result.is_ok(), "{label}: the permissions allow everything, or the owner has no password");
            allowed += 1;
        } else {
            match &result {
                Err(Error::Invalid(m)) => assert!(m.contains("owner password"), "{label}: {m}"),
                other => panic!("{label}: expected a refusal, got {:?}", other.as_ref().map(|o| o.pages)),
            }
            refused += 1;
        }
        // The owner password always may.
        let out = ops::decrypt(&by_owner, &row.owner).unwrap_or_else(|e| panic!("{label}: the owner is refused: {e}"));
        let plain = Document::from_bytes(out.data.clone()).unwrap();
        assert!(!plain.is_encrypted() && !plain.is_locked(), "{label}");
        assert!(!plain.was_repaired(), "{label}");
        assert_eq!(page_contents(&plain), page_contents(&by_owner), "{label}");
        assert_eq!(info_of(&plain), info_of(&by_owner), "{label}");
        let written = out_dir.join(format!("d{i}.pdf"));
        std::fs::write(&written, &out.data).unwrap();
        common::qpdf_accepts(&qpdf, &written, "", &[(&row.path, &row.user)], &label);
        let shown = std::process::Command::new(&qpdf).arg("--show-encryption").arg(&written).output().unwrap();
        assert!(String::from_utf8_lossy(&shown.stdout).contains("not encrypted"), "{label}");
        // Nothing of the encryption is left in the bytes.
        assert!(!common::contains(&out.data, b"/Encrypt"), "{label}");
    }
    println!("{allowed} decrypted by the user password (everything allowed), {refused} refused");
    assert!(allowed > 0 && refused > 0);
    // A file that is not encrypted is not "decrypted".
    let plain_path = common::corpus_root().join("public").join("xref-classic").join("hello_world.pdf");
    assert!(matches!(ops::decrypt(&Document::open(&plain_path).unwrap(), ""), Err(Error::Invalid(_))));
}

// --- a damaged file with encrypted object streams ----------------------------------------------

#[test]
fn a_damaged_file_with_encrypted_object_streams_is_repaired_and_read() {
    for part in ["objstm.r4-aes128-empty-print-none", "objstm.r5-aes256-user", "objstm.r6-aes256-empty-extract-n"] {
        let row = generated().into_iter().find(|r| r.path.to_string_lossy().contains(part)).unwrap();
        let original = open(&row.path, &row.user);
        assert!(original.uses_object_streams() && !original.was_repaired(), "{part}");
        let bytes = std::fs::read(&row.path).unwrap();
        // The offset after `startxref` points at nothing: the file is scanned for its objects, and the
        // object streams (encrypted) have to be opened to find the catalog and the page tree in them.
        let at = bytes.windows(10).rposition(|w| w == b"startxref\n").unwrap() + 10;
        let digits = bytes[at..].iter().take_while(|b| b.is_ascii_digit()).count();
        let mut broken = bytes[..at].to_vec();
        broken.extend_from_slice(b"7");
        broken.extend_from_slice(&bytes[at + digits..]);
        let doc = Document::from_bytes_with_password(broken.clone(), &row.user).unwrap_or_else(|e| panic!("{part}: {e}"));
        assert!(doc.was_repaired(), "{part}");
        assert_eq!(doc.page_count().unwrap(), original.page_count().unwrap(), "{part}");
        assert_eq!(page_contents(&doc), page_contents(&original), "{part}");
        assert_eq!(info_of(&doc), info_of(&original), "{part}");
        for r in original.object_refs() {
            let kind = original.get(r).ok().and_then(|o| o.as_dict().and_then(|d| d.get_name("Type")).map(|n| n.0.clone()));
            if matches!(kind.as_deref(), Some(b"XRef" | b"ObjStm")) {
                continue;
            }
            let (a, b) = (original.get(r).unwrap(), doc.get(r).unwrap());
            assert_eq!(strings_of(&a), strings_of(&b), "{part}: object {}", r.num);
        }
        // And a copy of it is whole.
        let copy = ops::copy_all(&doc).unwrap();
        let again = Document::from_bytes_with_password(copy.data, &row.user).unwrap();
        assert_eq!(page_contents(&again), page_contents(&original), "{part}");
        // The wrong password is still the wrong password after the repair.
        if !row.user.is_empty() {
            assert!(matches!(Document::from_bytes_with_password(broken.clone(), "not it"), Err(Error::WrongPassword)), "{part}");
            let locked = Document::from_bytes(broken).unwrap();
            assert!(locked.is_locked(), "{part}");
        }
    }
}

// --- the Identity crypt filter, EncryptMetadata false, per-stream crypt filters -------------------

/// Patch the bytes of a generated file (a text edit of its encryption
/// dictionary or of a stream dictionary), save it, and compare what we read
/// with what qpdf reads.
fn patched_agrees_with_qpdf(qpdf: &Path, row: &Row, edit: &dyn Fn(&mut Vec<u8>), tag: &str) -> usize {
    let out_dir = common::fresh_out_dir(&format!("encryption-patched-{tag}"));
    let mut bytes = std::fs::read(&row.path).unwrap();
    edit(&mut bytes);
    let bytes = common::with_fresh_xref(&bytes);
    let path = out_dir.join("patched.pdf");
    std::fs::write(&path, &bytes).unwrap();
    let doc = open(&path, &row.user);
    assert!(!doc.was_repaired(), "{tag}: the edited file should not need repair");
    compare_with_qpdf(qpdf, &path, &row.user, &doc, &out_dir, tag)
}

fn replace_first(bytes: &mut Vec<u8>, from: &[u8], to: &[u8]) {
    let at = bytes.windows(from.len()).position(|w| w == from).unwrap_or_else(|| panic!("{} not found", String::from_utf8_lossy(from)));
    bytes.splice(at..at + from.len(), to.iter().copied());
}

#[test]
fn identity_filters_and_stream_crypt_filters_read_like_qpdf() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found");
        return;
    };
    let rows = generated();
    let aes128 = rows.iter().find(|r| r.path.to_string_lossy().contains("pdf20utf8.r4-aes128-empty-modify-none")).unwrap();
    let rc4 = rows.iter().find(|r| r.path.to_string_lossy().contains("pdf20utf8.r4-128-rc4-v4-user")).unwrap();
    for row in [aes128, rc4] {
        // Streams are not encrypted, strings are; and the other way round. (The
        // key does not change, so the same passwords still open the files.)
        let streams = patched_agrees_with_qpdf(&qpdf, row, &|b| replace_first(b, b"/StmF /StdCF", b"/StmF /Identity"), "stmf-identity");
        let strings = patched_agrees_with_qpdf(&qpdf, row, &|b| replace_first(b, b"/StrF /StdCF", b"/StrF /Identity"), "strf-identity");
        assert!(streams > 0 && strings > 0);
        // A stream whose own /Filter names the Identity crypt filter is left alone.
        let n = patched_agrees_with_qpdf(
            &qpdf,
            row,
            &|b| {
                replace_first(
                    b,
                    b"/Filter /FlateDecode",
                    b"/Filter [/Crypt /FlateDecode] /DecodeParms [<< /Type /CryptFilterDecodeParms /Name /Identity >> null]",
                )
            },
            "stream-crypt-identity",
        );
        assert!(n > 0);
    }
}

#[test]
fn metadata_left_in_the_clear_is_read_like_qpdf() {
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED: qpdf was not found");
        return;
    };
    let out_dir = common::fresh_out_dir("encryption-metadata");
    for part in ["pdf20.r4-aes128-empty-cleartext-metadata", "pdf20.r6-aes256-empty-cleartext-metadata"] {
        let row = generated().into_iter().find(|r| r.path.to_string_lossy().contains(part)).unwrap();
        let doc = open(&row.path, &row.user);
        assert!(!doc.encryption().unwrap().encrypt_metadata, "{part}");
        // The XMP packet is plain text in the file and comes out plain.
        let catalog = doc.catalog().unwrap();
        let Some(Object::Ref(r)) = catalog.get("Metadata") else { panic!("{part}: no /Metadata") };
        let Object::Stream(s) = doc.get(*r).unwrap() else { panic!() };
        assert!(common::contains(&s.data, b"xpacket") || common::contains(&s.data, b"<x:xmpmeta") || common::contains(&s.data, b"<?xml"), "{part}: {:?}", &s.data[..s.data.len().min(60)]);
        assert!(common::contains(&std::fs::read(&row.path).unwrap(), &s.data[..32.min(s.data.len())]), "{part}: it is in the file as it is");
        // And it survives our writer in the clear and in the same place.
        let out = ops::copy_all(&doc).unwrap();
        let again = Document::from_bytes_with_password(out.data.clone(), &row.user).unwrap();
        let Some(Object::Ref(r2)) = again.catalog().unwrap().get("Metadata").cloned() else { panic!() };
        let Object::Stream(s2) = again.get(r2).unwrap() else { panic!() };
        assert_eq!(s2.data, s.data);
        assert!(common::contains(&out.data, &s.data[..32.min(s.data.len())]), "{part}: not encrypted in our output either");
        if let Some(qpdf) = Some(&qpdf) {
            let tag = format!("meta-{}", &part[..8]);
            let written = out_dir.join(format!("{tag}.pdf"));
            std::fs::write(&written, &out.data).unwrap();
            common::qpdf_accepts(qpdf, &written, &row.user, &[(&row.path, &row.user)], &tag);
            let theirs = common::qpdf_decrypt(qpdf, &written, &row.user, &out_dir.join(format!("{tag}.dec.pdf")));
            assert!(theirs);
            compare_with_qpdf(qpdf, &written, &row.user, &again, &out_dir, &tag);
        }
    }
}

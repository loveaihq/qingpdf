//! Item 2 of the feature list: a file written under a new encryption (`ops::protect`): AES-256 (revision 6) and AES-128 (revision 4),
//! with a user password, an owner password or both, the rights turned off one by one, Chinese passwords, sources of every build (a
//! classic table, object streams with a cross-reference stream, a file that is encrypted already). qpdf is the reference answer: it
//! checks and opens what we write with each password, finds the revision, the flags and the methods we set, refuses a wrong password,
//! and its decryption gives the pages of the source. PDFium's turn is `tests/tools/protect_check.py`. Without qpdf the checks that need
//! it say so and are not made.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};

use qingpdf_base::testutil::PdfBuilder;
use qingpdf_core::ops;
use qingpdf_core::security::{Method, PasswordKind, Permissions, Protection};
use qingpdf_core::text::TextExtractor;
use qingpdf_core::{Document, Error, info};

const GONGWEN: &str = "public/zh/gongwen/gongwen-2025-service-mfg-plan.pdf";
const OBJECT_STREAMS: &str = "public/zh/gongwen/gongwen-2024-nicheng-work-report.pdf";
const OBJECT_STREAMS_WORD: &str = "public/zh/lunwen/lunwen-arxiv-2401.01717-word.pdf";
const TINY_OBJECT_STREAMS: &str = "public/xref-stream-objstm/bug_757705.pdf";

/// A seed for a test: 32 bytes that differ from one `n` to the next (a real one is drawn from the system).
fn seed(n: u8) -> [u8; 32] {
    std::array::from_fn(|i| n.wrapping_mul(37).wrapping_add(u8::try_from(i).unwrap() + 1))
}

fn corpus(rel: &str) -> PathBuf {
    common::corpus_root().join(rel)
}

fn protection(method: Method, user: &str, owner: &str, permissions: Permissions) -> Protection {
    Protection { method, user_password: user.to_string(), owner_password: owner.to_string(), permissions, encrypt_metadata: true }
}

/// `print`: 0 none, 1 low quality, 2 the best; then copy, modify, annotate, forms, assemble, accessibility.
fn rights(print: u8, copy: bool, modify: bool, annotate: bool, forms: bool, assemble: bool, accessibility: bool) -> Permissions {
    Permissions { print: print > 0, print_high_quality: print > 1, modify, copy, annotate, fill_forms: forms, assemble, accessibility }
}

/// The reason an operation was refused (its output, if it was not refused, is not printed: it is a file).
fn refusal(result: Result<ops::Output, Error>) -> String {
    match result {
        Ok(out) => panic!("it was not refused: {} bytes were written", out.data.len()),
        Err(e) => e.to_string(),
    }
}

fn open_file(path: &Path, password: &str) -> Document {
    Document::open_with_password(path, password).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn page_texts(doc: &Document) -> Vec<String> {
    let mut extractor = TextExtractor::new(doc);
    doc.pages().unwrap().iter().map(|p| extractor.page_text(p).unwrap()).collect()
}

/// The decoded content of each page.
fn page_contents(doc: &Document) -> Vec<Vec<u8>> {
    let mut all = Vec::new();
    for page in doc.pages().unwrap() {
        let mut bytes = Vec::new();
        let streams: Vec<qingpdf_core::Object> = match page.dict.get("Contents").map(|c| doc.resolve(c).unwrap()) {
            Some(qingpdf_core::Object::Array(items)) => items.iter().map(|i| doc.resolve(i).unwrap()).collect(),
            Some(other) => vec![other],
            None => Vec::new(),
        };
        for s in streams {
            if let qingpdf_core::Object::Stream(s) = s {
                bytes.extend(doc.decode_stream(&s).unwrap());
            }
        }
        all.push(bytes);
    }
    all
}

/// What a check of an output needs to know of what it was asked for.
struct Expect<'a> {
    method: Method,
    user: &'a str,
    /// The owner password that was given (the user password when none was).
    owner: &'a str,
    permissions: Permissions,
}

/// Write `out` into `dir` and check it with our own engine and with qpdf against the source (`source` opened with its password
/// `source_password`, from the file `source_path`).
fn verify(label: &str, dir: &Path, out: &[u8], source: &Document, source_path: &Path, source_password: &str, e: &Expect<'_>) {
    let path = dir.join(format!("{label}.pdf"));
    std::fs::write(&path, out).unwrap();
    let want_texts = page_texts(source);
    let want_contents = page_contents(source);
    let want_info = info::describe(source).unwrap().info;
    let aes = e.method;

    // Our own engine: each password opens it, as what it is; a wrong one does not; what comes out is what went in.
    let (revision, version) = if aes == Method::AesV3 { (6, 5) } else { (4, 4) };
    let check_open = |password: &str, kind: PasswordKind| {
        let doc = open_file(&path, password);
        let enc = doc.encryption().unwrap();
        assert_eq!((enc.version, enc.revision), (version, revision), "{label}");
        assert_eq!((enc.stream_method, enc.string_method, enc.file_method), (aes, aes, aes), "{label}");
        assert_eq!(enc.permissions.list(), e.permissions.list().map(|(w, a)| (w, a && (kind == PasswordKind::User || true))), "{label}: the flags");
        assert!(enc.perms_valid, "{label}");
        assert_eq!(enc.opened.map(|a| a.kind), Some(kind), "{label}: {password:?}");
        assert_eq!(doc.page_count().unwrap(), want_texts.len(), "{label}");
        assert_eq!(page_texts(&doc), want_texts, "{label}: the text");
        assert_eq!(page_contents(&doc), want_contents, "{label}: the content streams");
        assert_eq!(info::describe(&doc).unwrap().info, want_info, "{label}: the document information");
        assert!(!doc.was_repaired(), "{label}: our own output needs no repair");
    };
    if !e.user.is_empty() {
        check_open(e.user, PasswordKind::User);
        assert!(Document::open(&path).unwrap().is_locked(), "{label}: it needs a password");
        for wrong in ["wrong-password", &format!("{}x", e.user), "密码错误"] {
            assert!(matches!(Document::open_with_password(&path, wrong), Err(Error::WrongPassword)), "{label}: {wrong:?}");
        }
    } else {
        check_open("", PasswordKind::User);
    }
    if e.owner != e.user {
        check_open(e.owner, PasswordKind::Owner);
    }

    // qpdf.
    let Some(qpdf) = common::find_qpdf() else {
        println!("{label}: qpdf is not here, so only our own checks were made");
        return;
    };
    let inputs = [(source_path, source_password)];
    common::qpdf_accepts(&qpdf, &path, e.user, &inputs, label);
    common::qpdf_accepts(&qpdf, &path, e.owner, &inputs, label);
    let (name, qpdf_revision) = if aes == Method::AesV3 { ("AESv3", 6) } else { ("AESv2", 4) };
    let flags = i64::from(e.permissions.to_flags() as i32);
    for (password, owner_too) in [(e.user, e.user == e.owner), (e.owner, true)] {
        let q = common::qpdf_encryption(&qpdf, &path, password).unwrap_or_else(|| panic!("{label}: qpdf refuses the password"));
        assert_eq!((q.revision, q.permissions), (qpdf_revision, flags), "{label}");
        assert!(q.methods.len() == 3 && q.methods.iter().all(|m| m == name), "{label}: {:?}", q.methods);
        if password == e.owner && owner_too {
            assert!(q.owner_matched, "{label}: the owner password");
        }
        if password == e.user && !e.user.is_empty() {
            assert!(q.user_matched, "{label}: the user password");
        }
    }
    if !e.user.is_empty() {
        for wrong in ["wrong-password", "密码错误"] {
            assert!(common::qpdf_encryption(&qpdf, &path, wrong).is_none(), "{label}: qpdf takes {wrong:?}");
        }
    }
    // qpdf's decryption is the source: the pages' content and the document information.
    let plain = dir.join(format!("{label}.qpdf-decrypted.pdf"));
    assert!(common::qpdf_decrypt(&qpdf, &path, e.owner, &plain), "{label}: qpdf cannot decrypt it");
    let theirs = Document::open(&plain).unwrap();
    assert!(!theirs.is_encrypted(), "{label}");
    assert_eq!(page_contents(&theirs), want_contents, "{label}: qpdf's content streams");
    assert_eq!(info::describe(&theirs).unwrap().info, want_info, "{label}: qpdf's document information");
    // Both programs find the same file key.
    let ours = open_file(&path, e.owner);
    let q = common::qpdf_encryption(&qpdf, &path, e.owner).unwrap();
    let key = ours.security().unwrap().file_key().unwrap().iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert_eq!(q.key, key, "{label}: the file key");
}

fn protect_file(rel: &str, source_password: &str, how: &Protection, n: u8) -> (Vec<u8>, Document, PathBuf) {
    let path = corpus(rel);
    let doc = open_file(&path, source_password);
    let out = ops::protect(&doc, source_password, how, &seed(n)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    assert_eq!(out.pages, doc.page_count().unwrap());
    (out.data, doc, path)
}

#[test]
fn every_combination_of_method_and_passwords_is_opened_by_qpdf_and_by_us() {
    let dir = common::fresh_out_dir("protect-matrix");
    let all = Permissions::all();
    // A user password only (nothing restricted: the owner password is the same one), both passwords, the owner password only (the file opens
    // without a password with its rights taken away), and both passwords the same.
    let restricted = rights(1, false, true, true, false, true, true);
    let cases: Vec<(&str, Method, &str, &str, Permissions)> = vec![
        ("256-user-only", Method::AesV3, "open-sesame", "", all),
        ("256-both", Method::AesV3, "open-sesame", "o-w-n-e-r", restricted),
        ("256-owner-only", Method::AesV3, "", "o-w-n-e-r", restricted),
        ("256-same", Method::AesV3, "same-one", "same-one", all),
        ("128-user-only", Method::AesV2, "open-sesame", "", all),
        ("128-both", Method::AesV2, "open-sesame", "o-w-n-e-r", restricted),
        ("128-owner-only", Method::AesV2, "", "o-w-n-e-r", restricted),
        ("128-same", Method::AesV2, "same-one", "same-one", all),
        ("256-nothing-printable", Method::AesV3, "u", "o", rights(0, true, true, true, true, true, true)),
        ("128-low-quality-print", Method::AesV2, "u", "o", rights(1, true, true, true, true, true, true)),
        ("256-no-copy", Method::AesV3, "", "o", rights(2, false, true, true, true, true, true)),
        ("256-no-modify", Method::AesV3, "", "o", rights(2, true, false, true, true, true, true)),
        ("256-no-annotate", Method::AesV3, "", "o", rights(2, true, true, false, true, true, true)),
        ("256-no-forms", Method::AesV3, "", "o", rights(2, true, true, true, false, true, true)),
        ("256-no-assemble", Method::AesV3, "", "o", rights(2, true, true, true, true, false, true)),
        ("256-no-accessibility", Method::AesV3, "", "o", rights(2, true, true, true, true, true, false)),
        ("128-everything-off", Method::AesV2, "", "o", rights(0, false, false, false, false, false, false)),
    ];
    for (n, (label, method, user, owner, permissions)) in cases.into_iter().enumerate() {
        let (out, doc, path) = protect_file(GONGWEN, "", &protection(method, user, owner, permissions), u8::try_from(n).unwrap());
        let owner = if owner.is_empty() { user } else { owner };
        verify(label, &dir, &out, &doc, &path, "", &Expect { method, user, owner, permissions });
    }
}

#[test]
fn chinese_passwords_work_with_aes_256_and_are_refused_with_aes_128() {
    let dir = common::fresh_out_dir("protect-chinese");
    let some = rights(2, false, true, true, true, true, true);
    let (out, doc, path) = protect_file(GONGWEN, "", &protection(Method::AesV3, "密码", "主人密码", some), 1);
    verify("chinese-256", &dir, &out, &doc, &path, "", &Expect { method: Method::AesV3, user: "密码", owner: "主人密码", permissions: some });
    // A password of the longest size in bytes: 42 Chinese characters are 126 bytes, 43 are 129.
    let long = "字".repeat(42);
    let (out, doc, path) = protect_file(GONGWEN, "", &protection(Method::AesV3, &long, "", Permissions::all()), 2);
    verify("chinese-256-long", &dir, &out, &doc, &path, "", &Expect { method: Method::AesV3, user: &long, owner: &long, permissions: Permissions::all() });
    // Full-width letters are written as the plain ones (the light SASLprep): the plain form opens it for qpdf, which does not prepare
    // passwords, and both forms open it for us.
    let (out, doc, path) = protect_file(GONGWEN, "", &protection(Method::AesV3, "ＡＢＣ１２３", "", Permissions::all()), 3);
    verify("fullwidth-256", &dir, &out, &doc, &path, "", &Expect { method: Method::AesV3, user: "ABC123", owner: "ABC123", permissions: Permissions::all() });
    assert!(Document::from_bytes_with_password(out, "ＡＢＣ１２３").is_ok());
    // AES-128 keeps passwords in PDFDocEncoding: Chinese does not fit, and the message says what to do; nothing is written.
    let before = doc.page_count().unwrap();
    for (user, owner) in [("密码", "owner"), ("user", "主人")] {
        let e = refusal(ops::protect(&doc, "", &protection(Method::AesV2, user, owner, Permissions::all()), &seed(4)));
        assert!(e.contains("AES-256") && !e.contains("密码") && !e.contains("主人"), "{e}");
    }
    assert_eq!(doc.page_count().unwrap(), before);
    // A password too long for the method.
    let e = refusal(ops::protect(&doc, "", &protection(Method::AesV3, &"字".repeat(43), "", Permissions::all()), &seed(5)));
    assert!(e.contains("127"), "{e}");
    let e = refusal(ops::protect(&doc, "", &protection(Method::AesV2, &"a".repeat(33), "", Permissions::all()), &seed(5)));
    assert!(e.contains("32"), "{e}");
    // A Latin-1 password for AES-128 is kept as PDFDocEncoding keeps it.
    let (out, doc, path) = protect_file(GONGWEN, "", &protection(Method::AesV2, "hôtel", "âge", rights(2, false, true, true, true, true, true)), 6);
    verify("latin-128", &dir, &out, &doc, &path, "", &Expect { method: Method::AesV2, user: "hôtel", owner: "âge", permissions: rights(2, false, true, true, true, true, true) });
}

#[test]
fn sources_with_object_streams_and_a_cross_reference_stream_are_rewritten_and_encrypted_everywhere() {
    let dir = common::fresh_out_dir("protect-objstm");
    let some = rights(1, false, true, true, true, true, true);
    for (n, rel) in [OBJECT_STREAMS, OBJECT_STREAMS_WORD, TINY_OBJECT_STREAMS].into_iter().enumerate() {
        let source = open_file(&corpus(rel), "");
        assert!(source.uses_xref_streams(), "{rel}: a cross-reference stream");
        for method in [Method::AesV3, Method::AesV2] {
            let label = format!("objstm-{n}-{}", if method == Method::AesV3 { 256 } else { 128 });
            let how = protection(method, "u1", "o1", some);
            let (out, doc, path) = protect_file(rel, "", &how, u8::try_from(n).unwrap() + 20);
            // The output is a classic file: its objects are plain objects, every one of them encrypted, and no cross-reference stream is
            // left to be left in the clear or encrypted by mistake.
            let reread = Document::from_bytes_with_password(out.clone(), "u1").unwrap();
            assert!(!reread.uses_xref_streams(), "{label}");
            verify(&label, &dir, &out, &doc, &path, "", &Expect { method, user: "u1", owner: "o1", permissions: some });
        }
    }
}

#[test]
fn an_encrypted_source_is_read_with_its_own_key_and_written_under_the_new_one() {
    let dir = common::fresh_out_dir("protect-reencrypt");
    let mut done = 0;
    for (n, row) in common::generated().into_iter().enumerate() {
        let label = common::corpus_name(&row.path);
        if label.contains("sig.") {
            continue; // signed: see the next test
        }
        // The owner password gives the rights to change the protection (a file that restricts needs it, and one that does not restrict
        // may be changed with either). The files with a Chinese password are re-encrypted with AES-256 only.
        let owner_pw = if row.owner.is_empty() { row.user.clone() } else { row.owner.clone() };
        let source = open_file(&row.path, &owner_pw);
        let from_aes256 = label.contains("aes256") || label.contains(".r5-") || label.contains(".r6-");
        let method = if from_aes256 { Method::AesV2 } else { Method::AesV3 };
        // Passwords AES-128 can keep, whatever the source had.
        let some = rights(2, true, false, true, true, true, true);
        let how = protection(method, "new-user", "new-owner", some);
        let out = ops::protect(&source, &owner_pw, &how, &seed(u8::try_from(n).unwrap() + 40)).unwrap_or_else(|e| panic!("{label}: {e}"));
        // (qpdf is shown the encrypted source with the password that gives the rights.)
        verify(&format!("re-{n}"), &dir, &out.data, &source, &row.path, &owner_pw, &Expect { method, user: "new-user", owner: "new-owner", permissions: some });
        // Nothing of the old encryption is left: the old passwords open nothing, unless they are the empty ones that never did.
        for old in [&row.user, &row.owner] {
            if !old.is_empty() && old != "new-user" && old != "new-owner" {
                assert!(matches!(Document::from_bytes_with_password(out.data.clone(), old), Err(Error::WrongPassword)), "{label}: {old:?}");
            }
        }
        done += 1;
    }
    assert!(done >= 25, "{done} encrypted sources");
}

#[test]
fn an_encrypted_file_is_changed_only_by_someone_who_has_the_owners_rights() {
    let generated = common::generated();
    let find = |name: &str| generated.iter().find(|r| common::corpus_name(&r.path).contains(name)).unwrap_or_else(|| panic!("{name}"));
    let new = protection(Method::AesV3, "x", "y", Permissions::all());
    // A file that restricts, opened with the user password: refused, and the message says what to give. With the owner password: done.
    for name in ["bookmarks.r4-aes128-user-assemble-n", "two.r6-aes256-user-everything-denied", "bookmarks.r3-128-rc4-user-modify-none"] {
        let row = find(name);
        let as_user = open_file(&row.path, &row.user);
        let e = refusal(ops::protect(&as_user, &row.user, &new, &seed(1)));
        assert!(e.contains("the protection is not changed") && e.contains("owner password"), "{name}: {e}");
        assert!(!e.contains(&row.user) || row.user == "user", "{e}");
        let as_owner = open_file(&row.path, &row.owner);
        assert!(ops::protect(&as_owner, &row.owner, &new, &seed(1)).is_ok(), "{name}");
        // Opened with the user password but told the owner's: the password is looked at too.
        assert!(ops::protect(&as_user, &row.owner, &new, &seed(1)).is_ok(), "{name}");
    }
    // A file whose owner password is empty: whoever holds it is the owner (the empty password has the owner's rights), restrictions or not.
    let free = find("bookmarks.r4-aes128-empty-owner-too");
    assert!(ops::protect(&open_file(&free.path, ""), "", &new, &seed(1)).is_ok());
    // A file that opens with the empty user password and restricts: the empty password has no rights.
    let empty = find("bookmarks.r6-aes256-empty-print-none");
    let e = refusal(ops::protect(&open_file(&empty.path, ""), "", &new, &seed(1)));
    assert!(e.contains("owner password"), "{e}");
    // A locked file cannot be written at all.
    let locked = Document::open(&find("bookmarks.r2-40-user").path).unwrap();
    assert!(matches!(ops::protect(&locked, "", &new, &seed(1)), Err(Error::PasswordRequired)));
}

#[test]
fn a_signed_file_is_refused() {
    let signed = corpus("public/incremental/signature_reason.pdf");
    let doc = open_file(&signed, "");
    let e = refusal(ops::protect(&doc, "", &protection(Method::AesV3, "u", "o", Permissions::all()), &seed(9)));
    assert!(e.contains("signed") && e.contains("signature"), "{e}");
    assert!(ops::ensure_unsigned(&doc).is_err());
    // The same file with its signature field emptied is not refused; the same goes for any file without one.
    assert!(ops::ensure_unsigned(&open_file(&corpus(GONGWEN), "")).is_ok());
}

/// A one-page file with a signature dictionary that no form lists: it hangs from an annotation's `/V`, by a reference or direct.
fn hidden_signature(direct: bool) -> Vec<u8> {
    let sig = "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] /Contents <00112233> >>";
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Annots [5 0 R] >>");
    let value = if direct { sig.to_string() } else { "6 0 R".to_string() };
    b.obj(5, &format!("<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [0 0 0 0] /V {value} >>"));
    if !direct {
        b.obj(6, sig);
    }
    b.finish_classic(7, "/Root 1 0 R")
}

#[test]
fn a_signature_that_no_form_lists_is_met_while_the_file_is_written_and_refuses_it_too() {
    for direct in [false, true] {
        let doc = Document::from_bytes(hidden_signature(direct)).unwrap();
        // The form says nothing (there is none), so the quick look finds nothing ...
        assert!(ops::ensure_unsigned(&doc).is_ok(), "direct {direct}");
        // ... and the rewrite meets the dictionary: refused, nothing written.
        let e = refusal(ops::protect(&doc, "", &protection(Method::AesV3, "u", "o", Permissions::all()), &seed(3)));
        assert!(e.contains("signed") && e.contains("signature"), "direct {direct}: {e}");
        let e = refusal(ops::protect(&doc, "", &protection(Method::AesV2, "u", "o", Permissions::all()), &seed(3)));
        assert!(e.contains("signed"), "direct {direct}: {e}");
    }
    // `decrypt` too: an encrypted file of that kind (qpdf encrypts it) is not written again.
    let Some(qpdf) = common::find_qpdf() else {
        println!("SKIPPED the decrypt half: qpdf was not found");
        return;
    };
    let dir = common::fresh_out_dir("protect-hidden-signature");
    for direct in [false, true] {
        let (plain, locked) = (dir.join(format!("hidden-{direct}.pdf")), dir.join(format!("locked-{direct}.pdf")));
        std::fs::write(&plain, hidden_signature(direct)).unwrap();
        let made = std::process::Command::new(&qpdf).args(["--encrypt", "", "owner-pw", "256", "--"]).arg(&plain).arg(&locked).output().unwrap();
        assert!(matches!(made.status.code(), Some(0 | 3)), "{}", String::from_utf8_lossy(&made.stderr));
        let doc = open_file(&locked, "owner-pw");
        assert!(ops::ensure_unsigned(&doc).is_ok(), "direct {direct}");
        let e = refusal(ops::decrypt(&doc, "owner-pw"));
        assert!(e.contains("signed") && e.contains("signature"), "direct {direct}: {e}");
    }
}

#[test]
fn decrypt_refuses_a_signed_file_as_protect_does() {
    for (name, owner) in [("sig.r4-aes128-empty-signature.pdf", "owner"), ("sig.r6-aes256-user-signature.pdf", "owner")] {
        let path = common::generated_dir().join(name);
        let doc = open_file(&path, owner);
        let e = refusal(ops::decrypt(&doc, owner));
        assert!(e.contains("signed") && e.contains("signature"), "{name}: {e}");
        let e = refusal(ops::protect(&doc, owner, &protection(Method::AesV3, "u", "o", Permissions::all()), &seed(4)));
        assert!(e.contains("signed"), "{name}: {e}");
    }
}

#[test]
fn a_password_of_invisible_characters_is_refused_not_written_as_no_password() {
    let doc = open_file(&corpus(GONGWEN), "");
    for (user, owner) in [("\u{200B}", ""), ("u", "\u{200B}\u{200D}"), ("\u{FEFF}\u{00AD}", "o")] {
        let e = refusal(ops::protect(&doc, "", &protection(Method::AesV3, user, owner, Permissions::all()), &seed(2)));
        assert!(e.contains("no password") && e.contains("password"), "{e}");
        // (AES-128 cannot keep such a character at all.)
        assert!(ops::protect(&doc, "", &protection(Method::AesV2, user, owner, Permissions::all()), &seed(2)).is_err());
    }
}

#[test]
fn a_seed_that_is_missing_is_refused_and_the_same_seed_gives_the_same_file() {
    let doc = open_file(&corpus(GONGWEN), "");
    let how = protection(Method::AesV3, "u", "o", Permissions::all());
    assert!(refusal(ops::protect(&doc, "", &how, &[0u8; 32])).contains("seed"));
    let (a, b) = (ops::protect(&doc, "", &how, &seed(1)).unwrap(), ops::protect(&doc, "", &how, &seed(1)).unwrap());
    assert_eq!(a.data, b.data, "the randomness comes from the seed alone");
    let c = ops::protect(&doc, "", &how, &seed(2)).unwrap();
    assert_ne!(a.data, c.data);
    assert!(a.data.len().abs_diff(c.data.len()) < 400, "another seed gives another file of the same size, within the length of the strings");
}

/// A file with an obvious string in the information dictionary, in the page content, in a form field's value and in a stream that is
/// not compressed: encrypted, none of it can be found in the bytes; decrypted, all of it is there.
fn marked_file() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>");
    b.stream_obj(4, "", b"BT /F1 12 Tf 10 50 Td (CONTENTMARKER) Tj ET");
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    b.obj(6, "<< /Title (TITLEMARKER) /Author (AUTHORMARKER) >>");
    b.stream_obj(7, "/Type /Metadata /Subtype /XML", b"<x:xmpmeta>METADATAMARKER</x:xmpmeta>");
    b.finish_classic(8, "/Root 1 0 R /Info 6 0 R")
}

const MARKERS: [&[u8]; 4] = [b"CONTENTMARKER", b"TITLEMARKER", b"AUTHORMARKER", b"METADATAMARKER"];

#[test]
fn nothing_of_the_file_is_in_the_clear_once_it_is_encrypted_and_the_passwords_are_nowhere() {
    let source = Document::from_bytes(marked_file()).unwrap();
    for method in [Method::AesV3, Method::AesV2] {
        let how = protection(method, "user-pw-needle", "owner-pw-needle", Permissions::all());
        let out = ops::protect(&source, "", &how, &seed(7)).unwrap();
        for marker in MARKERS {
            assert!(!common::contains(&out.data, marker), "{method:?}: {} is in the clear", String::from_utf8_lossy(marker));
        }
        for password in [b"user-pw-needle".as_slice(), b"owner-pw-needle"] {
            assert!(!common::contains(&out.data, password), "{method:?}: a password is in the file");
        }
        let doc = Document::from_bytes_with_password(out.data, "user-pw-needle").unwrap();
        let content = page_contents(&doc);
        assert!(common::contains(&content[0], b"CONTENTMARKER"));
        let info = info::describe(&doc).unwrap().info;
        assert!(info.iter().any(|(k, v)| k == "Title" && v == "TITLEMARKER") && info.iter().any(|(k, v)| k == "Author" && v == "AUTHORMARKER"), "{info:?}");
    }
    // Without the encryption of the metadata, it is the one thing that stays readable.
    let mut how = protection(Method::AesV3, "u", "o", Permissions::all());
    how.encrypt_metadata = false;
    let out = ops::protect(&source, "", &how, &seed(8)).unwrap();
    assert!(common::contains(&out.data, b"METADATAMARKER"));
    for marker in &MARKERS[..3] {
        assert!(!common::contains(&out.data, marker), "{}", String::from_utf8_lossy(marker));
    }
    let doc = Document::from_bytes_with_password(out.data.clone(), "u").unwrap();
    assert!(!doc.encryption().unwrap().encrypt_metadata && doc.encryption().unwrap().perms_valid);
    assert_eq!(page_contents(&doc).len(), 1);
    if let Some(qpdf) = common::find_qpdf() {
        let dir = common::fresh_out_dir("protect-cleartext-metadata");
        let path = dir.join("clear.pdf");
        std::fs::write(&path, &out.data).unwrap();
        assert_eq!(common::qpdf_check_pw(&qpdf, &path, "u").code, 0);
        let decrypted = dir.join("clear-dec.pdf");
        assert!(common::qpdf_decrypt(&qpdf, &path, "u", &decrypted));
        let text = std::fs::read(&decrypted).unwrap();
        assert!(common::contains(&text, b"CONTENTMARKER") && common::contains(&text, b"METADATAMARKER"));
    }
    // And with AES-128 (the key depends on /EncryptMetadata there).
    let mut how = protection(Method::AesV2, "u", "o", Permissions::all());
    how.encrypt_metadata = false;
    let out = ops::protect(&source, "", &how, &seed(9)).unwrap();
    assert!(common::contains(&out.data, b"METADATAMARKER") && !common::contains(&out.data, b"CONTENTMARKER"));
    let doc = Document::from_bytes_with_password(out.data, "u").unwrap();
    assert!(!doc.encryption().unwrap().encrypt_metadata);
    assert!(common::contains(&page_contents(&doc)[0], b"CONTENTMARKER"));
}

#[test]
fn a_file_without_an_id_is_given_one_and_a_file_with_one_keeps_it() {
    let bare = Document::from_bytes(marked_file()).unwrap();
    assert!(bare.trailer().get("ID").is_none());
    for method in [Method::AesV3, Method::AesV2] {
        let out = ops::protect(&bare, "", &protection(method, "u", "o", Permissions::all()), &seed(11)).unwrap();
        let doc = Document::from_bytes_with_password(out.data, "u").unwrap();
        let id = doc.first_file_id();
        assert_eq!(id.len(), 16, "{method:?}");
        assert!(id.iter().any(|&b| b != 0));
    }
    // The first part of an /ID the file has is the file's for good (the key of AES-128 depends on it); the second is new.
    let with_id = Document::open(corpus(GONGWEN)).unwrap();
    let first = with_id.first_file_id();
    assert!(!first.is_empty(), "the source has an /ID");
    for method in [Method::AesV3, Method::AesV2] {
        let out = ops::protect(&with_id, "", &protection(method, "u", "o", Permissions::all()), &seed(12)).unwrap();
        assert_eq!(Document::from_bytes_with_password(out.data, "u").unwrap().first_file_id(), first, "{method:?}");
    }
}

#[test]
fn removing_the_encryption_after_setting_it_gives_the_pages_back() {
    // `decrypt` is the way back (the rule is the same: the owner's rights).
    let source = open_file(&corpus(GONGWEN), "");
    let how = protection(Method::AesV3, "u", "o", rights(0, false, true, true, true, true, true));
    let protected = ops::protect(&source, "", &how, &seed(13)).unwrap();
    let user = Document::from_bytes_with_password(protected.data.clone(), "u").unwrap();
    let e = refusal(ops::decrypt(&user, "u"));
    assert!(e.contains("owner password"), "{e}");
    let owner = Document::from_bytes_with_password(protected.data, "o").unwrap();
    let plain = ops::decrypt(&owner, "o").unwrap();
    let back = Document::from_bytes(plain.data).unwrap();
    assert!(!back.is_encrypted());
    assert_eq!(page_texts(&back), page_texts(&source));
    assert_eq!(page_contents(&back), page_contents(&source));
}

// --- through the reader's engine (File > Passwords and permissions) --------------------------------------------------

use qingpdf_core::view::{Cipher, Event, ProtectChange, SaveStatus};

fn protected(v: &common::Viewer, id: u64) -> (SaveStatus, String, u64) {
    v.until(|e| match e {
        Event::Protected { id: i, status, message, bytes, .. } if i == id => Some((status, message, bytes)),
        _ => None,
    })
}

#[test]
fn the_engine_tells_how_the_open_file_is_protected_and_who_may_change_it() {
    use common::generated_dir;
    let plain = common::Viewer::open(&corpus(GONGWEN), "");
    let p = &plain.opened.protection;
    assert_eq!((p.cipher, p.needs_password, p.may_change, p.signed), (None, false, true, false));
    assert!(p.permissions.allows_everything());
    // (file, password, cipher, needs a password, may change, which rights are on)
    let rows = [
        ("bookmarks.r4-aes128-user-assemble-n.pdf", "user", Some(Cipher::Aes128), true, false, false),
        ("bookmarks.r4-aes128-user-assemble-n.pdf", "owner", Some(Cipher::Aes128), true, true, false),
        ("two.r6-aes256-user-everything-denied.pdf", "user", Some(Cipher::Aes256), true, false, false),
        ("bookmarks.r6-aes256-empty-print-none.pdf", "", Some(Cipher::Aes256), false, false, false),
        ("bookmarks.r6-aes256-empty-print-none.pdf", "owner", Some(Cipher::Aes256), false, true, false),
        ("bookmarks.r3-128-rc4-user-modify-none.pdf", "owner", Some(Cipher::Rc4), true, true, false),
        ("bookmarks.r2-40-user.pdf", "user", Some(Cipher::Rc4), true, true, true),
        ("bookmarks.r4-aes128-empty-owner-too.pdf", "", Some(Cipher::Aes128), false, true, false),
    ];
    for (file, password, cipher, needs, may_change, everything) in rows {
        let v = common::Viewer::open(&generated_dir().join(file), password);
        let p = &v.opened.protection;
        assert_eq!((p.cipher, p.needs_password, p.may_change, p.permissions.allows_everything()), (cipher, needs, may_change, everything), "{file} {password:?}");
        assert!(!p.signed, "{file}");
    }
    let signed = common::Viewer::open(&corpus("public/incremental/signature_reason.pdf"), "");
    assert!(signed.opened.protection.signed);
}

#[test]
fn the_engine_writes_the_document_with_its_unsaved_edits_under_the_new_protection() {
    let dir = common::fresh_out_dir("protect-engine");
    let original = dir.join("two.pdf");
    std::fs::copy(corpus("public/xref-classic/hello_world_2_pages.pdf"), &original).unwrap();
    let before = std::fs::read(&original).unwrap();
    let mut v = common::Viewer::open(&original, "");
    // An edit that is not saved: the first page is turned. It must be in the result.
    let id = v.id();
    v.engine.rotate_pages(1, id, vec![0], 90);
    assert_eq!(v.edited(id).status, qingpdf_core::view::EditStatus::Done);
    let target = dir.join("locked.pdf");
    let how = protection(Method::AesV3, "u-1", "o-1", rights(2, false, true, true, true, true, true));
    let id = v.id();
    v.engine.protect(1, id, &target.to_string_lossy(), ProtectChange::Set(Box::new(how)), seed(3));
    let (status, message, bytes) = protected(&v, id);
    assert_eq!(status, SaveStatus::Done, "{message}");
    assert_eq!(bytes, std::fs::metadata(&target).unwrap().len());
    // The open file was not touched; the new one is encrypted, opens with each password and has the turned page.
    assert_eq!(std::fs::read(&original).unwrap(), before);
    let doc = open_file(&target, "u-1");
    assert!(!doc.encryption().unwrap().permissions.copy);
    let pages = doc.pages().unwrap();
    assert_eq!((pages.len(), pages[0].rotate(), pages[1].rotate()), (2, 90, 0), "the edit that was not saved is in the file");
    assert!(Document::open_with_password(&target, "nope").is_err());
    // The same document over the file that is open (a whole rewrite, then the viewer opens it again): the old bytes are replaced whole.
    let id = v.id();
    v.engine.protect(1, id, &original.to_string_lossy(), ProtectChange::Set(Box::new(protection(Method::AesV2, "", "o-2", Permissions::all()))), seed(4));
    // (A password for the owner and nothing restricted: allowed; it opens without a password.)
    let (status, message, _) = protected(&v, id);
    assert_eq!(status, SaveStatus::Done, "{message}");
    let again = Document::open(&original).unwrap();
    assert!(again.is_encrypted() && !again.is_locked() && again.pages().unwrap()[0].rotate() == 90);
    // Taking the protection off needs an encrypted file; a file that is not encrypted says so.
    let id = v.id();
    v.engine.protect(1, id, &dir.join("never.pdf").to_string_lossy(), ProtectChange::Remove, seed(5));
    let (status, message, _) = protected(&v, id);
    assert_eq!(status, SaveStatus::Failed);
    assert!(message.contains("not encrypted") && !dir.join("never.pdf").exists(), "{message}");
    // The temporary files are gone.
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn the_engine_refuses_what_the_protection_rules_refuse_and_writes_nothing() {
    let dir = common::fresh_out_dir("protect-engine-refusals");
    let try_it = |file: PathBuf, password: &str, change: ProtectChange, needle: &str| {
        let mut v = common::Viewer::open(&file, password);
        let id = v.id();
        let target = dir.join("out.pdf");
        v.engine.protect(1, id, &target.to_string_lossy(), change, seed(6));
        let (status, message, _) = protected(&v, id);
        assert_eq!(status, SaveStatus::Failed, "{}: {message}", file.display());
        assert!(message.contains(needle), "{}: {message}", file.display());
        assert!(!target.exists(), "a file was written");
    };
    let new = || ProtectChange::Set(Box::new(protection(Method::AesV3, "u", "o", Permissions::all())));
    let generated = common::generated_dir();
    // Opened with the user password of a file that restricts: the owner's rights are needed, for a new protection and to take it off.
    try_it(generated.join("two.r6-aes256-user-everything-denied.pdf"), "user", new(), "owner password");
    try_it(generated.join("two.r6-aes256-user-everything-denied.pdf"), "user", ProtectChange::Remove, "owner password");
    // A signed file (and taking the protection off one: the file is written again either way).
    try_it(corpus("public/incremental/signature_reason.pdf"), "", new(), "signature");
    try_it(generated.join("sig.r6-aes256-user-signature.pdf"), "owner", ProtectChange::Remove, "signature");
    // A password that cannot be kept, and a right turned off without an owner password.
    try_it(corpus(GONGWEN), "", ProtectChange::Set(Box::new(protection(Method::AesV2, "密码", "o", Permissions::all()))), "AES-256");
    try_it(corpus(GONGWEN), "", ProtectChange::Set(Box::new(protection(Method::AesV3, "u", "", rights(2, false, true, true, true, true, true)))), "owner password");
}

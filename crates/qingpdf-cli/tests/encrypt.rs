//! `qingpdf encrypt` (passwords and permissions), run as a user would: arguments in, files and exit codes out. What it writes is checked
//! the way the engine's tests check it (`qpdf --check`, `--show-encryption`, the text of the pages is as it was); the engine's own tests
//! and `tests/tools/protect_check.py` (PDFium) go through the combinations. No password appears in anything the program prints.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::{Path, PathBuf};

use runner::{code, run, s, stderr, stdout};

fn source() -> PathBuf {
    common::corpus_root().join("public/zh/gongwen/gongwen-2025-service-mfg-plan.pdf")
}

fn text_of(file: &Path, password: &str) -> String {
    let out = run(["text", s(file), "--password", password]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    stdout(&out)
}

/// Everything the program printed, to look for passwords in.
fn printed(out: &std::process::Output) -> String {
    format!("{}{}", stdout(out), stderr(out))
}

#[test]
fn encrypt_has_help_and_wrong_command_lines_exit_with_2() {
    assert!(stdout(&run(["--help"])).contains("encrypt"));
    let help = stdout(&run(["encrypt", "--help"]));
    for word in ["--user-password", "--owner-password", "--password-file", "--aes128", "--print", "--no-copy", "--no-modify", "--no-annotate", "--no-forms", "--no-assemble", "--no-accessibility"] {
        assert!(help.contains(word), "the help does not mention {word}");
    }
    let dir = common::fresh_out_dir("cli-encrypt-usage");
    let (input, output) = (source(), dir.join("o.pdf"));
    let bad = |extra: &[&str]| {
        let mut args = vec!["encrypt", s(&input), "-o", s(&output)];
        args.extend_from_slice(extra);
        run(args)
    };
    assert_eq!(code(&bad(&["--print", "sometimes"])), 2);
    assert_eq!(code(&bad(&["--print"])), 2);
    assert_eq!(code(&bad(&["--aes128=yes"])), 2);
    assert_eq!(code(&bad(&["--password-file", "x.txt", "--user-password", "u"])), 2);
    assert_eq!(code(&bad(&["--user-password", "a", "--user-password", "b"])), 2);
    assert!(!output.exists());
    assert_eq!(code(&run(["encrypt", s(&input), "--user-password", "u"])), 2, "-o is required");
    assert_eq!(code(&run(["info", s(&input), "--aes128"])), 2, "the options belong to encrypt");
    assert_eq!(code(&run(["encrypt", "-o", s(&output), "--user-password", "u"])), 2, "an input is required");
    // What a mistyped option shows is the option, not what was given with it.
    let out = run(["encrypt", s(&input), "-o", s(&output), "--usr-password", "secret-value"]);
    assert_eq!(code(&out), 2);
    assert!(!printed(&out).contains("secret-value"));
}

#[test]
fn a_file_is_encrypted_with_aes_256_by_default_and_opens_with_each_password() {
    let dir = common::fresh_out_dir("cli-encrypt-256");
    let (input, output) = (source(), dir.join("out.pdf"));
    let out = run(["encrypt", s(&input), "-o", s(&output), "--user-password", "open-me-1", "--owner-password", "own-me-2", "--no-copy", "--print", "low"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let said = printed(&out);
    assert!(said.contains("wrote") && said.contains("AES-256") && said.contains("copy"), "{said}");
    assert!(!said.contains("open-me-1") && !said.contains("own-me-2"), "a password was printed: {said}");
    // The file needs the password; the user's and the owner's both open it; a wrong one does not.
    let locked = run(["text", s(&output)]);
    assert_eq!(code(&locked), 1);
    assert!(stderr(&locked).contains("needs a password"), "{}", stderr(&locked));
    let wrong = run(["text", s(&output), "--password", "nope"]);
    assert_eq!(code(&wrong), 1);
    assert!(stderr(&wrong).contains("wrong password"));
    let want = text_of(&input, "");
    assert_eq!(text_of(&output, "own-me-2"), want);
    // Copying is turned off: the user password opens the file but `text` is refused, as it is for any file that says so.
    let refused = run(["text", s(&output), "--password", "open-me-1"]);
    assert_eq!(code(&refused), 1);
    assert!(stderr(&refused).contains("did not allow copying"), "{}", stderr(&refused));
    // `info` tells what was set.
    let info = stdout(&run(["info", s(&output), "--password", "open-me-1"]));
    assert!(info.contains("AES-256 (V5, R6)") && info.contains("copy or extract text and graphics: no") && info.contains("print in high quality: no"), "{info}");
    assert!(info.contains("print: yes") && info.contains("modify the contents: yes"), "{info}");
    if let Some(qpdf) = common::find_qpdf() {
        for pw in ["open-me-1", "own-me-2"] {
            let v = common::qpdf_check_pw(&qpdf, &output, pw);
            assert!(v.code == 0, "qpdf says {}:\n{}", v.code, v.text);
        }
        let q = common::qpdf_encryption(&qpdf, &output, "own-me-2").unwrap();
        assert_eq!(q.revision, 6);
        assert!(q.owner_matched && q.methods.iter().all(|m| m == "AESv3"));
        // Bits 3 (print) and 12 (the best quality): print yes, quality no; bit 5 (copy) off; the rest on.
        assert_eq!(q.permissions, i64::from((0xFFFF_F0C0u32 | 0x4 | 0x8 | 0x20 | 0x100 | 0x200 | 0x400) as i32));
    }
}

#[test]
fn aes_128_is_chosen_with_a_flag_and_has_revision_4() {
    let dir = common::fresh_out_dir("cli-encrypt-128");
    let (input, output) = (source(), dir.join("out.pdf"));
    let out = run(["encrypt", s(&input), "-o", s(&output), "--aes128", "--owner-password", "o-1", "--no-modify", "--no-annotate", "--no-forms", "--no-assemble", "--no-accessibility", "--print", "none"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("AES-128") && stdout(&out).contains("opens without a password"), "{}", stdout(&out));
    // No user password: it opens at once, with everything but what the owner kept.
    let info = stdout(&run(["info", s(&output)]));
    assert!(info.contains("AES-128 (V4, R4)") && info.contains("not needed"), "{info}");
    for denied in ["print: no", "modify the contents: no", "add or change annotations: no", "fill in form fields: no", "assemble (insert, rotate, delete pages): no", "extract for accessibility: no"] {
        assert!(info.contains(denied), "{denied}\n{info}");
    }
    assert!(info.contains("copy or extract text and graphics: yes"), "{info}");
    assert_eq!(text_of(&output, ""), text_of(&input, ""));
    if let Some(qpdf) = common::find_qpdf() {
        let v = common::qpdf_check(&qpdf, &output);
        assert!(v.code == 0, "qpdf says {}:\n{}", v.code, v.text);
        let q = common::qpdf_encryption(&qpdf, &output, "o-1").unwrap();
        assert_eq!(q.revision, 4);
        assert!(q.methods.iter().all(|m| m == "AESv2"));
        assert_eq!(q.permissions, i64::from((0xFFFF_F0C0u32 | 0x10) as i32));
    }
}

#[test]
fn passwords_may_come_from_a_file_and_chinese_ones_work() {
    let dir = common::fresh_out_dir("cli-encrypt-file");
    let (input, output) = (source(), dir.join("out.pdf"));
    // Line 1 the user password, line 2 the owner password; Windows line ends and a final one are no part of them.
    let file = dir.join("pw.txt");
    std::fs::write(&file, "密码一二三\r\n主人密码\r\n").unwrap();
    let out = run(["encrypt", s(&input), "-o", s(&output), "--password-file", s(&file), "--no-modify"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!printed(&out).contains("密码"));
    let want = text_of(&input, "");
    assert_eq!(text_of(&output, "密码一二三"), want);
    assert_eq!(text_of(&output, "主人密码"), want);
    if let Some(qpdf) = common::find_qpdf() {
        for pw in ["密码一二三", "主人密码"] {
            let v = common::qpdf_check_pw(&qpdf, &output, pw);
            assert!(v.code == 0, "{pw}: qpdf says {}:\n{}", v.code, v.text);
        }
    }
    // A user password only (one line): the owner's is the same. Only an owner password (an empty first line).
    std::fs::write(&file, "only-user").unwrap();
    let both = dir.join("both.pdf");
    assert_eq!(code(&run(["encrypt", s(&input), "-o", s(&both), "--password-file", s(&file)])), 0);
    assert_eq!(text_of(&both, "only-user"), want);
    std::fs::write(&file, "\nonly-owner\n").unwrap();
    let owner_only = dir.join("owner-only.pdf");
    assert_eq!(code(&run(["encrypt", s(&input), "-o", s(&owner_only), "--password-file", s(&file), "--no-modify"])), 0);
    assert_eq!(text_of(&owner_only, ""), want, "no user password: it opens at once");
    assert!(stdout(&run(["info", s(&owner_only)])).contains("modify the contents: no"));
    // A file with a third line is a mistake, and so is one that is missing or not text.
    std::fs::write(&file, "a\nb\nc\n").unwrap();
    let out = run(["encrypt", s(&input), "-o", s(&dir.join("x.pdf")), "--password-file", s(&file)]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("more than two lines") && !printed(&out).contains("a\nb"), "{}", stderr(&out));
    std::fs::write(&file, [0xFFu8, 0xFE, 0xFD]).unwrap();
    assert_eq!(code(&run(["encrypt", s(&input), "-o", s(&dir.join("x.pdf")), "--password-file", s(&file)])), 1);
    assert_eq!(code(&run(["encrypt", s(&input), "-o", s(&dir.join("x.pdf")), "--password-file", s(&dir.join("nothing.txt"))])), 1);
    assert!(!dir.join("x.pdf").exists());
}

#[test]
fn a_password_file_is_read_as_it_is_written_and_a_slip_is_refused() {
    let dir = common::fresh_out_dir("cli-encrypt-pwfile");
    let (input, file) = (source(), dir.join("pw.txt"));
    let want = text_of(&input, "");
    let encrypt = |content: &[u8], name: &str| {
        std::fs::write(&file, content).unwrap();
        let output = dir.join(name);
        (run(["encrypt", s(&input), "-o", s(&output), "--password-file", s(&file)]), output)
    };
    // A line ends with CRLF, LF or a lone CR (and the last may have none), and that is no part of the password.
    for (i, content) in ["uu\nooo", "uu\r\nooo\r\n", "uu\rooo\r", "uu\rooo", "uu\r\nooo", "\u{feff}uu\nooo\n"].iter().enumerate() {
        let (out, output) = encrypt(content.as_bytes(), &format!("ok{i}.pdf"));
        assert_eq!(code(&out), 0, "{content:?}: {}", stderr(&out));
        assert_eq!(text_of(&output, "uu"), want, "{content:?}");
        assert_eq!(text_of(&output, "ooo"), want, "{content:?}");
    }
    // What cmd's `echo secret > pw.txt` writes, "secret " and a line end, is a slip: refused with the reason, and the password is not in it.
    // So are a space or a tab at either end, and a control character, in either password.
    for (i, content) in ["secret \r\n", " secret\n", "secret\t\n", "sec\u{7}ret\n", "uu\nsecret \r\n", "uu\n\u{3000}secret\n", "uu\nsec\u{1b}ret"].iter().enumerate() {
        let (out, output) = encrypt(content.as_bytes(), &format!("slip{i}.pdf"));
        assert_eq!(code(&out), 1, "{content:?}");
        let said = stderr(&out);
        assert!(said.contains("starts or ends with a space, or has a control character") && !said.contains("secret"), "{content:?}: {said}");
        assert!(!output.exists(), "{content:?}: a file was written");
    }
    // A password with a space inside is fine.
    let (out, output) = encrypt(b"two words\n", "inside.pdf");
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(text_of(&output, "two words"), want);
}

#[test]
fn what_cannot_be_protected_is_refused_and_nothing_is_written() {
    let dir = common::fresh_out_dir("cli-encrypt-refusals");
    let (input, output) = (source(), dir.join("out.pdf"));
    let refused = |extra: &[&str], needle: &str| {
        let mut args = vec!["encrypt", s(&input), "-o", s(&output)];
        args.extend_from_slice(extra);
        let out = run(args);
        assert_eq!(code(&out), 1, "{extra:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(needle), "{extra:?}: {}", stderr(&out));
        assert!(!output.exists(), "{extra:?}: a file was written");
        for secret in ["hunter2-user", "hunter2-owner"] {
            assert!(!printed(&out).contains(secret), "{extra:?}: a password was printed");
        }
    };
    // A permission off without an owner password; no password and no restriction.
    refused(&["--user-password", "hunter2-user", "--no-copy"], "owner password is needed");
    refused(&["--print", "none"], "owner password is needed");
    refused(&[], "nothing would be protected");
    // Chinese does not fit AES-128, and the words say what to do; a password too long.
    refused(&["--aes128", "--user-password", "密码", "--owner-password", "hunter2-owner"], "AES-256");
    refused(&["--aes128", "--user-password", &"a".repeat(33)], "32");
    refused(&["--user-password", &"b".repeat(128)], "127");
    // The input may not be the output; an output that exists needs --force.
    let same = run(["encrypt", s(&input), "-o", s(&input), "--user-password", "u", "--force"]);
    assert_eq!(code(&same), 1);
    assert!(stderr(&same).contains("same as an input"));
    let existing = dir.join("exists.pdf");
    std::fs::write(&existing, b"old").unwrap();
    let out = run(["encrypt", s(&input), "-o", s(&existing), "--user-password", "u"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("--force") && std::fs::read(&existing).unwrap() == b"old");
    assert_eq!(code(&run(["encrypt", s(&input), "-o", s(&existing), "--user-password", "u", "--force"])), 0);
    // Not a file, or not a PDF.
    assert_eq!(code(&run(["encrypt", s(&dir.join("missing.pdf")), "-o", s(&output), "--user-password", "u"])), 1);
    let junk = dir.join("junk.pdf");
    std::fs::write(&junk, b"this is not a pdf").unwrap();
    assert_eq!(code(&run(["encrypt", s(&junk), "-o", s(&output), "--user-password", "u"])), 1);
    assert!(!output.exists());
}

#[test]
fn a_file_that_is_encrypted_already_needs_its_password_and_the_owners_rights() {
    let dir = common::fresh_out_dir("cli-encrypt-again");
    let generated = common::generated_dir();
    let restricted = generated.join("two.r6-aes256-user-everything-denied.pdf"); // user "user", owner "owner", nothing allowed
    let output = dir.join("out.pdf");
    // No password: it needs one. The user's: refused, with what to do. The owner's: done.
    let out = run(["encrypt", s(&restricted), "-o", s(&output), "--user-password", "new"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("needs a password"), "{}", stderr(&out));
    let out = run(["encrypt", s(&restricted), "-o", s(&output), "--user-password", "new", "--password", "user"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("owner password") && stderr(&out).contains("the protection is not changed"), "{}", stderr(&out));
    assert!(!output.exists());
    let out = run(["encrypt", s(&restricted), "-o", s(&output), "--aes128", "--user-password", "new", "--password", "owner"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!printed(&out).contains("owner"));
    // The old passwords are gone, the new one is the user's and the owner's, and the pages are the same.
    assert_eq!(code(&run(["text", s(&output), "--password", "user"])), 1);
    let want = text_of(&restricted, "owner");
    assert_eq!(text_of(&output, "new"), want);
    assert!(stdout(&run(["info", s(&output), "--password", "new"])).contains("AES-128 (V4, R4)"));
    // Taking the passwords off is `decrypt`, with the same rule.
    let plain = dir.join("plain.pdf");
    assert_eq!(code(&run(["decrypt", s(&output), "-o", s(&plain), "--password", "new"])), 0);
    assert_eq!(text_of(&plain, ""), want);
}

#[test]
fn a_signed_file_is_refused() {
    let dir = common::fresh_out_dir("cli-encrypt-signed");
    let signed = common::corpus_root().join("public/incremental/signature_reason.pdf");
    let out = run(["encrypt", s(&signed), "-o", s(&dir.join("out.pdf")), "--user-password", "u"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("signed") && stderr(&out).contains("signature"), "{}", stderr(&out));
    assert!(!dir.join("out.pdf").exists());
    // `decrypt` writes the file again as a whole too: refused the same way.
    let locked = common::generated_dir().join("sig.r4-aes128-empty-signature.pdf");
    let out = run(["decrypt", s(&locked), "-o", s(&dir.join("plain.pdf")), "--password", "owner"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("signed") && stderr(&out).contains("signature"), "{}", stderr(&out));
    assert!(!dir.join("plain.pdf").exists());
}

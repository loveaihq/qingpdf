//! Layer 2: text extraction. The handmade Chinese files must come out exactly as
//! their `.expected.txt` say; damaged and hostile files must end in text or a clear
//! error, quickly.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use qingpdf_core::text::{self, TextExtractor};
use qingpdf_core::{Document, Error};

fn pages_of(doc: &Document) -> Result<Vec<String>, Error> {
    let mut extractor = TextExtractor::new(doc);
    doc.pages()?.iter().map(|p| extractor.page_text(p)).collect()
}

#[test]
fn handmade_files_come_out_exactly_as_expected() {
    let dir = common::corpus_root().join("public").join("zh").join("handmade");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("tests/corpus/public/zh/handmade must exist") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "pdf") {
            continue;
        }
        let expected = std::fs::read_to_string(path.with_extension("expected.txt")).expect("every handmade file has an .expected.txt");
        let doc = Document::open(&path).unwrap();
        let pages = pages_of(&doc).unwrap();
        assert_eq!(pages, vec![expected], "{}", path.display());
        checked += 1;
    }
    assert!(checked >= 6, "expected the 5 handmade files and the ToUnicode-differs one, found {checked}");
}

/// Text of one page (0-based) of a public corpus file.
fn page_text(rel: &str, page: usize) -> String {
    let doc = Document::open(common::public_file(rel)).unwrap();
    let pages = doc.pages().unwrap();
    TextExtractor::new(&doc).page_text(&pages[page]).unwrap()
}

#[test]
fn real_files_give_the_text_that_is_there() {
    // a Word/WPS government document: Identity-H with ToUnicode
    assert!(page_text("zh/gongwen/gongwen-2024-ai-standards-guide.pdf", 1).contains("二、总体要求\n以习近平新时代中国特色社会主义思想为指导"));
    // a Wikisource export: Type 3 fonts, whose ToUnicode writes ordinary characters as Kangxi radicals
    // (U+2F63 and the like): they come out as the ordinary characters
    let aq = page_text("zh/ebook/ebook-wikisource-aq-zhengzhuan.pdf", 1);
    assert!(aq.contains("趙太爺愈看愈生氣了"), "{aq}");
    assert!(!aq.chars().any(|c| ('\u{2F00}'..='\u{2FD5}').contains(&c)));
    // vertical text (Identity-V): one column per line
    let county = page_text("zh/vertical/vertical-nlc-hechuan-juan70.pdf", 1);
    assert!(county.contains("吳門總集庶幾輶軒觀風之遺惜其書軼不傳不知體例奚若\n明楊愼輯吾蜀各體文爲全蜀藝文志之濫觴使承學之士不\n"), "{county}");
    // Shift-JIS through a predefined CMap with no ToUnicode (PyMuPDF garbles this one)
    assert!(page_text("cjk/90ms_rksj_h_sample.pdf", 0).contains("日本語テスト"));
    // the pages of a scan have no text layer: empty, not an error
    assert_eq!(page_text("zh/gongwen/gongwen-1954-gazette02-scan.pdf", 0), "");
}

#[test]
fn text_of_the_whole_corpus_is_text_or_a_clear_error() {
    let mut files = 0;
    let mut chars = 0;
    let mut problems = Vec::new();
    for path in common::corpus_files() {
        let password = common::password_for(&path);
        let Ok(doc) = Document::open_with_password(&path, &password) else { continue };
        if doc.is_locked() {
            continue;
        }
        let Ok(pages) = doc.pages() else { continue };
        let started = std::time::Instant::now();
        let mut extractor = TextExtractor::new(&doc);
        for (i, page) in pages.iter().enumerate().take(60) {
            match extractor.page_text(page) {
                Ok(t) => {
                    chars += t.chars().count();
                    if t.chars().any(|c| c.is_control() && c != '\n') {
                        problems.push(format!("{} page {}: control character in the text", path.display(), i + 1));
                    }
                }
                Err(Error::Limit(_) | Error::Unsupported(_) | Error::Syntax { .. } | Error::MissingObject { .. } | Error::TooDeep(_)) => {}
                Err(e) => problems.push(format!("{} page {}: unexpected error {e}", path.display(), i + 1)),
            }
        }
        if started.elapsed().as_secs() > 20 {
            problems.push(format!("{}: {:?} for at most 60 pages", path.display(), started.elapsed()));
        }
        files += 1;
    }
    assert!(problems.is_empty(), "{problems:#?}");
    assert!(files > 100 && chars > 500_000, "read {files} files, {chars} characters");
}

#[test]
fn text_is_refused_when_the_author_did_not_allow_copying_and_the_owner_password_lifts_it() {
    let dir = common::generated_dir();
    // R2, no extracting; empty user password
    let path = dir.join("two.r2-40-empty-modify-extract-none.pdf");
    let doc = Document::open_with_password(&path, "").unwrap();
    assert!(matches!(text::check_extraction_allowed(&doc, ""), Err(Error::Invalid(m)) if m.contains("owner password")));
    let doc = Document::open_with_password(&path, "owner").unwrap();
    text::check_extraction_allowed(&doc, "owner").unwrap();
    let pages = pages_of(&doc).unwrap();
    assert_eq!(pages.len(), 2);
    assert!(pages.iter().all(|p| p.contains("Hello")), "{pages:?}");
    // R6, everything denied; user password
    let path = dir.join("two.r6-aes256-user-everything-denied.pdf");
    let doc = Document::open_with_password(&path, "user").unwrap();
    assert!(text::check_extraction_allowed(&doc, "user").is_err());
    let doc = Document::open_with_password(&path, "owner").unwrap();
    text::check_extraction_allowed(&doc, "owner").unwrap();
    assert!(pages_of(&doc).unwrap().iter().all(|p| p.contains("Hello")));
    // a user password with copying allowed: fine; a locked file: needs the password
    let path = dir.join("bookmarks.r2-40-user.pdf");
    let doc = Document::open_with_password(&path, "user").unwrap();
    text::check_extraction_allowed(&doc, "user").unwrap();
    let locked = Document::open(&path).unwrap();
    assert!(matches!(text::check_extraction_allowed(&locked, ""), Err(Error::PasswordRequired)));
    assert!(locked.is_locked());
    if let Some(page) = locked.pages().unwrap_or_default().first() {
        assert!(matches!(TextExtractor::new(&locked).page_text(page), Err(Error::PasswordRequired)));
    }
    // an unencrypted file has nothing to refuse
    let plain = Document::open(common::public_file("xref-classic/hello_world.pdf")).unwrap();
    text::check_extraction_allowed(&plain, "").unwrap();
}

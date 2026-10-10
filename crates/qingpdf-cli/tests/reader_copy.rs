//! What the reader copies from a page is what the `text` command gives for that page (3d-2): the reader's engine is asked for the
//! text between two places of a Chinese official document, and the command is run on the same page.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;

use std::process::Command;

use qingpdf_core::view::{Engine, Event, TOTAL_BYTES, TextStatus};

/// The text command's output for one page (1-based), as it writes it to a file.
fn cli_text(file: &std::path::Path, page: usize) -> String {
    let out = std::env::temp_dir().join(format!("qingpdf-copy-test-{}-{page}.txt", std::process::id()));
    let status = Command::new(env!("CARGO_BIN_EXE_qingpdf"))
        .arg("text")
        .arg(file)
        .args(["--pages", &page.to_string(), "-o"])
        .arg(&out)
        .arg("--force")
        .output()
        .expect("runs");
    assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
    let text = std::fs::read_to_string(&out).expect("the text file");
    let _ = std::fs::remove_file(&out);
    text
}

fn copy(engine: &Engine, id: u64, from: (u32, u32), to: (u32, u32)) -> String {
    engine.copy_text(1, id, from, to);
    loop {
        match engine.wait(20_000) {
            Some(Event::Copied { id: i, status, text, .. }) if i == id => {
                assert_eq!(status, TextStatus::Ok);
                return text;
            }
            Some(_) => {}
            None => panic!("no answer"),
        }
    }
}

#[test]
fn text_selected_in_the_reader_is_what_the_text_command_gives_for_the_page() {
    for rel in ["zh/gongwen/gongwen-2024-ai-standards-guide.pdf", "zh/gongwen/gongwen-2024-nicheng-work-report.pdf"] {
        let file = common::public_file(rel);
        let engine = Engine::start(Box::new(|| {}));
        engine.open_file(1, &file.to_string_lossy(), "", TOTAL_BYTES, 1920 * 1080);
        assert!(matches!(engine.wait(20_000), Some(Event::Opened(_))));
        for page in [3usize, 6] {
            let expected = cli_text(&file, page);
            assert!(expected.chars().count() > 200, "{rel} page {page} has text");
            let chars: Vec<char> = expected.chars().collect();
            let page0 = u32::try_from(page - 1).unwrap();
            // All of the page, from its first character to past its last.
            let all = copy(&engine, 10 + page as u64, (page0, 0), (page0, u32::MAX));
            assert_eq!(all, expected, "{rel} page {page}: the whole page");
            // A paragraph in the middle: from one place to another, over a line end or more.
            let (a, b) = (chars.len() / 4, chars.len() / 4 + 160);
            let part = copy(&engine, 100 + page as u64, (page0, a as u32), (page0, b as u32));
            let want: String = chars[a..b].iter().collect();
            assert_eq!(part, want, "{rel} page {page}: characters {a} to {b}");
            assert!(part.contains('\n'), "the piece spans a line end");
        }
    }
}

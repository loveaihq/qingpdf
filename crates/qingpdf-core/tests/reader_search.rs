//! The reader's search on real files (3d-2): Chinese full-width and half-width forms in an official document, and how fast the
//! engine finds words in a 468-page book (through the same requests the window makes).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use qingpdf_core::view::{Engine, Event, SearchStatus, TOTAL_BYTES};

fn open(path: &std::path::Path) -> (Engine, usize) {
    let engine = Engine::start(Box::new(|| {}));
    engine.open_file(1, &path.to_string_lossy(), "", TOTAL_BYTES, 1920 * 1080);
    match engine.wait(30_000) {
        Some(Event::Opened(o)) => {
            let pages = o.pages.len();
            (engine, pages)
        }
        other => panic!("{other:?}"),
    }
}

struct Found {
    /// (page, number of hits on it), in the order they came.
    pages: Vec<(u32, usize)>,
    first_hit: Option<Duration>,
    total: Duration,
    hits: u32,
    status: SearchStatus,
    skipped: u32,
}

fn search(engine: &Engine, id: u64, query: &str, start: u32) -> Found {
    let began = Instant::now();
    engine.search(1, id, query, start);
    let mut found = Found { pages: Vec::new(), first_hit: None, total: Duration::ZERO, hits: 0, status: SearchStatus::Finished, skipped: 0 };
    loop {
        match engine.wait(60_000) {
            Some(Event::SearchHits { id: i, page, hits, .. }) if i == id => {
                found.first_hit.get_or_insert_with(|| began.elapsed());
                found.pages.push((page, hits.len()));
            }
            Some(Event::SearchDone { id: i, status, pages_skipped, hits, .. }) if i == id => {
                found.total = began.elapsed();
                (found.status, found.skipped, found.hits) = (status, pages_skipped, hits);
                return found;
            }
            Some(_) => {}
            None => panic!("the search said nothing for a minute"),
        }
    }
}

fn pages_with(found: &Found) -> Vec<u32> {
    let mut p: Vec<u32> = found.pages.iter().map(|(p, _)| *p).collect();
    p.sort_unstable();
    p
}

#[test]
fn full_width_and_half_width_forms_are_the_same_in_a_chinese_document() {
    let (engine, pages) = open(&common::public_file("zh/gongwen/gongwen-2024-ai-standards-guide.pdf"));
    assert_eq!(pages, 13);
    // The text has "（一）人工智能标准体系结构" with full-width brackets, on page 3 (and "（一）基础共性标准" on page 5).
    let half = search(&engine, 1, "(一)人工智能标准体系结构", 0);
    let full = search(&engine, 2, "（一）人工智能标准体系结构", 0);
    assert_eq!(pages_with(&half), vec![2], "half-width query: {:?}", half.pages);
    assert_eq!(pages_with(&half), pages_with(&full));
    let brackets = search(&engine, 3, "(一)", 0);
    assert!(brackets.hits >= 3 && pages_with(&brackets).len() >= 3, "{:?}", brackets.pages);
    // A digit and a Chinese character with a space the layout put between them: "7 个部分" (pages 3 and 4) is found as "7个部分"
    // and as "７个部分".
    assert_eq!(pages_with(&search(&engine, 4, "7个部分", 0)), vec![2, 3]);
    assert_eq!(pages_with(&search(&engine, 5, "７个部分", 0)), vec![2, 3]);
    // A word that is cut by the end of a line ("协" and "同" are in two lines).
    assert_eq!(pages_with(&search(&engine, 6, "全产业链标准化工作协同", 0)), vec![2]);
    // The year in the title is written 2024: a full-width query finds it, and so do capital and small letters of a Latin word.
    let year = search(&engine, 7, "（２０２４版）", 0);
    assert_eq!(pages_with(&year), vec![0], "{:?}", year.pages);
    assert!(search(&engine, 8, "Zzzz没有这个词", 0).pages.is_empty());
}

#[test]
fn the_first_place_is_found_at_once_and_the_whole_book_is_searched_in_a_second() {
    let book = common::corpus_root().join("local").join("zh").join("ebook").join("ebook-wikisource-yijikao-468p.pdf");
    if !book.is_file() {
        eprintln!("skipped: {} is not here (the local corpus is not in the repository)", book.display());
        return;
    }
    let (engine, pages) = open(&book);
    assert_eq!(pages, 468);
    // The best of three, so that a busy moment of the machine does not decide.
    let mut best_first = Duration::MAX;
    let mut best_total = Duration::MAX;
    let mut last = None;
    for round in 0..3 {
        let found = search(&engine, 10 + round, "素问", 0);
        println!("round {round}: first hit {:.1} ms, whole book {:.1} ms", found.first_hit.map_or(0.0, |d| d.as_secs_f64() * 1000.0), found.total.as_secs_f64() * 1000.0);
        best_first = best_first.min(found.first_hit.expect("a first hit"));
        best_total = best_total.min(found.total);
        last = Some(found);
    }
    let found = last.expect("a search was made");
    println!(
        "search \"素问\" in {pages} pages: first hit after {:.1} ms (page {}), whole book {:.1} ms, {} hits on {} pages, skipped {}",
        best_first.as_secs_f64() * 1000.0,
        found.pages.first().map_or(0, |p| p.0 + 1),
        best_total.as_secs_f64() * 1000.0,
        found.hits,
        found.pages.len(),
        found.skipped
    );
    assert!(found.hits > 50 && found.status == SearchStatus::Finished && found.skipped == 0);
    // A word that is in the last pages: the search starts at the front, so the first hit comes late; from the page before, at once.
    let late = search(&engine, 20, "丹波元胤", 0);
    println!("search \"丹波元胤\": first hit after {:.1} ms, whole book {:.1} ms", late.first_hit.map_or(0.0, |d| d.as_secs_f64() * 1000.0), late.total.as_secs_f64() * 1000.0);
    // Searching again from the middle wraps round and still covers every page.
    let wrapped = search(&engine, 21, "素问", 300);
    assert_eq!(wrapped.hits, found.hits, "the same places whichever page it starts at");
    if !cfg!(debug_assertions) {
        assert!(best_first <= Duration::from_millis(100), "first hit after {best_first:?}");
        assert!(best_total <= Duration::from_secs(1), "whole book {best_total:?}");
    }
}

#[test]
fn the_citations_of_a_latex_paper_are_links_to_its_bibliography() {
    use qingpdf_core::view::LinkKind;
    let (engine, pages) = open(&common::public_file("zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf"));
    assert_eq!(pages, 24);
    engine.links(1, 5, 0);
    let links = loop {
        match engine.wait(20_000) {
            Some(Event::Links { id: 5, links, .. }) => break links,
            Some(_) => {}
            None => panic!("no answer"),
        }
    };
    assert_eq!(links.len(), 21, "the citations of the first page");
    assert!(links.iter().all(|l| l.rect[2] > l.rect[0] && l.rect[3] > l.rect[1]));
    assert!(links.iter().filter(|l| l.kind == LinkKind::Page && l.page > 0).count() >= 12);
    // The first citation is the "1" of "[1-4]": a link 5.9 points wide, 564 down the page, to the bibliography on page 18.
    assert!((links[0].rect[1] - 564.3).abs() < 0.1 && (links[0].rect[2] - links[0].rect[0] - 5.88).abs() < 0.05, "{:?}", links[0]);
    assert_eq!(links[0].page, 17);
}

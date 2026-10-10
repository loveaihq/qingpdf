//! Tests of the 3d-2 requests: bookmarks, links, boxes of characters, search, copy and print.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use super::tests::{open, pdf, request, square};
use super::*;
use crate::testutil::PdfBuilder;

/// A document of pages 612 by 792 whose page `i` shows the lines of `pages[i]` (separated by `|`) in Helvetica 12 from (72, 700),
/// 20 points apart. Page `i` is object `10 + 2 * i`.
fn text_pdf(pages: &[&str]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..pages.len()).map(|i| format!("{} 0 R", 10 + 2 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len()));
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    for (i, text) in pages.iter().enumerate() {
        let n = 10 + 2 * i as u32;
        b.obj(n, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents {} 0 R >>", n + 1));
        let mut content = String::from("BT /F1 12 Tf 72 700 Td ");
        for (k, line) in text.split('|').enumerate() {
            if k > 0 {
                content.push_str("0 -20 Td ");
            }
            content.push_str(&format!("({line}) Tj "));
        }
        content.push_str("ET");
        b.stream_obj(n + 1, "", content.as_bytes());
    }
    b.finish_classic(10 + 2 * pages.len() as u32, "/Root 1 0 R")
}

/// Events until `pick` finds one it wants (up to ten seconds for each); the ones it passes are dropped.
fn until<T>(engine: &Engine, mut pick: impl FnMut(Event) -> Option<T>) -> T {
    loop {
        match engine.wait(10_000) {
            Some(e) => {
                if let Some(t) = pick(e) {
                    return t;
                }
            }
            None => panic!("the engine said nothing for ten seconds"),
        }
    }
}

type SearchResult = (Vec<(u32, Vec<SearchHit>)>, SearchStatus, u32, u32);

/// What a search told: the pages with hits in the order they came, and how it ended (status, pages skipped, hits).
fn search_all(engine: &Engine, id: u64, query: &str, start: u32) -> SearchResult {
    engine.search(1, id, query, start);
    let mut pages = Vec::new();
    let (status, skipped, hits) = until(engine, |e| match e {
        Event::SearchHits { id: i, page, hits, .. } if i == id => {
            pages.push((page, hits));
            None
        }
        Event::SearchDone { id: i, status, pages_skipped, hits, .. } if i == id => Some((status, pages_skipped, hits)),
        _ => None,
    });
    (pages, status, skipped, hits)
}

fn boxes_of(engine: &Engine, id: u64, page: u32) -> CharBoxes {
    engine.char_boxes(1, id, page);
    until(engine, |e| match e {
        Event::CharBoxes(b) if b.id == id => Some(b),
        _ => None,
    })
}

fn copied(engine: &Engine, id: u64, from: (u32, u32), to: (u32, u32)) -> (TextStatus, String, bool) {
    engine.copy_text(1, id, from, to);
    until(engine, |e| match e {
        Event::Copied { id: i, status, text, truncated, .. } if i == id => Some((status, text, truncated)),
        _ => None,
    })
}

#[test]
fn a_search_finds_words_page_by_page_from_where_it_starts_and_tells_where() {
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, text_pdf(&["Hello World|second line", "nothing here", "HELLO again"]));
    let (pages, status, skipped, hits) = search_all(&engine, 7, "hello", 1);
    // Page 1 is looked at first (it has none), then 2, then back round to 0.
    assert_eq!(pages.iter().map(|(p, _)| *p).collect::<Vec<_>>(), [2, 0]);
    assert_eq!((status, skipped, hits), (SearchStatus::Finished, 0, 2));
    let hit = &pages[1].1[0];
    assert_eq!((hit.start, hit.len, hit.rects.len()), (0, 5, 1));
    // "Hello" in Helvetica 12 from (72, 700), on the baseline 92 points from the top of the page. (A font with no /Widths is
    // taken to be half an em wide: 5 letters, 30 points.)
    let r = hit.rects[0];
    assert!((r[0] - 72.0).abs() < 0.5 && (r[2] - r[0] - 30.0).abs() < 1.0, "{r:?}");
    assert!((r[1] - 82.4).abs() < 0.5 && (r[3] - 94.4).abs() < 0.5, "{r:?}");
    // A word that runs over a line end is found, in two rectangles.
    let (pages, _, _, hits) = search_all(&engine, 8, "WORLD  second", 0);
    assert_eq!((hits, pages.len(), pages[0].1[0].rects.len()), (1, 1, 2));
    // Nothing to look for: over at once.
    let (pages, status, _, hits) = search_all(&engine, 9, "   ", 0);
    assert!(pages.is_empty() && hits == 0 && status == SearchStatus::Finished);
    // A start page past the end is the first page.
    let (pages, _, _, hits) = search_all(&engine, 10, "zzz", 99);
    assert!(pages.is_empty() && hits == 0);
}

#[test]
fn every_character_of_a_page_has_a_box() {
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, text_pdf(&["Hello World|second line"]));
    let b = boxes_of(&engine, 1, 0);
    assert_eq!((b.status, b.boxes.len()), (TextStatus::Ok, "Hello World\nsecond line\n".chars().count()));
    // The line ends have no box of their own; the letters (and a space that is drawn) have one, in a row.
    assert!(is_blank(&b.boxes[11]) && is_blank(&b.boxes[23]));
    assert!(!is_blank(&b.boxes[0]) && !is_blank(&b.boxes[5]) && b.boxes[1][0] >= b.boxes[0][2] - 0.01);
    // The second line is 20 points lower.
    assert!((b.boxes[12][1] - b.boxes[0][1] - 20.0).abs() < 0.01);
    // The nearest character to a point on the 'W' of World is the 'W'.
    let w = b.boxes[6];
    assert_eq!(nearest_char(&b.boxes, (w[0] + w[2]) / 2.0, (w[1] + w[3]) / 2.0), Some(6));
    // No such page.
    assert_eq!(boxes_of(&engine, 2, 5).status, TextStatus::Failed);
}

#[test]
fn a_page_turned_by_rotate_has_its_boxes_on_the_page_as_shown() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>");
    b.stream_obj(4, "", b"BT /F1 12 Tf 72 700 Td (Turned) Tj ET");
    b.obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    let engine = Engine::start(Box::new(|| {}));
    let o = open(&engine, b.finish_classic(6, "/Root 1 0 R"));
    assert_eq!(o.pages[0], PageInfo { width: 792.0, height: 612.0 });
    let boxes = boxes_of(&engine, 1, 0).boxes;
    // Turned a quarter clockwise, the user space point (x, y) is shown at (y, x) from the top left: the text starts near
    // (700, 72) and runs down the page.
    let first = boxes[0];
    assert!(first[0] > 690.0 && first[0] < 702.0 && first[1] > 70.0 && first[1] < 74.0, "{first:?}");
    assert!(boxes[1][1] > first[1], "the next letter is lower down the page");
}

#[test]
fn copied_text_is_what_the_text_command_gives_for_the_same_pages() {
    let engine = Engine::start(Box::new(|| {}));
    let bytes = text_pdf(&["Hello World|second line", "nothing here", "HELLO again"]);
    let doc = crate::Document::from_bytes(bytes.clone()).expect("opens");
    let mut ex = text::TextExtractor::new(&doc);
    let pages = doc.pages().expect("pages");
    let all: Vec<String> = pages.iter().map(|p| ex.page_text(p).expect("text")).collect();
    open(&engine, bytes);
    // A piece of a line, in either order of the two ends.
    assert_eq!(copied(&engine, 1, (0, 0), (0, 5)), (TextStatus::Ok, "Hello".to_string(), false));
    assert_eq!(copied(&engine, 2, (0, 5), (0, 0)).1, "Hello");
    // From the middle of the first page to the middle of the second: the rest of a page and the start of the next.
    let (_, text_ab, truncated) = copied(&engine, 3, (0, 6), (1, 7));
    assert_eq!((text_ab.as_str(), truncated), ("World\nsecond line\nnothing", false));
    // All of every page is exactly the pages' text.
    let (_, everything, _) = copied(&engine, 4, (0, 0), (2, u32::MAX));
    assert_eq!(everything, all.concat());
    // A place past the end of the page's text is the end.
    assert_eq!(copied(&engine, 5, (1, 8), (1, 1000)).1, "here\n");
}

#[test]
fn a_copy_is_cut_at_the_most_pages() {
    let many: Vec<&str> = vec!["abc"; MAX_COPY_PAGES + 20];
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, text_pdf(&many));
    let (status, text, truncated) = copied(&engine, 1, (0, 0), (u32::try_from(many.len()).expect("len") - 1, u32::MAX));
    assert_eq!(status, TextStatus::Ok);
    assert!(truncated);
    assert_eq!(text, "abc\n".repeat(MAX_COPY_PAGES));
}

fn opened_file(engine: &Engine, doc: u64, name: &str, password: &str) -> Opened {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/encrypted/qpdf-generated/");
    engine.open_file(doc, &format!("{dir}{name}"), password, TOTAL_BYTES, 1920 * 1080);
    match engine.wait(10_000) {
        Some(Event::Opened(o)) => o,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_that_does_not_allow_copying_gives_no_text_and_one_that_does_not_allow_printing_does_not_print() {
    let engine = Engine::start(Box::new(|| {}));
    // R2, "no modifying or extracting": opened with no password, it may be printed but not copied.
    let o = opened_file(&engine, 1, "two.r2-40-empty-modify-extract-none.pdf", "");
    assert_eq!(o.rights, Rights { copy: false, print: true, print_high_quality: true });
    assert_eq!(boxes_of(&engine, 1, 0).status, TextStatus::Denied);
    let (status, text, _) = copied(&engine, 2, (0, 0), (0, 100));
    assert_eq!((status, text.as_str()), (TextStatus::Denied, ""));
    // Looking for words is not taking the text out: it works.
    let (pages, ..) = search_all(&engine, 3, "hello", 0);
    assert!(!pages.is_empty());
    // With the owner password everything is allowed.
    let o = opened_file(&engine, 2, "two.r2-40-empty-modify-extract-none.pdf", "owner");
    assert!(o.rights.copy && o.rights.print);
    // Printing is refused for a file that does not allow it.
    let o = opened_file(&engine, 3, "utf8.r3-128-rc4-empty-print-none.pdf", "");
    assert!(!o.rights.print && o.rights.copy);
    engine.print(3, 9, vec![PrintPage { page: 0, dpi: 100.0, rotation: 0 }]);
    match until(&engine, |e| matches!(e, Event::PrintDone { .. }).then_some(e)) {
        Event::PrintDone { status: PrintStatus::Failed, message, .. } => assert!(message.contains("printing"), "{message}"),
        other => panic!("{other:?}"),
    }
    // Everything denied, opened with the user password; the owner password lifts it.
    let o = opened_file(&engine, 4, "two.r6-aes256-user-everything-denied.pdf", "user");
    assert_eq!(o.rights, Rights { copy: false, print: false, print_high_quality: false });
    let o = opened_file(&engine, 5, "two.r6-aes256-user-everything-denied.pdf", "owner");
    assert_eq!(o.rights, Rights { copy: true, print: true, print_high_quality: true });
}

#[test]
fn a_search_that_is_cancelled_before_it_starts_says_nothing() {
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(Some(gate_rx));
    let engine = Engine::start(Box::new(move || {
        if let Some(rx) = gate.lock().expect("lock").take() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }));
    engine.open_bytes(1, text_pdf(&["abc", "abc", "abc"]), "", TOTAL_BYTES, 1920 * 1080);
    engine.search(1, 5, "abc", 0);
    engine.search(1, 6, "abc", 0);
    engine.cancel(5);
    gate_tx.send(()).expect("release");
    assert!(matches!(engine.wait(10_000), Some(Event::Opened(_))));
    let mut ids = Vec::new();
    while let Some(e) = engine.wait(500) {
        match e {
            Event::SearchHits { id, .. } | Event::SearchDone { id, .. } => ids.push(id),
            Event::SearchProgress { .. } => {}
            other => panic!("{other:?}"),
        }
    }
    assert!(!ids.is_empty() && ids.iter().all(|&i| i == 6), "{ids:?}");
}

#[test]
fn drawing_goes_on_between_the_pages_of_a_search_and_a_cancelled_search_stops() {
    let pages: Vec<String> = (0..400).map(|i| format!("page {i} has a word|and another line")).collect();
    let refs: Vec<&str> = pages.iter().map(String::as_str).collect();
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, text_pdf(&refs));
    engine.search(1, 1, "another", 0);
    // Drawing is asked for while the search runs and comes back before it ends.
    engine.render(request(2, 3, 0));
    let mut seen = 0;
    let drawn_after = until(&engine, |e| match e {
        Event::Rendered(r) if r.id == 2 => Some(seen),
        Event::SearchHits { .. } => {
            seen += 1;
            None
        }
        _ => None,
    });
    assert!(drawn_after < 400, "drawn after {drawn_after} pages of hits: not before the search was over");
    engine.cancel(1);
    // Whatever is told after the cancel is only what was ready; then nothing more.
    let mut after = 0;
    let mut done = false;
    while let Some(e) = engine.wait(300) {
        match e {
            Event::SearchHits { .. } => after += 1,
            Event::SearchDone { .. } => done = true,
            _ => {}
        }
    }
    assert!(done || drawn_after + after < 400, "a cancelled search went on to the end: {drawn_after} + {after}");
}

#[test]
fn printing_sends_a_page_in_bands_one_at_a_time() {
    let engine = Engine::start(Box::new(|| {}));
    let o = open(&engine, text_pdf(&["Hello World|second line", "second page"]));
    // A page at 300 dpi is 2550 by 3300 pixels: more than a canvas holds, so it comes in bands as wide as the page.
    let rows_per_band = u64::from(o.max_tile_pixels) / 2550;
    assert!(rows_per_band < 3300);
    engine.print(1, 4, vec![PrintPage { page: 0, dpi: 1000.0, rotation: 0 }, PrintPage { page: 1, dpi: 72.0, rotation: 90 }]);
    let mut next_y = 0u32;
    let mut index = 0u32;
    let mut bands = 0;
    let mut painted = false;
    loop {
        match until(&engine, |e| matches!(e, Event::PrintBand(_) | Event::PrintDone { .. }).then_some(e)) {
            Event::PrintBand(b) => {
                assert_eq!((b.id, b.index, b.y), (4, index, next_y));
                if index == 0 {
                    // 1000 dpi was asked for; 300 is the most.
                    assert_eq!((b.page_width, b.page_height, b.width), (2550, 3300, 2550));
                    assert_eq!(b.dpi, 300.0);
                    assert!(u64::from(b.height) <= rows_per_band);
                    painted |= b.bgra.as_chunks::<4>().0.iter().any(|p| p[..3] != [255, 255, 255]);
                } else {
                    // Page 1 turned a quarter: 792 by 612 pixels at 72 dpi, in one band.
                    assert_eq!((b.page, b.page_width, b.page_height, b.height), (1, 792, 612, 612));
                    assert_eq!(b.dpi, 72.0);
                }
                assert_eq!(b.bgra.len(), (b.width * b.height * 4) as usize);
                bands += 1;
                next_y += b.height;
                if next_y >= b.page_height {
                    next_y = 0;
                    index += 1;
                }
                // Until this band is used, no other is drawn.
                assert!(engine.wait(150).is_none());
                engine.print_next(4);
            }
            Event::PrintDone { status, pages_done, .. } => {
                assert_eq!((status, pages_done), (PrintStatus::Finished, 2));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(bands >= 3 && painted, "{bands} bands, painted: {painted}");
}

/// A 3-page document with bookmarks: the outline is object 30, `items` are further objects.
fn outline_pdf(extra_catalog: &str, items: &[(u32, &str)]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, &format!("<< /Type /Catalog /Pages 2 0 R /Outlines 30 0 R {extra_catalog} >>"));
    b.obj(2, "<< /Type /Pages /Kids [10 0 R 12 0 R 14 0 R] /Count 3 >>");
    for i in 0..3u32 {
        let extra = if i == 2 { "/Rotate 90" } else { "" };
        b.obj(10 + 2 * i, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] {extra} /Contents {} 0 R >>", 11 + 2 * i));
        b.stream_obj(11 + 2 * i, "", b"");
    }
    let mut top = 30;
    for (n, body) in items {
        b.obj(*n, body);
        top = top.max(*n);
    }
    b.finish_classic(top + 1, "/Root 1 0 R")
}

fn outline_of(bytes: Vec<u8>) -> (Vec<OutlineItem>, bool) {
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, bytes);
    engine.outline(1, 3);
    until(&engine, |e| match e {
        Event::Outline { id: 3, items, truncated, .. } => Some((items, truncated)),
        _ => None,
    })
}

#[test]
fn bookmarks_come_in_display_order_with_depth_titles_and_places() {
    let (items, truncated) = outline_of(outline_pdf(
        "/Names << /Dests << /Names [(chap) [14 0 R /FitH 30]] >> >>",
        &[
            (30, "<< /Type /Outlines /First 31 0 R /Last 33 0 R /Count 4 >>"),
            (31, "<< /Title (One) /Parent 30 0 R /Next 32 0 R /First 34 0 R /Last 35 0 R /Count 2 /Dest [10 0 R /XYZ 20 80 0] >>"),
            (34, "<< /Title <FEFF4E2D6587> /Parent 31 0 R /Next 35 0 R /Dest [12 0 R /Fit] >>"),
            (35, "<< /Title (Deep\\311) /Parent 31 0 R /A << /S /GoTo /D (chap) >> >>"),
            (32, "<< /Title (Two) /Parent 30 0 R /Next 33 0 R /Count -1 /First 36 0 R /Last 36 0 R >>"),
            (36, "<< /Title (Hidden child) /Parent 32 0 R /Dest [99 0 R /Fit] >>"),
            // The last item names the first as its next: a loop, which ends where it comes back.
            (33, "<< /Title (Three) /Parent 30 0 R /Next 31 0 R /A << /S /URI /URI (https://example.com) >> >>"),
        ],
    ));
    assert!(!truncated);
    let got: Vec<(String, u32, Option<u32>, bool)> = items.iter().map(|i| (i.title.clone(), i.depth, i.page, i.open)).collect();
    assert_eq!(
        got,
        vec![
            ("One".to_string(), 0, Some(0), true),
            ("中文".to_string(), 1, Some(1), false),
            ("Deep\u{c9}".to_string(), 1, Some(2), false),
            ("Two".to_string(), 0, None, false),
            ("Hidden child".to_string(), 1, None, false),
            ("Three".to_string(), 0, None, false),
        ]
    );
    // /XYZ 20 80: 80 points up a page 100 high is 20 points from the top.
    assert_eq!(items[0].y, Some(20.0));
    assert_eq!(items[1].y, None);
    // /FitH 30 on a page turned a quarter: the top is in x there, so the place shown is 30 points from the top.
    assert!(items[2].y.is_some());
}

#[test]
fn bookmarks_are_capped_in_number_and_depth_and_loops_end() {
    // A chain of children 40 deep: cut at the depth limit.
    let mut items: Vec<(u32, String)> = vec![(30, "<< /Type /Outlines /First 100 0 R /Last 100 0 R /Count 1 >>".to_string())];
    for i in 0..40u32 {
        let child = if i < 39 { format!("/First {} 0 R /Last {} 0 R", 101 + i, 101 + i) } else { String::new() };
        items.push((100 + i, format!("<< /Title (d{i}) /Parent 30 0 R {child} /Dest [10 0 R /Fit] >>")));
    }
    let refs: Vec<(u32, &str)> = items.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let (got, truncated) = outline_of(outline_pdf("", &refs));
    assert_eq!(got.len(), MAX_OUTLINE_DEPTH);
    assert_eq!(got.last().map(|i| i.depth as usize), Some(MAX_OUTLINE_DEPTH - 1));
    assert!(truncated);

    // A very long row of items: cut at the number limit.
    let n = MAX_OUTLINE_ITEMS as u32 + 500;
    let mut items: Vec<(u32, String)> = vec![(30, format!("<< /Type /Outlines /First 100 0 R /Last {} 0 R >>", 99 + n))];
    for i in 0..n {
        let next = if i + 1 < n { format!("/Next {} 0 R", 101 + i) } else { String::new() };
        items.push((100 + i, format!("<< /Title (item {i}) /Parent 30 0 R {next} >>")));
    }
    let refs: Vec<(u32, &str)> = items.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let (got, truncated) = outline_of(outline_pdf("", &refs));
    assert_eq!(got.len(), MAX_OUTLINE_ITEMS);
    assert!(truncated);

    // An item that is its own child, and one that is its own next.
    let (got, _) = outline_of(outline_pdf(
        "",
        &[
            (30, "<< /Type /Outlines /First 31 0 R /Last 32 0 R >>"),
            (31, "<< /Title (A) /Parent 30 0 R /First 31 0 R /Next 32 0 R >>"),
            (32, "<< /Title (B) /Parent 30 0 R /Next 32 0 R >>"),
        ],
    ));
    assert_eq!(got.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), ["A", "B"]);

    // A very long title is cut; no bookmarks gives an empty list.
    let long = format!("<< /Title ({}) /Parent 30 0 R >>", "x".repeat(100_000));
    let (got, _) = outline_of(outline_pdf("", &[(30, "<< /Type /Outlines /First 31 0 R /Last 31 0 R >>"), (31, long.as_str())]));
    assert_eq!(got[0].title.chars().count(), MAX_TITLE_CHARS);
    let (got, truncated) = outline_of(pdf(1, square));
    assert!(got.is_empty() && !truncated);
}

#[test]
fn links_lead_to_places_in_the_file_or_to_safe_addresses_only() {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [10 0 R 12 0 R] /Count 2 >>");
    b.obj(10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Annots [20 0 R 21 0 R 22 0 R 23 0 R 24 0 R 25 0 R 26 0 R 27 0 R 28 0 R] >>");
    b.obj(12, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Rotate 90 /Annots [20 0 R] >>");
    let link = |rect: &str, rest: &str| format!("<< /Type /Annot /Subtype /Link /Rect [{rect}] {rest} >>");
    b.obj(20, &link("10 10 50 30", "/Dest [12 0 R /XYZ 5 60 0]"));
    b.obj(21, &link("60 10 90 30", "/A << /S /URI /URI (https://example.com/a?b=c) >>"));
    b.obj(22, &link("60 40 90 60", "/A << /S /URI /URI (javascript:alert\\(1\\)) >>"));
    b.obj(23, &link("60 70 90 90", "/A << /S /URI /URI (file:///C:/Windows/System32/calc.exe) >>"));
    b.obj(24, &link("100 10 140 30", "/A << /S /GoTo /D [10 0 R /Fit] >>"));
    b.obj(25, &link("100 40 140 60", "/F 2 /A << /S /URI /URI (https://hidden.example) >>"));
    b.obj(26, "<< /Type /Annot /Subtype /Text /Rect [100 70 140 90] /Contents (a note) >>");
    b.obj(27, &link("150 10 150 30", "/A << /S /URI /URI (https://flat.example) >>"));
    b.obj(28, &link("150 40 190 60", "/A 29 0 R"));
    b.obj(29, "<< /S /URI /URI (mailto:me@example.com) >>");
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, b.finish_classic(30, "/Root 1 0 R"));
    engine.links(1, 5, 0);
    let (links, truncated) = until(&engine, |e| match e {
        Event::Links { id: 5, page: 0, links, truncated, .. } => Some((links, truncated)),
        _ => None,
    });
    assert!(!truncated);
    let summary: Vec<(LinkKind, u32, &str)> = links.iter().map(|l| (l.kind, l.page, l.uri.as_str())).collect();
    assert_eq!(
        summary,
        [(LinkKind::Page, 1, ""), (LinkKind::Uri, 0, "https://example.com/a?b=c"), (LinkKind::Page, 0, ""), (LinkKind::Uri, 0, "mailto:me@example.com")]
    );
    // The rectangle is on the page as shown: 10..50 across, 30..10 up a page 100 high is 70..90 from the top.
    assert_eq!(links[0].rect, [10.0, 70.0, 50.0, 90.0]);
    // The destination is on page 1, which is turned a quarter: its top (60 up) is across, 5 from the left, so 5 down.
    assert_eq!(links[0].y, Some(5.0));
    // On the page turned a quarter, the same rectangle is turned with it: (x, y) is shown at (y, x).
    engine.links(1, 6, 1);
    let turned = until(&engine, |e| match e {
        Event::Links { id: 6, links, .. } => Some(links),
        _ => None,
    });
    assert_eq!(turned[0].rect, [10.0, 10.0, 30.0, 50.0]);
    // A page that is not there has none.
    engine.links(1, 7, 9);
    let none = until(&engine, |e| match e {
        Event::Links { id: 7, links, .. } => Some(links),
        _ => None,
    });
    assert!(none.is_empty());
}

#[test]
fn a_page_that_asks_for_more_work_than_its_meter_holds_ends_with_a_limit_and_a_cancelled_one_with_cancelled() {
    use crate::render::work::Work;
    let doc = crate::Document::from_bytes(text_pdf(&["a long enough line of text to cost something"])).expect("opens");
    let pages = doc.pages().expect("pages");
    let mut ex = text::TextExtractor::new(&doc);
    assert!(ex.page_chars(&pages[0], &Work::new()).is_ok());
    assert!(matches!(ex.page_chars(&pages[0], &Work::with_allowance(3_000.0)), Err(Error::Limit(_))));
    let flag = Arc::new(AtomicBool::new(true));
    assert!(matches!(ex.page_chars(&pages[0], &Work::new().with_cancel(Some(flag))), Err(Error::Cancelled)));
    // The same page is read again afterwards.
    let again = ex.page_chars(&pages[0], &Work::new()).expect("text");
    assert_eq!(again.text, "a long enough line of text to cost something\n");
    assert_eq!(again.boxes.len(), again.text.chars().count());
}

/// The units the meter charges for the text of whole documents (the text, the boxes, the folding and a search), against the
/// time it took: they are meant to be about a nanosecond a unit (see `render::work::cost`).
#[test]
fn the_meter_charges_the_text_of_a_document_about_what_it_takes() {
    use crate::render::work::{Work, cost};
    use std::time::Instant;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let files = [
        root.join("public/zh/gongwen/gongwen-2024-nicheng-work-report.pdf"),
        root.join("public/zh/lunwen/lunwen-arxiv-2601.14329-latex.pdf"),
        root.join("local/zh/ebook/ebook-wikisource-yijikao-468p.pdf"),
    ];
    let needle = search::fold_query("的");
    for path in files.iter().filter(|p| p.is_file()) {
        let doc = crate::Document::open(path).expect("opens");
        let pages = doc.pages().expect("pages");
        let mut ex = text::TextExtractor::new(&doc);
        let (mut units, mut chars) = (0.0f64, 0usize);
        let began = Instant::now();
        for page in &pages {
            let work = Work::new();
            let Ok(c) = ex.page_chars(page, &work) else { continue };
            let (hay, _) = search::fold(&c.text);
            assert!(work.charge(hay.len() as f64 * cost::SEARCH_CHAR));
            let _ = search::find_all(&hay, &needle, 2000, &mut |n| work.charge(n as f64 * cost::SEARCH_STEP));
            units += work.spent();
            chars += c.text.chars().count();
        }
        let nanos = began.elapsed().as_nanos() as f64;
        let ratio = units / nanos;
        println!("{}: {} pages, {chars} characters, {:.1} ms, {:.1} Munits, units per ns {ratio:.2}", path.file_name().unwrap().to_string_lossy(), pages.len(), nanos / 1e6, units / 1e6);
        if !cfg!(debug_assertions) {
            assert!((0.2..=6.0).contains(&ratio), "{ratio}");
        }
    }
}

// --- the review of 3d-2: shared big objects, the order of work, the names of destinations ----------------------------------

/// The longest a request for a thousand times shared megabyte may take here (it took 45 seconds before the objects were charged).
const SHARED_BIG_LIMIT: Duration = Duration::from_secs(5);

#[test]
fn twenty_thousand_bookmarks_that_share_one_megabyte_title_are_cheap() {
    let n = 20_000u32;
    let mut items: Vec<(u32, String)> = vec![(30, format!("<< /Type /Outlines /First 100 0 R /Last {} 0 R /Count {n} >>", 99 + n))];
    items.push((40, format!("({})", "a".repeat(1_000_000))));
    for i in 0..n {
        let next = if i + 1 < n { format!("/Next {} 0 R", 101 + i) } else { String::new() };
        items.push((100 + i, format!("<< /Title 40 0 R /Parent 30 0 R {next} /Dest [10 0 R /Fit] >>")));
    }
    let refs: Vec<(u32, &str)> = items.iter().map(|(n, s)| (*n, s.as_str())).collect();
    let started = std::time::Instant::now();
    let (got, truncated) = outline_of(outline_pdf("", &refs));
    assert!(started.elapsed() < SHARED_BIG_LIMIT, "{:?}", started.elapsed());
    // The titles are read (cut to the limit); the item limit is the only thing that could have cut the list.
    assert_eq!(got.len(), n as usize);
    assert!(!truncated);
    assert_eq!(got[0].title.chars().count(), MAX_TITLE_CHARS);
    assert_eq!(got[0].page, Some(0));
}

#[test]
fn twenty_thousand_links_that_share_one_megabyte_address_or_rectangle_are_cheap() {
    let n = 20_000u32;
    let annots: String = (0..n).map(|i| format!("{} 0 R ", 100 + i)).collect();
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [10 0 R 12 0 R] /Count 2 >>");
    b.obj(10, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Annots [{annots}] >>"));
    b.obj(12, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Annots [{annots}] >>"));
    b.obj(20, "<< /S /URI /URI 21 0 R >>");
    b.obj(21, &format!("(http://example.com/{})", "a".repeat(1_000_000)));
    b.obj(22, &format!("[{}]", "1 ".repeat(250_000)));
    for i in 0..n {
        // Page 0: the rectangle is fine and the address is the big shared string; page 1 shares the big array as the rectangle.
        let rect = if i % 2 == 0 { "[10 10 100 20]" } else { "22 0 R" };
        b.obj(100 + i, &format!("<< /Type /Annot /Subtype /Link /Rect {rect} /A 20 0 R >>"));
    }
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, b.finish_classic(100 + n, "/Root 1 0 R"));
    for (id, page) in [(5u64, 0u32), (6, 1)] {
        let started = std::time::Instant::now();
        engine.links(1, id, page);
        let links = until(&engine, |e| match e {
            Event::Links { id: i, links, .. } if i == id => Some(links),
            _ => None,
        });
        assert!(started.elapsed() < SHARED_BIG_LIMIT, "page {page}: {:?}", started.elapsed());
        // The address is longer than an address may be, and an array of that many numbers is not a rectangle.
        assert!(links.is_empty(), "page {page}");
    }
}

#[test]
fn the_quick_requests_go_after_what_is_in_view_and_before_the_rest_of_the_drawing() {
    // The first event holds the engine thread until all the requests are in.
    let order = |priority: u32| {
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate = std::sync::Mutex::new(Some(gate_rx));
        let engine = Engine::start(Box::new(move || {
            if let Some(rx) = gate.lock().expect("lock").take() {
                let _ = rx.recv_timeout(Duration::from_secs(10));
            }
        }));
        engine.open_bytes(1, pdf(2, square), "", TOTAL_BYTES, 1920 * 1080);
        engine.links(1, 5, 0);
        engine.render(request(6, 1, priority));
        gate_tx.send(()).expect("release");
        let mut seen = Vec::new();
        while seen.len() < 2 {
            match engine.wait(10_000) {
                Some(Event::Links { id, .. }) => seen.push(id),
                Some(Event::Rendered(r)) => seen.push(r.id),
                Some(_) => {}
                None => panic!("the engine said nothing for ten seconds"),
            }
        }
        seen
    };
    assert_eq!(order(10), [6, 5], "a piece in view is drawn before the links");
    assert_eq!(order(BACKGROUND_PRIORITY), [5, 6], "the links come before a piece nobody is looking at yet");
}

#[test]
fn the_named_destinations_are_charged_by_their_entries_and_a_cancel_stops_the_reading() {
    use crate::document::Document;
    use crate::render::work::Work;
    use super::targets::Names;
    let entries: String = (0..5_000).map(|i| format!("(k{i}) [10 0 R /Fit] ")).collect();
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /Dests 3 0 R >> >>");
    b.obj(2, "<< /Type /Pages /Kids [10 0 R] /Count 1 >>");
    b.obj(3, &format!("<< /Names [{entries}] >>"));
    b.obj(10, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>");
    let doc = Document::from_bytes(b.finish_classic(11, "/Root 1 0 R")).expect("a document");
    // A meter that cannot pay for the entries: the file has no names (for good, whatever meter comes later).
    let names = Names::new(&doc);
    assert!(names.get(&Work::with_allowance(100_000.0)).is_none());
    assert!(names.get(&Work::new()).is_none());
    // A meter that is cancelled stops the reading without a verdict: the next ask reads them.
    let names = Names::new(&doc);
    let flag = Arc::new(AtomicBool::new(true));
    let work = Work::new().with_cancel(Some(flag));
    assert!(names.get(&work).is_none() && work.was_cancelled());
    assert_eq!(names.get(&Work::new()).map(|n| n.tree().len()), Some(5_000));
}

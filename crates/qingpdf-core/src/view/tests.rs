use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::testutil::PdfBuilder;

/// A document of `pages` pages, 200 by 100 points; page `i` has a red square at (10 * i, 0) and a rotated page 2.
pub(super) fn pdf(pages: u32, content_of: impl Fn(u32) -> String) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 10 + 2 * i)).collect();
    b.obj(2, &format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));
    for i in 0..pages {
        let extra = if i == 2 { "/Rotate 90" } else { "" };
        b.obj(10 + 2 * i, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] {extra} /Contents {} 0 R >>", 11 + 2 * i));
        b.stream_obj(11 + 2 * i, "", content_of(i).as_bytes());
    }
    b.finish_classic(10 + 2 * pages, "/Root 1 0 R")
}

pub(super) fn square(i: u32) -> String {
    format!("1 0 0 rg {} 0 20 20 re f", 10 * i)
}

pub(super) fn request(id: u64, page: u32, priority: u32) -> RenderRequest {
    RenderRequest { doc: 1, id, page, dpi: 72.0, rotation: 0, x: 0, y: 0, width: 0, height: 0, priority }
}

pub(super) fn open(engine: &Engine, bytes: Vec<u8>) -> Opened {
    engine.open_bytes(1, bytes, "", TOTAL_BYTES, 1920 * 1080);
    match engine.wait(10_000) {
        Some(Event::Opened(o)) => o,
        other => panic!("not opened: {other:?}"),
    }
}

fn rendered(engine: &Engine) -> Rendered {
    match engine.wait(10_000) {
        Some(Event::Rendered(r)) => r,
        other => panic!("not rendered: {other:?}"),
    }
}

/// The pixel (x, y) of a BGRA bitmap, as [r, g, b].
fn px(r: &Rendered, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * r.width + x) * 4) as usize;
    [r.bgra[i + 2], r.bgra[i + 1], r.bgra[i]]
}

#[test]
fn a_file_is_opened_and_its_page_sizes_are_told() {
    let engine = Engine::start(Box::new(|| {}));
    let o = open(&engine, pdf(3, square));
    assert_eq!(o.pages.len(), 3);
    assert_eq!(o.pages[0], PageInfo { width: 200.0, height: 100.0 });
    // /Rotate 90 turns the page: the sizes are those of the page as shown.
    assert_eq!(o.pages[2], PageInfo { width: 100.0, height: 200.0 });
    assert!(o.bitmap_cache_bytes > 8_000_000 && o.bitmap_cache_bytes < TOTAL_BYTES);
    assert!(o.file_bytes > 100);
}

#[test]
fn a_page_comes_back_as_bgra_and_a_region_is_the_same_piece_of_it() {
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, pdf(2, square));
    engine.render(request(1, 1, 0));
    let whole = rendered(&engine);
    assert_eq!((whole.id, whole.page, whole.status, whole.width, whole.height), (1, 1, RenderStatus::Complete, 200, 100));
    assert_eq!(whole.bgra.len(), 200 * 100 * 4);
    // Page 1 has its square at x 10..30, the bottom of the page (user space has y up). Blue is the first byte.
    assert_eq!(px(&whole, 20, 90), [255, 0, 0]);
    assert_eq!(&whole.bgra[((90 * 200 + 20) * 4) as usize..][..4], &[0, 0, 255, 255]);
    assert_eq!(px(&whole, 50, 90), [255, 255, 255]);
    // A piece of it: 40 by 30 pixels from (5, 75).
    engine.render(RenderRequest { x: 5, y: 75, width: 40, height: 30, ..request(2, 1, 0) });
    let piece = rendered(&engine);
    assert_eq!((piece.width, piece.height), (40, 25), "cut to the page");
    for y in 0..25 {
        for x in 0..40 {
            assert_eq!(px(&piece, x, y), px(&whole, x + 5, y + 75), "({x}, {y})");
        }
    }
    // Zoomed to 200 percent, the same piece of the bigger page.
    engine.render(RenderRequest { dpi: 144.0, x: 10, y: 150, width: 100, height: 50, ..request(3, 1, 0) });
    let big = rendered(&engine);
    assert_eq!((big.width, big.height), (100, 50));
    assert_eq!(px(&big, 30, 40), [255, 0, 0]);
    // Turned a further 90 degrees: the page is 100 by 200.
    engine.render(RenderRequest { rotation: 90, ..request(4, 1, 0) });
    let turned = rendered(&engine);
    assert_eq!((turned.width, turned.height), (100, 200));
}

#[test]
fn the_most_urgent_request_is_drawn_first_and_equal_ones_in_order() {
    // The first event holds the engine thread back until the requests are in.
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(Some(gate_rx));
    let engine = Engine::start(Box::new(move || {
        if let Some(rx) = gate.lock().expect("lock").take() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }));
    engine.open_bytes(1, pdf(5, square), "", TOTAL_BYTES, 1920 * 1080);
    // (id, page, priority)
    for (id, page, priority) in [(1, 0, 50), (2, 1, 10), (3, 2, 10), (4, 3, 99), (5, 4, 0)] {
        engine.render(request(id, page, priority));
    }
    gate_tx.send(()).expect("release");
    assert!(matches!(engine.wait(10_000), Some(Event::Opened(_))));
    let order: Vec<u64> = (0..5).map(|_| rendered(&engine).id).collect();
    assert_eq!(order, [5, 2, 3, 1, 4]);
}

#[test]
fn a_request_that_is_cancelled_before_it_starts_gets_no_result() {
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let gate = std::sync::Mutex::new(Some(gate_rx));
    let engine = Engine::start(Box::new(move || {
        if let Some(rx) = gate.lock().expect("lock").take() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }));
    engine.open_bytes(1, pdf(3, square), "", TOTAL_BYTES, 1920 * 1080);
    for id in 1..=3 {
        engine.render(request(id, id as u32 - 1, 0));
    }
    engine.cancel(2);
    gate_tx.send(()).expect("release");
    assert!(matches!(engine.wait(10_000), Some(Event::Opened(_))));
    assert_eq!(rendered(&engine).id, 1);
    assert_eq!(rendered(&engine).id, 3);
    assert!(engine.wait(200).is_none());
}

#[test]
fn a_page_that_is_being_drawn_can_be_stopped() {
    // A page of 400,000 fills: it takes a while to draw, long enough to be stopped.
    let heavy = pdf(2, |i| if i == 0 { "0 0 1 rg 0 0 150 80 re f ".repeat(400_000) } else { square(i) });
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, heavy);
    engine.render(request(1, 0, 0));
    engine.render(request(2, 1, 5));
    std::thread::sleep(Duration::from_millis(20));
    engine.cancel(1);
    // Page 0 gives no result (or, on a machine fast enough to have finished it, the finished one comes first).
    let first = rendered(&engine);
    let second = if first.id == 1 { rendered(&engine) } else { first };
    assert_eq!(second.id, 2);
    assert_eq!(second.status, RenderStatus::Complete);
    assert!(engine.wait(200).is_none());
}

#[test]
fn requests_for_another_document_or_a_missing_page_are_answered_or_dropped() {
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, pdf(1, square));
    engine.render(RenderRequest { doc: 7, ..request(1, 0, 0) });
    engine.render(request(2, 9, 0));
    let failed = rendered(&engine);
    assert_eq!((failed.id, failed.status, failed.bgra.len()), (2, RenderStatus::Failed, 0));
    assert!(failed.message.contains("no such page"));
    engine.render(RenderRequest { dpi: 5000.0, ..request(3, 0, 0) });
    let bad = rendered(&engine);
    assert_eq!(bad.status, RenderStatus::Failed);
    assert!(engine.wait(100).is_none());
}

#[test]
fn a_file_that_cannot_be_opened_says_why() {
    let engine = Engine::start(Box::new(|| {}));
    engine.open_bytes(4, b"this is not a PDF file".to_vec(), "", TOTAL_BYTES, 1920 * 1080);
    match engine.wait(10_000) {
        Some(Event::OpenFailed { doc: 4, failure: OpenFailure::Damaged, .. }) => {}
        other => panic!("{other:?}"),
    }
    engine.open_bytes(5, pdf(1, square), "", 1000, 0);
    match engine.wait(10_000) {
        Some(Event::OpenFailed { doc: 5, failure: OpenFailure::TooLarge, .. }) => {}
        other => panic!("{other:?}"),
    }
    engine.open_file(6, "/this/file/does/not/exist.pdf", "", TOTAL_BYTES, 1920 * 1080);
    match engine.wait(10_000) {
        Some(Event::OpenFailed { doc: 6, failure: OpenFailure::Unreadable, .. }) => {}
        other => panic!("{other:?}"),
    }
    // A good file still opens afterwards.
    assert_eq!(open(&engine, pdf(1, square)).pages.len(), 1);
}

#[test]
fn an_encrypted_file_asks_for_its_password_and_takes_it_once() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/corpus/public/encrypted/qpdf-generated/bookmarks.r2-40-user.pdf");
    let engine = Engine::start(Box::new(|| {}));
    engine.open_file(1, path, "", TOTAL_BYTES, 1920 * 1080);
    assert!(matches!(engine.wait(10_000), Some(Event::OpenFailed { failure: OpenFailure::NeedsPassword, .. })));
    engine.open_file(2, path, "wrong", TOTAL_BYTES, 1920 * 1080);
    assert!(matches!(engine.wait(10_000), Some(Event::OpenFailed { failure: OpenFailure::WrongPassword, .. })));
    engine.open_file(3, path, "user", TOTAL_BYTES, 1920 * 1080);
    let opened = match engine.wait(10_000) {
        Some(Event::Opened(o)) => o,
        other => panic!("{other:?}"),
    };
    assert!(!opened.pages.is_empty());
    engine.render(RenderRequest { doc: 3, ..request(1, 0, 0) });
    assert_eq!(rendered(&engine).status, RenderStatus::Complete);
}

#[test]
fn a_new_document_replaces_the_old_one_and_its_requests() {
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, pdf(3, square));
    engine.open_bytes(2, pdf(1, square), "", TOTAL_BYTES, 1920 * 1080);
    match engine.wait(10_000) {
        Some(Event::Opened(o)) => assert_eq!((o.doc, o.pages.len()), (2, 1)),
        other => panic!("{other:?}"),
    }
    engine.render(request(1, 0, 0));
    assert!(engine.wait(200).is_none(), "doc 1 is closed");
    engine.render(RenderRequest { doc: 2, ..request(2, 0, 0) });
    assert_eq!(rendered(&engine).id, 2);
    engine.close();
    engine.render(RenderRequest { doc: 2, ..request(3, 0, 0) });
    assert!(engine.wait(200).is_none());
}

#[test]
fn opening_a_file_stops_the_request_being_drawn() {
    // A page that takes a long while to draw: 3000 fills of a piece of 2048 by 2048 pixels (some seconds).
    let heavy = pdf(1, |_| "0 0 1 rg 0 0 200 100 re f ".repeat(3000));
    let engine = Engine::start(Box::new(|| {}));
    open(&engine, heavy);
    engine.render(RenderRequest { dpi: 1000.0, x: 0, y: 0, width: 2048, height: 2048, ..request(1, 0, 0) });
    std::thread::sleep(Duration::from_millis(100));
    let started = std::time::Instant::now();
    engine.open_bytes(2, pdf(1, square), "", TOTAL_BYTES, 1920 * 1080);
    // The new file is opened without waiting for the page to be drawn to its end (the page had seconds to go).
    match engine.wait(30_000) {
        Some(Event::Opened(o)) => assert_eq!(o.doc, 2),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    // And the page that was stopped gives no result.
    assert!(engine.wait(200).is_none());
}

#[test]
fn a_request_for_more_pixels_than_the_plan_allows_is_refused() {
    let engine = Engine::start(Box::new(|| {}));
    let o = open(&engine, pdf(1, square));
    let cap = o.max_tile_pixels;
    assert!(cap > 3_000_000 && cap <= 4 << 20, "{cap}");
    let side = (f64::from(cap).sqrt()) as u32;
    // The page is 200 by 100 points: at 1500 dpi 4167 by 2083 pixels, 8.7 million, a bigger canvas than the plan has
    // (though under the 16 million the renderer accepts on its own).
    engine.render(RenderRequest { dpi: 1500.0, ..request(1, 0, 0) });
    let whole = rendered(&engine);
    assert_eq!(whole.status, RenderStatus::Failed);
    assert!(whole.message.contains("most"), "{}", whole.message);
    engine.render(RenderRequest { dpi: 1500.0, x: 0, y: 0, width: side + 1, height: side + 1, ..request(2, 0, 0) });
    assert_eq!(rendered(&engine).status, RenderStatus::Failed);
    engine.render(RenderRequest { dpi: 1500.0, x: 0, y: 0, width: side, height: side, ..request(3, 0, 0) });
    let ok = rendered(&engine);
    assert_eq!((ok.status, ok.width, ok.height), (RenderStatus::Complete, side, side));
}

#[test]
fn a_page_with_a_huge_box_is_told_at_the_limit_and_can_be_drawn() {
    let huge = format!("1{}", "0".repeat(300));
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>");
    b.obj(3, &format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {huge} {huge}] /Contents 4 0 R >>"));
    b.stream_obj(4, "", b"1 0 0 rg 0 0 50 50 re f");
    b.obj(5, "<< /Type /Page /Parent 2 0 R /MediaBox [-1000000000000000000000000000 0 1000000000000000000000000000 20000] >>");
    let bytes = b.finish_classic(6, "/Root 1 0 R");
    let engine = Engine::start(Box::new(|| {}));
    let o = open(&engine, bytes);
    assert_eq!(o.pages[0], PageInfo { width: MAX_PAGE_UNITS, height: MAX_PAGE_UNITS });
    assert_eq!(o.pages[1], PageInfo { width: MAX_PAGE_UNITS, height: 20_000.0_f64.min(MAX_PAGE_UNITS) });
    // A piece of such a page can be asked for, and comes back (it may be blank) or fails; it must not bring the engine down.
    for (id, page) in [(1, 0), (2, 1)] {
        engine.render(RenderRequest { x: 0, y: 0, width: 300, height: 300, ..request(id, page, 0) });
        let r = rendered(&engine);
        assert!(r.status != RenderStatus::Complete || (r.width, r.height) == (300, 300), "{:?}", r.status);
    }
    // The engine still works afterwards.
    assert_eq!(open(&engine, pdf(1, square)).pages.len(), 1);
}

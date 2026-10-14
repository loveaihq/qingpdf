//! `qingpdf watermark`, `stamp` and `addtext` (4c), run as a user would: arguments in, files and exit codes out. What they write is
//! checked the way the engine's tests check it (the original bytes are a prefix, `qpdf --check`, our own reader draws and extracts
//! it); the pictures and files are made here.
// Test code may panic; that is how a test fails.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

#[path = "../../qingpdf-core/tests/common/mod.rs"]
mod common;
mod runner;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use runner::{code, run, s, stderr, stdout};

fn has_chinese_font() -> bool {
    ["msyh.ttc", "simsun.ttc", "simhei.ttf"].iter().any(|f| Path::new("C:/Windows/Fonts").join(f).is_file())
}

/// A PNG of `w` by `h` pixels: a red ring on a transparent ground.
fn ring_png(w: u32, h: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = ((f64::from(x) - f64::from(w) / 2.0) / (f64::from(w) / 2.0), (f64::from(y) - f64::from(h) / 2.0) / (f64::from(h) / 2.0));
            let on = (0.6..=1.0).contains(&dx.hypot(dy));
            rgba.extend_from_slice(&[220, 20, 20, if on { 255 } else { 0 }]);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&rgba).unwrap();
    out
}

/// A file of `pages` pages of 300 by 400 points that share one `/Resources` (a font) and inherit their `/MediaBox`; each page has
/// a content stream of its own.
fn many_pages(pages: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    let mut put = |out: &mut Vec<u8>, n: u32, body: String| {
        offsets.insert(n, out.len());
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    put(&mut out, 1, "<< /Type /Catalog /Pages 2 0 R >>".to_string());
    let kids: String = (0..pages).map(|i| format!("{} 0 R ", 10 + 2 * i)).collect();
    put(&mut out, 2, format!("<< /Type /Pages /Kids [{kids}] /Count {pages} /Resources 3 0 R /MediaBox [0 0 300 400] >>"));
    put(&mut out, 3, "<< /Font << /Helv 4 0 R >> >>".to_string());
    put(&mut out, 4, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());
    for i in 0..pages {
        let text = format!("BT /Helv 14 Tf 30 350 Td (page {}) Tj ET", i + 1);
        put(&mut out, 10 + 2 * i, format!("<< /Type /Page /Parent 2 0 R /Contents {} 0 R >>", 11 + 2 * i));
        put(&mut out, 11 + 2 * i, format!("<< /Length {} >>\nstream\n{text}\nendstream", text.len()));
    }
    let size = 10 + 2 * pages;
    let at = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    for n in 0..size {
        match offsets.get(&n) {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None if n == 0 => out.extend_from_slice(b"0000000000 65535 f \n"),
            None => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }
    out.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
    out
}

fn check_written(input: &Path, output: &Path, password: &str) {
    let before = std::fs::read(input).unwrap();
    let after = std::fs::read(output).unwrap();
    assert!(after.len() > before.len() && after.starts_with(&before), "the bytes of the input are not a prefix of the output");
    if let Some(qpdf) = common::find_qpdf() {
        let verdict = if password.is_empty() { common::qpdf_check(&qpdf, output) } else { common::qpdf_check_pw(&qpdf, output, password) };
        assert!(verdict.code == 0 || verdict.code == 3, "qpdf --check says {}:\n{}", verdict.code, verdict.text);
        assert!(verdict.code == 0, "qpdf warns: {}", verdict.text);
    }
}

fn text_of(file: &Path, page: usize, password: &[&str]) -> String {
    let mut args = vec!["text", s(file), "--pages"];
    let page = page.to_string();
    args.push(&page);
    args.extend_from_slice(password);
    let out = run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    stdout(&out)
}

/// Does a content stream of this page (counting from 1) draw a watermark: marked as an artifact (14.8.2.2)? The text of a
/// watermark is not extracted, so this is how the tests see it.
fn watermarked(file: &Path, page: usize) -> bool {
    let doc = qingpdf_core::Document::open(file).unwrap();
    let page = &doc.pages().unwrap()[page - 1];
    let Ok(qingpdf_core::Object::Dict(dict)) = doc.get(page.obj_ref) else { return false };
    let Some(qingpdf_core::Object::Array(list)) = dict.get("Contents") else { return false };
    list.iter().filter_map(|o| o.as_obj_ref()).any(|r| match doc.get(r) {
        Ok(qingpdf_core::Object::Stream(stream)) => String::from_utf8_lossy(&doc.decode_stream(&stream).unwrap()).contains("/Subtype /Watermark>> BDC"),
        _ => false,
    })
}

fn setup(name: &str) -> (PathBuf, PathBuf) {
    let dir = common::fresh_out_dir(name);
    let input = dir.join("in.pdf");
    std::fs::write(&input, many_pages(3)).unwrap();
    (dir, input)
}

#[test]
fn the_three_commands_have_help_and_wrong_command_lines_exit_with_2() {
    let main = stdout(&run(["--help"]));
    for word in ["watermark", "stamp", "addtext"] {
        assert!(main.contains(word), "main help should mention {word}");
        let out = run([word, "--help"]);
        assert_eq!(code(&out), 0, "{word}");
        assert!(stdout(&out).contains(&format!("qingpdf {word}")), "{word}");
    }
    assert!(stdout(&run(["watermark", "--help"])).contains("--opacity") && stdout(&run(["watermark", "--help"])).contains("--tile"));
    assert!(stdout(&run(["stamp", "--help"])).contains("lower left"));
    let (dir, input) = setup("cli-content-usage");
    let (a, out_pdf, png) = (s(&input).to_string(), s(&dir.join("o.pdf")).to_string(), s(&dir.join("p.png")).to_string());
    for args in [
        vec!["watermark", &a, "-o", &out_pdf],
        vec!["watermark", &a, "--text", "x"],
        vec!["watermark", &a, "--text", "x", "--opacity", "0", "-o", &out_pdf],
        vec!["watermark", &a, "--text", "x", "--opacity", "1.5", "-o", &out_pdf],
        vec!["watermark", &a, "--text", "x", "--opacity", "abc", "-o", &out_pdf],
        vec!["watermark", &a, "--text", "x", "--angle", "NaN", "-o", &out_pdf],
        vec!["watermark", &a, "--text", "x", "--tile=yes", "-o", &out_pdf],
        vec!["watermark", &a, "--text", "x", "--text", "y", "-o", &out_pdf],
        vec!["watermark", &a, "--text", "x", "--width", "9", "-o", &out_pdf],
        vec!["stamp", &a, "--page", "1", "--at", "1,1", "--width", "10", "-o", &out_pdf],
        vec!["stamp", &a, "--image", &png, "--at", "1,1", "--width", "10", "-o", &out_pdf],
        vec!["stamp", &a, "--image", &png, "--page", "0", "--at", "1,1", "--width", "10", "-o", &out_pdf],
        vec!["stamp", &a, "--image", &png, "--page", "1", "--at", "1", "--width", "10", "-o", &out_pdf],
        vec!["stamp", &a, "--image", &png, "--page", "1", "--at", "1,x", "--width", "10", "-o", &out_pdf],
        vec!["stamp", &a, "--image", &png, "--page", "1", "--at", "1,1", "-o", &out_pdf],
        vec!["stamp", &a, "--image", &png, "--page", "1", "--at", "1,1", "--width", "-5", "-o", &out_pdf],
        vec!["stamp", &a, "--image", &png, "--page", "1", "--at", "1,1", "--width", "10", "--tile", "-o", &out_pdf],
        vec!["addtext", &a, "--page", "1", "--at", "1,1", "--text", "x", "-o", &out_pdf],
        vec!["addtext", &a, "--page", "1", "--at", "1,1", "--size", "12", "-o", &out_pdf],
        vec!["addtext", &a, "--at", "1,1", "--size", "12", "--text", "x", "-o", &out_pdf],
        vec!["addtext", &a, "--page", "1", "--size", "12", "--text", "x", "-o", &out_pdf],
        vec!["addtext", &a, "--page", "1", "--at", "1,1", "--size", "12", "--text", " ", "-o", &out_pdf],
        vec!["addtext", &a, "--page", "1", "--at", "1,1", "--size", "12", "--text", "x", "--image", &png, "-o", &out_pdf],
    ] {
        let out = run(&args);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("Try 'qingpdf"), "{args:?}");
    }
    assert!(!dir.join("o.pdf").exists());
}

#[test]
fn addtext_puts_chinese_text_on_a_page_and_keeps_the_original_bytes() {
    if !has_chinese_font() {
        println!("SKIPPED: no Chinese TrueType font on this machine");
        return;
    }
    let (dir, input) = setup("cli-addtext");
    let output = dir.join("out.pdf");
    let out = run(["addtext", s(&input), "--page", "2", "--at", "40,300", "--size", "20", "--text", "你好，世界\\nSecond 第二行", "-o", s(&output)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("wrote") && stdout(&out).contains("3 pages"), "{}", stdout(&out));
    check_written(&input, &output, "");
    let text = text_of(&output, 2, &[]);
    assert!(text.contains("你好，世界") && text.contains("Second 第二行") && text.contains("page 2"), "{text:?}");
    assert!(!text_of(&output, 1, &[]).contains("你好") && !text_of(&output, 3, &[]).contains("你好"));
    // The output is never the input, and never an existing file without --force.
    let again = run(["addtext", s(&input), "--page", "1", "--at", "1,1", "--size", "9", "--text", "x", "-o", s(&input), "--force"]);
    assert_eq!(code(&again), 1);
    assert!(stderr(&again).contains("same as an input"), "{}", stderr(&again));
    let exists = run(["addtext", s(&input), "--page", "1", "--at", "1,1", "--size", "9", "--text", "x", "-o", s(&output)]);
    assert_eq!(code(&exists), 1);
    assert!(stderr(&exists).contains("--force"));
    let forced = run(["addtext", s(&input), "--page", "1", "--at", "1,1", "--size", "9", "--text", "x", "-o", s(&output), "--force"]);
    assert_eq!(code(&forced), 0, "{}", stderr(&forced));
    // A page that is not there, and a text no font can show, are errors with a reason (and write nothing).
    let none = dir.join("none.pdf");
    let far = run(["addtext", s(&input), "--page", "9", "--at", "1,1", "--size", "9", "--text", "x", "-o", s(&none)]);
    assert_eq!(code(&far), 1);
    assert!(stderr(&far).contains("no page 9") && !none.exists(), "{}", stderr(&far));
    let odd = run(["addtext", s(&input), "--page", "1", "--at", "1,1", "--size", "9", "--text", "\u{10FFFD}", "-o", s(&none)]);
    assert_eq!(code(&odd), 1);
    assert!(stderr(&odd).contains("no installed font has U+10FFFD") && !none.exists(), "{}", stderr(&odd));
}

#[test]
fn stamp_puts_a_picture_with_its_transparency_where_it_is_asked() {
    let (dir, input) = setup("cli-stamp");
    let picture = dir.join("ring.png");
    std::fs::write(&picture, ring_png(100, 50)).unwrap();
    let output = dir.join("out.pdf");
    let out = run(["stamp", s(&input), "--image", s(&picture), "--page", "1", "--at", "50,100", "--width", "120", "-o", s(&output)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    check_written(&input, &output, "");
    // It is drawn: red ink where the ring is (the picture is 120 by 60 points with its lower left corner 50 right and 100 up).
    let png_out = dir.join("page.png");
    assert_eq!(code(&run(["render", s(&output), "--pages", "1", "--dpi", "72", "-o", s(&png_out), "--force"])), 0);
    let mut decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&png_out).unwrap()));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    let channels = info.color_type.samples();
    let (mut found, mut inside) = (0usize, true);
    for (i, px) in buf.chunks(channels).enumerate() {
        if px[0] > 180 && px[1] < 80 && px[2] < 80 {
            let (x, y) = ((i % info.width as usize) as f64, (i / info.width as usize) as f64);
            inside &= (49.0..=171.0).contains(&x) && (400.0 - 161.0..=400.0 - 99.0).contains(&y);
            found += 1;
        }
    }
    assert!(found > 400 && inside, "{found} red pixels, all inside the place: {inside}");
    // The picture's middle is see-through: the page's own text can be read through a ring (the page says "page 1" at the top; here it is just
    // that the file is whole). A JPEG works too.
    let jpeg = std::fs::read_dir(common::workspace_root().join("crates").join("qingpdf-core").join("tests").join("fixtures")).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "jpg")).unwrap();
    let with_jpeg = dir.join("jpeg.pdf");
    let out = run(["stamp", s(&input), "--image", s(&jpeg), "--page", "3", "--at", "10,10", "--width", "80", "-o", s(&with_jpeg)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    check_written(&input, &with_jpeg, "");
    // Not a picture, and a header that claims 25 million pixels (its checksum is wrong too: refused either way).
    let junk = dir.join("junk.png");
    std::fs::write(&junk, b"not a picture").unwrap();
    let out = run(["stamp", s(&input), "--image", s(&junk), "--page", "1", "--at", "1,1", "--width", "10", "-o", s(&dir.join("x.pdf"))]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("neither a JPEG nor a PNG"), "{}", stderr(&out));
    let mut big = ring_png(100, 50);
    big[16..20].copy_from_slice(&5000u32.to_be_bytes());
    big[20..24].copy_from_slice(&5000u32.to_be_bytes());
    std::fs::write(&junk, big).unwrap();
    let out = run(["stamp", s(&input), "--image", s(&junk), "--page", "1", "--at", "1,1", "--width", "10", "-o", s(&dir.join("x.pdf"))]);
    assert_eq!(code(&out), 1);
    assert!(!dir.join("x.pdf").exists(), "{}", stderr(&out));
    let out = run(["stamp", s(&input), "--image", s(&dir.join("missing.png")), "--page", "1", "--at", "1,1", "--width", "10", "-o", s(&dir.join("x.pdf"))]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("cannot read"), "{}", stderr(&out));
}

#[test]
fn watermark_goes_on_the_pages_asked_with_the_opacity_angle_tiling_and_place() {
    if !has_chinese_font() {
        println!("SKIPPED: no Chinese TrueType font on this machine");
        return;
    }
    let (dir, input) = setup("cli-watermark");
    let output = dir.join("out.pdf");
    let out = run(["watermark", s(&input), "--text", "机密 CONFIDENTIAL", "--opacity", "0.3", "--angle", "45", "--tile", "--pages", "1,3", "-o", s(&output)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    check_written(&input, &output, "");
    assert!(watermarked(&output, 1) && watermarked(&output, 3) && !watermarked(&output, 2), "the pages asked for are marked, the other is not");
    assert!((1..=3).all(|p| !text_of(&output, p, &[]).contains("机密")), "a watermark is not text");
    assert!(std::fs::read(&output).unwrap().windows(10).any(|w| w == b"/FontFile2"), "the font of the text is in the file");
    // Under the content, one mark in the middle, a picture and text together, and the default (over, 30 per cent, not turned).
    let picture = dir.join("ring.png");
    std::fs::write(&picture, ring_png(60, 60)).unwrap();
    for (name, extra) in [("under", vec!["--under"]), ("both", vec!["--image", s(&picture)]), ("plain", vec![])] {
        let path = dir.join(format!("{name}.pdf"));
        let mut args = vec!["watermark", s(&input), "--text", "机密"];
        args.extend(extra);
        args.extend(["-o", s(&path)]);
        let out = run(&args);
        assert_eq!(code(&out), 0, "{name}: {}", stderr(&out));
        check_written(&input, &path, "");
        assert!(watermarked(&path, 2) && !text_of(&path, 2, &[]).contains("机密"), "{name}");
    }
    // A picture alone.
    let only = dir.join("only.pdf");
    let out = run(["watermark", s(&input), "--image", s(&picture), "--tile", "--angle", "30", "-o", s(&only)]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    check_written(&input, &only, "");
    // Pages that are not there, text that is too long and a text no font has: errors with a reason, nothing written.
    let none = dir.join("none.pdf");
    for (args, what) in [
        (vec!["watermark", s(&input), "--text", "x", "--pages", "9", "-o", s(&none)], "page"),
        (vec!["watermark", s(&input), "--text", "\u{10FFFD}", "-o", s(&none)], "no installed font"),
    ] {
        let out = run(&args);
        assert_eq!(code(&out), 1, "{args:?}");
        assert!(stderr(&out).contains(what) && !none.exists(), "{args:?}: {}", stderr(&out));
    }
    let long = "x".repeat(201);
    let out = run(["watermark", s(&input), "--text", &long, "-o", s(&none)]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("at most 200") && !none.exists(), "{}", stderr(&out));
}

#[test]
fn files_that_are_encrypted_signed_or_not_to_be_changed_are_handled_as_the_rules_say() {
    let (dir, input) = setup("cli-content-rules");
    // Encrypted: a password is needed (and the result keeps the encryption); the author's rule (bit 4) is kept unless the owner password is given.
    if let Some(qpdf) = common::find_qpdf() {
        let locked = dir.join("locked.pdf");
        let status = Command::new(&qpdf).args(["--encrypt", "pw", "owner", "256", "--modify=none", "--", s(&input), s(&locked)]).status().unwrap();
        assert!(status.code().is_some_and(|c| c == 0 || c == 3));
        let out = dir.join("o.pdf");
        let r = run(["addtext", s(&locked), "--page", "1", "--at", "9,9", "--size", "9", "--text", "x", "-o", s(&out)]);
        assert_eq!(code(&r), 1);
        assert!(stderr(&r).contains("needs a password"), "{}", stderr(&r));
        let r = run(["addtext", s(&locked), "--page", "1", "--at", "9,9", "--size", "9", "--text", "x", "-o", s(&out), "--password", "pw"]);
        assert_eq!(code(&r), 1, "{}", stdout(&r));
        assert!(stderr(&r).contains("permission bit 4") && stderr(&r).contains("owner password") && !out.exists(), "{}", stderr(&r));
        let r = run(["watermark", s(&locked), "--text", "x", "-o", s(&out), "--password", "pw"]);
        assert_eq!(code(&r), 1);
        assert!(!out.exists());
        let r = run(["addtext", s(&locked), "--page", "1", "--at", "9,9", "--size", "9", "--text", "x", "-o", s(&out), "--password", "owner"]);
        assert_eq!(code(&r), 0, "{}", stderr(&r));
        check_written(&locked, &out, "owner");
        // The result is still encrypted: its text cannot be read without the password.
        assert_eq!(code(&run(["text", s(&out)])), 1);
        assert!(text_of(&out, 1, &["--password", "owner"]).contains("page 1"));
    }
    // A signed file, and a certified one: refused, with the reason, and nothing is written.
    let signed = |extra: &str, sig: &str| {
        let objects = format!(
            "%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R {extra} >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Annots [10 0 R] >>\nendobj\n10 0 obj\n<< /Type /Annot /Subtype /Widget /FT /Sig /T (s) /Rect [0 0 0 0] /V 20 0 R >>\nendobj\n20 0 obj\n{sig}\nendobj\n"
        );
        let mut bytes = objects.into_bytes();
        let offsets: Vec<usize> = [1, 2, 3, 10, 20].iter().map(|n| String::from_utf8_lossy(&bytes).find(&format!("\n{n} 0 obj")).unwrap() + 1).collect();
        let at = bytes.len();
        let mut xref = String::from("xref\n0 21\n0000000000 65535 f \n");
        for n in 1..21usize {
            let entry = [1usize, 2, 3, 10, 20].iter().position(|&k| k == n).map_or("0000000000 00000 f \n".to_string(), |i| format!("{:010} 00000 n \n", offsets[i]));
            xref.push_str(&entry);
        }
        bytes.extend_from_slice(format!("{xref}trailer\n<< /Size 21 /Root 1 0 R >>\nstartxref\n{at}\n%%EOF\n").as_bytes());
        bytes
    };
    let signature = "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 1 2 3] /Contents <00> >>";
    let certified = "<< /Type /Sig /Reference [ << /Type /SigRef /TransformMethod /DocMDP /TransformParams << /Type /TransformParams /P 3 /V /1.2 >> >> ] >>";
    for (name, bytes, word) in [("signed", signed("/AcroForm << /Fields [10 0 R] /SigFlags 3 >>", signature), "signed"), ("certified", signed("/Perms << /DocMDP 20 0 R >>", certified), "certified")] {
        let path = dir.join(format!("{name}.pdf"));
        std::fs::write(&path, bytes).unwrap();
        let out = dir.join(format!("{name}-out.pdf"));
        for args in [
            vec!["addtext", s(&path), "--page", "1", "--at", "9,9", "--size", "9", "--text", "x", "-o", s(&out)],
            vec!["watermark", s(&path), "--text", "x", "-o", s(&out)],
        ] {
            let r = run(&args);
            assert_eq!(code(&r), 1, "{name}: {args:?}");
            assert!(stderr(&r).contains(word) && stderr(&r).contains("page"), "{name}: {}", stderr(&r));
            assert!(!out.exists(), "{name}: nothing is written");
        }
    }
}

#[test]
fn a_file_whose_structure_is_damaged_is_written_afresh_with_a_warning() {
    if !has_chinese_font() {
        println!("SKIPPED: no system font on this machine");
        return;
    }
    let (dir, input) = setup("cli-content-damaged");
    // The start of the cross-reference table is wrong: the file is read by scanning it, and written afresh before the change.
    let mut bytes = std::fs::read(&input).unwrap();
    let at = bytes.windows(9).rposition(|w| w == b"startxref").unwrap();
    bytes.truncate(at);
    bytes.extend_from_slice(b"startxref\n7\n%%EOF\n");
    let damaged = dir.join("damaged.pdf");
    std::fs::write(&damaged, bytes).unwrap();
    let output = dir.join("out.pdf");
    let r = run(["watermark", s(&damaged), "--text", "ABC", "-o", s(&output)]);
    assert_eq!(code(&r), 0, "{}", stderr(&r));
    assert!(stderr(&r).contains("written afresh"), "{}", stderr(&r));
    assert!(watermarked(&output, 2) && !text_of(&output, 2, &[]).contains("ABC"));
}

#[test]
fn a_watermark_on_500_pages_takes_under_two_seconds_and_makes_each_shared_thing_once() {
    let dir = common::fresh_out_dir("cli-content-500");
    let input = dir.join("big.pdf");
    std::fs::write(&input, many_pages(500)).unwrap();
    let picture = dir.join("ring.png");
    std::fs::write(&picture, ring_png(300, 300)).unwrap();
    for (name, args) in [
        ("picture", vec!["--image", s(&picture), "--tile", "--angle", "45", "--opacity", "0.3"]),
        ("text", vec!["--text", "机密 CONFIDENTIAL", "--tile", "--angle", "45", "--opacity", "0.3", "--under"]),
    ] {
        if name == "text" && !has_chinese_font() {
            continue;
        }
        let output = dir.join(format!("{name}.pdf"));
        let mut all = vec!["watermark", s(&input)];
        all.extend(args);
        all.extend(["-o", s(&output)]);
        let started = Instant::now();
        let out = run(&all);
        let took = started.elapsed();
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        println!("{name}: 500 pages in {took:?}");
        if !cfg!(debug_assertions) {
            assert!(took.as_secs_f64() <= 2.0, "{name}: 500 pages took {took:?}");
        }
        check_written(&input, &output, "");
        // The shared things are made once: one picture (and its mask) or one font, one copy of the shared resources, one stream for all
        // the pages of the same size, and the `q` stream; not a set of them for each page.
        let section = &std::fs::read(&output).unwrap()[std::fs::read(&input).unwrap().len()..];
        let text = String::from_utf8_lossy(section);
        assert!(text.matches("/Subtype /Image").count() <= 2, "{name}: the picture was made again for pages");
        assert!(text.matches("/Type /FontDescriptor").count() <= 1, "{name}: the font was made again for pages");
        assert!(text.matches("/Font <<").count() <= 2, "{name}: the resources were copied for each page");
        assert!(section.len() < 400_000, "{name}: the update is {} bytes for 500 pages", section.len());
        assert_eq!(text.matches("/Type /Page ").count(), 500, "{name}");
    }
}

#[test]
fn numbers_out_of_range_say_what_is_wrong_and_a_size_or_place_that_is_changed_or_off_the_page_is_warned_of() {
    let (dir, input) = setup("cli-content-ranges");
    let output = dir.join("o.pdf");
    let png = dir.join("p.png");
    std::fs::write(&png, ring_png(20, 10)).unwrap();
    let (a, o, p) = (s(&input), s(&output), s(&png));
    // Numbers that fit in a double but not in the engine: usage errors that name the option and the range, not "not a number".
    for (args, word) in [
        (vec!["watermark", a, "--text", "x", "--angle", "1e300", "-o", o], "--angle must be from -36000 to 36000 degrees, not '1e300'"),
        (vec!["watermark", a, "--text", "x", "--angle", "-40000", "-o", o], "--angle must be from -36000 to 36000"),
        (vec!["stamp", a, "--image", p, "--page", "1", "--at", "1,1", "--width", "1e300", "-o", o], "--width must be at most 10000000 points, not '1e300'"),
        (vec!["stamp", a, "--image", p, "--page", "1", "--at", "1e300,1", "--width", "10", "-o", o], "--at must be between -10000000 and 10000000 points, not '1e300,1'"),
        (vec!["addtext", a, "--page", "1", "--at", "1,-1e30", "--size", "12", "--text", "x", "-o", o], "--at must be between"),
    ] {
        let out = run(&args);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(word), "{args:?}: {}", stderr(&out));
        assert!(!stderr(&out).contains("not a number"), "{args:?}: {}", stderr(&out));
    }
    assert!(!output.exists());
    // A size outside what text may be is changed to the nearest one, and the person is told; one inside is not.
    for (size, said) in [("1000", Some("size 1000 was changed to 144")), ("1e300", Some("size 1e300 was changed to 144")), ("0.5", Some("size 0.5 was changed to 4")), ("12", None)] {
        let out = run(["addtext", a, "--page", "1", "--at", "10,10", "--size", size, "--text", "x", "-o", o, "--force"]);
        assert_eq!(code(&out), 0, "{size}: {}", stderr(&out));
        match said {
            Some(words) => assert!(stderr(&out).contains("warning:") && stderr(&out).contains(words), "{size}: {}", stderr(&out)),
            None => assert!(stderr(&out).is_empty(), "{size}: {}", stderr(&out)),
        }
        check_written(&input, &output, "");
    }
    // A place off the page (the page is 300 x 400): the text or the picture is cut or not seen; warned of, and still written.
    for args in [
        vec!["addtext", a, "--page", "1", "--at", "350,10", "--size", "12", "--text", "x", "-o", o, "--force"],
        vec!["addtext", a, "--page", "1", "--at", "10,-5", "--size", "12", "--text", "x", "-o", o, "--force"],
        vec!["stamp", a, "--image", p, "--page", "2", "--at", "10,401", "--width", "10", "-o", o, "--force"],
    ] {
        let out = run(&args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains("warning: the place") && stderr(&out).contains("is off page") && stderr(&out).contains("300 by 400 points"), "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).contains("wrote"), "{args:?}");
    }
    // On the page (the edges count as on it): nothing to say.
    for args in [
        vec!["addtext", a, "--page", "1", "--at", "0,0", "--size", "12", "--text", "x", "-o", o, "--force"],
        vec!["stamp", a, "--image", p, "--page", "3", "--at", "300,400", "--width", "10", "-o", o, "--force"],
    ] {
        let out = run(&args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).is_empty(), "{args:?}: {}", stderr(&out));
    }
}

//! Layer 3, step 3b: how many characters of the whole test corpus are still drawn as boxes, by cause, and which
//! font warnings the pages give. Every page of every file is drawn at a low resolution (the glyphs are looked
//! up all the same).
//!
//!   cargo run --release --example box_census -p qingpdf-cli [dpi] [path-fragment]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use qingpdf_core::Document;
use qingpdf_core::render::Renderer;

fn pdfs_below(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            pdfs_below(&path, out);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
            out.push(path);
        }
    }
}

fn main() {
    let dpi: f64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(24.0);
    let only = std::env::args().nth(2).unwrap_or_default();
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("tests").join("corpus");
    let mut files = Vec::new();
    pdfs_below(&corpus, &mut files);
    files.sort();
    let started = Instant::now();
    let (mut pages, mut boxed, mut failed, mut absent) = (0usize, 0usize, 0usize, 0usize);
    let mut absent_files: Vec<(usize, String)> = Vec::new();
    let mut causes = [0usize; 3];
    let mut per_file: Vec<(usize, String)> = Vec::new();
    let mut warnings: BTreeMap<String, usize> = BTreeMap::new();
    for path in files {
        let name = path.strip_prefix(&corpus).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        if !only.is_empty() && !name.contains(&only) {
            continue;
        }
        let Ok(doc) = Document::open(&path) else { continue };
        if doc.is_locked() {
            continue;
        }
        let Ok(list) = doc.pages() else { continue };
        let mut renderer = Renderer::new(&doc);
        let mut file_boxes = 0usize;
        let mut file_absent = 0usize;
        for page in &list {
            match renderer.render_page(page, dpi) {
                Ok(b) => {
                    pages += 1;
                    boxed += b.boxed_characters;
                    file_boxes += b.boxed_characters;
                    absent += b.absent_glyphs;
                    file_absent += b.absent_glyphs;
                    for (c, n) in causes.iter_mut().zip(b.boxed_causes) {
                        *c += n;
                    }
                }
                Err(_) => failed += 1,
            }
            for w in renderer.take_warnings() {
                if w.starts_with("font ") || w.contains("font program") {
                    *warnings.entry(format!("{name}: {w}")).or_insert(0) += 1;
                }
            }
        }
        if file_absent > 0 {
            absent_files.push((file_absent, name.clone()));
        }
        if file_boxes > 0 {
            per_file.push((file_boxes, name));
        }
    }
    println!("{pages} pages drawn at {dpi} dpi in {:.1} s, {failed} pages failed", started.elapsed().as_secs_f64());
    println!("characters drawn as boxes: {boxed} (no font for them: {}; code with no character: {}; character not in the font: {})", causes[0], causes[1], causes[2]);
    per_file.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (n, name) in per_file.iter().take(25) {
        println!("  {n:7} {name}");
    }
    println!("characters whose glyph the font does not have (nothing drawn): {absent}");
    absent_files.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (n, name) in absent_files.iter().take(25) {
        println!("  {n:7} {name}");
    }
    println!("font warnings:");
    for (w, n) in &warnings {
        println!("  {n:3}x {w}");
    }
}

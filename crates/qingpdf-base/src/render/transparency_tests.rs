//! Step 3c: transparency groups, blend modes, soft masks, knockout, shadings, tiling patterns and optional
//! content, each drawn on a 100 by 100 point page at 72 dpi (one pixel a point; rows from the top, so the
//! user point (x, y) is the pixel (x, 99 - y)). The hostile cases that go with them are in `hostile_tests`.

use miniz_oxide::deflate::compress_to_vec_zlib;

use super::tests::{WHITE, draw, page_doc, pixel};
use super::*;
use crate::testutil::PdfBuilder;

/// Within `tol` of `want` in every channel.
fn near(got: [u8; 3], want: [u8; 3], tol: u8) -> bool {
    got.iter().zip(want).all(|(a, b)| a.abs_diff(b) <= tol)
}

fn drawn(doc: &Document) -> Bitmap {
    let (b, w) = draw(doc);
    assert!(w.is_empty(), "{w:?}");
    b.expect("renders")
}

const GROUP_FORM: &str = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency /I true >> /Resources << /ExtGState << /A 6 0 R >> >>";

#[test]
fn a_group_is_blended_as_one_object() {
    // Blue and, over it, green inside a group drawn at half opacity over red: the blue under the green does not
    // show through (11.4.7). Drawn object by object (no /Group) it does.
    let res = "<< /XObject << /G 5 0 R >> /ExtGState << /A 6 0 R >> >>";
    let content = b"1 0 0 rg 0 0 100 100 re f /A gs /G Do";
    let content_forms = b"0 0 1 rg 20 20 60 60 re f 0 1 0 rg 40 40 60 60 re f";
    let doc = page_doc("", res, content, &[(6, "<< /ca 0.5 >>")], &[(5, GROUP_FORM, content_forms)]);
    let b = drawn(&doc);
    // Blue alone: half of it over red.
    assert!(near(pixel(&b, 30, 70), [128, 0, 128], 2), "{:?}", pixel(&b, 30, 70));
    // Green over blue inside the group: half of the green over red, no blue.
    assert!(near(pixel(&b, 50, 50), [128, 128, 0], 2), "{:?}", pixel(&b, 50, 50));
    // The same form without a group: the objects are each half transparent.
    let plain = "/Type /XObject /Subtype /Form /BBox [0 0 100 100]";
    let doc = page_doc("", res, content, &[(6, "<< /ca 0.5 >>")], &[(5, plain, content_forms)]);
    let b = drawn(&doc);
    let p = pixel(&b, 50, 50);
    assert!(near(p, [64, 128, 64], 3), "{p:?}");
}

#[test]
fn blend_modes_follow_the_formulas() {
    // Backdrop (0.5, 0.5, 1.0), source (1.0, 0.5, 0.5).
    let res = "<< /ExtGState << /M 5 0 R /S 6 0 R /D 7 0 R /L 8 0 R >> >>";
    let back = "0.5 0.5 1 rg 0 0 100 100 re f 1 0.5 0.5 rg ";
    let doc = |gs: &str| {
        page_doc(
            "",
            res,
            format!("{back}{gs} gs 0 0 100 100 re f").as_bytes(),
            &[(5, "<< /BM /Multiply >>"), (6, "<< /BM /Screen >>"), (7, "<< /BM [/Foo /Difference] >>"), (8, "<< /BM /Luminosity >>")],
            &[],
        )
    };
    let multiply = drawn(&doc("/M"));
    assert!(near(pixel(&multiply, 50, 50), [128, 64, 128], 2), "{:?}", pixel(&multiply, 50, 50));
    let screen = drawn(&doc("/S"));
    assert!(near(screen_px(&screen), [255, 191, 255], 2), "{:?}", screen_px(&screen));
    // An array: the first mode that is known counts.
    let difference = drawn(&doc("/D"));
    assert!(near(pixel(&difference, 50, 50), [128, 0, 128], 2), "{:?}", pixel(&difference, 50, 50));
    // A non-separable one: the luminosity of the source on the colour of the backdrop. Source luminosity is
    // 0.3 + 0.295 + 0.055 = 0.65, the backdrop's 0.15 + 0.295 + 0.11 = 0.555: every channel moves up by 0.095,
    // and blue, now over 1, is brought back to 1 with the luminosity kept (11.3.5.3, ClipColor).
    let luminosity = drawn(&doc("/L"));
    assert!(near(pixel(&luminosity, 50, 50), [155, 155, 255], 4), "{:?}", pixel(&luminosity, 50, 50));
}

fn screen_px(b: &Bitmap) -> [u8; 3] {
    pixel(b, 50, 50)
}

fn smask_page(mask: &str, group_content: &[u8], extra_objs: &[(u32, &str)]) -> Document {
    let res = "<< /ExtGState << /GS 6 0 R >> >>";
    let mut objs: Vec<(u32, &str)> = vec![(6, mask)];
    objs.extend_from_slice(extra_objs);
    page_doc(
        "",
        res,
        b"/GS gs 1 0 0 rg 0 0 100 100 re f",
        &objs,
        &[(7, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency /CS /DeviceRGB >>", group_content)],
    )
}

#[test]
fn soft_masks_by_luminosity_and_alpha() {
    // The mask group is white on the left, mid grey in the middle, nothing on the right.
    let content = b"1 g 0 0 40 100 re f 0.5 g 40 0 30 100 re f";
    let lum = drawn(&smask_page("<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 7 0 R >> >>", content, &[]));
    assert_eq!(pixel(&lum, 20, 50), [255, 0, 0]);
    // Half of the red over white.
    assert!(near(pixel(&lum, 55, 50), [255, 127, 127], 3), "{:?}", pixel(&lum, 55, 50));
    // Outside the group the backdrop is black: nothing is painted.
    assert_eq!(pixel(&lum, 85, 50), WHITE);
    // /BC: a white backdrop shows the red outside what the group draws.
    let bc = drawn(&smask_page("<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 7 0 R /BC [1] >> >>", content, &[]));
    assert_eq!(pixel(&bc, 85, 50), [255, 0, 0]);
    // /TR turns the mask over.
    let tr = drawn(&smask_page(
        "<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 7 0 R /TR 8 0 R >> >>",
        content,
        &[(8, "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>")],
    ));
    assert_eq!(pixel(&tr, 20, 50), WHITE);
    assert_eq!(pixel(&tr, 85, 50), [255, 0, 0]);
    // Alpha: the group's opacity, whatever its colour. Black on the left counts as much as white.
    let alpha = drawn(&smask_page("<< /Type /ExtGState /SMask << /Type /Mask /S /Alpha /G 7 0 R >> >>", b"0 g 0 0 50 100 re f", &[]));
    assert_eq!(pixel(&alpha, 25, 50), [255, 0, 0]);
    assert_eq!(pixel(&alpha, 75, 50), WHITE);
    // /None ends it, and so does Q.
    let doc = page_doc(
        "",
        "<< /ExtGState << /GS 6 0 R /N 8 0 R >> >>",
        b"q /GS gs 1 0 0 rg 0 0 50 100 re f Q 0 0 1 rg 50 0 50 50 re f /GS gs /N gs 0 1 0 rg 50 50 50 50 re f",
        &[(6, "<< /SMask << /S /Alpha /G 7 0 R >> >>"), (8, "<< /SMask /None >>")],
        &[(7, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency >>", b"0 g 0 0 20 20 re f")],
    );
    let b = drawn(&doc);
    assert_eq!(pixel(&b, 70, 70), [0, 0, 255]);
    assert_eq!(pixel(&b, 70, 20), [0, 255, 0]);
}

#[test]
fn a_soft_mask_works_on_images_and_forms_too() {
    // A mask applied to a group drawn through it; the mask is where the CTM was at `gs` (moved 50 right).
    let doc = page_doc(
        "",
        "<< /ExtGState << /GS 6 0 R >> /XObject << /G 5 0 R >> >>",
        b"q 1 0 0 1 50 0 cm /GS gs 1 0 0 1 -50 0 cm /G Do Q",
        &[(6, "<< /SMask << /S /Alpha /G 7 0 R >> >>")],
        &[
            (5, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency >>", b"0 0 1 rg 0 0 100 100 re f"),
            (7, "/Type /XObject /Subtype /Form /BBox [0 0 30 100] /Group << /S /Transparency >>", b"0 g 0 0 30 100 re f"),
        ],
    );
    let b = drawn(&doc);
    assert_eq!(pixel(&b, 30, 50), WHITE);
    assert_eq!(pixel(&b, 60, 50), [0, 0, 255]);
    assert_eq!(pixel(&b, 90, 50), WHITE);
}

#[test]
fn knockout_groups_replace_what_is_under_each_object() {
    // Two overlapping half-transparent squares in a group: in a knockout group the second replaces the first
    // where they meet (11.4.6.2); in a plain one they add up.
    let res = "<< /XObject << /G 5 0 R >> >>";
    let content = b"/G Do";
    let body = b"/A gs 1 0 0 rg 10 10 50 50 re f 0 0 1 rg 40 40 50 50 re f";
    let knock = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency /I true /K true >> /Resources << /ExtGState << /A 6 0 R >> >>";
    let b = drawn(&page_doc("", res, content, &[(6, "<< /ca 0.5 >>")], &[(5, knock, body)]));
    // Overlap: the blue alone, half over white; red alone elsewhere.
    assert!(near(pixel(&b, 50, 50), [128, 128, 255], 2), "{:?}", pixel(&b, 50, 50));
    assert!(near(pixel(&b, 20, 80), [255, 128, 128], 2), "{:?}", pixel(&b, 20, 80));
    let plain = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency /I true >> /Resources << /ExtGState << /A 6 0 R >> >>";
    let b = drawn(&page_doc("", res, content, &[(6, "<< /ca 0.5 >>")], &[(5, plain, body)]));
    // Blue half over (red half over white).
    assert!(near(pixel(&b, 50, 50), [128, 64, 191], 3), "{:?}", pixel(&b, 50, 50));
}

fn shading_page(shading: &str, extra: &[(u32, &str)], streams: &[(u32, &str, &[u8])]) -> Document {
    let mut objs: Vec<(u32, &str)> = vec![(5, shading)];
    objs.extend_from_slice(extra);
    page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh", &objs, streams)
}

#[test]
fn axial_and_radial_shadings_extend_and_stop() {
    let axial = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [25 0 75 0] /Function 8 0 R >>";
    let f = (8, "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>");
    let b = drawn(&shading_page(axial, &[f], &[]));
    // Not extended: the page shows outside the two ends.
    assert_eq!(pixel(&b, 10, 50), WHITE);
    assert_eq!(pixel(&b, 90, 50), WHITE);
    assert!(near(pixel(&b, 50, 50), [128, 0, 128], 4), "{:?}", pixel(&b, 50, 50));
    assert!(pixel(&b, 27, 50)[0] > 240);
    let extended = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [25 0 75 0] /Function 8 0 R /Extend [true true] >>";
    let b = drawn(&shading_page(extended, &[f], &[]));
    // (The table of colours has 256 entries, the last a step short of the end, as in PDFium.)
    assert!(near(pixel(&b, 10, 50), [255, 0, 0], 2), "{:?}", pixel(&b, 10, 50));
    assert!(near(pixel(&b, 90, 50), [0, 0, 255], 2), "{:?}", pixel(&b, 90, 50));
    // Radial: red at the centre, blue at a radius of 40; the page outside, unless extended.
    let radial = "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [50 50 0 50 50 40] /Function 8 0 R >>";
    let b = drawn(&shading_page(radial, &[f], &[]));
    assert!(pixel(&b, 50, 50)[0] > 245, "{:?}", pixel(&b, 50, 50));
    assert!(near(pixel(&b, 70, 50), [128, 0, 128], 6), "{:?}", pixel(&b, 70, 50));
    assert_eq!(pixel(&b, 95, 95), WHITE);
    let radial = "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [50 50 0 50 50 40] /Function 8 0 R /Extend [false true] >>";
    let b = drawn(&shading_page(radial, &[f], &[]));
    assert!(near(pixel(&b, 95, 95), [0, 0, 255], 2), "{:?}", pixel(&b, 95, 95));
    // /BBox cuts it; /Domain says what t runs over.
    let boxed = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Function 8 0 R /BBox [0 0 50 100] /Domain [0 0.5] >>";
    let b = drawn(&shading_page(boxed, &[f], &[]));
    assert_eq!(pixel(&b, 75, 50), WHITE);
    // Halfway of the half of the range that /Domain gives: a quarter of the way from red to blue.
    assert!(near(pixel(&b, 49, 50), [192, 0, 63], 8), "{:?}", pixel(&b, 49, 50));
}

#[test]
fn function_based_shading() {
    // (x, y) to (x, y, 0), over the page by the matrix.
    let sh = "<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] /Matrix [100 0 0 100 0 0] /Function 8 0 R >>";
    let b = drawn(&shading_page(sh, &[], &[(8, "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]", b"{ 0 }")]));
    let p = pixel(&b, 10, 10);
    assert!(near(p, [26, 230, 0], 5), "{p:?}");
    let p = pixel(&b, 90, 90);
    assert!(near(p, [230, 26, 0], 5), "{p:?}");
}

/// A mesh stream: 8-bit flags, coordinates and components, the coordinates decoded onto 0 to 100.
fn mesh_dict(ty: u32, extra: &str) -> String {
    format!("/ShadingType {ty} /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 100 0 100 0 1 0 1 0 1] {extra}")
}

#[test]
fn triangle_meshes() {
    // Type 4: one triangle, red at the origin, green at (100, 0), blue at (0, 100).
    let data: Vec<u8> = vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255];
    let doc = page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh", &[], &[(5, &mesh_dict(4, ""), &data)]);
    let b = drawn(&doc);
    // The triangle covers the lower left half of the page; its corner at the origin is red.
    assert!(pixel(&b, 2, 97)[0] > 230 && pixel(&b, 2, 97)[1] < 25, "{:?}", pixel(&b, 2, 97));
    assert!(pixel(&b, 97, 99)[1] > 230, "{:?}", pixel(&b, 97, 99));
    assert!(pixel(&b, 2, 2)[2] > 230, "{:?}", pixel(&b, 2, 2));
    // The far corner is outside.
    assert_eq!(pixel(&b, 90, 10), WHITE);
    // Halfway along the red-green edge: half and half.
    let p = pixel(&b, 50, 99);
    assert!(near(p, [128, 128, 0], 10), "{p:?}");

    // Type 4 with an edge flag: a second triangle on the side of the first, from one more vertex.
    let mut more = data.clone();
    more.extend_from_slice(&[1, 255, 255, 255, 255, 0]);
    let doc = page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh", &[], &[(5, &mesh_dict(4, ""), &more)]);
    let b = drawn(&doc);
    // (vb, vc, vd) = (green, blue, yellow at (100, 100)): the upper right half is covered now.
    assert!(near(pixel(&b, 97, 3), [238, 247, 8], 40), "{:?}", pixel(&b, 97, 3));

    // Type 5: two rows of two vertices make a square with the four colours at the corners.
    let lattice = [0u8, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 255, 255, 255, 255, 0];
    let doc = page_doc(
        "",
        "<< /Shading << /Sh 5 0 R >> >>",
        b"/Sh sh",
        &[],
        &[(5, "/ShadingType 5 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /VerticesPerRow 2 /Decode [0 100 0 100 0 1 0 1 0 1]", &lattice)],
    );
    let b = drawn(&doc);
    assert!(pixel(&b, 1, 98)[0] > 240, "{:?}", pixel(&b, 1, 98));
    assert!(pixel(&b, 98, 98)[1] > 240, "{:?}", pixel(&b, 98, 98));
    assert!(pixel(&b, 1, 1)[2] > 240, "{:?}", pixel(&b, 1, 1));
    assert!(near(pixel(&b, 98, 1), [255, 255, 0], 12), "{:?}", pixel(&b, 98, 1));
}

/// The boundary of a square patch, 12 points in the order of the data, and then (for a tensor patch) the 4 inside.
fn square_patch(tensor: bool) -> Vec<u8> {
    let mut d = vec![0u8];
    // (0,0) (0,1/3) (0,2/3) (0,1), then along the top, down the right, back along the bottom.
    let pts: [(u8, u8); 12] = [(0, 0), (0, 85), (0, 170), (0, 255), (85, 255), (170, 255), (255, 255), (255, 170), (255, 85), (255, 0), (170, 0), (85, 0)];
    for (x, y) in pts {
        d.extend_from_slice(&[x, y]);
    }
    if tensor {
        for (x, y) in [(85u8, 85u8), (85, 170), (170, 170), (170, 85)] {
            d.extend_from_slice(&[x, y]);
        }
    }
    // Colours at the corners (0,0) red, (0,1) green, (1,1) blue, (1,0) white.
    d.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
    d
}

#[test]
fn coons_and_tensor_patches() {
    for (ty, tensor) in [(6, false), (7, true)] {
        let data = square_patch(tensor);
        let doc = page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh", &[], &[(5, &mesh_dict(ty, ""), &data)]);
        let b = drawn(&doc);
        // User (0,0) is the pixel (0, 99): red; (0,1) the top left: green; (1,1) the top right: blue; (1,0): white.
        let (bl, tl, tr, br) = (pixel(&b, 1, 98), pixel(&b, 1, 1), pixel(&b, 98, 1), pixel(&b, 98, 98));
        assert!(bl[0] > 235 && bl[1] < 30 && bl[2] < 30, "type {ty}: {bl:?}");
        assert!(tl[1] > 235 && tl[0] < 30 && tl[2] < 30, "type {ty}: {tl:?}");
        assert!(tr[2] > 235 && tr[0] < 30 && tr[1] < 30, "type {ty}: {tr:?}");
        assert!(br.iter().all(|&v| v > 235), "type {ty}: {br:?}");
        // The middle is the mean of the four: (255+0+0+255)/4 ...
        assert!(near(pixel(&b, 50, 50), [128, 128, 128], 12), "type {ty}: {:?}", pixel(&b, 50, 50));
    }
    // A patch with a curved side: the top edge bows up; the page above the curve stays white at a corner.
    let mut data = square_patch(false);
    // The inside points of the top edge sink: the curve is at 213 of 255 in the middle, not at 255.
    data[10] = 170;
    data[12] = 170;
    let doc = page_doc("", "<< /Shading << /Sh 5 0 R >> >>", b"/Sh sh", &[], &[(5, &mesh_dict(6, ""), &data)]);
    let b = drawn(&doc);
    assert_eq!(pixel(&b, 50, 5), WHITE);
    assert_ne!(pixel(&b, 50, 50), WHITE);
}

#[test]
fn shading_patterns_fill_shapes_and_strokes() {
    let res = "<< /Pattern << /P 5 0 R >> >>";
    let shading = "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] /Function 8 0 R /Extend [true true] /Background [0 1 0] >>";
    let f = (8, "<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>");
    let doc = page_doc(
        "",
        res,
        b"/Pattern cs /P scn 10 10 80 30 re f /Pattern CS /P SCN 8 w 10 70 m 90 70 l S",
        &[(5, "<< /PatternType 2 /Shading 9 0 R >>"), (9, shading), f],
        &[],
    );
    let b = drawn(&doc);
    // The shape is filled with the gradient (red at x = 10 going to blue at 90 by 0.8 of the way), nothing outside.
    assert!(pixel(&b, 12, 75)[0] > 200 && pixel(&b, 12, 75)[2] < 60, "{:?}", pixel(&b, 12, 75));
    assert!(pixel(&b, 88, 75)[2] > 180, "{:?}", pixel(&b, 88, 75));
    assert_eq!(pixel(&b, 50, 50), WHITE);
    // The stroke too.
    assert!(near(pixel(&b, 50, 29), [128, 0, 128], 8), "{:?}", pixel(&b, 50, 29));
    // The pattern matrix moves it: shifted 50 to the right the red end is at x = 50.
    let doc = page_doc(
        "",
        res,
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[(5, "<< /PatternType 2 /Shading 9 0 R /Matrix [1 0 0 1 50 0] >>"), (9, shading.replace("/Extend [true true]", "/Extend [false false]").leak()), f],
        &[],
    );
    let b = drawn(&doc);
    // Left of the shading's start the Background shows (green), not the page.
    assert_eq!(pixel(&b, 20, 50), [0, 255, 0]);
    assert!(pixel(&b, 55, 50)[0] > 230, "{:?}", pixel(&b, 55, 50));
}

#[test]
fn tiling_patterns_coloured_and_uncoloured() {
    let res = "<< /Pattern << /P 5 0 R /U 6 0 R >> /ColorSpace << /Cs [/Pattern /DeviceRGB] >> >>";
    let tile = "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>";
    let stencil = "/PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 /Resources << >>";
    let doc = page_doc(
        "",
        res,
        b"/Pattern cs /P scn 0 0 50 100 re f /Cs cs 0 0 1 /U scn 50 0 50 100 re f",
        &[],
        // Content of a stencil cell may name its own colour: it is ignored.
        &[(5, tile, b"1 0 0 rg 0 0 5 5 re f"), (6, stencil, b"1 0 0 rg 0 0 5 5 re f")],
    );
    let b = drawn(&doc);
    // Red squares at the lower left of each 10 by 10 cell, white elsewhere in the cell; blue on the right half.
    assert_eq!(pixel(&b, 2, 97), [255, 0, 0]);
    assert_eq!(pixel(&b, 7, 97), WHITE);
    assert_eq!(pixel(&b, 2, 92), WHITE);
    assert_eq!(pixel(&b, 22, 77), [255, 0, 0]);
    assert_eq!(pixel(&b, 52, 97), [0, 0, 255]);
    assert_eq!(pixel(&b, 57, 97), WHITE);
    // The upper rows of a cell are empty too.
    assert_eq!(pixel(&b, 2, 2), WHITE);
    // The pattern matrix scales and moves the cell: here by 2 and 5.
    let doc = page_doc(
        "",
        "<< /Pattern << /P 5 0 R >> >>",
        b"/Pattern cs /P scn 0 0 100 100 re f",
        &[],
        &[(5, &tile.replace("/XStep 10", "/XStep 10 /Matrix [2 0 0 2 5 0]"), b"1 0 0 rg 0 0 5 5 re f")],
    );
    let b = drawn(&doc);
    // Cells are 20 wide from x = 5: the square is x 5 to 15 and the bottom 10 rows.
    assert_eq!(pixel(&b, 10, 95), [255, 0, 0]);
    assert_eq!(pixel(&b, 20, 95), WHITE);
    assert_eq!(pixel(&b, 30, 95), [255, 0, 0]);
    assert_eq!(pixel(&b, 10, 85), WHITE);
}

#[test]
fn a_cell_larger_than_its_step_overlaps_its_neighbours() {
    // A 10 wide box repeated every 5: the right half of the box of the cell before reaches into each cell.
    let tile = "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 5] /XStep 5 /YStep 5 /Resources << >>";
    let doc = page_doc("", "<< /Pattern << /P 5 0 R >> >>", b"/Pattern cs /P scn 0 0 100 100 re f", &[], &[(5, tile, b"0 0 1 rg 0 0 2 5 re f 0 1 0 rg 7 0 2 5 re f")]);
    let b = drawn(&doc);
    // In a cell: blue at 0 to 2, green (the neighbour's, shifted 5 to the left) at 2 to 4, empty from 4 to 5.
    assert_eq!(pixel(&b, 11, 97), [0, 0, 255]);
    assert_eq!(pixel(&b, 13, 97), [0, 255, 0]);
    assert_eq!(pixel(&b, 14, 97), WHITE);
}

/// A document with a default optional content configuration: object 10 is the OCG that is off, 11 the one that is on.
fn oc_doc(content: &str, extra: &[(u32, &str)], streams: &[(u32, &str, &[u8])]) -> Document {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [10 0 R 11 0 R] /D << /OFF [10 0 R] >> >> >>");
    b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    b.obj(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Resources << /Properties << /Off 10 0 R /On 11 0 R /Ocmd 12 0 R >> /XObject << /Hid 20 0 R /Vis 21 0 R /Img 22 0 R >> >> >>",
    );
    b.stream_obj(4, "", content.as_bytes());
    b.obj(10, "<< /Type /OCG /Name (off) >>");
    b.obj(11, "<< /Type /OCG /Name (on) >>");
    b.obj(12, "<< /Type /OCMD /OCGs [10 0 R 11 0 R] /P /AllOn >>");
    let mut top = 12;
    for (n, body) in extra {
        b.obj(*n, body);
        top = top.max(*n);
    }
    for (n, dict, data) in streams {
        b.stream_obj(*n, dict, data);
        top = top.max(*n);
    }
    Document::from_bytes(b.finish_classic(top + 1, "/Root 1 0 R")).expect("opens")
}

#[test]
fn hidden_layers_are_not_drawn() {
    let form = "/Type /XObject /Subtype /Form /BBox [0 0 100 100]";
    let streams: &[(u32, &str, &[u8])] = &[
        (20, &format!("{form} /OC 10 0 R"), b"1 0 0 rg 0 0 100 100 re f"),
        (21, &format!("{form} /OC 11 0 R"), b"0 1 0 rg 0 0 20 20 re f"),
    ];
    let content = "/Hid Do /Vis Do \
        /OC /Off BDC 1 0 0 rg 40 40 20 20 re f /OC /On BDC 0 0 1 rg 45 45 5 5 re f EMC EMC \
        /OC /Ocmd BDC 0 0 0 rg 70 70 10 10 re f EMC \
        /OC /On BDC 0 1 1 rg 70 10 10 10 re f EMC \
        0 0 0 rg 90 90 5 5 re f";
    let b = drawn(&oc_doc(content, &[], streams));
    // The XObject whose group is off and the marked content that is off are not there; the others are.
    assert_eq!(pixel(&b, 50, 50), WHITE);
    assert_eq!(pixel(&b, 47, 52), WHITE);
    assert_eq!(pixel(&b, 10, 90), [0, 255, 0]);
    // The OCMD wants both on (AllOn): one is off.
    assert_eq!(pixel(&b, 75, 25), WHITE);
    assert_eq!(pixel(&b, 75, 85), [0, 255, 255]);
    // Marked content that is closed leaves the page as it was: the next shape shows.
    assert_eq!(pixel(&b, 92, 7), [0, 0, 0]);
}

#[test]
fn hidden_content_still_changes_the_graphics_state() {
    // The colour set in hidden content is still set; only the painting does not happen.
    let content = "/OC /Off BDC 1 0 0 rg 0 0 50 50 re f EMC 0 0 100 100 re f";
    let b = drawn(&oc_doc(content, &[], &[]));
    // The colour set in the hidden part is the colour of the fill after it.
    assert_eq!(pixel(&b, 50, 50), [255, 0, 0]);
    assert_eq!(pixel(&b, 50, 70), [255, 0, 0]);
}

#[test]
fn image_xobjects_have_optional_content_too() {
    let img = "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /OC 10 0 R";
    let b = drawn(&oc_doc("q 100 0 0 100 0 0 cm /Img Do Q", &[], &[(22, img, &[0])]));
    assert_eq!(pixel(&b, 50, 50), WHITE);
    let img = "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /OC 11 0 R";
    let b = drawn(&oc_doc("q 100 0 0 100 0 0 cm /Img Do Q", &[], &[(22, img, &[0])]));
    assert_eq!(pixel(&b, 50, 50), [0, 0, 0]);
}

#[test]
fn a_compressed_group_is_drawn_with_its_opacity() {
    // The layers do not care how the bytes of a form arrived.
    let content = compress_to_vec_zlib(b"0 0 1 rg 0 0 100 100 re f", 6);
    let doc = page_doc(
        "",
        "<< /XObject << /G 5 0 R >> /ExtGState << /A 6 0 R >> >>",
        b"/A gs /G Do",
        &[(6, "<< /ca 0.25 >>")],
        &[(5, "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency >> /Filter /FlateDecode", &content)],
    );
    let b = drawn(&doc);
    assert!(near(pixel(&b, 50, 50), [191, 191, 255], 2), "{:?}", pixel(&b, 50, 50));
}

#[test]
fn a_group_inside_a_knockout_group_is_one_object() {
    // 11.4.6.2: in a knockout group each object replaces what is under it, and a group inside is one object, whatever it
    // is made of. Here the child is plain (opaque, Normal): it still has to be drawn on a layer, to be put on the parent as one.
    // Two overlapping half-transparent squares in the child add up there; the parent's knockout takes the child as a whole.
    let knock = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency /I true /K true >> /Resources << /XObject << /G 7 0 R >> >>";
    let child = "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Group << /S /Transparency >> /Resources << /ExtGState << /A 6 0 R >> >>";
    let body = b"/A gs 0 1 0 rg 10 10 50 50 re f 0 0 1 rg 40 40 50 50 re f";
    let b = drawn(&page_doc("", "<< /XObject << /K 5 0 R >> >>", b"/K Do", &[(6, "<< /ca 0.5 >>")], &[(5, knock, b"/G Do"), (7, child, body)]));
    // Where they meet: the blue half over the green half over white (inside the child they are not knocked out).
    assert!(near(pixel(&b, 50, 50), [64, 128, 191], 3), "{:?}", pixel(&b, 50, 50));
    assert!(near(pixel(&b, 20, 79), [128, 255, 128], 3), "{:?}", pixel(&b, 20, 79));
    assert!(near(pixel(&b, 80, 20), [128, 128, 255], 3), "{:?}", pixel(&b, 80, 20));
}

#[test]
fn marked_content_nested_deeper_than_any_cap_still_hides() {
    // The level that hides is the 4101st: it used to be left uncounted, and what it hid showed.
    let content = format!("{}/OC /Off BDC 1 0 0 rg 0 0 100 100 re f EMC 0 0 1 rg 0 0 10 10 re f", "/Tag BMC ".repeat(4100));
    let b = drawn(&oc_doc(&content, &[], &[]));
    assert_eq!(pixel(&b, 50, 50), WHITE);
    // Closed again, the page shows what comes next.
    assert_eq!(pixel(&b, 5, 95), [0, 0, 255]);
}

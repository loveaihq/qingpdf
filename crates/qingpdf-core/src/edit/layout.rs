//! Laying out the text of a form field or a text box and drawing the parts of a form control (12.7.3.3, 12.5.6.6): the lines,
//! the size when it is left to us (a `/DA` size of 0), the alignment (`/Q`), comb cells, the background and border of
//! `/MK`, the marks of check boxes and radio buttons. What comes out is the content stream of an appearance.
//!
//! Text is measured with a [`Face`], the widths of the font that will draw it. Nothing here reads a file.

use std::fmt::Write;

use super::appearance::num;
use super::fontsub::Embedded;

/// Space between the border and the text, in points.
const PAD: f64 = 2.0;
/// The size an automatic font size starts from and the smallest it goes to (single line fields stay at 12 at most, as in Acrobat).
const AUTO_MAX: f64 = 12.0;
const AUTO_MIN: f64 = 4.0;
/// Most lines of a laid out text.
pub(crate) const MAX_LINES: usize = 2_000;

/// What the layout needs to know about a font.
pub(crate) trait Face {
    /// The width of a character in thousandths of an em.
    fn width(&self, c: char) -> f64;
    /// Ascent and descent in ems (the descent is below zero).
    fn ascent_descent(&self) -> (f64, f64);
    /// The string operand of `Tj` for `text`.
    fn encode(&self, text: &str) -> String;
}

impl Face for Embedded {
    fn width(&self, c: char) -> f64 {
        Embedded::width(self, c)
    }

    fn ascent_descent(&self) -> (f64, f64) {
        (self.asc_em, self.desc_em)
    }

    fn encode(&self, text: &str) -> String {
        Embedded::encode(self, text)
    }
}

/// The default appearance string of a field (12.7.3.3): the font name, its size and the colour operator.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Da {
    pub font: Vec<u8>,
    /// 0: left to us.
    pub size: f64,
    /// The colour as an operator with its operands (`0 g`).
    pub color: String,
}

/// Most operands kept while a `/DA` is read (the longest operator takes four, `Tf` two): a string of millions of numbers costs time but
/// no memory.
const MAX_OPERANDS: usize = 8;

/// Read a `/DA` string: the operands of `Tf` and of the last `g`, `rg` or `k`. Whatever else it holds is ignored. The colour is written
/// again in PDF number syntax (7.3.3: no exponent, four decimals at most) and kept between 0 and 1, whatever the string had.
pub(crate) fn parse_da(da: &[u8]) -> Da {
    let mut out = Da { font: Vec::new(), size: 0.0, color: "0 g".to_string() };
    let text = String::from_utf8_lossy(da);
    let mut operands: Vec<&str> = Vec::new();
    for token in text.split_ascii_whitespace() {
        match token {
            "Tf" => {
                if let [.., name, size] = operands.as_slice() {
                    out.font = name.trim_start_matches('/').as_bytes().to_vec();
                    out.size = size.parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0).unwrap_or(0.0).min(1000.0);
                }
                operands.clear();
            }
            "g" | "rg" | "k" => {
                let want = match token {
                    "g" => 1,
                    "rg" => 3,
                    _ => 4,
                };
                if operands.len() >= want {
                    let tail = operands.get(operands.len() - want..).unwrap_or_default();
                    let values: Vec<f64> = tail.iter().filter_map(|t| t.parse::<f64>().ok().filter(|v| v.is_finite())).collect();
                    if values.len() == want {
                        let numbers: Vec<String> = values.iter().map(|v| num(v.clamp(0.0, 1.0))).collect();
                        out.color = format!("{} {token}", numbers.join(" "));
                    }
                }
                operands.clear();
            }
            t if t.chars().next().is_some_and(|c| c.is_ascii_alphabetic() && !t.starts_with('/')) => operands.clear(),
            t => {
                if operands.len() >= MAX_OPERANDS {
                    operands.remove(0);
                }
                operands.push(t);
            }
        }
    }
    out
}

/// A colour of `/MK` (`/BG`, `/BC`; 1, 3 or 4 numbers, 12.5.6.19) as a fill or a stroke operator; `None` for no colour (an empty
/// array, anything else).
pub(crate) fn color_op(c: &[f64], stroke: bool) -> Option<String> {
    let unit = |v: f64| num(v.clamp(0.0, 1.0));
    let op = |fill: &'static str, line: &'static str| if stroke { line } else { fill };
    match c {
        [g] => Some(format!("{} {}", unit(*g), op("g", "G"))),
        [r, g, b] => Some(format!("{} {} {} {}", unit(*r), unit(*g), unit(*b), op("rg", "RG"))),
        [c, m, y, k] => Some(format!("{} {} {} {} {}", unit(*c), unit(*m), unit(*y), unit(*k), op("k", "K"))),
        _ => None,
    }
}

/// How the control is drawn apart from its contents (`/MK` and `/BS`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Style {
    /// Fill and stroke operators (`1 g`, `0 0 0 RG`).
    pub bg: Option<String>,
    pub bc: Option<String>,
    pub width: f64,
    pub dashed: bool,
    pub underline: bool,
    /// `/MK /CA`, the caption of a button or the mark of a check box (a character of ZapfDingbats).
    pub caption: String,
}

/// The background and the border, drawn first.
pub(crate) fn frame(style: &Style, w: f64, h: f64, round: bool) -> String {
    let mut s = String::new();
    if let Some(bg) = &style.bg {
        let _ = writeln!(s, "q\n{bg}");
        if round {
            circle(&mut s, w / 2.0, h / 2.0, w.min(h) / 2.0);
            s.push_str("f\n");
        } else {
            let _ = writeln!(s, "0 0 {} {} re f", num(w), num(h));
        }
        s.push_str("Q\n");
    }
    if let Some(bc) = style.bc.as_ref().filter(|_| style.width > 0.0) {
        let bw = style.width;
        let _ = writeln!(s, "q\n{bc}\n{} w", num(bw));
        if style.dashed {
            s.push_str("[3 3] 0 d\n");
        }
        if round {
            circle(&mut s, w / 2.0, h / 2.0, (w.min(h) - bw) / 2.0);
            s.push_str("S\n");
        } else if style.underline {
            let _ = writeln!(s, "0 {y} m {} {y} l S", num(w), y = num(bw / 2.0));
        } else {
            let _ = writeln!(s, "{h2} {h2} {} {} re S", num(w - bw), num(h - bw), h2 = num(bw / 2.0));
        }
        s.push_str("Q\n");
    }
    s
}

/// A circle as four curves (the path only).
fn circle(s: &mut String, cx: f64, cy: f64, r: f64) {
    const K: f64 = 0.552_284_75;
    let r = r.max(0.0);
    let k = r * K;
    let (x0, x1, y0, y1) = (cx - r, cx + r, cy - r, cy + r);
    let _ = writeln!(s, "{} {} m", num(x1), num(cy));
    let _ = writeln!(s, "{} {} {} {} {} {} c", num(x1), num(cy + k), num(cx + k), num(y1), num(cx), num(y1));
    let _ = writeln!(s, "{} {} {} {} {} {} c", num(cx - k), num(y1), num(x0), num(cy + k), num(x0), num(cy));
    let _ = writeln!(s, "{} {} {} {} {} {} c", num(x0), num(cy - k), num(cx - k), num(y0), num(cx), num(y0));
    let _ = writeln!(s, "{} {} {} {} {} {} c h", num(cx + k), num(y0), num(x1), num(cy - k), num(x1), num(cy));
}

/// The mark of a check box or radio button that is on: a check (`4`), a cross (`8`), a circle (`l`), a diamond (`u`) or a square
/// (`n`) of ZapfDingbats, drawn as a shape in the middle of the box; anything else is a check. `radio` makes a circle the default.
pub(crate) fn mark(style: &Style, color: &str, w: f64, h: f64, radio: bool) -> String {
    let kind = style.caption.chars().next().unwrap_or(if radio { 'l' } else { '4' });
    let side = w.min(h);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let mut s = String::new();
    let _ = writeln!(s, "q\n{color}");
    let stroke_color = color.replace(" g", " G").replace(" rg", " RG").replace(" k", " K");
    match kind {
        'l' => {
            circle(&mut s, cx, cy, side * 0.28);
            s.push_str("f\n");
        }
        'u' => {
            let r = side * 0.32;
            let _ = writeln!(s, "{} {} m {} {} l {} {} l {} {} l h f", num(cx), num(cy + r), num(cx + r), num(cy), num(cx), num(cy - r), num(cx - r), num(cy));
        }
        'n' => {
            let r = side * 0.25;
            let _ = writeln!(s, "{} {} {} {} re f", num(cx - r), num(cy - r), num(2.0 * r), num(2.0 * r));
        }
        '8' => {
            let r = side * 0.3;
            let _ = writeln!(s, "{stroke_color}\n{} w 1 J", num((side * 0.1).max(0.5)));
            let _ = writeln!(s, "{} {} m {} {} l {} {} m {} {} l S", num(cx - r), num(cy - r), num(cx + r), num(cy + r), num(cx - r), num(cy + r), num(cx + r), num(cy - r));
        }
        _ => {
            let _ = writeln!(s, "{stroke_color}\n{} w 1 J 1 j", num((side * 0.11).max(0.5)));
            let _ = writeln!(s, "{} {} m {} {} l {} {} l S", num(cx - side * 0.3), num(cy), num(cx - side * 0.08), num(cy - side * 0.24), num(cx + side * 0.32), num(cy + side * 0.28));
        }
    }
    s.push_str("Q\n");
    s
}

fn wide(c: char) -> bool {
    c >= '\u{2E80}'
}

/// A character a line does not begin with (closing punctuation).
fn no_line_start(c: char) -> bool {
    ",.;:?!)]}%，。、；：？！）］｝》」』】〕”’…".contains(c)
}

fn line_width(face: &dyn Face, line: &str, size: f64) -> f64 {
    line.chars().map(|c| face.width(c)).sum::<f64>() * size / 1000.0
}

/// `text` broken into lines no wider than `max` points at `size`: at the line ends of the text, after spaces and between
/// characters of Chinese, Japanese and Korean (not before closing punctuation); a word wider than a line is cut where it overflows.
/// At most `limit` lines; the flag says the text went on after them (more than white space was left out).
pub(crate) fn wrap(face: &dyn Face, text: &str, size: f64, max: f64, limit: usize) -> (Vec<String>, bool) {
    let scale = size / 1000.0;
    let mut lines: Vec<String> = Vec::new();
    let mut paragraphs = text.split('\n');
    while let Some(paragraph) = paragraphs.next() {
        if lines.len() >= limit {
            let cut = !paragraph.trim().is_empty() || paragraphs.any(|p| !p.trim().is_empty());
            return (lines, cut);
        }
        let chars: Vec<char> = paragraph.chars().collect();
        let (mut start, mut i, mut width) = (0usize, 0usize, 0.0f64);
        let mut breakable: Option<usize> = None;
        while i < chars.len() && lines.len() < limit {
            let Some(&c) = chars.get(i) else { break };
            let cw = face.width(c) * scale;
            if width + cw > max && i > start && c != ' ' {
                let at = breakable.filter(|&b| b > start).unwrap_or(i);
                lines.push(chars.get(start..at).unwrap_or_default().iter().collect::<String>().trim_end().to_string());
                start = at;
                width = chars.get(start..i).unwrap_or_default().iter().map(|&k| face.width(k) * scale).sum();
                breakable = None;
                continue;
            }
            width += cw;
            let next = chars.get(i + 1).copied();
            if (c == ' ' || wide(c) || next.is_some_and(wide)) && !next.is_some_and(no_line_start) {
                breakable = Some(i + 1);
            }
            i += 1;
        }
        if lines.len() >= limit {
            let cut = chars.get(start..).unwrap_or_default().iter().any(|c| !c.is_whitespace()) || paragraphs.any(|p| !p.trim().is_empty());
            return (lines, cut);
        }
        lines.push(chars.get(start..).unwrap_or_default().iter().collect());
    }
    (lines, false)
}

/// The size an automatic font size comes to: the biggest size from [`AUTO_MAX`] down to [`AUTO_MIN`] at which the text fits the
/// room (a single line by height and width, several lines by height).
pub(crate) fn auto_size(face: &dyn Face, text: &str, multiline: bool, room_w: f64, room_h: f64) -> f64 {
    let (asc, desc) = face.ascent_descent();
    let lead = asc - desc;
    let mut size = AUTO_MAX;
    while size > AUTO_MIN {
        let fits = if multiline { wrap(face, text, size, room_w, MAX_LINES).0.len() as f64 * lead * size <= room_h } else { lead * size <= room_h && line_width(face, text, size) <= room_w };
        if fits {
            return size;
        }
        size -= 1.0;
    }
    AUTO_MIN
}

/// What is laid out in a box.
pub(crate) struct Spec<'a> {
    pub text: &'a str,
    pub multiline: bool,
    /// 0 left, 1 centred, 2 right.
    pub q: i64,
    /// Cells of a comb field.
    pub comb: Option<usize>,
}

/// The content of a text field's appearance in a box of `w` by `h` (the border is `inset` wide), with the font used under the
/// resource name `font_name`. `Q` aligns lines; a comb puts each character in the middle of its cell. The flag says that the text
/// has more than [`MAX_LINES`] lines and the rest is left out.
pub(crate) fn text_content(face: &dyn Face, spec: &Spec<'_>, da: &Da, w: f64, h: f64, inset: f64, font_name: &str) -> (String, bool) {
    let pad = inset + PAD;
    let (room_w, room_h) = ((w - 2.0 * pad).max(1.0), (h - 2.0 * inset).max(1.0));
    let (asc, desc) = face.ascent_descent();
    let size = if da.size > 0.0 { da.size } else { auto_size(face, spec.text, spec.multiline, room_w, room_h) };
    let lead = (asc - desc) * size;
    let mut out = String::new();
    // The characters of a comb are far apart: a reader that takes the gaps for spaces is told what the text is (14.9.4).
    let comb = spec.comb.filter(|&n| n > 0 && !spec.multiline && !spec.text.is_empty());
    let span = match comb {
        Some(cells) => {
            let hex: String = spec.text.chars().take(cells).flat_map(|c| c.encode_utf16(&mut [0u16; 2]).iter().map(|u| format!("{u:04X}")).collect::<Vec<_>>()).collect();
            format!("/Span << /ActualText <FEFF{hex}> >> BDC\n")
        }
        None => String::new(),
    };
    let _ = writeln!(out, "/Tx BMC\nq\n{} {} {} {} re W n\n{span}BT\n/{font_name} {} Tf\n{}", num(inset), num(inset), num(w - 2.0 * inset), num(h - 2.0 * inset), num(size), da.color);
    if spec.text.is_empty() {
        out.push_str("ET\nQ\nEMC\n");
        return (out, false);
    }
    if let Some(cells) = comb {
        let chars: Vec<char> = spec.text.chars().take(cells).collect();
        let cell = (w - 2.0 * inset) / cells as f64;
        let first = match spec.q {
            1 => (cells - chars.len()) / 2,
            2 => cells - chars.len(),
            _ => 0,
        };
        let y = inset + (room_h - lead) / 2.0 - desc * size;
        let mut at = 0.0;
        for (k, c) in chars.iter().enumerate() {
            let cw = face.width(*c) * size / 1000.0;
            let x = inset + (first + k) as f64 * cell + (cell - cw) / 2.0;
            let _ = writeln!(out, "{} {} Td\n{} Tj", num(x - at), num(if k == 0 { y } else { 0.0 }), face.encode(&c.to_string()));
            at = x;
        }
        out.push_str("ET\nEMC\nQ\nEMC\n");
        return (out, false);
    }
    // (Only the lines that can be seen are laid out; the text is cut when the box could show more than MAX_LINES of them.)
    let visible = ((room_h / lead.max(0.1)) as usize).saturating_add(2);
    let (lines, more) = if spec.multiline { wrap(face, spec.text, size, room_w, visible.min(MAX_LINES)) } else { (vec![spec.text.replace('\n', " ")], false) };
    let cut = more && visible > MAX_LINES;
    let first_y = if spec.multiline { h - pad - asc * size } else { inset + (room_h - lead) / 2.0 - desc * size };
    let mut x_before = 0.0;
    for (k, line) in lines.iter().enumerate() {
        let lw = line_width(face, line, size);
        let x = pad + match spec.q {
            1 => ((room_w - lw) / 2.0).max(0.0),
            2 => (room_w - lw).max(0.0),
            _ => 0.0,
        };
        let dy = if k == 0 { first_y } else { -lead };
        let _ = writeln!(out, "{} {} Td\n{} Tj", num(x - x_before), num(dy), face.encode(line));
        x_before = x;
        if lead * (k + 1) as f64 > room_h + lead {
            break;
        }
    }
    out.push_str("ET\nQ\nEMC\n");
    (out, cut)
}

/// The content of a list box's appearance: the options (export value, shown text) one under another from `top`, those `chosen`
/// says so of on a coloured band. Only the lines that can be seen are looked at.
#[allow(clippy::too_many_arguments)]
pub(crate) fn list_content(face: &dyn Face, items: &[(String, String)], chosen: &dyn Fn(&str, &str) -> bool, top: usize, da: &Da, w: f64, h: f64, inset: f64, font_name: &str) -> String {
    let size = if da.size > 0.0 { da.size } else { 12.0 };
    let (asc, desc) = face.ascent_descent();
    let lead = (asc - desc) * size;
    let pad = inset + PAD;
    let mut out = format!("/Tx BMC\nq\n{} {} {} {} re W n\n", num(inset), num(inset), num(w - 2.0 * inset), num(h - 2.0 * inset));
    let mut y = h - inset;
    let mut text = String::new();
    for (export, display) in items.iter().skip(top).take(((h / lead.max(1.0)) as usize).saturating_add(1)) {
        let chosen = chosen(export, display);
        if chosen {
            // The chosen ones are white on dark blue, as PDFium draws them.
            let _ = writeln!(out, "0 0.2 0.44 rg {} {} {} {} re f", num(inset), num(y - lead), num(w - 2.0 * inset), num(lead));
        }
        let color = if chosen { "1 g" } else { da.color.as_str() };
        let _ = writeln!(text, "{color}\n1 0 0 1 {} {} Tm\n{} Tj", num(pad), num(y - asc * size), face.encode(&display.replace('\n', " ")));
        y -= lead;
    }
    let _ = writeln!(out, "BT\n/{font_name} {} Tf\n{}\n{text}ET\nQ\nEMC", num(size), da.color);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character 500 thousandths wide (1000 for those of Chinese), 0.8 above and 0.2 below the baseline.
    struct Fixed;

    impl Face for Fixed {
        fn width(&self, c: char) -> f64 {
            if wide(c) { 1000.0 } else { 500.0 }
        }

        fn ascent_descent(&self) -> (f64, f64) {
            (0.8, -0.2)
        }

        fn encode(&self, text: &str) -> String {
            format!("<{}>", text.chars().map(|c| format!("{:04X}", c as u32)).collect::<String>())
        }
    }

    #[test]
    fn the_default_appearance_gives_font_size_and_colour() {
        let d = parse_da(b"/Helv 9 Tf 0 0 1 rg");
        assert_eq!((d.font.as_slice(), d.size, d.color.as_str()), (b"Helv".as_slice(), 9.0, "0 0 1 rg"));
        let d = parse_da(b"0 g /Arial,Bold 0 Tf");
        assert_eq!((d.font.as_slice(), d.size, d.color.as_str()), (b"Arial,Bold".as_slice(), 0.0, "0 g"));
        let d = parse_da(b"nonsense 1 2 3 (x)");
        assert_eq!((d.size, d.color.as_str()), (0.0, "0 g"));
        assert_eq!(parse_da(b"/F1 -4 Tf").size, 0.0);
        // Colours are written as PDF numbers: no exponent, no plus sign, kept between 0 and 1.
        assert_eq!(parse_da(b"/F1 9 Tf 1e-2 +.5 2 rg").color, "0.01 0.5 1 rg");
        assert_eq!(parse_da(b"1.5e0 g").color, "1 g");
        assert_eq!(parse_da(b"0 0 0.5.3 rg").color, "0 g");
        // A string of millions of numbers keeps only the last operands.
        let long = format!("{}0.25 g", "9 ".repeat(1_000_000));
        assert_eq!(parse_da(long.as_bytes()).color, "0.25 g");
    }

    #[test]
    fn colours_come_as_operators() {
        assert_eq!(color_op(&[0.5], false).as_deref(), Some("0.5 g"));
        assert_eq!(color_op(&[1.0, 0.0, 0.0], true).as_deref(), Some("1 0 0 RG"));
        assert_eq!(color_op(&[0.0, 0.0, 0.0, 1.0], false).as_deref(), Some("0 0 0 1 k"));
        assert_eq!(color_op(&[], false), None);
        assert_eq!(color_op(&[0.1, 0.2], false), None);
    }

    #[test]
    fn lines_break_after_spaces_between_chinese_characters_and_not_before_punctuation() {
        // 10 points wide a character at size 20 (500/1000), a line of 100 holds 10 of them.
        let lines = wrap(&Fixed, "aaaa bbbb cccc", 20.0, 100.0, MAX_LINES).0;
        assert_eq!(lines, vec!["aaaa bbbb", "cccc"]);
        // Chinese characters are 20 wide: five to the line, and a comma does not begin a line.
        let lines = wrap(&Fixed, "一二三四五，六七", 20.0, 100.0, MAX_LINES).0;
        assert_eq!(lines, vec!["一二三四", "五，六七"]);
        // A word wider than the line is cut.
        assert_eq!(wrap(&Fixed, "abcdefghijklmnop", 20.0, 100.0, MAX_LINES).0, vec!["abcdefghij", "klmnop"]);
        // Line ends in the text are kept.
        assert_eq!(wrap(&Fixed, "a\n\nb", 20.0, 100.0, MAX_LINES), (vec!["a".to_string(), String::new(), "b".to_string()], false));
        // Never more than the limit, and the cut is told. Text that ends exactly at the limit (or only has white space after it) is not cut.
        let (lines, cut) = wrap(&Fixed, &"x\n".repeat(MAX_LINES + 50), 10.0, 100.0, MAX_LINES);
        assert_eq!((lines.len(), cut), (MAX_LINES, true));
        assert!(!wrap(&Fixed, &"x\n".repeat(MAX_LINES), 10.0, 100.0, MAX_LINES).1);
        // A shorter limit is the caller's: the lines beyond it are told.
        assert_eq!(wrap(&Fixed, "a\nb\nc", 10.0, 100.0, 2), (vec!["a".to_string(), "b".to_string()], true));
        assert!(!wrap(&Fixed, &format!("{}x  \n\n", "x\n".repeat(MAX_LINES - 1)), 10.0, 100.0, MAX_LINES).1);
        assert!(wrap(&Fixed, &"x".repeat(MAX_LINES * 11), 20.0, 100.0, MAX_LINES).1);
    }

    #[test]
    fn an_automatic_size_fits_the_room() {
        // One line of ten characters in 100 x 30: 12 is the most; in 40 x 30 it is smaller (500/1000 of the size a character).
        assert_eq!(auto_size(&Fixed, "aaaaaaaaaa", false, 100.0, 30.0), 12.0);
        assert_eq!(auto_size(&Fixed, "aaaaaaaaaa", false, 40.0, 30.0), 8.0);
        // Too long for any size: the smallest.
        assert_eq!(auto_size(&Fixed, "aaaaaaaaaa", false, 5.0, 30.0), AUTO_MIN);
    }

    #[test]
    fn text_is_placed_by_alignment_and_a_comb_gets_a_character_in_each_cell() {
        let da = Da { font: b"Helv".to_vec(), size: 10.0, color: "0 g".to_string() };
        let spec = |text: &'static str, q: i64, comb: Option<usize>| Spec { text, multiline: false, q, comb };
        let left = text_content(&Fixed, &spec("ab", 0, None), &da, 100.0, 20.0, 1.0, "F0").0;
        assert!(left.contains("/F0 10 Tf") && left.contains("3 ") && left.contains("<00610062> Tj"), "{left}");
        let right = text_content(&Fixed, &spec("ab", 2, None), &da, 100.0, 20.0, 1.0, "F0").0;
        // The line is 10 wide; the room is 94 (100 less 3 on each side): it begins at 3 + 84.
        assert!(right.contains("\n87 "), "{right}");
        let comb = text_content(&Fixed, &spec("ab", 0, Some(4)), &da, 100.0, 20.0, 0.0, "F0").0;
        assert_eq!(comb.matches(" Tj").count(), 2, "{comb}");
        // Cells are 25 wide: the first character (5 wide) begins at 10, the second 25 further on.
        assert!(comb.contains("10 ") && comb.contains("\n25 0 Td"), "{comb}");
    }

    #[test]
    fn lines_that_cannot_be_seen_are_not_laid_out_and_lines_that_could_be_but_are_not_laid_out_are_told() {
        let da = Da { font: b"Helv".to_vec(), size: 10.0, color: "0 g".to_string() };
        let spec = Spec { text: &"a\n".repeat(MAX_LINES + 100), multiline: true, q: 0, comb: None };
        // A small box shows a few lines: the rest of the text does not matter.
        assert!(!text_content(&Fixed, &spec, &da, 100.0, 40.0, 1.0, "F0").1);
        // A box that could show more lines than the layout does is cut, and says so.
        assert!(text_content(&Fixed, &spec, &da, 100.0, 100_000.0, 1.0, "F0").1);
        let short = Spec { text: &"a\n".repeat(100), multiline: true, q: 0, comb: None };
        assert!(!text_content(&Fixed, &short, &da, 100.0, 100_000.0, 1.0, "F0").1);
    }

    #[test]
    fn marks_and_frames_are_drawn() {
        let style = Style { bg: Some("1 g".to_string()), bc: Some("0 G".to_string()), width: 1.0, ..Style::default() };
        let f = frame(&style, 20.0, 10.0, false);
        assert!(f.contains("0 0 20 10 re f") && f.contains("0.5 0.5 19 9 re S"), "{f}");
        assert!(frame(&style, 20.0, 20.0, true).contains(" c"));
        assert!(mark(&style, "0 g", 20.0, 20.0, false).contains(" l S"));
        assert!(mark(&style, "0 g", 20.0, 20.0, true).contains(" c h"));
        assert!(frame(&Style::default(), 20.0, 10.0, false).is_empty());
    }
}

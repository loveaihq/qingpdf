//! The loops of making a picture smaller, where speed decides: the JPEG encoder ([`jpeg`]) and the averaging down to a smaller size
//! ([`average`]). `qingpdf-core` (built for size) decides what to make smaller and how; these run on its worker threads.

pub mod jpeg;

/// For each of `dst` pixels along a line of `src` pixels: the first source pixel, one past the last, and how much of the first and of
/// the last lies under it (in source pixels; the ones between count whole). Averaging over this is the area under a box the size of
/// a destination pixel laid over the source.
fn spans(src: usize, dst: usize) -> Vec<(usize, usize, f32, f32)> {
    let ratio = src as f64 / dst as f64;
    (0..dst)
        .map(|d| {
            let (x0, x1) = (d as f64 * ratio, ((d + 1) as f64 * ratio).min(src as f64));
            let first = (x0.floor() as usize).min(src.saturating_sub(1));
            let end = (x1.ceil() as usize).clamp(first + 1, src);
            let left = (((first + 1) as f64).min(x1) - x0).max(0.0);
            let right = (x1 - (end - 1) as f64).max(0.0);
            (first, end, left as f32, right as f32)
        })
        .collect()
}

/// How much of the source pixel `at` of a span `(first, end, left, right)` counts.
fn share(at: usize, (first, end, left, right): (usize, usize, f32, f32)) -> f32 {
    if at == first {
        left
    } else if at + 1 == end {
        right
    } else {
        1.0
    }
}

/// The samples of a picture of `w` by `h` pixels (`comps` interleaved 8-bit samples to a pixel) averaged down to `dw` by `dh`, neither
/// more than the picture has nor less than 1: the colour of each new pixel is the average of the old ones it covers, each in proportion to
/// the part of it that is covered. A line at a time, so that only a line of sums is held. A picture that has not `w * h * comps`
/// samples is read as if the missing ones were 0.
pub fn average(data: &[u8], (w, h): (usize, usize), comps: usize, (dw, dh): (usize, usize)) -> Vec<u8> {
    let (w, h, c) = (w.max(1), h.max(1), comps.max(1));
    let (dw, dh) = (dw.clamp(1, w), dh.clamp(1, h));
    let (across, down) = (spans(w, dw), spans(h, dh));
    let (norm_x, norm_y) = (dw as f32 / w as f32, dh as f32 / h as f32);
    let mut out = Vec::with_capacity(dw * dh * c);
    // A source line made `dw` pixels wide (in floats), kept while the next destination line still needs it.
    let mut narrow = vec![0.0f32; dw * c];
    let mut cached: Option<usize> = None;
    let mut sum = vec![0.0f32; dw * c];
    for &span in &down {
        sum.fill(0.0);
        for y in span.0..span.1 {
            if cached != Some(y) {
                let row = data.get(y * w * c..(y + 1) * w * c).unwrap_or(&[]);
                for (slot, &across) in narrow.chunks_exact_mut(c).zip(&across) {
                    for (k, v) in slot.iter_mut().enumerate() {
                        let mut total = 0.0f32;
                        for x in across.0..across.1 {
                            total += share(x, across) * f32::from(row.get(x * c + k).copied().unwrap_or(0));
                        }
                        *v = total * norm_x;
                    }
                }
                cached = Some(y);
            }
            let weight = share(y, span);
            for (a, &n) in sum.iter_mut().zip(&narrow) {
                *a += weight * n;
            }
        }
        out.extend(sum.iter().map(|&v| (v * norm_y + 0.5).clamp(0.0, 255.0) as u8));
    }
    out
}

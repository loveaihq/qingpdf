//! Sorting for the paths that are not hot, through a few shared copies of the code.
//!
//! Each call of `slice::sort*` with a type and a closure of its own is compiled into a copy of the sorting code of its own (1 to
//! 7 KB each, and there were about 45 calls). The code that reads a font's tables, writes a file or orders the pieces the reader
//! asks for goes through here instead: the sort is a plain merge sort of numbers, with the position as the tie-break, so what
//! is equal stays in the order it was in (a stable sort, like `sort`, `sort_by` and `sort_by_key`). The hot paths of drawing
//! and reading do not sort through here.

use std::cmp::Ordering;
use std::collections::BTreeSet;

/// The number that stands for `x` in the order of [`f64::total_cmp`] (so `-0.0` is before `0.0`, and the not-a-numbers are at the ends).
pub fn f64_key(x: f64) -> u64 {
    let bits = x.to_bits();
    if bits >> 63 == 1 { !bits } else { bits | (1 << 63) }
}

/// The number that stands for `x` in the order of [`f32::total_cmp`], and [`f32_from_key`] is the way back.
pub fn f32_key(x: f32) -> u64 {
    let bits = x.to_bits();
    u64::from(if bits >> 31 == 1 { !bits } else { bits | (1 << 31) })
}

/// The `f32` that [`f32_key`] made `key` of (the low 32 bits of `key`).
pub fn f32_from_key(key: u64) -> f32 {
    let key = key as u32;
    f32::from_bits(if key >> 31 == 1 { key ^ (1 << 31) } else { !key })
}

/// The number that stands for `x` in the order of the integers.
pub fn i64_key(x: i64) -> u64 {
    (x as u64) ^ (1 << 63)
}

/// Runs of this many are put in order by insertion; the merges start from there.
const RUN: usize = 8;

/// A stable sort of numbers or small plain values: `le(a, b)` says that `a` may stay before `b`.
fn merge_sort<T: Copy>(v: &mut [T], le: &impl Fn(&T, &T) -> bool) {
    for run in v.chunks_mut(RUN) {
        for i in 1..run.len() {
            let mut j = i;
            while j > 0 {
                let (Some(before), Some(here)) = (run.get(j - 1), run.get(j)) else { break };
                if le(before, here) {
                    break;
                }
                run.swap(j - 1, j);
                j -= 1;
            }
        }
    }
    if v.len() <= RUN {
        return;
    }
    let mut other = v.to_vec();
    let (mut width, mut in_v) = (RUN, true);
    while width < v.len() {
        if in_v {
            merge_pass(v, &mut other, width, le);
        } else {
            merge_pass(&other, v, width, le);
        }
        in_v = !in_v;
        width = width.saturating_mul(2);
    }
    if !in_v {
        v.copy_from_slice(&other);
    }
}

/// Merges the neighbouring runs of `width` items of `src` into `dst`.
fn merge_pass<T: Copy>(src: &[T], dst: &mut [T], width: usize, le: &impl Fn(&T, &T) -> bool) {
    let mut start = 0;
    while start < src.len() {
        let mid = start.saturating_add(width).min(src.len());
        let end = mid.saturating_add(width).min(src.len());
        let (Some(left), Some(right), Some(out)) = (src.get(start..mid), src.get(mid..end), dst.get_mut(start..end)) else { return };
        let (mut i, mut j) = (0, 0);
        for slot in out {
            let next = match (left.get(i), right.get(j)) {
                (Some(a), Some(b)) if !le(a, b) => {
                    j += 1;
                    Some(b)
                }
                (Some(a), _) => {
                    i += 1;
                    Some(a)
                }
                (None, b) => {
                    j += 1;
                    b
                }
            };
            if let Some(&item) = next {
                *slot = item;
            }
        }
        start = end;
    }
}

/// Sorts numbers.
pub fn sort_u64(v: &mut [u64]) {
    merge_sort(v, &|a, b| a <= b);
}

/// The positions `0..keys.len()` in the stable order of their keys.
fn order_of(keys: &[u64]) -> Vec<usize> {
    let mut pairs: Vec<(u64, usize)> = keys.iter().copied().zip(0..).collect();
    merge_sort(&mut pairs, &|a, b| a <= b);
    pairs.into_iter().map(|p| p.1).collect()
}

/// The positions `0..n` in the stable order `cmp` says.
fn order_by(n: usize, cmp: &dyn Fn(usize, usize) -> Ordering) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    merge_sort(&mut order, &|&a, &b| cmp(a, b) != Ordering::Greater);
    order
}

/// Puts the items in the places `order` names (`order[k]` is the place the item that ends up at `k` came from).
fn permute<T>(items: &mut Vec<T>, order: &[usize]) {
    let mut slots: Vec<Option<T>> = items.drain(..).map(Some).collect();
    items.extend(order.iter().filter_map(|&i| slots.get_mut(i).and_then(Option::take)));
}

/// `items.into_iter().collect::<BTreeSet<_>>()` without the sort that `collect` does first (a copy of the sorting code for each type).
pub fn set_of<T: Ord>(items: impl IntoIterator<Item = T>) -> BTreeSet<T> {
    let mut set = BTreeSet::new();
    set.extend(items);
    set
}

/// `items.sort_by_key(key)`, where the key is a number: a stable sort (what has the same key stays in the order it was in).
pub fn sort_by_key<T>(items: &mut Vec<T>, key: impl Fn(&T) -> u64) {
    let keys: Vec<u64> = items.iter().map(key).collect();
    if keys.is_sorted() {
        return;
    }
    permute(items, &order_of(&keys));
}

/// `items.sort_by(cmp)`: a stable sort.
pub fn sort_by<T>(items: &mut Vec<T>, cmp: impl Fn(&T, &T) -> Ordering) {
    if items.is_sorted_by(|a, b| cmp(a, b) != Ordering::Greater) {
        return;
    }
    let order = order_by(items.len(), &|a, b| match (items.get(a), items.get(b)) {
        (Some(x), Some(y)) => cmp(x, y),
        _ => Ordering::Equal,
    });
    permute(items, &order);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numbers that repeat, from a fixed seed.
    fn numbers(n: usize, seed: u64, modulus: u64) -> Vec<u64> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                (state >> 33) % modulus
            })
            .collect()
    }

    #[test]
    fn numbers_come_out_as_the_standard_sort_has_them() {
        for n in (0..70).chain([127, 128, 129, 1000, 4097]) {
            for modulus in [1, 3, 50, u64::MAX] {
                let mut mine = numbers(n, n as u64 + modulus, modulus);
                let mut std_sorted = mine.clone();
                std_sorted.sort_unstable();
                sort_u64(&mut mine);
                assert_eq!(mine, std_sorted, "n = {n}, modulus = {modulus}");
            }
        }
    }

    #[test]
    fn a_sort_by_key_is_stable_like_the_standard_one() {
        for n in (0..70).chain([200, 1000, 3001]) {
            for modulus in [1, 4, 40, 1_000_000] {
                let keys = numbers(n, 7 + n as u64, modulus);
                let items: Vec<(u64, usize)> = keys.iter().copied().zip(0..).collect();
                let mut mine = items.clone();
                sort_by_key(&mut mine, |e| e.0);
                let mut std_sorted = items;
                std_sorted.sort_by_key(|e| e.0);
                assert_eq!(mine, std_sorted, "n = {n}, modulus = {modulus}");
            }
        }
    }

    #[test]
    fn a_sort_by_is_stable_like_the_standard_one() {
        for n in (0..70).chain([200, 1000]) {
            let items: Vec<(Vec<u8>, usize)> = numbers(n, 3 + n as u64, 12).into_iter().zip(0..).map(|(k, i)| (k.to_string().into_bytes(), i)).collect();
            let mut mine = items.clone();
            sort_by(&mut mine, |a, b| a.0.cmp(&b.0));
            let mut std_sorted = items;
            std_sorted.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(mine, std_sorted, "n = {n}");
        }
    }

    #[test]
    fn items_that_are_in_order_already_are_left_alone() {
        let mut v = vec![1u8, 1, 2, 3, 3];
        sort_by_key(&mut v, |&x| u64::from(x));
        assert_eq!(v, [1, 1, 2, 3, 3]);
        sort_by(&mut v, |a, b| a.cmp(b));
        assert_eq!(v, [1, 1, 2, 3, 3]);
        let mut empty: Vec<u8> = Vec::new();
        sort_by_key(&mut empty, |&x| u64::from(x));
        sort_by(&mut empty, |a, b| a.cmp(b));
        assert!(empty.is_empty());
    }

    #[test]
    fn the_keys_of_the_floating_point_numbers_are_in_the_order_of_total_cmp() {
        let specials = [f64::NEG_INFINITY, -1e300, -2.5, -1.0, -f64::MIN_POSITIVE, -0.0, 0.0, f64::MIN_POSITIVE, 1.0, 2.5, 1e300, f64::INFINITY, f64::NAN, -f64::NAN];
        for &a in &specials {
            for &b in &specials {
                assert_eq!(f64_key(a).cmp(&f64_key(b)), a.total_cmp(&b), "{a} {b}");
                let (a, b) = (a as f32, b as f32);
                assert_eq!(f32_key(a).cmp(&f32_key(b)), a.total_cmp(&b), "{a} {b}");
                assert_eq!(f32_from_key(f32_key(a)).to_bits(), a.to_bits());
            }
        }
        for (a, b) in [(i64::MIN, -1), (-1, 0), (0, 1), (1, i64::MAX), (-5, -4)] {
            assert!(i64_key(a) < i64_key(b));
        }
    }
}

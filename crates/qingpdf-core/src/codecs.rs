//! The simple stream decoders: ASCIIHexDecode (ISO 32000-1 7.4.2),
//! ASCII85Decode (7.4.3), LZWDecode (7.4.4) and RunLengthDecode (7.4.5). Each
//! appends to `out` and never lets it grow past `cap` bytes; the caller
//! charges what was produced to the decoding budget, whether the decoder
//! succeeded or not.

/// Why a decoder stopped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Fail {
    /// The data breaks the rules of the encoding.
    Corrupt(&'static str),
    /// The output would exceed the cap.
    Over,
}

type Res = Result<(), Fail>;

fn room(out: &[u8], extra: usize, cap: usize) -> Res {
    if out.len().saturating_add(extra) > cap { Err(Fail::Over) } else { Ok(()) }
}

/// White space (7.2.2, Table 1).
fn is_white(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

/// 7.4.2: pairs of hex digits, white space ignored, `>` is the end, anything
/// else is an error; an odd last digit counts as followed by 0.
pub(crate) fn ascii_hex(input: &[u8], out: &mut Vec<u8>, cap: usize) -> Res {
    let mut high: Option<u8> = None;
    for &b in input {
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            b'>' => break,
            b if is_white(b) => continue,
            _ => return Err(Fail::Corrupt("invalid character in ASCIIHexDecode data")),
        };
        match high.take() {
            Some(h) => {
                room(out, 1, cap)?;
                out.push(h << 4 | v);
            }
            None => high = Some(v),
        }
    }
    if let Some(h) = high {
        room(out, 1, cap)?;
        out.push(h << 4);
    }
    Ok(())
}

/// 7.4.3: groups of five characters `!`..`u` make four bytes, `z` stands for
/// four zero bytes, `~>` ends the data (a missing end marker is tolerated, and
/// so is the `<~` some writers put in front). A last group of 2 to 4
/// characters is padded with `u`.
pub(crate) fn ascii85(input: &[u8], out: &mut Vec<u8>, cap: usize) -> Res {
    let mut rest = input;
    let skip = rest.iter().take_while(|&&b| is_white(b)).count();
    rest = rest.get(skip..).unwrap_or(&[]);
    if let Some(after) = rest.strip_prefix(b"<~") {
        rest = after;
    }
    let mut group = [84u8; 5];
    let mut n = 0usize;
    for &b in rest {
        match b {
            b'~' => break,
            b if is_white(b) => {}
            b'z' => {
                if n != 0 {
                    return Err(Fail::Corrupt("z in the middle of an ASCII85Decode group"));
                }
                room(out, 4, cap)?;
                out.extend_from_slice(&[0; 4]);
            }
            b'!'..=b'u' => {
                if let Some(slot) = group.get_mut(n) {
                    *slot = b - b'!';
                }
                n += 1;
                if n == 5 {
                    let value = group_value(&group)?;
                    room(out, 4, cap)?;
                    out.extend_from_slice(&value.to_be_bytes());
                    n = 0;
                }
            }
            _ => return Err(Fail::Corrupt("invalid character in ASCII85Decode data")),
        }
    }
    match n {
        0 => Ok(()),
        1 => Err(Fail::Corrupt("ASCII85Decode data ends with a single character")),
        _ => {
            // The characters not present count as `u`.
            for slot in group.iter_mut().skip(n) {
                *slot = 84;
            }
            let value = group_value(&group)?;
            let keep = n - 1;
            room(out, keep, cap)?;
            out.extend_from_slice(value.to_be_bytes().get(..keep).unwrap_or(&[]));
            Ok(())
        }
    }
}

fn group_value(group: &[u8; 5]) -> Result<u32, Fail> {
    let v = group.iter().fold(0u64, |acc, &d| acc * 85 + u64::from(d));
    u32::try_from(v).map_err(|_| Fail::Corrupt("ASCII85Decode group is larger than 2^32 - 1"))
}

/// 7.4.5: a length byte 0..=127 is followed by that many plus one literal
/// bytes, 129..=255 by one byte to repeat 257 minus the length times, 128 ends
/// the data. Data that stops early gives what was read.
pub(crate) fn run_length(input: &[u8], out: &mut Vec<u8>, cap: usize) -> Res {
    let mut pos = 0usize;
    while let Some(&n) = input.get(pos) {
        pos += 1;
        match n {
            128 => break,
            0..=127 => {
                let len = usize::from(n) + 1;
                let from = input.get(pos..).unwrap_or(&[]);
                let chunk = from.get(..len).unwrap_or(from);
                room(out, chunk.len(), cap)?;
                out.extend_from_slice(chunk);
                pos += len;
            }
            _ => {
                let Some(&b) = input.get(pos) else { break };
                pos += 1;
                let len = 257 - usize::from(n);
                room(out, len, cap)?;
                out.resize(out.len() + len, b);
            }
        }
    }
    Ok(())
}

const TABLE: usize = 4096;
const CLEAR: usize = 256;
const END: usize = 257;
const FIRST_FREE: usize = 258;

/// 7.4.4.2: LZW with codes of 9 to 12 bits, most significant bit first. With
/// `early_change` (the default) the code length grows one code early. Ends at
/// the end-of-data code or when the input runs out.
pub(crate) fn lzw(input: &[u8], early_change: bool, out: &mut Vec<u8>, cap: usize) -> Res {
    // Entry i is the string of entry prefix[i] followed by suffix[i]; `first` is the
    // first byte of the string and `len` its length.
    let mut prefix = [0u16; TABLE];
    let mut suffix = [0u8; TABLE];
    let mut first = [0u8; TABLE];
    let mut len = [0u16; TABLE];
    for i in 0..CLEAR {
        let b = u8::try_from(i).unwrap_or(0);
        if let (Some(s), Some(f), Some(l)) = (suffix.get_mut(i), first.get_mut(i), len.get_mut(i)) {
            *s = b;
            *f = b;
            *l = 1;
        }
    }
    let early = usize::from(early_change);
    let mut next = FIRST_FREE;
    let mut bits = 9u32;
    let mut prev: Option<usize> = None;
    let mut acc = 0u32;
    let mut have = 0u32;
    let mut input = input.iter();
    loop {
        while have < bits {
            match input.next() {
                Some(&b) => {
                    acc = acc << 8 | u32::from(b);
                    have += 8;
                }
                None => return Ok(()),
            }
        }
        have -= bits;
        let code = usize::try_from(acc >> have).unwrap_or(0) & ((1 << bits) - 1);
        acc &= (1 << have) - 1;
        if code == CLEAR {
            next = FIRST_FREE;
            bits = 9;
            prev = None;
            continue;
        }
        if code == END {
            return Ok(());
        }
        let mut grow = None;
        match prev {
            // After a clear, only a literal makes sense.
            None if code >= CLEAR => return Err(Fail::Corrupt("bad code in LZWDecode data")),
            None => {}
            Some(p) => {
                // The code of the entry about to be made is allowed: the string is
                // the previous one followed by its own first byte.
                let known = code < next;
                if !known && code != next {
                    return Err(Fail::Corrupt("bad code in LZWDecode data"));
                }
                let f = if known { first.get(code) } else { first.get(p) };
                grow = Some((p, f.copied().unwrap_or(0)));
            }
        }
        if let Some((p, f)) = grow
            && next < TABLE
        {
            let new_first = first.get(p).copied().unwrap_or(0);
            let new_len = len.get(p).copied().unwrap_or(0).saturating_add(1);
            if let (Some(pre), Some(suf), Some(fi), Some(l)) =
                (prefix.get_mut(next), suffix.get_mut(next), first.get_mut(next), len.get_mut(next))
            {
                *pre = u16::try_from(p).unwrap_or(0);
                *suf = f;
                *fi = new_first;
                *l = new_len;
                next += 1;
            }
        }
        // Write the string of `code` (it exists now, also in the special case).
        let n = usize::from(len.get(code).copied().unwrap_or(0));
        if n == 0 {
            return Err(Fail::Corrupt("bad code in LZWDecode data"));
        }
        room(out, n, cap)?;
        let start = out.len();
        out.resize(start + n, 0);
        let mut c = code;
        for i in (0..n).rev() {
            if let Some(slot) = out.get_mut(start + i) {
                *slot = suffix.get(c).copied().unwrap_or(0);
            }
            c = usize::from(prefix.get(c).copied().unwrap_or(0));
        }
        prev = Some(code);
        if next + early >= (1 << bits) && bits < 12 {
            bits += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 1 << 20;

    fn run(f: impl Fn(&[u8], &mut Vec<u8>, usize) -> Res, input: &[u8]) -> Result<Vec<u8>, Fail> {
        let mut out = Vec::new();
        f(input, &mut out, CAP).map(|()| out)
    }

    /// An LZW encoder for the tests: codes of `bits`.. as 7.4.4.2 describes, with a clear code first
    /// and the end code last.
    fn lzw_encode(data: &[u8], early_change: bool) -> Vec<u8> {
        use std::collections::HashMap;
        let early = usize::from(early_change);
        let mut codes: Vec<usize> = vec![256];
        let mut dict: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut next = 258usize;
        let mut cur: Vec<u8> = Vec::new();
        for &b in data {
            let mut trial = cur.clone();
            trial.push(b);
            if trial.len() == 1 || dict.contains_key(&trial) {
                cur = trial;
            } else {
                let code = if cur.len() == 1 { usize::from(cur[0]) } else { dict[&cur] };
                codes.push(code);
                dict.insert(trial, next);
                next += 1;
                cur = vec![b];
                if next == 4095 {
                    codes.push(256);
                    dict.clear();
                    next = 258;
                }
            }
        }
        if !cur.is_empty() {
            codes.push(if cur.len() == 1 { usize::from(cur[0]) } else { dict[&cur] });
        }
        codes.push(257);
        // Pack: the width follows the decoder's table size, which lags the encoder's by one entry.
        let mut bits_out = Vec::new();
        let mut width = 9u32;
        let mut table = 258usize;
        let mut prev_code = false;
        for &c in &codes {
            for i in (0..width).rev() {
                bits_out.push((c >> i) & 1 == 1);
            }
            if c == 256 {
                table = 258;
                width = 9;
                prev_code = false;
                continue;
            }
            if prev_code {
                table += 1;
            }
            prev_code = true;
            if table + early >= (1 << width) && width < 12 {
                width += 1;
            }
        }
        bits_out
            .chunks(8)
            .map(|c| c.iter().enumerate().fold(0u8, |a, (i, &b)| a | (u8::from(b) << (7 - i))))
            .collect()
    }

    #[test]
    fn ascii_hex_vectors() {
        assert_eq!(run(ascii_hex, b"48 65 6C6c 6F>").unwrap(), b"Hello");
        assert_eq!(run(ascii_hex, b"48656C6C6F").unwrap(), b"Hello");
        // An odd last digit is followed by 0 (7.4.2); data after > is ignored.
        assert_eq!(run(ascii_hex, b"7>ff").unwrap(), [0x70]);
        assert_eq!(run(ascii_hex, b"").unwrap(), b"");
        assert!(matches!(run(ascii_hex, b"4x"), Err(Fail::Corrupt(_))));
        let mut out = Vec::new();
        assert_eq!(ascii_hex(b"41424344", &mut out, 3), Err(Fail::Over));
    }

    #[test]
    fn ascii85_vectors() {
        // Vectors made with Python's base64.a85encode.
        assert_eq!(run(ascii85, b"87cURD]i,\"Ebo8@~>").unwrap(), b"Hello World1");
        assert_eq!(run(ascii85, b"<~87cURD]i,\"Ebo8@~>").unwrap(), b"Hello World1");
        assert_eq!(run(ascii85, b"87cURD_*#4DfTZ)+T~>").unwrap(), b"Hello, World!");
        // A full group, then final groups of 2 to 4 characters (n characters give n - 1 bytes).
        assert_eq!(run(ascii85, b"87cUR~>").unwrap(), b"Hell");
        assert_eq!(run(ascii85, b"87cURDZ~>").unwrap(), b"Hello");
        assert_eq!(run(ascii85, b"87cURD]f~>").unwrap(), b"Hello ");
        assert_eq!(run(ascii85, b"87cURD]i*~>").unwrap(), b"Hello W");
        // z is four zero bytes; white space is ignored; the end marker may be missing.
        assert_eq!(run(ascii85, b"z
87 cUR").unwrap(), [0, 0, 0, 0, b'H', b'e', b'l', b'l']);
        assert_eq!(run(ascii85, b"~>").unwrap(), b"");
        assert_eq!(run(ascii85, b"s8W-!~>").unwrap(), [0xff; 4]);
    }

    #[test]
    fn ascii85_errors() {
        // z inside a group, a value above 2^32 - 1, a lone final character, a stray character.
        assert!(matches!(run(ascii85, b"87zcUR~>"), Err(Fail::Corrupt(_))));
        assert!(matches!(run(ascii85, b"s8W-\"~>"), Err(Fail::Corrupt(_))));
        assert!(matches!(run(ascii85, b"uuuuu~>"), Err(Fail::Corrupt(_))));
        assert!(matches!(run(ascii85, b"87cUR D~>"), Err(Fail::Corrupt(_))));
        assert!(matches!(run(ascii85, b"87cUR\x80~>"), Err(Fail::Corrupt(_))));
        let mut out = Vec::new();
        assert_eq!(ascii85(b"zzzz", &mut out, 8), Err(Fail::Over));
    }

    #[test]
    fn run_length_vectors() {
        // 7.4.5: 0..=127 literal runs, 129..=255 repeats, 128 ends.
        assert_eq!(run(run_length, &[2, b'a', b'b', b'c', 254, b'x', 128, 9, 9]).unwrap(), b"abcxxx");
        assert_eq!(run(run_length, &[0, 7]).unwrap(), [7]);
        assert_eq!(run(run_length, &[129, 1]).unwrap(), [1; 128]);
        // Truncated: the literal run is cut short, the repeat has no byte, there is no end byte.
        assert_eq!(run(run_length, &[5, 1, 2]).unwrap(), [1, 2]);
        assert_eq!(run(run_length, &[0, 1, 200]).unwrap(), [1]);
        let mut out = Vec::new();
        assert_eq!(run_length(&[129, 0, 129, 0], &mut out, 200), Err(Fail::Over));
        // The cap is a cap: 300 repeats of 128 would be 38 400 bytes.
        let bomb: Vec<u8> = (0..300).flat_map(|_| [129u8, 0]).collect();
        let mut out = Vec::new();
        assert_eq!(run_length(&bomb, &mut out, 1000), Err(Fail::Over));
        assert!(out.len() <= 1000);
    }

    #[test]
    fn lzw_spec_example() {
        // 7.4.4.2: the data 45 45 45 45 45 65 45 45 45 66 encodes to 80 0B 60 50 22 0C 0C 85 01.
        let encoded = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        let expected = [45, 45, 45, 45, 45, 65, 45, 45, 45, 66];
        assert_eq!(run(|i, o, c| lzw(i, true, o, c), &encoded).unwrap(), expected);
        assert_eq!(lzw_encode(&expected, true), encoded);
    }

    #[test]
    fn lzw_round_trips_in_both_early_change_modes() {
        let mut data = Vec::new();
        for i in 0..20_000u32 {
            data.push(u8::try_from((i * i / 7 + i / 3) % 251).unwrap());
        }
        data.extend_from_slice(&[7; 3000]);
        for early in [true, false] {
            let encoded = lzw_encode(&data, early);
            assert_eq!(run(|i, o, c| lzw(i, early, o, c), &encoded).unwrap(), data, "early {early}");
        }
    }

    #[test]
    fn lzw_hostile_input() {
        // A first code that is not a literal, a code beyond the table, and cut-off data.
        let bits = |codes: &[(usize, u32)]| {
            let mut v = Vec::new();
            for &(c, w) in codes {
                for i in (0..w).rev() {
                    v.push((c >> i) & 1 == 1);
                }
            }
            v.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |a, (i, &b)| a | (u8::from(b) << (7 - i)))).collect::<Vec<u8>>()
        };
        let l = |d: &[u8]| run(|i, o, c| lzw(i, true, o, c), d);
        assert!(matches!(l(&bits(&[(258, 9)])), Err(Fail::Corrupt(_))));
        assert!(matches!(l(&bits(&[(65, 9), (300, 9)])), Err(Fail::Corrupt(_))));
        assert_eq!(l(&bits(&[(65, 9), (257, 9)])).unwrap(), b"A");
        assert_eq!(l(&[0xFF]).unwrap(), b"");
        assert_eq!(l(&[]).unwrap(), b"");
        // The cap holds: a long run of one byte decodes with strings of growing length.
        let data = vec![9u8; 100_000];
        let encoded = lzw_encode(&data, true);
        let mut out = Vec::new();
        assert_eq!(lzw(&encoded, true, &mut out, 5000), Err(Fail::Over));
        assert!(out.len() <= 5000);
    }
}

//! Tests of the TrueType instruction interpreter: what instructions do, and hostile programs built in code
//! (endless loops, deep recursion, an empty or overfull stack, indexes out of range, huge counts).

use std::time::{Duration, Instant};

use super::*;

const SVTCA_Y: u8 = 0x00;
const SVTCA_X: u8 = 0x01;
const POP: u8 = 0x21;
const FDEF: u8 = 0x2C;
const ENDF: u8 = 0x2D;
const CALL: u8 = 0x2B;
const LOOPCALL: u8 = 0x2A;
const WS: u8 = 0x42;
const RS: u8 = 0x43;
const SCFS: u8 = 0x48;
const IUP_Y: u8 = 0x30;
const JMPR: u8 = 0x1C;
const IF: u8 = 0x58;
const ELSE: u8 = 0x1B;
const EIF: u8 = 0x59;

/// NPUSHW: signed 16-bit values.
fn words(v: &[i32]) -> Vec<u8> {
    let mut out = vec![0x41, v.len() as u8];
    for x in v {
        out.extend((*x as i16).to_be_bytes());
    }
    out
}

/// NPUSHB.
fn bytes(v: &[u8]) -> Vec<u8> {
    let mut out = vec![0x40, v.len() as u8];
    out.extend(v);
    out
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.iter().flat_map(|p| p.iter().copied()).collect()
}

fn setup(fpgm: &[u8], prep: &[u8], cvt: &[i16]) -> Res<Globals> {
    let mut fuel = 200_000;
    Globals::setup(fpgm, prep, cvt, Env::for_test(2048), &mut fuel)
}

/// A zone of one contour (all the points, on the curve) and four phantom points.
fn zone_of(pts: &[(i32, i32)]) -> Zone {
    let cur: Vec<Pt> = pts.iter().map(|&(x, y)| Pt { x, y }).chain([Pt::default(); 4]).collect();
    let n = pts.len();
    let mut tags = vec![ON_CURVE; n];
    tags.extend([0; 4]);
    Zone { org: cur.clone(), cur, tags, ends: vec![n] }
}

fn scale() -> i64 {
    Env::for_test(2048).scale
}

/// Run a glyph program over the points with the font programs `fpgm`, `prep` run first (the control values are 100 and 200 units).
fn glyph_with(fpgm: &[u8], prep: &[u8], code: &[u8], pts: &[(i32, i32)]) -> Res<Zone> {
    let g = setup(fpgm, prep, &[100, 200])?;
    let mut fuel = 100_000;
    g.run_glyph(code, zone_of(pts), scale(), &mut fuel)
}

fn glyph(code: &[u8], pts: &[(i32, i32)]) -> Res<Zone> {
    glyph_with(&[], &[], code, pts)
}

fn ys(z: &Zone) -> Vec<i32> {
    z.cur.iter().map(|p| p.y).collect()
}

/// Write the value the instructions leave on the stack into storage cell `i`.
fn store(i: u8, ops: &[u8]) -> Vec<u8> {
    cat(&[&bytes(&[i]), ops, &[WS]])
}

// --- what instructions do ----------------------------------------------------------------------------

#[test]
fn rounding_and_control_values_place_points() {
    // MDAP[round] puts point 0 on the grid (130 is 2.03 pixels: 128); MIRP[round] then keeps point 1 a whole number of pixels from it.
    let code = cat(&[&[SVTCA_Y], &words(&[0]), &[0x2F], &words(&[1, 0]), &[0xE4]]);
    let z = glyph(&code, &[(0, 130), (0, 200)]).unwrap();
    assert_eq!(ys(&z)[..2], [128, 192]);
    // Without the rounding flag the control value (100 units, 6400 in 26.6) is the distance.
    let code = cat(&[&[SVTCA_Y], &words(&[0]), &[0x2F], &words(&[1, 0]), &[0xE0]]);
    let z = glyph(&code, &[(0, 130), (0, 200)]).unwrap();
    assert_eq!(ys(&z)[..2], [128, 128 + 6400]);
    // MIAP puts a point at a control value (200 units).
    let code = cat(&[&[SVTCA_X], &words(&[0, 1]), &[0x3E]]);
    let z = glyph(&code, &[(5, 7)]).unwrap();
    assert_eq!(z.cur.first().copied(), Some(Pt { x: 12800, y: 7 }));
}

#[test]
fn iup_places_the_untouched_points_between_the_touched_ones() {
    // Points 0 and 2 are set (touched) in y; point 1 lay half way between them and stays half way.
    let code = cat(&[&[SVTCA_Y], &words(&[0, 0]), &[SCFS], &words(&[2, 1280]), &[SCFS, IUP_Y]]);
    let z = glyph(&code, &[(0, 0), (0, 320), (0, 640)]).unwrap();
    assert_eq!(ys(&z)[..3], [0, 640, 1280]);
    // One touched point: the whole contour moves with it.
    let code = cat(&[&[SVTCA_Y], &words(&[1, 500]), &[SCFS, IUP_Y]]);
    let z = glyph(&code, &[(0, 0), (0, 320), (0, 640)]).unwrap();
    assert_eq!(ys(&z)[..3], [180, 500, 820]);
}

#[test]
fn functions_loopcall_and_storage_work() {
    // Function 7 adds one to storage cell 0; the control value program calls it ten times.
    let body = cat(&[&bytes(&[0, 0]), &[RS], &bytes(&[1]), &[0x60, WS]]);
    let fpgm = cat(&[&bytes(&[7]), &[FDEF], &body, &[ENDF]]);
    let prep = cat(&[&bytes(&[10, 7]), &[LOOPCALL]]);
    let g = setup(&fpgm, &prep, &[]).unwrap();
    assert_eq!(g.storage.first(), Some(&10));
    // A function that calls another, and leaves its result on the stack for the caller.
    let fpgm = cat(&[&bytes(&[1]), &[FDEF], &bytes(&[9]), &[ENDF], &bytes(&[2]), &[FDEF], &bytes(&[1]), &[CALL, ENDF]]);
    let g = setup(&fpgm, &store(3, &cat(&[&bytes(&[2]), &[CALL]])), &[]).unwrap();
    assert_eq!(g.storage.get(3), Some(&9));
}

#[test]
fn branches_arithmetic_and_stack_instructions() {
    let pick = |cond: u8| cat(&[&bytes(&[0, cond]), &[IF], &bytes(&[5]), &[ELSE], &bytes(&[6]), &[EIF, WS]]);
    assert_eq!(setup(&[], &pick(1), &[]).unwrap().storage.first(), Some(&5));
    assert_eq!(setup(&[], &pick(0), &[]).unwrap().storage.first(), Some(&6));
    // DIV is a * 64 / b, MUL is a * b / 64; FLOOR, CEILING, ROUND (to the grid), MAX, MIN, ROLL, LT, EVEN, GETINFO.
    let mut prep = Vec::new();
    prep.extend(store(0, &cat(&[&words(&[192, 128]), &[0x62]])));
    prep.extend(store(1, &cat(&[&words(&[192, 128]), &[0x63]])));
    prep.extend(store(2, &cat(&[&words(&[100]), &[0x66]])));
    prep.extend(store(3, &cat(&[&words(&[100]), &[0x67]])));
    prep.extend(store(4, &cat(&[&words(&[100]), &[0x68]])));
    prep.extend(store(5, &cat(&[&words(&[3, 9]), &[0x8B]])));
    prep.extend(store(6, &cat(&[&words(&[3, 9]), &[0x8C]])));
    // ROLL turns 1 2 3 into 2 3 1; two POPs leave the 2.
    prep.extend(store(7, &cat(&[&words(&[1, 2, 3]), &[0x8A, POP, POP]])));
    prep.extend(store(8, &cat(&[&words(&[4, 5]), &[0x50]])));
    prep.extend(store(9, &cat(&[&words(&[4]), &[0x57]])));
    prep.extend(store(10, &cat(&[&bytes(&[1]), &[0x88]])));
    let g = setup(&[], &prep, &[]).unwrap();
    assert_eq!(g.storage.get(..11).unwrap(), [96, 384, 64, 128, 128, 9, 3, 2, 1, 1, 35]);
}

#[test]
fn instruct_control_is_for_the_control_value_program() {
    let prep = cat(&[&bytes(&[1, 1]), &[0x8E]]);
    assert!(setup(&[], &prep, &[]).unwrap().glyph_programs_off());
    assert!(!setup(&[], &[], &[]).unwrap().glyph_programs_off());
    // A glyph program may not use it (and nothing happens).
    assert!(glyph(&cat(&[&bytes(&[1, 1]), &[0x8E]]), &[(0, 0)]).is_ok());
}

#[test]
fn definitions_of_instructions_stand_in_for_unknown_ones() {
    // IDEF 0x91 pushes 5; the control value program uses it.
    let fpgm = cat(&[&bytes(&[0x91]), &[0x89], &bytes(&[5]), &[ENDF]]);
    let prep = store(0, &[0x91]);
    assert_eq!(setup(&fpgm, &prep, &[]).unwrap().storage.first(), Some(&5));
    // Without the definition the instruction is an error.
    assert!(setup(&[], &store(0, &[0x91]), &[]).is_err());
}

#[test]
fn the_twilight_zone_holds_points_made_by_the_programs() {
    // Zone 0 for all: MIAP puts twilight point 3 at control value 0 (10 units = 640) along x; GC reads it back.
    let prep = cat(&[&bytes(&[0]), &[0x16], &words(&[3, 0]), &[0x3E], &store(0, &cat(&[&words(&[3]), &[0x46]]))]);
    assert_eq!(setup(&[], &prep, &[10]).unwrap().storage.first(), Some(&640));
    // MSIRP, MIRP and SCFS on twilight points, and points beyond the zone, are fine.
    let prep = cat(&[&bytes(&[0]), &[0x16], &words(&[3, 0]), &[0x3E], &words(&[5, 64]), &[0x3A], &words(&[6, 0]), &[0xE4], &words(&[30000, 0]), &[0x3E], &words(&[3, 7]), &[SCFS]]);
    assert!(setup(&[], &prep, &[10]).is_ok());
}

// --- hostile programs ------------------------------------------------------------------------------------

/// The program as `fpgm`, as `prep` and as a glyph program.
fn all_three(code: &[u8]) -> [Res<()>; 3] {
    [setup(code, &[], &[]).map(|_| ()), setup(&[], code, &[]).map(|_| ()), glyph(code, &[(0, 0), (64, 64)]).map(|_| ())]
}

#[test]
fn endless_loops_stop() {
    let started = Instant::now();
    // JMPR back to the start, forever; and the same with JROT and JROF.
    let jmpr = cat(&[&words(&[-4]), &[JMPR]]);
    let jrot = cat(&[&words(&[-6, 1]), &[0x78]]);
    let jrof = cat(&[&words(&[-6, 0]), &[0x79]]);
    for code in [jmpr, jrot, jrof] {
        for r in all_three(&code) {
            assert!(r.is_err());
        }
    }
    // A straight line of instructions longer than the instruction limit.
    let long = [0xB0u8, 1, POP].repeat(300_000);
    for r in all_three(&long) {
        assert!(r.is_err());
    }
    // A jump to itself, out of the code, and before it.
    for code in [cat(&[&words(&[0]), &[JMPR]]), cat(&[&words(&[100]), &[JMPR]]), cat(&[&words(&[-100]), &[JMPR]])] {
        assert!(glyph(&code, &[(0, 0)]).is_err());
    }
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}

#[test]
fn deep_and_endless_recursion_stops() {
    let started = Instant::now();
    // Function 0 calls itself.
    let fpgm = cat(&[&bytes(&[0]), &[FDEF], &bytes(&[0]), &[CALL, ENDF]]);
    assert!(setup(&fpgm, &cat(&[&bytes(&[0]), &[CALL]]), &[]).is_err());
    // Two functions that call each other.
    let fpgm = cat(&[&bytes(&[0]), &[FDEF], &bytes(&[1]), &[CALL, ENDF], &bytes(&[1]), &[FDEF], &bytes(&[0]), &[CALL, ENDF]]);
    assert!(glyph_with(&fpgm, &[], &cat(&[&bytes(&[1]), &[CALL]]), &[(0, 0)]).is_err());
    // LOOPCALL of a function that does LOOPCALL, with the biggest counts: the iterations in all are limited.
    let fpgm = cat(&[&bytes(&[0]), &[FDEF], &words(&[32767, 0]), &[LOOPCALL, ENDF]]);
    assert!(setup(&fpgm, &cat(&[&words(&[32767, 0]), &[LOOPCALL]]), &[]).is_err());
    // One LOOPCALL of a function that does nothing is fine, and is charged.
    let fpgm = cat(&[&bytes(&[0]), &[FDEF], &[ENDF]]);
    assert!(setup(&fpgm, &cat(&[&words(&[20000, 0]), &[LOOPCALL]]), &[]).is_ok());
    assert!(setup(&fpgm, &cat(&[&words(&[32767, 0]), &[LOOPCALL, 0x20]]), &[]).is_err());
    // A call of an undefined function.
    assert!(setup(&[], &cat(&[&bytes(&[3]), &[CALL]]), &[]).is_err());
    assert!(setup(&[], &cat(&[&bytes(&[3, 3]), &[LOOPCALL]]), &[]).is_err());
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
}

#[test]
fn the_stack_is_checked() {
    // Underflow.
    for code in [vec![POP], vec![0x60], vec![0x23], cat(&[&bytes(&[5]), &[0x25]]), cat(&[&bytes(&[5]), &[0x26]]), vec![0x20], vec![0x8A], vec![0x2E]] {
        for r in all_three(&code) {
            assert!(r.is_err(), "{code:?}");
        }
    }
    // Overflow (the test font allows 256 values).
    let big = cat(&[&bytes(&[1; 200]), &bytes(&[1; 200])]);
    for r in all_three(&big) {
        assert!(r.is_err());
    }
    // DUP in a loop (each turn leaves one more value).
    let dup = cat(&[&bytes(&[1]), &[0x20], &words(&[-5]), &[JMPR]]);
    for r in all_three(&dup) {
        assert!(r.is_err());
    }
    // A push that is cut off.
    assert!(glyph(&[0x40, 200, 1, 2], &[(0, 0)]).is_err());
    assert!(glyph(&[0x41, 5, 1], &[(0, 0)]).is_err());
    assert!(glyph(&[0xB8, 1], &[(0, 0)]).is_err());
    // A division by zero, and the biggest numbers in arithmetic.
    assert!(glyph(&cat(&[&bytes(&[1, 0]), &[0x62]]), &[(0, 0)]).is_err());
    let big_mul = cat(&[&words(&[32767, 32767]), &[0x63, 0x20, 0x63, 0x20, 0x63, 0x20, 0x63, 0x64, 0x65, 0x66, 0x67]]);
    assert!(glyph(&big_mul, &[(0, 0)]).is_ok());
}

#[test]
fn indexes_out_of_range_are_ignored_not_trusted() {
    let far = 30000;
    let programs: Vec<Vec<u8>> = vec![
        cat(&[&words(&[far]), &[0x2E]]),                                        // MDAP
        cat(&[&words(&[far]), &[0x2F]]),                                        // MDAP[round]
        cat(&[&words(&[far, 0]), &[0x3E]]),                                     // MIAP: the point
        cat(&[&words(&[0, far]), &[0x3E]]),                                     // MIAP: the control value
        cat(&[&words(&[0, -1]), &[0x3F]]),                                      // MIAP: a negative control value index
        cat(&[&words(&[far, far]), &[0xE5]]),                                   // MIRP
        cat(&[&words(&[0, -1]), &[0xF5]]),                                      // MIRP: the "no control value" index
        cat(&[&words(&[far]), &[0xC5]]),                                        // MDRP
        cat(&[&words(&[far, 5]), &[0x3A]]),                                     // MSIRP
        cat(&[&words(&[0, far]), &[0x10, 0x3C]]),                               // SRP0, ALIGNRP
        cat(&[&words(&[0, far, far]), &[0x11, 0x12, 0x39]]),                    // SRP1, SRP2, IP
        cat(&[&words(&[far]), &[0x46]]),                                        // GC
        cat(&[&words(&[far, 5]), &[0x48]]),                                     // SCFS
        cat(&[&words(&[far, far]), &[0x49]]),                                   // MD
        cat(&[&words(&[far, 1]), &[WS]]),                                       // WS
        cat(&[&words(&[far]), &[RS]]),                                          // RS
        cat(&[&words(&[-1]), &[RS]]),
        cat(&[&words(&[far, 1]), &[0x44]]),                                     // WCVTP
        cat(&[&words(&[far, 1]), &[0x70]]),                                     // WCVTF
        cat(&[&words(&[far]), &[0x45]]),                                        // RCVT
        cat(&[&words(&[0, far]), &[0x27]]),                                     // ALIGNPTS
        cat(&[&words(&[far]), &[0x29]]),                                        // UTP
        cat(&[&words(&[far, far]), &[0x81]]),                                   // FLIPRGON
        cat(&[&words(&[far, 0]), &[0x82]]),                                     // FLIPRGOFF
        cat(&[&words(&[far]), &[0x80]]),                                        // FLIPPT
        cat(&[&words(&[far, far, 0, 1, far]), &[0x0F]]),                        // ISECT
        cat(&[&words(&[far, far]), &[0x06]]),                                   // SPVTL
        cat(&[&words(&[far, far]), &[0x86]]),                                   // SDPVTL
        cat(&[&words(&[far]), &[0x34]]),                                        // SHC
        cat(&[&words(&[0, 5]), &[0x38]]),                                       // SHPIX
        cat(&[&words(&[far]), &[0x32]]),                                        // SHP
        cat(&[&words(&[0, far, 1]), &[0x5D]]),                                  // DELTAP1
        cat(&[&words(&[0, far, 1]), &[0x73]]),                                  // DELTAC1
    ];
    for (i, code) in programs.iter().enumerate() {
        // The glyph has two points.
        assert!(glyph(code, &[(0, 0), (64, 64)]).is_ok(), "program {i}: {code:?}");
        assert!(setup(&[], code, &[1]).is_ok(), "program {i} as prep");
    }
    // A zone that is not 0 or 1.
    assert!(glyph(&cat(&[&bytes(&[2]), &[0x13]]), &[(0, 0)]).is_err());
    assert!(glyph(&cat(&[&words(&[-1]), &[0x16]]), &[(0, 0)]).is_err());
    assert!(glyph(&cat(&[&bytes(&[5]), &[0x36]]), &[(0, 0)]).is_err());
}

#[test]
fn huge_loop_counts_have_nothing_to_pop() {
    for op in [0x32u8, 0x33, 0x39, 0x3C, 0x80, 0x38] {
        for count in [1000, 32767] {
            let r = glyph(&cat(&[&words(&[count]), &[0x17, op]]), &[(0, 0), (64, 64)]);
            assert!(r.is_err(), "op {op:#x} count {count}");
        }
        // 65534, made by arithmetic.
        let r = glyph(&cat(&[&words(&[32767]), &[0x20, 0x60, 0x17, op]]), &[(0, 0), (64, 64)]);
        assert!(r.is_err(), "op {op:#x}");
    }
    // A negative loop count; and DELTAP, DELTAC and SDS with counts the stack cannot hold.
    assert!(glyph(&cat(&[&words(&[-5]), &[0x17]]), &[(0, 0)]).is_err());
    assert!(glyph(&cat(&[&words(&[32767]), &[0x5D]]), &[(0, 0)]).is_err());
    assert!(glyph(&cat(&[&words(&[32767]), &[0x73]]), &[(0, 0)]).is_err());
    assert!(glyph(&cat(&[&words(&[32767]), &[0x5F]]), &[(0, 0)]).is_err());
    // A loop count the stack does hold works, once, and the count goes back to 1.
    let z = glyph(&cat(&[&words(&[0, 1, 2]), &[0x17], &[0x80, 0x80]]), &[(0, 0), (64, 64)]);
    assert!(z.is_err());
    let z = glyph(&cat(&[&words(&[0, 1, 0, 2]), &[0x17], &[0x80, 0x80]]), &[(0, 0), (64, 64)]);
    assert!(z.is_ok());
}

#[test]
fn definitions_are_checked_and_limited() {
    // Too many FDEF (the test font allows 64) and IDEF (8).
    let mut many = Vec::new();
    for i in 0..100u8 {
        many.extend(cat(&[&bytes(&[i]), &[FDEF, ENDF]]));
    }
    assert!(setup(&many, &[], &[]).is_err());
    let mut fits = Vec::new();
    for i in 0..60u8 {
        fits.extend(cat(&[&bytes(&[i]), &[FDEF, ENDF]]));
    }
    assert!(setup(&fits, &[], &[]).is_ok());
    let mut idefs = Vec::new();
    for i in 0..20u8 {
        idefs.extend(cat(&[&bytes(&[0x91 + i]), &[0x89, ENDF]]));
    }
    assert!(setup(&idefs, &[], &[]).is_err());
    // A definition inside a definition, without an end, with a bad number, and in a glyph program.
    assert!(setup(&cat(&[&bytes(&[0]), &[FDEF], &bytes(&[1]), &[FDEF, ENDF, ENDF]]), &[], &[]).is_err());
    assert!(setup(&cat(&[&bytes(&[0]), &[FDEF], &bytes(&[1])]), &[], &[]).is_err());
    assert!(setup(&cat(&[&words(&[-1]), &[FDEF, ENDF]]), &[], &[]).is_err());
    assert!(glyph(&cat(&[&bytes(&[0]), &[FDEF, ENDF]]), &[(0, 0)]).is_err());
    // ENDF without a function; IF without EIF; ELSE after a true branch with no EIF; an unknown instruction.
    assert!(glyph(&[ENDF], &[(0, 0)]).is_err());
    assert!(glyph(&cat(&[&bytes(&[0]), &[IF], &bytes(&[1])]), &[(0, 0)]).is_err());
    assert!(glyph(&cat(&[&bytes(&[1]), &[IF, ELSE]]), &[(0, 0)]).is_err());
    for op in [0x28u8, 0x7B, 0x83, 0x84, 0x8F, 0x90, 0x91, 0xA0] {
        assert!(glyph(&[op], &[(0, 0)]).is_err(), "{op:#x}");
    }
}

#[test]
fn skipped_instructions_are_charged() {
    // A long block jumped over by a false IF, again and again: the skipping is work too.
    let block = [0xB0u8, 1, POP].repeat(5000);
    let body = cat(&[&bytes(&[0]), &[IF], &block, &[EIF]]);
    let looped = cat(&[&body, &words(&[-(body.len() as i32 + 4)]), &[JMPR]]);
    let g = setup(&[], &[], &[]).unwrap();
    let mut fuel = 200_000;
    assert!(g.run_glyph(&looped, zone_of(&[(0, 0)]), scale(), &mut fuel).is_err());
    assert_eq!(fuel, 0);
    // The fuel handed back says what was used.
    let mut fuel = 1000;
    assert!(g.run_glyph(&bytes(&[1, 2, 3]), zone_of(&[(0, 0)]), scale(), &mut fuel).is_ok());
    assert_eq!(fuel, 999);
}

#[test]
fn garbage_programs_never_panic() {
    let mut seed = 0x1234_5678_u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let started = Instant::now();
    for round in 0..3000 {
        let len = 1 + (next() % 300) as usize;
        let code: Vec<u8> = (0..len).map(|_| if next() % 4 == 0 { (next() % 256) as u8 } else { (next() % 0xC0) as u8 }).collect();
        // Pushes first, so that something is on the stack.
        let mut prog = words(&[(next() % 9) as i32, (next() % 70000) as i32 - 5000, (next() % 5) as i32, 1, 64, -64]);
        prog.extend(&code);
        let pts: Vec<(i32, i32)> = (0..(1 + next() % 8)).map(|_| ((next() % 2000) as i32 - 500, (next() % 2000) as i32 - 500)).collect();
        let g = match round % 3 {
            0 => setup(&prog, &[], &[1, 2, 3]),
            1 => setup(&[], &prog, &[1, 2, 3]),
            _ => setup(&[], &[], &[1, 2, 3]),
        };
        if let Ok(g) = g {
            let mut fuel = 20_000;
            let _ = g.run_glyph(&prog, zone_of(&pts), scale(), &mut fuel);
            let mut fuel = 20_000;
            let _ = g.run_glyph(&prog, zone_of(&pts), 1 << 16, &mut fuel);
        }
    }
    assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
}

#[test]
fn programs_of_valid_instructions_with_arguments_never_panic() {
    let mut seed = 0xDEAD_BEEF_u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let started = Instant::now();
    for round in 0..4000 {
        // Each step: a few small or large arguments, then one instruction (never a push, so the arguments are there).
        let mut prog = Vec::new();
        for _ in 0..(5 + next() % 40) {
            let args: Vec<i32> = (0..next() % 6).map(|_| if next() % 3 == 0 { (next() % 70000) as i32 - 35000 } else { (next() % 12) as i32 - 1 }).collect();
            if !args.is_empty() {
                prog.extend(words(&args));
            }
            let mut op = (next() % 256) as u8;
            while matches!(op, 0x40 | 0x41 | 0xB0..=0xBF | 0x2C | 0x89 | 0x1C | 0x78 | 0x79 | 0x2A | 0x2B | 0x58 | 0x1B) {
                op = (next() % 256) as u8;
            }
            prog.push(op);
        }
        let pts: Vec<(i32, i32)> = (0..(1 + next() % 12)).map(|_| ((next() % 4000) as i32 - 1000, (next() % 4000) as i32 - 1000)).collect();
        let g = if round % 2 == 0 { setup(&[], &prog, &[5, -7, 300, 12000]) } else { setup(&[], &[], &[5, -7, 300, 12000]) };
        if let Ok(g) = g {
            let mut fuel = 20_000;
            let _ = g.run_glyph(&prog, zone_of(&pts), scale(), &mut fuel);
        }
    }
    assert!(started.elapsed() < Duration::from_secs(60), "{:?}", started.elapsed());
}

#[test]
fn iup_is_charged_for_every_point() {
    // IUP in an endless loop over a glyph with many points: each call must cost its points, not one instruction.
    let pts: Vec<(i32, i32)> = (0..39_000).map(|i| (i % 100, i / 100)).collect();
    let code = cat(&[&[IUP_Y], &words(&[-5]), &[JMPR]]);
    let started = Instant::now();
    assert!(glyph(&code, &pts).is_err());
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
}

//! Puts the application manifest and the program's icon into qingpdf-view.exe through linker arguments (no resource compiler needed):
//! the manifest asks for Common Controls 6 (the look of buttons and text boxes) and per-monitor DPI awareness (version 2); the icon is
//! the pictures of `qingpdf.ico`, written here as a resource file (RT_ICON for each picture and one RT_GROUP_ICON that lists them, the
//! format of the `.res` files that `rc.exe` makes) which the linker takes as it takes an object file.
//!
//! `qingpdf.ico` is made by `tests/tools/make_icon.py` and kept in the repository, so that building needs nothing but Rust.

use std::path::Path;

/// The type numbers of the resources (winuser.h).
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
/// The number the program's icon has in its resources (`ICON_ID` of the window's code asks for it).
const ICON_ID: u16 = 1;

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// One entry of a resource file: the type and the number of the resource (no names, no language) and its bytes, padded to a 4-byte
/// boundary as the format asks.
fn entry(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes()); // the size of the header that follows (this one is 32 bytes whole)
    out.extend([0xFF, 0xFF]);
    out.extend(kind.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(id.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // data version
    out.extend(0x0030u16.to_le_bytes()); // memory flags: movable, pure
    out.extend(0u16.to_le_bytes()); // language: neutral
    out.extend(0u32.to_le_bytes()); // version
    out.extend(0u32.to_le_bytes()); // characteristics
    out.extend(data);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// The resource file for an .ico: every picture as an RT_ICON and the group that names them.
fn resource_file(ico: &[u8]) -> Option<Vec<u8>> {
    if le16(ico, 0)? != 0 || le16(ico, 2)? != 1 {
        return None;
    }
    let count = usize::from(le16(ico, 4)?);
    let mut out = Vec::new();
    // Every resource file starts with an empty entry.
    entry(&mut out, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((count as u16).to_le_bytes());
    for i in 0..count {
        let at = 6 + 16 * i;
        let (size, offset) = (le32(ico, at + 8)? as usize, le32(ico, at + 12)? as usize);
        let picture = ico.get(offset..offset.checked_add(size)?)?;
        let id = ICON_ID + 1 + i as u16;
        entry(&mut out, RT_ICON, id, picture);
        // The group lists each picture as the .ico did, with the number of the resource where the file had the offset.
        group.extend(ico.get(at..at + 12)?);
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, ICON_ID, &group);
    Some(out)
}

fn main() {
    println!("cargo:rerun-if-changed=qingpdf-view.manifest");
    println!("cargo:rerun-if-changed=qingpdf.ico");
    println!("cargo:rerun-if-changed=build.rs");
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|v| v == "windows") && std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|v| v == "msvc");
    if !windows_msvc {
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}/qingpdf-view.manifest");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
    // The icon: a program with no icon still works, so what cannot be read is a warning and not a failure of the build.
    let res = std::fs::read(Path::new(&dir).join("qingpdf.ico")).ok().and_then(|ico| resource_file(&ico));
    match (res, std::env::var("OUT_DIR")) {
        (Some(res), Ok(out)) => {
            let path = Path::new(&out).join("qingpdf-icon.res");
            if std::fs::write(&path, res).is_ok() {
                println!("cargo:rustc-link-arg-bins={}", path.display());
            } else {
                println!("cargo:warning=could not write the icon resource file");
            }
        }
        _ => println!("cargo:warning=qingpdf.ico is missing or is not an icon file: the program has no icon"),
    }
}

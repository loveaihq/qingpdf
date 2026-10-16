//! Random bytes from the operating system, for the seed of an encryption (`encrypt`). This is the one module of the command line
//! that calls Windows (everything else in the crate is `deny(unsafe_code)`; the engine takes the bytes from its caller and never
//! looks for a source of its own, and is `forbid`). The declaration is written here by hand.

#![allow(unsafe_code)]

/// 32 random bytes from the system's cryptographic generator, or why there are none. Nothing weaker is ever used instead: if the
/// system does not give bytes, the file is not encrypted.
pub fn seed() -> Result<[u8; 32], String> {
    let mut bytes = [0u8; 32];
    fill(&mut bytes)?;
    Ok(bytes)
}

#[cfg(windows)]
fn fill(buffer: &mut [u8]) -> Result<(), String> {
    use std::ffi::c_void;

    #[link(name = "bcrypt")]
    unsafe extern "system" {
        /// `NTSTATUS BCryptGenRandom(BCRYPT_ALG_HANDLE algorithm, PUCHAR buffer, ULONG size, ULONG flags)`; the status is 0 for
        /// success and negative for an error.
        fn BCryptGenRandom(algorithm: *mut c_void, buffer: *mut u8, size: u32, flags: u32) -> i32;
    }
    /// Use the system's preferred generator; the algorithm handle must then be null.
    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

    let size = u32::try_from(buffer.len()).map_err(|_| "too many random bytes asked for".to_string())?;
    // SAFETY: `buffer` is a live, writable slice of exactly `size` bytes, which is what Windows is told; with
    // BCRYPT_USE_SYSTEM_PREFERRED_RNG the algorithm handle is null as the documentation requires; Windows writes into the buffer
    // during the call and keeps no pointer to it.
    let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), buffer.as_mut_ptr(), size, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status == 0 { Ok(()) } else { Err(format!("Windows gave no random bytes (status 0x{:08x}); nothing was encrypted", status as u32)) }
}

#[cfg(not(windows))]
fn fill(_buffer: &mut [u8]) -> Result<(), String> {
    Err("this build has no source of random bytes for encryption; nothing was encrypted".to_string())
}

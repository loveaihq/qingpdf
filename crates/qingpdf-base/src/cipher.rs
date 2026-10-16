//! The ciphers and hashes behind the standard security handler (ISO 32000-1
//! 7.6): RC4, AES-CBC with PKCS#7 padding as PDF uses it, the revision 6 hash,
//! and a source of initialisation vectors.
//!
//! RC4 and the CBC mode are written here (a few lines each); the AES block
//! function, SHA-2 and MD5 come from the RustCrypto crates `aes`, `sha2` and
//! `md-5`. `aes` finds the CPU's AES instructions at run time, which the
//! engine could not do itself without `unsafe`.

use std::cell::{Cell, RefCell};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use aes::cipher::array::Array;
use aes::cipher::consts::{U16, U32};
use aes::cipher::{
    BlockCipherDecrypt, BlockCipherEncBackend, BlockCipherEncClosure, BlockCipherEncrypt, BlockSizeUser, KeyInit,
};
use aes::{Aes128, Aes256};
use md5::Md5;
use sha2::{Digest, Sha256, Sha384, Sha512};

/// What can go wrong with a piece of ciphertext. The caller says which object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CipherError {
    /// AES data whose length is not a multiple of 16 bytes (7.6.2: the data
    /// is a 16-byte initialisation vector followed by whole blocks).
    Length,
    /// The AES key is neither 16 nor 32 bytes long.
    Key,
}

impl CipherError {
    pub fn message(self) -> &'static str {
        match self {
            CipherError::Length => "AES data is not a whole number of 16-byte blocks",
            CipherError::Key => "the encryption key has the wrong length for AES",
        }
    }
}

pub fn md5(data: &[u8]) -> [u8; 16] {
    Md5::digest(data).into()
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// RC4 (7.6.2 names it as the stream cipher of the standard handler). It
/// encrypts and decrypts alike, in place. An empty key does nothing.
pub fn rc4(key: &[u8], data: &mut [u8]) {
    if key.is_empty() {
        return;
    }
    let mut s: [u8; 256] = std::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
    let at = |s: &[u8; 256], i: u8| s.get(usize::from(i)).copied().unwrap_or(0);
    let mut j = 0u8;
    for i in 0..=255u8 {
        let k = key.get(usize::from(i) % key.len()).copied().unwrap_or(0);
        j = j.wrapping_add(at(&s, i)).wrapping_add(k);
        s.swap(usize::from(i), usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    for byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(at(&s, i));
        s.swap(usize::from(i), usize::from(j));
        *byte ^= at(&s, at(&s, i).wrapping_add(at(&s, j)));
    }
}

/// Blocks of 16 bytes of `data` (which must be a multiple of 16 long; any
/// tail is left out) as the block type of the `aes` crate.
fn blocks(data: &mut [u8]) -> &mut [Array<u8, U16>] {
    let (chunks, _) = data.as_chunks_mut::<16>();
    Array::cast_slice_from_core_mut(chunks)
}

fn xor16(a: &mut [u8], b: &[u8]) {
    for (x, y) in a.iter_mut().zip(b) {
        *x ^= *y;
    }
}

/// CBC encryption of whole blocks in place, chained from `iv`: the work of the
/// closure that [`cbc_encrypt_in_place`] hands to the cipher. It runs inside one
/// call of the cipher's backend, so the AES instructions are used block after
/// block without a function call in between; the revision 6 hash, which
/// encrypts hundreds of kilobytes in the course of one password check, depends
/// on that.
struct CbcEncrypt<'a> {
    iv: [u8; 16],
    data: &'a mut [u8],
}

impl BlockSizeUser for CbcEncrypt<'_> {
    type BlockSize = U16;
}

impl BlockCipherEncClosure for CbcEncrypt<'_> {
    #[inline(always)]
    fn call<B: BlockCipherEncBackend<BlockSize = U16>>(self, backend: &B) {
        let mut previous: Array<u8, U16> = Array::from(self.iv);
        for block in blocks(self.data) {
            xor16(block.as_mut_slice(), previous.as_slice());
            backend.encrypt_block_inplace(block);
            previous = *block;
        }
    }
}

/// CBC encryption of whole blocks in place, chained from `iv`. No padding.
fn cbc_encrypt_in_place<C: BlockCipherEncrypt + BlockSizeUser<BlockSize = U16>>(cipher: &C, iv: &[u8], data: &mut [u8]) {
    let mut first = [0u8; 16];
    for (dst, src) in first.iter_mut().zip(iv) {
        *dst = *src;
    }
    cipher.encrypt_with_backend(CbcEncrypt { iv: first, data });
}

/// CBC encryption of whole blocks in place, chained from `iv`, one call of the cipher for each block. The data of a file is encrypted
/// with AES-256 this way: it is the cheaper way in code (the closure of [`cbc_encrypt_in_place`] is compiled again for each of the
/// cipher's backends, and revision 6's hash needs that for AES-128 only), and the few nanoseconds a call costs are nothing beside
/// rewriting the file.
fn cbc_encrypt_blocks<C: BlockCipherEncrypt + BlockSizeUser<BlockSize = U16>>(cipher: &C, iv: [u8; 16], data: &mut [u8]) {
    let mut previous = iv;
    let (chunks, _) = data.as_chunks_mut::<16>();
    for chunk in chunks {
        xor16(chunk, &previous);
        let mut block: Array<u8, U16> = Array::from(*chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(block.as_slice());
        previous = *chunk;
    }
}

/// How much of a stream is decrypted at a time: small enough to stay in the
/// processor's cache between the AES instructions and the chaining.
const CHUNK: usize = 4096;

/// CBC decryption of `data`, which is the initialisation vector followed by
/// whole blocks, in place: the plaintext, without the vector, is what comes
/// back (the same `Vec`, no second copy of a big stream). No padding handling.
/// A chunk at a time: the blocks are decrypted all at once (the AES
/// instructions work on several at a time) into a buffer, chained with the
/// ciphertext that precedes them, and written over the data one block (the
/// vector) lower than they were read.
fn cbc_decrypt_vec<C: BlockCipherDecrypt + BlockSizeUser<BlockSize = U16>>(
    cipher: &C,
    mut data: Vec<u8>,
) -> Result<Vec<u8>, CipherError> {
    let total = data.len().checked_sub(16).ok_or(CipherError::Length)?;
    if total % 16 != 0 {
        return Err(CipherError::Length);
    }
    let mut carry = [0u8; 16];
    carry.copy_from_slice(data.get(..16).ok_or(CipherError::Length)?);
    let mut cipher_buf = [0u8; CHUNK];
    let mut plain_buf = [0u8; CHUNK];
    let mut done = 0usize;
    while done < total {
        let len = CHUNK.min(total - done);
        let ciphertext = cipher_buf.get_mut(..len).ok_or(CipherError::Length)?;
        ciphertext.copy_from_slice(data.get(16 + done..16 + done + len).ok_or(CipherError::Length)?);
        let plaintext = plain_buf.get_mut(..len).ok_or(CipherError::Length)?;
        {
            let (from, _) = ciphertext.as_chunks::<16>();
            let (to, _) = plaintext.as_chunks_mut::<16>();
            cipher
                .decrypt_blocks_b2b(Array::cast_slice_from_core(from), Array::cast_slice_from_core_mut(to))
                .map_err(|_| CipherError::Length)?;
        }
        // Each plaintext block is XORed with the ciphertext block before it.
        let (first, rest) = plaintext.split_at_mut(16.min(len));
        xor16(first, &carry);
        xor16(rest, ciphertext);
        carry.copy_from_slice(ciphertext.get(len.saturating_sub(16)..len).ok_or(CipherError::Length)?);
        data.get_mut(done..done + len).ok_or(CipherError::Length)?.copy_from_slice(plaintext);
        done += len;
    }
    data.truncate(total);
    Ok(data)
}

/// Decrypt an AES string or stream as PDF stores it (7.6.2): a 16-byte
/// initialisation vector, then CBC-encrypted blocks with PKCS#7 padding.
/// `key` is 16 bytes (AES-128) or 32 (AES-256). The data is used up and its
/// buffer is the plaintext's: no second copy of a big stream is made. Data of
/// no bytes at all, or of the vector alone, is an empty string: some writers
/// leave an empty string unencrypted, and nothing is lost by reading it as
/// empty.
pub fn aes_decrypt_vec(key: &[u8], data: Vec<u8>) -> Result<Vec<u8>, CipherError> {
    if data.is_empty() || data.len() == 16 {
        // Empty, or the vector and nothing else: check the key anyway.
        check_key(key)?;
        return Ok(Vec::new());
    }
    if data.len() < 16 || !data.len().is_multiple_of(16) {
        return Err(CipherError::Length);
    }
    let mut plain = match key.len() {
        16 => cbc_decrypt_vec(&Aes128::new_from_slice(key).map_err(|_| CipherError::Key)?, data)?,
        32 => cbc_decrypt_vec(&Aes256::new_from_slice(key).map_err(|_| CipherError::Key)?, data)?,
        _ => return Err(CipherError::Key),
    };
    // PKCS#7 (7.6.2): the last byte p says how many bytes of padding there are, 1
    // to 16, and all of them are p. qpdf (Pl_AES_PDF) and pdf.js strip the padding
    // only when all p bytes are p, and keep every byte otherwise (a writer that did
    // not pad, whose last byte happens to be small, or damaged data): so do we,
    // because refusing the object would lose it and stripping a text's last bytes
    // would cut it.
    let pad = usize::from(plain.last().copied().unwrap_or(0));
    if (1..=16).contains(&pad)
        && let Some(start) = plain.len().checked_sub(pad)
        && plain.get(start..).is_some_and(|tail| tail.iter().all(|&b| usize::from(b) == pad))
    {
        plain.truncate(start);
    }
    Ok(plain)
}

/// Would [`aes_decrypt_vec`] accept `len` bytes of data under `key` (no error:
/// the key is 16 or 32 bytes, and the data is empty, only a vector, or a vector
/// and whole blocks)?
pub fn aes_can_decrypt(key: &[u8], len: usize) -> bool {
    check_key(key).is_ok() && (len == 0 || len == 16 || (len >= 32 && len.is_multiple_of(16)))
}

/// One block decrypted with AES-256 in ECB mode, as `/Perms` is stored (the
/// extension of level 3 and ISO 32000-2).
pub fn aes256_decrypt_block(key: &[u8], block: &[u8]) -> Result<[u8; 16], CipherError> {
    let cipher = Aes256::new_from_slice(key).map_err(|_| CipherError::Key)?;
    let mut array: Array<u8, U16> = Array::from([0u8; 16]);
    if block.len() != 16 {
        return Err(CipherError::Length);
    }
    for (dst, src) in array.iter_mut().zip(block) {
        *dst = *src;
    }
    cipher.decrypt_block(&mut array);
    let mut out = [0u8; 16];
    for (dst, src) in out.iter_mut().zip(array.iter()) {
        *dst = *src;
    }
    Ok(out)
}

fn check_key(key: &[u8]) -> Result<(), CipherError> {
    if key.len() == 16 || key.len() == 32 { Ok(()) } else { Err(CipherError::Key) }
}

/// How many bytes `aes_encrypt_into` appends for `len` bytes of plaintext:
/// the vector, the data and the padding, which is 1 to 16 bytes.
pub fn aes_encrypted_len(len: usize) -> usize {
    16usize.saturating_add(len.saturating_add(16) / 16 * 16)
}

/// Encrypt as PDF stores AES data (7.6.2): append the vector `iv`, then the
/// CBC encryption of `data` with PKCS#7 padding, to `out`.
pub fn aes_encrypt_into(key: &[u8], iv: [u8; 16], data: &[u8], out: &mut Vec<u8>) -> Result<(), CipherError> {
    check_key(key)?;
    let start = out.len();
    out.extend_from_slice(&iv);
    out.extend_from_slice(data);
    let pad = 16 - data.len() % 16;
    out.extend(std::iter::repeat_n(u8::try_from(pad).unwrap_or(16), pad));
    let body = out.get_mut(start + 16..).ok_or(CipherError::Length)?;
    if key.len() == 16 {
        cbc_encrypt_in_place(&Aes128::new_from_slice(key).map_err(|_| CipherError::Key)?, &iv, body);
    } else {
        cbc_encrypt_blocks(&Aes256::new_from_slice(key).map_err(|_| CipherError::Key)?, iv, body);
    }
    Ok(())
}

/// AES-256-CBC without padding and with a zero vector, decrypting: how the
/// file key is stored in `/UE` and `/OE` (Algorithm 2.A, as pdf.js and qpdf
/// implement it). `data` must be whole blocks.
pub fn aes256_decrypt_no_padding(key: &[u8], data: &[u8]) -> Result<Vec<u8>, CipherError> {
    if !data.len().is_multiple_of(16) {
        return Err(CipherError::Length);
    }
    let cipher = Aes256::new_from_slice(key).map_err(|_| CipherError::Key)?;
    // The zero vector in front, then the blocks.
    let mut with_iv = vec![0u8; 16];
    with_iv.extend_from_slice(data);
    cbc_decrypt_vec(&cipher, with_iv)
}

/// AES-256-CBC without padding and with a zero vector, encrypting: how the file key is put into `/UE` and
/// `/OE` (the other way of [`aes256_decrypt_no_padding`]). `data` must be whole blocks.
pub fn aes256_encrypt_no_padding(key: &[u8], data: &[u8]) -> Result<Vec<u8>, CipherError> {
    if !data.len().is_multiple_of(16) {
        return Err(CipherError::Length);
    }
    let cipher = Aes256::new_from_slice(key).map_err(|_| CipherError::Key)?;
    let mut out = data.to_vec();
    cbc_encrypt_blocks(&cipher, [0u8; 16], &mut out);
    Ok(out)
}

/// One block encrypted with AES-256 in ECB mode, as `/Perms` is stored (the other way of [`aes256_decrypt_block`]).
pub fn aes256_encrypt_block(key: &[u8], block: [u8; 16]) -> Result<[u8; 16], CipherError> {
    let cipher = Aes256::new_from_slice(key).map_err(|_| CipherError::Key)?;
    let mut array: Array<u8, U16> = Array::from(block);
    cipher.encrypt_block(&mut array);
    let mut out = [0u8; 16];
    out.copy_from_slice(array.as_slice());
    Ok(out)
}

/// The hash of revision 6 (ISO 32000-2 Algorithm 2.B, as pdf.js `PDF20` and
/// qpdf `hash_V5` do it), or of revision 5 (a single SHA-256, Adobe's
/// extension level 3). `udata` is empty for the user password and the first 48
/// bytes of `/U` for the owner password.
pub fn password_hash(revision: i64, password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut first = Vec::with_capacity(password.len() + salt.len() + udata.len());
    first.extend_from_slice(password);
    first.extend_from_slice(salt);
    first.extend_from_slice(udata);
    let mut k: Vec<u8> = sha256(&first).to_vec();
    if revision < 6 {
        return k;
    }
    let mut round = 0usize;
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        // K1 is (password, K, udata) written 64 times over: once, then the
        // buffer doubled six times.
        buffer.clear();
        buffer.extend_from_slice(password);
        buffer.extend_from_slice(&k);
        buffer.extend_from_slice(udata);
        for _ in 0..6 {
            buffer.extend_from_within(..);
        }
        // AES-128 in CBC mode without padding: the key is the first 16 bytes
        // of K, the vector the next 16.
        let (key, iv) = (k.get(..16).unwrap_or(&[]), k.get(16..32).unwrap_or(&[]));
        if let Ok(cipher) = Aes128::new_from_slice(key) {
            cbc_encrypt_in_place(&cipher, iv, &mut buffer);
        }
        // The first 16 bytes of E as a big-endian number, modulo 3. Since 256
        // is 1 modulo 3, that is the sum of the bytes modulo 3.
        let sum: usize = buffer.iter().take(16).map(|&b| usize::from(b)).sum();
        k = match sum % 3 {
            0 => Sha256::digest(&buffer).to_vec(),
            1 => Sha384::digest(&buffer).to_vec(),
            _ => Sha512::digest(&buffer).to_vec(),
        };
        round += 1;
        // At least 64 rounds, then until the last byte of E is at most the
        // round number less 32. Round 288 ends it whatever E is.
        if round >= 64 && buffer.last().is_none_or(|&last| usize::from(last) <= round - 32) {
            break;
        }
    }
    k.truncate(32);
    k
}

/// A generator of random bytes for a file that is written with an encryption of its own: AES-256 in counter mode, keyed with
/// a seed that the caller drew from the system's cryptographic generator (`BCryptGenRandom` on Windows). The engine has no
/// source of randomness and does not look for one: the file key, the salts, the file identifier and every initialisation vector
/// come from here, and a seed that is guessable would make all of them guessable, so the caller must draw a new seed from the
/// system for every file it writes and not use one twice. The output of AES under a secret key is as good as random for the
/// 2^64 blocks a counter can count, which is more than a file needs.
pub struct Drbg {
    cipher: Aes256,
    counter: u128,
}

impl Drbg {
    /// `seed` is 32 bytes from the system's generator (see above).
    pub fn new(seed: &[u8; 32]) -> Drbg {
        let key: Array<u8, U32> = Array::from(*seed);
        Drbg { cipher: Aes256::new(&key), counter: 0 }
    }

    /// The next 16 random bytes.
    pub fn block(&mut self) -> [u8; 16] {
        let mut block: Array<u8, U16> = Array::from(self.counter.to_be_bytes());
        self.counter = self.counter.wrapping_add(1);
        self.cipher.encrypt_block(&mut block);
        let mut out = [0u8; 16];
        out.copy_from_slice(block.as_slice());
        out
    }

    /// Fill `out` with random bytes.
    pub fn fill(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(16) {
            let block = self.block();
            for (dst, src) in chunk.iter_mut().zip(block) {
                *dst = src;
            }
        }
    }

    /// `N` random bytes.
    pub fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        self.fill(&mut out);
        out
    }
}

/// Where the initialisation vectors of the AES data we write come from. Either a [`Drbg`] (a file with an encryption of its own,
/// whose caller drew a seed from the system), or, for a file that keeps the encryption it was read with, a hash chain: the vector
/// of CBC mode has to be unpredictable, not secret, and the first 16 bytes of SHA-256 over a seed and a counter are that. The seed
/// hashes what the standard library's `RandomState` offers (keys the operating system drew at random for this process) together
/// with the clock, the process number and an address (which moves with address space randomisation). The counter makes every
/// vector different from the last.
pub struct IvSource {
    seed: [u8; 32],
    counter: Cell<u64>,
    drbg: Option<RefCell<Drbg>>,
}

impl Default for IvSource {
    fn default() -> Self {
        IvSource::new()
    }
}

impl IvSource {
    pub fn new() -> Self {
        let mut material = Vec::with_capacity(64);
        for _ in 0..2 {
            // A new `RandomState` has fresh random keys; a hasher built from it
            // turns them into numbers.
            material.extend_from_slice(&RandomState::new().build_hasher().finish().to_le_bytes());
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        material.extend_from_slice(&now.as_nanos().to_le_bytes());
        material.extend_from_slice(&std::process::id().to_le_bytes());
        let local = 0u8;
        material.extend_from_slice(&(std::ptr::from_ref(&local) as usize).to_le_bytes());
        IvSource { seed: sha256(&material), counter: Cell::new(0), drbg: None }
    }

    /// The vectors are the output of `drbg`.
    pub fn from_drbg(drbg: Drbg) -> Self {
        IvSource { seed: [0u8; 32], counter: Cell::new(0), drbg: Some(RefCell::new(drbg)) }
    }

    /// The next 16 bytes of the generator, if the vectors come from one (a file with an encryption of its own).
    pub fn generated(&self) -> Option<[u8; 16]> {
        self.drbg.as_ref().map(|drbg| drbg.borrow_mut().block())
    }

    pub fn next_iv(&self) -> [u8; 16] {
        if let Some(block) = self.generated() {
            return block;
        }
        let n = self.counter.get();
        self.counter.set(n.wrapping_add(1));
        let mut input = [0u8; 40];
        for (dst, src) in input.iter_mut().zip(self.seed.iter().chain(n.to_le_bytes().iter())) {
            *dst = *src;
        }
        let digest = sha256(&input);
        let mut iv = [0u8; 16];
        for (dst, src) in iv.iter_mut().zip(digest.iter()) {
            *dst = *src;
        }
        iv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aes_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, CipherError> {
        aes_decrypt_vec(key, data.to_vec())
    }

    fn hex(s: &str) -> Vec<u8> {
        let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        digits
            .chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn rc4_known_answers() {
        // RFC 6229 section 2, key 0102030405 (40 bits), offset 0.
        let mut zeros = vec![0u8; 16];
        rc4(&hex("0102030405"), &mut zeros);
        assert_eq!(zeros, hex("b2396305f03dc027ccc3524a0a1118a8"));
        // The classic "Key" / "Plaintext" vector.
        let mut text = b"Plaintext".to_vec();
        rc4(b"Key", &mut text);
        assert_eq!(text, hex("bbf316e8d940af0ad3"));
        // Symmetric.
        rc4(b"Key", &mut text);
        assert_eq!(text, b"Plaintext");
        // A key longer than 256 bytes still works; an empty key changes nothing.
        let mut data = b"abc".to_vec();
        rc4(&[], &mut data);
        assert_eq!(data, b"abc");
        rc4(&[7u8; 300], &mut data);
        assert_ne!(data, b"abc");
    }

    #[test]
    fn hashes_known_answers() {
        assert_eq!(md5(b"abc").to_vec(), hex("900150983cd24fb0d6963f7d28e17f72"));
        assert_eq!(
            sha256(b"abc").to_vec(),
            hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn aes_cbc_known_answers() {
        // NIST SP 800-38A F.2.1 (AES-128 CBC, first two blocks) and F.2.5 (AES-256).
        let iv = hex("000102030405060708090a0b0c0d0e0f");
        let plain = hex("6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51");
        let key128 = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let mut out = Vec::new();
        aes_encrypt_into(&key128, iv.clone().try_into().unwrap(), &plain, &mut out).unwrap();
        assert_eq!(out.get(..16).unwrap(), &iv[..]);
        assert_eq!(
            out.get(16..48).unwrap(),
            &hex("7649abac8119b246cee98e9b12e9197d 5086cb9b507219ee95db113a917678b2")[..]
        );
        assert_eq!(out.len(), aes_encrypted_len(plain.len()));
        assert_eq!(aes_decrypt(&key128, &out).unwrap(), plain);
        let key256 = hex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let mut out = Vec::new();
        aes_encrypt_into(&key256, iv.try_into().unwrap(), &plain, &mut out).unwrap();
        assert_eq!(
            out.get(16..48).unwrap(),
            &hex("f58c4c04d6e5f1ba779eabfb5f7bfbd6 9cfc4e967edb808d679f777bc6702c7d")[..]
        );
        assert_eq!(aes_decrypt(&key256, &out).unwrap(), plain);
    }

    #[test]
    fn aes_round_trips_every_length() {
        let key = [9u8; 16];
        for len in 0..70usize {
            let data: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let mut out = Vec::new();
            aes_encrypt_into(&key, [3u8; 16], &data, &mut out).unwrap();
            assert_eq!(out.len(), aes_encrypted_len(len));
            assert_eq!(aes_decrypt(&key, &out).unwrap(), data, "length {len}");
        }
    }

    #[test]
    fn aes_damaged_data_is_an_error_not_a_panic() {
        let key = [1u8; 16];
        let mut good = Vec::new();
        aes_encrypt_into(&key, [0u8; 16], b"hello world", &mut good).unwrap();
        // Not a multiple of 16, a truncated vector, a bad key.
        assert_eq!(aes_decrypt(&key, good.get(..31).unwrap()), Err(CipherError::Length));
        assert_eq!(aes_decrypt(&key, good.get(..7).unwrap()), Err(CipherError::Length));
        assert_eq!(aes_decrypt(&[1u8; 5], &good), Err(CipherError::Key));
        // Empty and vector-only data read as an empty string.
        assert_eq!(aes_decrypt(&key, &[]).unwrap(), Vec::<u8>::new());
        assert_eq!(aes_decrypt(&key, &[0u8; 16]).unwrap(), Vec::<u8>::new());
    }

    /// Data encrypted the way a writer does that does not pad (or pads wrongly): the
    /// decrypted bytes are all kept, as qpdf and pdf.js keep them.
    #[test]
    fn aes_data_without_valid_padding_keeps_every_byte() {
        let key = [1u8; 16];
        let encrypt_raw = |plain: &[u8]| {
            let cipher = Aes128::new_from_slice(&key).unwrap();
            let mut body = plain.to_vec();
            cbc_encrypt_in_place(&cipher, &[7u8; 16], &mut body);
            let mut out = vec![7u8; 16];
            out.extend_from_slice(&body);
            out
        };
        // Whole blocks of text: the last byte is 'x', far beyond 16.
        let text = b"BT /F1 12 Tf (no padding here, 32 bytes) Tj ET.".get(..32).unwrap().to_vec();
        assert_eq!(aes_decrypt(&key, &encrypt_raw(&text)).unwrap(), text);
        // A last byte of 0: nothing to strip.
        let mut zero = vec![b'a'; 31];
        zero.push(0);
        assert_eq!(aes_decrypt(&key, &encrypt_raw(&zero)).unwrap(), zero);
        // A last byte of 5 whose four neighbours are not 5 is not padding: every byte
        // stays (qpdf and pdf.js strip only when all five are 5).
        let mut five = vec![b'a'; 31];
        five.push(5);
        assert_eq!(aes_decrypt(&key, &encrypt_raw(&five)).unwrap(), five);
        // A last byte of 10 (a newline) in unpadded text: kept.
        let mut newline = vec![b'a'; 31];
        newline.push(b'\n');
        assert_eq!(aes_decrypt(&key, &encrypt_raw(&newline)).unwrap(), newline);
        // Five bytes of 5: padding.
        let mut padded = vec![b'a'; 27];
        padded.extend([5u8; 5]);
        assert_eq!(aes_decrypt(&key, &encrypt_raw(&padded)).unwrap(), vec![b'a'; 27]);
        // Valid padding still works, a whole block of it too.
        let mut sixteen = vec![b'b'; 16];
        sixteen.extend([16u8; 16]);
        assert_eq!(aes_decrypt(&key, &encrypt_raw(&sixteen)).unwrap(), vec![b'b'; 16]);
    }

    #[test]
    fn one_block_of_ecb_is_decrypted() {
        // FIPS 197 appendix C.3: AES-256.
        let key: Vec<u8> = (0u8..32).collect();
        let cipher_text = hex("8ea2b7ca516745bfeafc49904b496089");
        assert_eq!(aes256_decrypt_block(&key, &cipher_text).unwrap().to_vec(), hex("00112233445566778899aabbccddeeff"));
        assert_eq!(aes256_decrypt_block(&key, &cipher_text[..15]), Err(CipherError::Length));
        assert_eq!(aes256_decrypt_block(&key[..16], &cipher_text), Err(CipherError::Key));
    }

    #[test]
    fn revision_6_hash_terminates_and_depends_on_everything() {
        let a = password_hash(6, b"secret", b"saltsalt", b"");
        assert_eq!(a.len(), 32);
        assert_eq!(a, password_hash(6, b"secret", b"saltsalt", b""));
        assert_ne!(a, password_hash(6, b"secreT", b"saltsalt", b""));
        assert_ne!(a, password_hash(6, b"secret", b"saltsalT", b""));
        assert_ne!(a, password_hash(6, b"secret", b"saltsalt", b"u"));
        assert_ne!(a, password_hash(5, b"secret", b"saltsalt", b""));
        // Revision 5 is one SHA-256.
        assert_eq!(password_hash(5, b"secret", b"saltsalt", b"").to_vec(), sha256(b"secretsaltsalt").to_vec());
    }

    #[test]
    fn initialisation_vectors_differ() {
        let source = IvSource::new();
        let a = source.next_iv();
        let b = source.next_iv();
        assert_ne!(a, b);
        assert_ne!(IvSource::new().next_iv(), a);
    }

    #[test]
    fn one_block_of_ecb_is_encrypted_and_the_file_key_wrapping_round_trips() {
        // FIPS 197 appendix C.3: AES-256.
        let key: Vec<u8> = (0u8..32).collect();
        let plain: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        assert_eq!(aes256_encrypt_block(&key, plain).unwrap().to_vec(), hex("8ea2b7ca516745bfeafc49904b496089"));
        assert_eq!(aes256_encrypt_block(&key[..16], plain), Err(CipherError::Key));
        // The file key in /UE and /OE: two blocks, a zero vector, no padding; the reader's unwrapping gives it back.
        let file_key = [0x5Au8; 32];
        let wrapped = aes256_encrypt_no_padding(&key, &file_key).unwrap();
        assert_eq!(wrapped.len(), 32);
        assert_ne!(wrapped, file_key);
        assert_eq!(aes256_decrypt_no_padding(&key, &wrapped).unwrap(), file_key);
        assert_eq!(aes256_encrypt_no_padding(&key, &file_key[..20]), Err(CipherError::Length));
    }

    #[test]
    fn the_generator_is_aes_in_counter_mode_under_the_seed() {
        let seed: [u8; 32] = std::array::from_fn(|i| i as u8 + 1);
        let mut a = Drbg::new(&seed);
        // The blocks are the encryptions of 0, 1, 2 (big-endian, 128 bits) under the seed as the key.
        for counter in 0u128..3 {
            assert_eq!(a.block(), aes256_encrypt_block(&seed, counter.to_be_bytes()).unwrap(), "block {counter}");
        }
        // The same seed gives the same bytes, another seed other bytes; `fill` is the blocks one after the other.
        let (mut b, mut c) = (Drbg::new(&seed), Drbg::new(&[9u8; 32]));
        let mut forty = [0u8; 40];
        b.fill(&mut forty);
        let mut again = Drbg::new(&seed);
        assert_eq!(forty[..16], again.block());
        assert_eq!(forty[16..32], again.block());
        assert_eq!(forty[32..], again.block()[..8]);
        assert_ne!(c.block(), forty[..16]);
        // Nothing repeats over a long run.
        let mut seen = std::collections::HashSet::new();
        let mut d = Drbg::new(&seed);
        assert!((0..5000).all(|_| seen.insert(d.block())));
    }

    #[test]
    fn initialisation_vectors_from_a_generator_are_its_blocks() {
        let seed = [3u8; 32];
        let source = IvSource::from_drbg(Drbg::new(&seed));
        let mut same = Drbg::new(&seed);
        assert_eq!(source.next_iv(), same.block());
        assert_eq!(source.next_iv(), same.block());
    }
}

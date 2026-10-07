//! Encryption: the standard security handler of ISO 32000-1 7.6 (revisions 2
//! to 4: RC4 and AES-128) and its extension for AES-256 (revisions 5 and 6,
//! ISO 32000-2, whose text we do not have: those parts follow what pdf.js
//! `src/core/crypto.js` and qpdf `libqpdf/QPDF_encryption.cc` do, and are
//! checked against qpdf byte for byte by the tests).
//!
//! A [`Security`] holds what the `/Encrypt` dictionary says and, once a
//! password has been accepted, the file key. The document uses it to decrypt
//! every string and stream as an object is read (so everything above the
//! reader sees plain data), and the writer uses a copy of it to encrypt the
//! objects of a new file under the same key ([`OutputCrypt`]).

use crate::cipher::{self, CipherError, IvSource};
use crate::error::{Error, Result};
use crate::info::pdf_doc_char;
use crate::object::{Dict, Name, ObjRef, Object};

/// The padding string of Algorithm 2 (7.6.3.3 step a).
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00,
    0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Revision 5 and 6 passwords are cut to 127 bytes of UTF-8 (pdf.js, qpdf).
const MAX_PASSWORD_V5: usize = 127;

/// How data is encrypted: a crypt filter's method (7.6.5 Table 25), or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Not encrypted (the `Identity` filter, or `/CFM /None`).
    None,
    /// RC4 (`/CFM /V2`, or any file below version 4).
    Rc4,
    /// AES-128 in CBC mode (`/CFM /AESV2`).
    AesV2,
    /// AES-256 in CBC mode (`/CFM /AESV3`).
    AesV3,
}

/// Which password opened the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordKind {
    User,
    Owner,
}

/// How the file was opened: with the user or the owner password, and whether
/// that was the empty password that is always tried first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Auth {
    pub kind: PasswordKind,
    pub empty: bool,
}

/// What the document's permission flags `/P` allow (7.6.3.2 Table 22), worded
/// as qpdf words them: which bit answers which question depends on the
/// revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    pub print: bool,
    pub print_high_quality: bool,
    pub modify: bool,
    pub copy: bool,
    pub annotate: bool,
    pub fill_forms: bool,
    pub assemble: bool,
    pub accessibility: bool,
}

impl Permissions {
    /// Nothing is allowed.
    pub fn none() -> Permissions {
        Permissions::from_flags(0, 3)
    }

    pub fn from_flags(p: u32, revision: i64) -> Permissions {
        let bit = |n: u32| p & (1u32 << (n - 1)) != 0;
        let old = revision < 3;
        let print = bit(3);
        Permissions {
            print,
            // Revision 2 has no separate bit for the best quality.
            print_high_quality: print && (old || bit(12)),
            modify: bit(4),
            copy: bit(5),
            annotate: bit(6),
            fill_forms: if old { bit(6) } else { bit(9) },
            assemble: if old { bit(4) } else { bit(11) },
            accessibility: if old { bit(5) } else { bit(10) },
        }
    }

    /// Is everything allowed?
    pub fn allows_everything(&self) -> bool {
        self.list().iter().all(|(_, allowed)| *allowed)
    }

    /// Each permission in words, in a fixed order.
    pub fn list(&self) -> [(&'static str, bool); 8] {
        [
            ("print", self.print),
            ("print in high quality", self.print_high_quality),
            ("modify the contents", self.modify),
            ("copy or extract text and graphics", self.copy),
            ("add or change annotations", self.annotate),
            ("fill in form fields", self.fill_forms),
            ("assemble (insert, rotate, delete pages)", self.assemble),
            ("extract for accessibility", self.accessibility),
        ]
    }
}

/// What `info` says about the encryption of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encryption {
    /// `/V` and `/R` of the encryption dictionary.
    pub version: i64,
    pub revision: i64,
    pub key_bits: usize,
    pub stream_method: Method,
    pub string_method: Method,
    /// How embedded files are encrypted (`/EFF`; the same as the streams unless
    /// the file says otherwise).
    pub file_method: Method,
    pub encrypt_metadata: bool,
    /// What the author allows: the flags of the file, or nothing at all when it
    /// was opened with the user password and the check value `/Perms` does
    /// not agree with `/P` (see [`Encryption::perms_valid`]).
    pub permissions: Permissions,
    /// Revisions 5 and 6 keep a check value of the permissions, which only the
    /// file key can decrypt; false when it is missing, unreadable or says
    /// something other than `/P` (someone edited `/P`, which is not part of the
    /// key in those revisions). Always true for the older revisions, where
    /// `/P` goes into the key.
    pub perms_valid: bool,
    /// How the file was opened; `None` when it is still locked.
    pub opened: Option<Auth>,
    /// Is a password needed: the empty one does not open the file.
    pub password_needed: bool,
}

impl Encryption {
    /// Is a password needed to open the file: the empty one does not open it.
    pub fn needs_password(&self) -> bool {
        self.password_needed
    }

    /// How strong the method is, for comparing two files: none, RC4 40-bit,
    /// longer RC4, AES-128, AES-256.
    pub fn strength(&self) -> u8 {
        let rank = |m: Method| match m {
            Method::None => 0,
            Method::Rc4 if self.key_bits <= 40 => 1,
            Method::Rc4 => 2,
            Method::AesV2 => 3,
            Method::AesV3 => 4,
        };
        rank(self.stream_method).max(rank(self.string_method)).max(rank(self.file_method))
    }

    fn method_text(&self, m: Method) -> String {
        match m {
            Method::None => "none".to_string(),
            Method::Rc4 => format!("RC4 {}-bit", self.key_bits),
            Method::AesV2 => "AES-128".to_string(),
            Method::AesV3 => "AES-256".to_string(),
        }
    }

    /// "RC4 40-bit", "RC4 128-bit", "AES-128" or "AES-256"; when streams and
    /// strings differ, both are named; embedded files that are encrypted
    /// differently from the streams are named too ("attachments only" when
    /// that is all that is encrypted).
    pub fn method_name(&self) -> String {
        let (stream, string, file) = (self.stream_method, self.string_method, self.file_method);
        if stream == Method::None && string == Method::None && file != Method::None {
            return format!("attachments only ({})", self.method_text(file));
        }
        let mut text = if stream == string {
            self.method_text(stream)
        } else {
            format!("streams {}, strings {}", self.method_text(stream), self.method_text(string))
        };
        if file != stream && file != Method::None {
            text.push_str(&format!(", attachments {}", self.method_text(file)));
        }
        text
    }
}

/// Which of "this is the user password" and "this is the owner password" to
/// try, and in what order (the second is only tried if the first fails).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    UserThenOwner,
    UserOnly,
    OwnerOnly,
}

/// Everything the encryption dictionary says.
#[derive(Debug, Clone)]
struct Params {
    v: i64,
    r: i64,
    key_bytes: usize,
    p: u32,
    o: Vec<u8>,
    u: Vec<u8>,
    oe: Vec<u8>,
    ue: Vec<u8>,
    /// `/Perms` (revisions 5 and 6), cut or padded to 16 bytes; `None` if the
    /// file has none.
    perms: Option<Vec<u8>>,
    encrypt_metadata: bool,
    /// The first element of the trailer's `/ID` (empty if there is none): part
    /// of the key for revisions 2 to 4.
    id0: Vec<u8>,
    /// `/CF`: crypt filter names and their methods; `None` for a method we do
    /// not know.
    filters: Vec<(Name, Option<Method>)>,
    stream: Method,
    string: Method,
    file: Method,
    /// The dictionary with every reference followed, to be written as it is.
    dict: Dict,
}

/// A file's encryption: the dictionary, and the key once a password has been
/// accepted.
#[derive(Clone)]
pub struct Security {
    params: Params,
    key: Option<Vec<u8>>,
    auth: Option<Auth>,
    /// The object number of the encryption dictionary, which is not encrypted.
    encrypt_num: Option<u32>,
    /// The object number of the document's metadata stream (the catalog's
    /// `/Metadata`), which `/EncryptMetadata false` leaves in the clear.
    root_metadata: Option<u32>,
    /// Does `/Perms` agree with `/P` (revisions 5 and 6; see
    /// [`Encryption::perms_valid`])? Set when a password is accepted.
    perms_valid: bool,
    /// Whether the empty password would not have opened the file, worked out the
    /// first time `describe` is asked and kept (it can cost a hash).
    password_needed: std::cell::Cell<Option<bool>>,
}

/// What the check value `/Perms` says once the file key decrypts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PermsState {
    /// It says what `/P` and `/EncryptMetadata` say.
    Valid,
    /// It is a `/Perms` (it has the marker `adb`), and says something else.
    Disagrees,
    /// Missing, or not what a `/Perms` looks like: the key is wrong, or it is damaged.
    Garbage,
}

impl std::fmt::Debug for Security {
    /// Never shows the key, so that it cannot end up in a log or a panic message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Security")
            .field("v", &self.params.v)
            .field("r", &self.params.r)
            .field("unlocked", &self.key.is_some())
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

fn syntax(message: impl Into<String>) -> Error {
    Error::syntax(None, format!("encryption dictionary: {}", message.into()))
}

fn string_entry(dict: &Dict, key: &str) -> Option<Vec<u8>> {
    match dict.get(key) {
        Some(Object::String(s)) => Some(s.bytes.clone()),
        _ => None,
    }
}

/// Exactly `len` bytes: cut, or padded with zeros (qpdf does the same for a
/// value that is too short, so a damaged `/O` is a wrong password, not an
/// unreadable file).
fn fit(mut bytes: Vec<u8>, len: usize) -> Vec<u8> {
    bytes.resize(len, 0);
    bytes
}

fn integer(dict: &Dict, key: &str) -> Result<i64> {
    match dict.get(key) {
        Some(Object::Integer(i)) => Ok(*i),
        Some(Object::Real(r)) if r.is_finite() && r.fract() == 0.0 && r.abs() < 1e15 => Ok(*r as i64),
        Some(_) => Err(syntax(format!("/{key} is not a number"))),
        None => Err(syntax(format!("/{key} is missing"))),
    }
}

/// The method a `/CFM` name stands for (7.6.5 Table 25).
fn method_of_cfm(dict: &Dict) -> Option<Method> {
    match dict.get("CFM") {
        None => Some(Method::None),
        Some(Object::Name(n)) => match n.as_bytes() {
            b"None" => Some(Method::None),
            b"V2" => Some(Method::Rc4),
            b"AESV2" => Some(Method::AesV2),
            b"AESV3" => Some(Method::AesV3),
            _ => None,
        },
        Some(_) => None,
    }
}

impl Security {
    /// Read the encryption dictionary `dict` (references already followed).
    /// `id0` is the first element of the trailer's `/ID`; `encrypt_num` the
    /// number of the object holding the dictionary, if it is an object of its
    /// own. No password has been tried yet.
    pub(crate) fn from_dict(dict: Dict, id0: Vec<u8>, encrypt_num: Option<u32>) -> Result<Security> {
        match dict.get("Filter") {
            Some(Object::Name(n)) if n == "Standard" => {}
            Some(Object::Name(n)) if n == "Adobe.PubSec" => {
                return Err(Error::Unsupported(
                    "public-key (certificate) encryption, /Adobe.PubSec, is not supported; only files protected by a password can be opened"
                        .to_string(),
                ));
            }
            Some(Object::Name(n)) => {
                return Err(Error::Unsupported(format!(
                    "the security handler /{} is not supported; only the standard password handler is",
                    String::from_utf8_lossy(n.as_bytes())
                )));
            }
            _ => return Err(syntax("/Filter is missing")),
        }
        let v = integer(&dict, "V")?;
        let r = integer(&dict, "R")?;
        if !matches!(v, 1 | 2 | 4 | 5) {
            return Err(Error::Unsupported(format!("encryption algorithm /V {v} is not supported")));
        }
        if !(2..=6).contains(&r) {
            return Err(Error::Unsupported(format!("security handler revision /R {r} is not supported")));
        }
        if (v == 5) != (r >= 5) {
            return Err(syntax(format!("/R {r} does not go with /V {v}")));
        }
        // 7.6.3.2: /P is an unsigned 32-bit value that files write as a signed
        // one, and some as a big positive number; the low 32 bits are the flags.
        let p = u32::try_from(integer(&dict, "P")? & 0xFFFF_FFFF).map_err(|_| syntax("/P is out of range"))?;
        let o = string_entry(&dict, "O").ok_or_else(|| syntax("/O is missing or not a string"))?;
        let u = string_entry(&dict, "U").ok_or_else(|| syntax("/U is missing or not a string"))?;
        let (o, u, oe, ue) = if v == 5 {
            let oe = string_entry(&dict, "OE").ok_or_else(|| syntax("/OE is missing or not a string"))?;
            let ue = string_entry(&dict, "UE").ok_or_else(|| syntax("/UE is missing or not a string"))?;
            (fit(o, 48), fit(u, 48), fit(oe, 32), fit(ue, 32))
        } else {
            (fit(o, 32), fit(u, 32), Vec::new(), Vec::new())
        };
        let perms = if v == 5 { string_entry(&dict, "Perms").map(|p| fit(p, 16)) } else { None };
        let encrypt_metadata = v < 4 || !matches!(dict.get("EncryptMetadata"), Some(Object::Bool(false)));

        // Crypt filters (7.6.5); below version 4 everything is RC4.
        let mut filters: Vec<(Name, Option<Method>)> = Vec::new();
        if v >= 4
            && let Some(Object::Dict(cf)) = dict.get("CF")
        {
            for (name, value) in cf.iter() {
                if let Object::Dict(filter) = value {
                    filters.push((name.clone(), method_of_cfm(filter)));
                }
            }
        }
        let named = |key: &str| -> Option<Name> {
            match dict.get(key) {
                Some(Object::Name(n)) => Some(n.clone()),
                _ => None,
            }
        };
        let lookup = |name: &Option<Name>| -> Result<Method> {
            let Some(name) = name else { return Ok(Method::None) };
            if name == "Identity" {
                return Ok(Method::None);
            }
            match filters.iter().find(|(n, _)| n == name) {
                Some((_, Some(m))) => Ok(*m),
                Some((_, None)) => Err(Error::Unsupported(format!(
                    "the crypt filter /{} uses an encryption method this program does not know",
                    String::from_utf8_lossy(name.as_bytes())
                ))),
                None => Err(syntax(format!("the crypt filter /{} is not defined in /CF", String::from_utf8_lossy(name.as_bytes())))),
            }
        };
        let (mut stream, mut string, mut file) = if v >= 4 {
            let stmf = named("StmF");
            let eff = named("EFF").or_else(|| stmf.clone());
            (lookup(&stmf)?, lookup(&named("StrF"))?, lookup(&eff)?)
        } else {
            (Method::Rc4, Method::Rc4, Method::Rc4)
        };
        if v == 5 {
            // Version 5 is always AES-256 under the file key (pdf.js does the
            // same for a producer that wrongly names /AESV2).
            for m in [&mut stream, &mut string, &mut file] {
                if *m != Method::None {
                    *m = Method::AesV3;
                }
            }
        }

        // The length of the key in bytes. Version 1 and revision 2 use 40 bits;
        // versions 2 and 3 say it in /Length; for version 4 /Length, else the
        // crypt filter's, else 128 bits; AES-128 is 128 bits whatever is said.
        let length_bits = match dict.get("Length") {
            None => None,
            Some(Object::Integer(n)) => Some(*n),
            Some(_) => return Err(syntax("/Length is not an integer")),
        };
        let key_bits = if v == 5 {
            256
        } else if v == 1 || r == 2 {
            40
        } else if v == 4 && (stream == Method::AesV2 || string == Method::AesV2 || file == Method::AesV2) {
            128
        } else {
            match length_bits {
                Some(bits) => bits,
                None if v == 4 => {
                    // The crypt filter's length counts bytes for the standard
                    // handler (Table 25), though some writers give bits.
                    let from_filter = named("StmF")
                        .and_then(|stmf| match dict.get("CF") {
                            Some(Object::Dict(cf)) => cf.iter().find(|(n, _)| **n == stmf).map(|(_, f)| f),
                            _ => None,
                        })
                        .and_then(|f| f.as_dict())
                        .and_then(|f| f.get_int("Length"));
                    match from_filter {
                        Some(n) if (5..=16).contains(&n) => n * 8,
                        Some(n) => n,
                        None => 128,
                    }
                }
                None => 40,
            }
        };
        if !(40..=128).contains(&key_bits) && v != 5 || key_bits % 8 != 0 {
            return Err(syntax(format!("the key length /Length {key_bits} is not a multiple of 8 from 40 to 128")));
        }
        let key_bytes = usize::try_from(key_bits / 8).map_err(|_| syntax("the key length is out of range"))?;
        Ok(Security {
            params: Params { v, r, key_bytes, p, o, u, oe, ue, perms, encrypt_metadata, id0, filters, stream, string, file, dict },
            key: None,
            auth: None,
            encrypt_num,
            root_metadata: None,
            perms_valid: true,
            password_needed: std::cell::Cell::new(None),
        })
    }

    /// Has a password been accepted?
    pub fn is_unlocked(&self) -> bool {
        self.key.is_some()
    }

    pub(crate) fn encrypt_metadata(&self) -> bool {
        self.params.encrypt_metadata
    }

    /// Say which object is the document's metadata stream.
    pub(crate) fn set_root_metadata(&mut self, num: Option<u32>) {
        self.root_metadata = num;
    }

    /// What the author allows: the flags `/P`. For revisions 5 and 6 they are
    /// not part of the key, so anyone could have edited them; `/Perms` (which
    /// only the file key decrypts) is the check. When it does not agree, a file
    /// opened with the user password is taken to allow nothing; the owner has
    /// every right anyway.
    pub fn permissions(&self) -> Permissions {
        let owner = self.auth.is_some_and(|a| a.kind == PasswordKind::Owner);
        if self.perms_valid || owner {
            Permissions::from_flags(self.params.p, self.params.r)
        } else {
            Permissions::none()
        }
    }

    /// Does `/Perms` agree with `/P` (always true below revision 5)?
    pub fn perms_valid(&self) -> bool {
        self.perms_valid
    }

    /// Is this the same encryption as `other`: the same file key and the same
    /// encryption dictionary (a file and the parts that were split from it)?
    /// Both must be open. Then what one file may be written under, the other may.
    pub fn same_protection(&self, other: &Security) -> bool {
        let (Some(a), Some(b)) = (&self.key, &other.key) else { return false };
        a == b
            && self.params.dict == other.params.dict
            && self.params.id0 == other.params.id0
            && self.params.v == other.params.v
            && self.params.r == other.params.r
            && self.params.p == other.params.p
            && self.params.o == other.params.o
            && self.params.u == other.params.u
            && self.params.oe == other.params.oe
            && self.params.ue == other.params.ue
            && self.params.perms == other.params.perms
    }
    /// The file key, if a password has been accepted. Tests compare it with
    /// the one qpdf shows.
    pub fn file_key(&self) -> Option<&[u8]> {
        self.key.as_deref()
    }

    /// A summary for `info`.
    pub fn describe(&self) -> Encryption {
        let p = &self.params;
        let key_bits = match (p.stream, p.string) {
            (Method::AesV3, _) | (_, Method::AesV3) => 256,
            (Method::AesV2, _) | (_, Method::AesV2) => 128,
            _ => p.key_bytes * 8,
        };
        // Whether the empty password would have opened the file is only worked out
        // here (for `info` and the commands that look at the permissions): when a
        // password was given, the empty one was not tried.
        let password_needed = match self.password_needed.get() {
            Some(known) => known,
            None => {
                let worked_out = match self.auth {
                    None => true,
                    Some(a) if a.empty => false,
                    Some(_) => self.try_text("", self.first_order()).is_none(),
                };
                self.password_needed.set(Some(worked_out));
                worked_out
            }
        };
        Encryption {
            version: p.v,
            revision: p.r,
            key_bits,
            stream_method: p.stream,
            string_method: p.string,
            file_method: p.file,
            encrypt_metadata: p.encrypt_metadata,
            permissions: self.permissions(),
            perms_valid: self.perms_valid,
            opened: self.auth,
            password_needed,
        }
    }

    // --- passwords --------------------------------------------------------------

    /// Try `password` (if one is given) and then the empty password, as user
    /// password and as owner password. Returns whether one of them opened the
    /// file; then the key and the kind of password are recorded. A password that
    /// is the user's and the owner's both is recorded as the user's; whether the
    /// holder of the file has the owner's rights is asked with
    /// [`Security::is_owner_password`].
    ///
    /// The cases are kept to what is needed, because each check of a revision 5
    /// or 6 password is a hash that takes about a millisecond (revision 6 runs
    /// at least 64 rounds of AES and SHA-2, and the owner's hash covers more
    /// data): a password that is given goes first as the user's and then as the
    /// owner's, and the empty password (which opens most files that have
    /// restrictions only) comes after it, as the user's and, for the older
    /// revisions where it costs nothing, as the owner's. pdf.js does not try an
    /// empty owner password either. (The task asked for the empty password
    /// first; with a password given that spends a hash on a password nobody
    /// meant, so for a given password it is tried last.)
    pub(crate) fn authenticate(&mut self, password: &str) -> bool {
        self.password_needed.set(None);
        let mut found = None;
        if !password.is_empty() {
            found = self.try_text(password, Order::UserThenOwner).map(|(key, kind)| (key, Auth { kind, empty: false }));
        }
        if found.is_none() {
            found = self.try_text("", self.first_order()).map(|(key, kind)| (key, Auth { kind, empty: true }));
        }
        match found {
            Some((key, auth)) => {
                self.perms_valid = self.params.r < 5 || self.perms_state(&key) == PermsState::Valid;
                self.key = Some(key);
                self.auth = Some(auth);
                true
            }
            None => false,
        }
    }

    /// The empty password is tried as the user's, and for the older revisions
    /// also as the owner's.
    fn first_order(&self) -> Order {
        if self.params.r >= 5 { Order::UserOnly } else { Order::UserThenOwner }
    }

    /// Is `password` the owner password, whether or not it is what opened the
    /// file? The user password may be the owner's too, and a file may open with
    /// the empty password as the user's. Revisions 5 and 6: the check hash only,
    /// which is what costs least when the answer is no.
    pub fn is_owner_password(&self, password: &str) -> bool {
        if self.params.r >= 5 {
            let p = &self.params;
            let (Some(o_hash), Some(o_val), Some(u48)) = (p.o.get(..32), p.o.get(32..40), p.u.get(..48)) else {
                return false;
            };
            return self.encodings(password).iter().any(|pw| cipher::password_hash(p.r, pw, o_val, u48) == o_hash);
        }
        self.try_text(password, Order::OwnerOnly).is_some()
    }

    fn try_text(&self, pw: &str, order: Order) -> Option<(Vec<u8>, PasswordKind)> {
        let encodings = self.encodings(pw);
        // Every way of writing the password as the user's before any as the
        // owner's: the owner's check costs more.
        let as_user = || encodings.iter().find_map(|bytes| self.try_password(bytes, Order::UserOnly));
        let as_owner = || encodings.iter().find_map(|bytes| self.try_password(bytes, Order::OwnerOnly));
        match order {
            Order::UserThenOwner => as_user().or_else(as_owner),
            Order::UserOnly => as_user(),
            Order::OwnerOnly => as_owner(),
        }
    }

    /// The byte strings a password may stand for. Revisions 2 to 4: the text in
    /// PDFDocEncoding if it can be written in it, and its UTF-8 bytes (files
    /// exist with either; PDFium's tests use both). Revisions 5 and 6: UTF-8
    /// as typed, then after the light preparation of [`prepare_password`]
    /// (SASLprep proper needs Unicode tables; pdf.js tries the prepared and
    /// then the raw text, qpdf neither), each cut to 127 bytes.
    fn encodings(&self, pw: &str) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut add = |bytes: Vec<u8>| {
            if !out.contains(&bytes) {
                out.push(bytes);
            }
        };
        if self.params.r >= 5 {
            let mut raw = pw.as_bytes().to_vec();
            raw.truncate(MAX_PASSWORD_V5);
            add(raw);
            let mut prepared = prepare_password(pw).into_bytes();
            prepared.truncate(MAX_PASSWORD_V5);
            add(prepared);
        } else {
            if let Some(doc) = pdf_doc_bytes(pw) {
                add(doc);
            }
            add(pw.as_bytes().to_vec());
        }
        out
    }

    fn try_password(&self, pw: &[u8], order: Order) -> Option<(Vec<u8>, PasswordKind)> {
        if self.params.r >= 5 { self.try_password_v5(pw, order) } else { self.try_password_v4(pw, order) }
    }

    // Revisions 2 to 4 (7.6.3.3, 7.6.3.4).

    fn try_password_v4(&self, pw: &[u8], order: Order) -> Option<(Vec<u8>, PasswordKind)> {
        // One of the two: `order` is `UserOnly` or `OwnerOnly` here.
        let as_user = || {
            let key = self.file_key_v4(&pad_password(pw));
            self.user_password_ok(&key).then_some((key, PasswordKind::User))
        };
        // Algorithm 7: the owner password unlocks the user password stored in /O.
        let as_owner = || {
            let user_password = self.user_password_from_owner(pw);
            let key = self.file_key_v4(&user_password);
            self.user_password_ok(&key).then_some((key, PasswordKind::Owner))
        };
        if order == Order::OwnerOnly { as_owner() } else { as_user() }
    }

    /// Algorithm 2: the file key for a user password (already padded).
    fn file_key_v4(&self, padded: &[u8; 32]) -> Vec<u8> {
        let p = &self.params;
        let mut input = Vec::with_capacity(32 + 32 + 4 + p.id0.len() + 4);
        input.extend_from_slice(padded);
        input.extend_from_slice(&p.o);
        input.extend_from_slice(&p.p.to_le_bytes());
        input.extend_from_slice(&p.id0);
        if p.r >= 4 && !p.encrypt_metadata {
            input.extend_from_slice(&[0xFF; 4]);
        }
        let mut digest = cipher::md5(&input);
        let n = p.key_bytes.min(16);
        if p.r >= 3 {
            for _ in 0..50 {
                digest = cipher::md5(digest.get(..n).unwrap_or(&digest));
            }
        }
        digest.get(..n).unwrap_or(&digest).to_vec()
    }

    /// Algorithms 4 to 6: does the file key `key` (made from the user password)
    /// reproduce `/U`?
    fn user_password_ok(&self, key: &[u8]) -> bool {
        let p = &self.params;
        if p.r == 2 {
            let mut data = PAD;
            cipher::rc4(key, &mut data);
            return p.u.get(..32) == Some(data.as_slice());
        }
        let mut input = PAD.to_vec();
        input.extend_from_slice(&p.id0);
        let mut data = cipher::md5(&input);
        cipher::rc4(key, &mut data);
        for i in 1..=19u8 {
            let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            cipher::rc4(&k, &mut data);
        }
        p.u.get(..16) == Some(data.as_slice())
    }

    /// Algorithm 7 steps a and b: what `/O` decrypts to under the owner password
    /// `pw`: the padded user password, if `pw` is right.
    fn user_password_from_owner(&self, pw: &[u8]) -> [u8; 32] {
        let p = &self.params;
        let mut digest = cipher::md5(&pad_password(pw));
        if p.r >= 3 {
            for _ in 0..50 {
                digest = cipher::md5(&digest);
            }
        }
        let n = p.key_bytes.min(16);
        let key = digest.get(..n).unwrap_or(&digest);
        let mut data = [0u8; 32];
        for (dst, src) in data.iter_mut().zip(&p.o) {
            *dst = *src;
        }
        if p.r == 2 {
            cipher::rc4(key, &mut data);
        } else {
            for i in (0..=19u8).rev() {
                let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
                cipher::rc4(&k, &mut data);
            }
        }
        data
    }

    // Revisions 5 and 6 (Algorithm 2.A and its checks, after pdf.js and qpdf).

    fn try_password_v5(&self, pw: &[u8], order: Order) -> Option<(Vec<u8>, PasswordKind)> {
        let p = &self.params;
        let r = p.r;
        let (o_hash, o_val, o_key) = (p.o.get(..32)?, p.o.get(32..40)?, p.o.get(40..48)?);
        let (u_hash, u_val, u_key) = (p.u.get(..32)?, p.u.get(32..40)?, p.u.get(40..48)?);
        let u48 = p.u.get(..48)?;
        let file_key = |salt: &[u8], udata: &[u8], wrapped: &[u8]| -> Option<Vec<u8>> {
            let intermediate = cipher::password_hash(r, pw, salt, udata);
            cipher::aes256_decrypt_no_padding(&intermediate, wrapped).ok()
        };
        // The owner's hash covers more data than the user's, so it is the one to
        // save: the file key comes first, and if `/Perms` decrypts to what it
        // should look like under that key, the password is right (a wrong one
        // passes with a chance of 2^-24) and the hash that checks it is not
        // computed. Only when `/Perms` is missing or damaged does the check hash
        // decide.
        let as_owner = || {
            let key = file_key(o_key, u48, &p.oe)?;
            match self.perms_state(&key) {
                PermsState::Valid | PermsState::Disagrees => Some((key, PasswordKind::Owner)),
                PermsState::Garbage => (cipher::password_hash(r, pw, o_val, u48) == o_hash).then_some((key, PasswordKind::Owner)),
            }
        };
        let as_user = || {
            (cipher::password_hash(r, pw, u_val, &[]) == u_hash)
                .then(|| file_key(u_key, &[], &p.ue).map(|k| (k, PasswordKind::User)))
                .flatten()
        };
        if order == Order::OwnerOnly { as_owner() } else { as_user() }
    }

    /// Decrypt `/Perms` with the file key and see what it says (ISO 32000-2
    /// Algorithm 2.A, and what qpdf and pdf.js check): bytes 0 to 3 are `/P`
    /// (low byte first), 4 to 7 are 0xFF, byte 8 is `T` or `F` for
    /// `/EncryptMetadata`, bytes 9 to 11 are `adb`. Only the marker and `/P` are
    /// compared: `/P` is what the file is not allowed to be edited in, and real
    /// files get the byte for the metadata wrong (bug_644.pdf of pdf.js says `F`
    /// where the dictionary has no `/EncryptMetadata`).
    fn perms_state(&self, key: &[u8]) -> PermsState {
        let p = &self.params;
        let Some(block) = p.perms.as_deref().and_then(|perms| cipher::aes256_decrypt_block(key, perms).ok()) else {
            return PermsState::Garbage;
        };
        // The marker, and the four bytes after `/P` that are all ones.
        if block.get(9..12) != Some(b"adb".as_slice()) || block.get(4..8) != Some([0xFF_u8; 4].as_slice()) {
            return PermsState::Garbage;
        }
        if block.get(..4) == Some(p.p.to_le_bytes().as_slice()) { PermsState::Valid } else { PermsState::Disagrees }
    }

    // --- decrypting -------------------------------------------------------------

    /// The key for data of `method` in object `r` (Algorithm 1, 7.6.2; version
    /// 5 uses the file key as it is).
    fn object_key(&self, method: Method, r: ObjRef) -> Vec<u8> {
        let Some(key) = &self.key else { return Vec::new() };
        match method {
            Method::None => Vec::new(),
            Method::AesV3 => key.clone(),
            Method::Rc4 | Method::AesV2 => {
                let mut input = key.clone();
                if self.params.v == 4 && input.len() < 16 {
                    input.resize(16, 0);
                }
                let key_len = input.len();
                let [n0, n1, n2, _] = r.num.to_le_bytes();
                input.extend_from_slice(&[n0, n1, n2]);
                input.extend_from_slice(&r.generation.to_le_bytes());
                if method == Method::AesV2 {
                    input.extend_from_slice(b"sAlT");
                }
                let digest = cipher::md5(&input);
                let n = key_len.saturating_add(5).min(16);
                digest.get(..n).unwrap_or(&digest).to_vec()
            }
        }
    }

    /// Decrypt `data` of object `r` with `method` under `key`.
    fn decrypt_bytes(&self, method: Method, key: &[u8], r: ObjRef, mut data: Vec<u8>) -> Result<Vec<u8>> {
        match method {
            Method::None => Ok(data),
            Method::Rc4 => {
                cipher::rc4(key, &mut data);
                Ok(data)
            }
            Method::AesV2 | Method::AesV3 => cipher::aes_decrypt_vec(key, data).map_err(|e| damaged(r, e)),
        }
    }

    /// How the data of the stream with dictionary `dict` is encrypted (7.6.5):
    /// cross-reference streams not at all; a stream that names a crypt filter in
    /// its own `/Filter` array by that one; the document's metadata stream not
    /// at all when `/EncryptMetadata` is false; embedded files by `/EFF`; the
    /// rest by `/StmF`. `is_root_metadata` says the stream is the catalog's
    /// `/Metadata`. Only direct `/Filter` and `/DecodeParms` are looked at.
    pub(crate) fn stream_method(&self, dict: &Dict, is_root_metadata: bool) -> Result<Method> {
        let p = &self.params;
        if matches!(dict.get("Type"), Some(Object::Name(n)) if n == "XRef") {
            return Ok(Method::None);
        }
        if p.v < 4 {
            return Ok(p.stream);
        }
        if let Some(name) = crypt_filter_name(dict) {
            return self.filter_method(&name);
        }
        if is_root_metadata
            && !p.encrypt_metadata
            && matches!(dict.get("Type"), Some(Object::Name(n)) if n == "Metadata")
        {
            return Ok(Method::None);
        }
        if matches!(dict.get("Type"), Some(Object::Name(n)) if n == "EmbeddedFile") {
            return Ok(p.file);
        }
        Ok(p.stream)
    }

    fn filter_method(&self, name: &Name) -> Result<Method> {
        if name == "Identity" {
            return Ok(Method::None);
        }
        match self.params.filters.iter().find(|(n, _)| n == name) {
            Some((_, Some(m))) => Ok(if self.params.v == 5 && *m != Method::None { Method::AesV3 } else { *m }),
            Some((_, None)) => Err(Error::Unsupported(format!(
                "the crypt filter /{} uses an encryption method this program does not know",
                String::from_utf8_lossy(name.as_bytes())
            ))),
            None => Err(Error::syntax(
                None,
                format!(
                    "a stream names the crypt filter /{}, which the encryption dictionary does not define",
                    String::from_utf8_lossy(name.as_bytes())
                ),
            )),
        }
    }

    /// Decrypt, in place, the strings of object `obj` (which is object `r`) and
    /// the data of the stream it may be. Not for the encryption dictionary, for
    /// cross-reference streams or for the contents of signatures. Returns how
    /// many strings could not be decrypted and were left as they are: a string
    /// that is not a whole number of AES blocks was not encrypted (a program
    /// that does not know about the encryption wrote it, almost always as it
    /// is), and one such string must not cost the whole object. A stream whose
    /// data cannot be decrypted is an error.
    pub(crate) fn decrypt_object(&self, r: ObjRef, obj: &mut Object) -> Result<u32> {
        let mut kept = 0u32;
        if self.key.is_none() || self.encrypt_num == Some(r.num) {
            return Ok(kept);
        }
        let string_method = self.params.string;
        let string_key = self.object_key(string_method, r);
        match obj {
            Object::Stream(stream) => {
                let is_xref = matches!(stream.dict.get("Type"), Some(Object::Name(n)) if n == "XRef");
                if !is_xref && string_method != Method::None {
                    decrypt_dict(self, string_method, &string_key, r, &mut stream.dict, &mut kept);
                }
                let method = self.stream_method(&stream.dict, self.root_metadata == Some(r.num))?;
                if method != Method::None {
                    let key = self.object_key(method, r);
                    stream.data = self.decrypt_bytes(method, &key, r, std::mem::take(&mut stream.data))?;
                }
            }
            other if string_method != Method::None => decrypt_strings(self, string_method, &string_key, r, other, &mut kept),
            _ => {}
        }
        Ok(kept)
    }

    /// The data of a stream that was read from the file as it is, decrypted
    /// (used for object streams while a damaged file is scanned).
    pub(crate) fn decrypt_stream_data(&self, r: ObjRef, dict: &Dict, data: Vec<u8>) -> Result<Vec<u8>> {
        if self.key.is_none() || self.encrypt_num == Some(r.num) {
            return Ok(data);
        }
        let method = self.stream_method(dict, self.root_metadata == Some(r.num))?;
        let key = self.object_key(method, r);
        self.decrypt_bytes(method, &key, r, data)
    }
}

fn damaged(r: ObjRef, e: CipherError) -> Error {
    Error::syntax(None, format!("object {} {}: {}", r.num, r.generation, e.message()))
}

/// The crypt filter a stream names in its own `/Filter` and `/DecodeParms`
/// (7.4.10, 7.6.5), if it has a `/Crypt` filter: the parameters' `/Name`, or
/// `Identity` without one.
pub(crate) fn crypt_filter_name(dict: &Dict) -> Option<Name> {
    let filter = dict.get("Filter")?;
    let filters: Vec<&Object> = match filter {
        Object::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let index = filters.iter().position(|f| matches!(f, Object::Name(n) if n == "Crypt"))?;
    let parms = match dict.get("DecodeParms") {
        Some(Object::Dict(d)) => Some(d),
        Some(Object::Array(items)) => match items.get(index) {
            Some(Object::Dict(d)) => Some(d),
            _ => None,
        },
        _ => None,
    };
    match parms.and_then(|d| d.get("Name")) {
        Some(Object::Name(n)) => Some(n.clone()),
        _ => Some(Name::from("Identity")),
    }
}

/// Is this a signature dictionary, whose `/Contents` is not encrypted (the
/// signature covers those bytes)? One with `/Type /Sig` (or `/DocTimeStamp`) or
/// a `/ByteRange` of any kind, direct or an indirect reference: qpdf wants
/// `/Type /Sig` and `/ByteRange` and a string `/Contents`; `/Type` is optional
/// in the standard and a reference is a legal value for the range.
pub(crate) fn is_signature_dict(dict: &Dict) -> bool {
    dict.contains_key("ByteRange")
        || matches!(dict.get("Type"), Some(Object::Name(n)) if n == "Sig" || n == "DocTimeStamp")
}

fn decrypt_dict(sec: &Security, method: Method, key: &[u8], r: ObjRef, dict: &mut Dict, kept: &mut u32) {
    let signature = is_signature_dict(dict);
    for (name, value) in dict.iter_mut() {
        if signature && name == "Contents" {
            continue;
        }
        decrypt_strings(sec, method, key, r, value, kept);
    }
}

fn decrypt_strings(sec: &Security, method: Method, key: &[u8], r: ObjRef, obj: &mut Object, kept: &mut u32) {
    match obj {
        Object::String(s) => {
            // Not decryptable (AES data of the wrong length, say): left as it is.
            if method != Method::Rc4 && !cipher::aes_can_decrypt(key, s.bytes.len()) {
                *kept = kept.saturating_add(1);
            } else if let Ok(plain) = sec.decrypt_bytes(method, key, r, std::mem::take(&mut s.bytes)) {
                s.bytes = plain;
            }
        }
        Object::Array(items) => {
            for item in items {
                decrypt_strings(sec, method, key, r, item, kept);
            }
        }
        Object::Dict(d) => decrypt_dict(sec, method, key, r, d, kept),
        _ => {}
    }
}

/// Algorithm 2 step a: the password padded or cut to 32 bytes.
fn pad_password(pw: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (dst, src) in out.iter_mut().zip(pw.iter().take(32).chain(PAD.iter())) {
        *dst = *src;
    }
    out
}

/// The text as PDFDocEncoding bytes (Annex D), or `None` if some character is
/// not in it.
fn pdf_doc_bytes(text: &str) -> Option<Vec<u8>> {
    text.chars()
        .map(|c| (0..=255u8).find(|&b| c != '\u{FFFD}' && pdf_doc_char(b) == c))
        .collect()
}

/// The light version of SASLprep (RFC 4013) for revision 5 and 6 passwords,
/// without the Unicode tables: full-width ASCII (U+FF01 to U+FF5E) becomes
/// ASCII, no-break and other space characters (including U+3000) become a
/// plain space, and the characters that are mapped to nothing (the soft
/// hyphen, zero-width characters, the byte order mark) are removed.
pub(crate) fn prepare_password(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(u32::from(c) - 0xFF01 + 0x21),
            '\u{00A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' => Some(' '),
            '\u{00AD}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' => None,
            other => Some(other),
        })
        .collect()
}

// --- the writer's side ----------------------------------------------------------

/// What a writer needs to keep a file's encryption: the same dictionary and
/// key, and a source of initialisation vectors.
pub(crate) struct OutputCrypt {
    sec: Security,
    iv: IvSource,
}

/// How one stream is to be encrypted.
pub(crate) struct StreamPlan {
    method: Method,
    key: Vec<u8>,
}

impl OutputCrypt {
    /// `None` if `sec` is still locked.
    pub fn new(sec: &Security) -> Option<OutputCrypt> {
        sec.key.as_ref()?;
        Some(OutputCrypt { sec: sec.clone(), iv: IvSource::new() })
    }

    /// The encryption dictionary to write into the new file: the same as the
    /// old one's.
    pub fn encrypt_dict(&self) -> &Dict {
        &self.sec.params.dict
    }

    /// The first element of `/ID`, which the key depends on for revisions 2 to
    /// 4 and which the new file must therefore keep.
    pub fn id0(&self) -> &[u8] {
        &self.sec.params.id0
    }

    /// The encryption of what object number `num` of the new file holds.
    /// `is_metadata` says it is the catalog's metadata stream.
    pub fn object(&self, num: u32, is_metadata: bool) -> ObjectCrypt<'_> {
        let r = ObjRef::new(num, 0);
        let method = self.sec.params.string;
        ObjectCrypt { owner: self, num, is_metadata, string_method: method, string_key: self.sec.object_key(method, r) }
    }
}

/// The encryption of the strings and the stream of one object.
pub(crate) struct ObjectCrypt<'a> {
    owner: &'a OutputCrypt,
    num: u32,
    is_metadata: bool,
    string_method: Method,
    string_key: Vec<u8>,
}

impl ObjectCrypt<'_> {
    /// Are the strings of this object encrypted?
    pub fn encrypts_strings(&self) -> bool {
        self.string_method != Method::None
    }

    pub fn encrypt_string(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.encrypt_into(self.string_method, &self.string_key, bytes, &mut out)?;
        Ok(out)
    }

    /// How the stream whose dictionary is `dict` is encrypted; `None` if it is
    /// written as it is.
    pub fn stream_plan(&self, dict: &Dict) -> Result<Option<StreamPlan>> {
        let method = self.owner.sec.stream_method(dict, self.is_metadata)?;
        if method == Method::None {
            return Ok(None);
        }
        Ok(Some(StreamPlan { method, key: self.owner.sec.object_key(method, ObjRef::new(self.num, 0)) }))
    }

    /// How many bytes `len` bytes of stream data come to once encrypted.
    pub fn encrypted_len(plan: &StreamPlan, len: usize) -> usize {
        match plan.method {
            Method::AesV2 | Method::AesV3 => cipher::aes_encrypted_len(len),
            _ => len,
        }
    }

    /// Append the encryption of `data` to `out`.
    pub fn encrypt_stream(&self, plan: &StreamPlan, data: &[u8], out: &mut Vec<u8>) -> Result<()> {
        self.encrypt_into(plan.method, &plan.key, data, out)
    }

    fn encrypt_into(&self, method: Method, key: &[u8], data: &[u8], out: &mut Vec<u8>) -> Result<()> {
        match method {
            Method::None => out.extend_from_slice(data),
            Method::Rc4 => {
                let start = out.len();
                out.extend_from_slice(data);
                if let Some(tail) = out.get_mut(start..) {
                    cipher::rc4(key, tail);
                }
            }
            Method::AesV2 | Method::AesV3 => {
                cipher::aes_encrypt_into(key, self.owner.iv.next_iv(), data, out).map_err(|e| {
                    Error::syntax(None, format!("object {}: {}", self.num, e.message()))
                })?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_follow_the_revision() {
        // Revision 2, P = -64: only bits 1-6 reserved/clear: nothing is allowed.
        let none = Permissions::from_flags(0xFFFF_FFC0, 2);
        assert!(!none.print && !none.modify && !none.copy && !none.annotate);
        assert!(!none.allows_everything());
        // Revision 3 with everything but high-quality printing.
        let p = Permissions::from_flags(!(1u32 << 11), 3);
        assert!(p.print && !p.print_high_quality && p.modify && p.copy && p.fill_forms && p.assemble);
        assert!(!p.allows_everything());
        assert!(Permissions::from_flags(u32::MAX, 3).allows_everything());
        assert!(Permissions::from_flags(u32::MAX, 2).allows_everything());
        // Revision 2 fills forms with the annotation bit and assembles with the modify bit.
        let r2 = Permissions::from_flags(!(1u32 << 5), 2);
        assert!(!r2.annotate && !r2.fill_forms && r2.assemble);
    }

    #[test]
    fn light_password_preparation() {
        assert_eq!(prepare_password("ＡＢＣ１２３"), "ABC123");
        assert_eq!(prepare_password("a\u{3000}b\u{00A0}c"), "a b c");
        assert_eq!(prepare_password("pass\u{200B}word\u{00AD}!"), "password!");
        assert_eq!(prepare_password("密码"), "密码");
    }

    #[test]
    fn pdf_doc_encoding_of_passwords() {
        assert_eq!(pdf_doc_bytes("hôtel"), Some(vec![b'h', 0xF4, b't', b'e', b'l']));
        assert_eq!(pdf_doc_bytes("abc"), Some(b"abc".to_vec()));
        assert_eq!(pdf_doc_bytes("€"), Some(vec![0xA0]));
        assert_eq!(pdf_doc_bytes("密码"), None);
        assert_eq!(pdf_doc_bytes(""), Some(Vec::new()));
    }

    #[test]
    fn padding_a_password() {
        let p = pad_password(b"ab");
        assert_eq!(&p[..2], b"ab");
        assert_eq!(&p[2..], &PAD[..30]);
        assert_eq!(pad_password(b""), PAD);
        assert_eq!(pad_password(&[b'x'; 40]), [b'x'; 32]);
    }

    fn dict(text: &str) -> Dict {
        let mut parser = crate::parser::Parser::new(text.as_bytes(), 0);
        match parser.parse_object().unwrap() {
            Object::Dict(d) => d,
            other => panic!("{other:?}"),
        }
    }

    fn v2(extra: &str) -> String {
        format!(
            "<< /Filter /Standard /V 2 /R 3 /P -4 /O <{}> /U <{}> {extra} >>",
            "00".repeat(32),
            "11".repeat(32)
        )
    }

    #[test]
    fn dictionary_problems_are_clear_errors() {
        let ok = Security::from_dict(dict(&v2("/Length 128")), Vec::new(), None).unwrap();
        assert_eq!(ok.params.key_bytes, 16);
        assert!(!ok.is_unlocked());
        // Length: not a multiple of 8, absurd, too small, not a number.
        for bad in ["/Length 100", "/Length 100000", "/Length 24", "/Length 0", "/Length (x)"] {
            assert!(matches!(Security::from_dict(dict(&v2(bad)), Vec::new(), None), Err(Error::Syntax { .. })), "{bad}");
        }
        // Unknown revision or version.
        let r99 = v2("").replace("/R 3", "/R 99");
        assert!(matches!(Security::from_dict(dict(&r99), Vec::new(), None), Err(Error::Unsupported(_))));
        let v99 = v2("").replace("/V 2", "/V 99");
        assert!(matches!(Security::from_dict(dict(&v99), Vec::new(), None), Err(Error::Unsupported(_))));
        // V 5 needs R 5 or 6 and /OE, /UE.
        let mismatch = v2("").replace("/V 2", "/V 5");
        assert!(matches!(Security::from_dict(dict(&mismatch), Vec::new(), None), Err(Error::Syntax { .. })));
        let no_oe = v2("").replace("/V 2", "/V 5").replace("/R 3", "/R 6");
        assert!(matches!(Security::from_dict(dict(&no_oe), Vec::new(), None), Err(Error::Syntax { .. })));
        // Missing entries.
        for missing in ["/V 2 ", "/R 3 ", "/P -4 "] {
            let text = v2("").replace(missing, "");
            assert!(matches!(Security::from_dict(dict(&text), Vec::new(), None), Err(Error::Syntax { .. })), "{missing}");
        }
        // Another handler, public-key encryption.
        let other = v2("").replace("/Standard", "/Adobe.PubSec");
        match Security::from_dict(dict(&other), Vec::new(), None) {
            Err(Error::Unsupported(m)) => assert!(m.contains("public-key"), "{m}"),
            other => panic!("{other:?}"),
        }
        let custom = v2("").replace("/Standard", "/Acme");
        assert!(matches!(Security::from_dict(dict(&custom), Vec::new(), None), Err(Error::Unsupported(_))));
        assert!(matches!(Security::from_dict(Dict::new(), Vec::new(), None), Err(Error::Syntax { .. })));
        // A /O that is too short is padded, not refused (the password is then wrong).
        let short = v2("").replace(&"00".repeat(32), "0011");
        assert!(Security::from_dict(dict(&short), Vec::new(), None).is_ok());
    }

    #[test]
    fn crypt_filters_of_version_4() {
        let text = "<< /Filter /Standard /V 4 /R 4 /P -4 /Length 128 /O (x) /U (y) /CF << /StdCF << /CFM /AESV2 /Length 16 >> /Plain << /CFM /None >> /Odd << /CFM /Weird >> >> /StmF /StdCF /StrF /Identity /EFF /Plain >>";
        let s = Security::from_dict(dict(text), Vec::new(), None).unwrap();
        assert_eq!((s.params.stream, s.params.string, s.params.file), (Method::AesV2, Method::None, Method::None));
        assert_eq!(s.params.key_bytes, 16);
        // An undefined filter, or one with an unknown method, is an error when named.
        let undefined = text.replace("/StrF /Identity", "/StrF /Nope");
        assert!(matches!(Security::from_dict(dict(&undefined), Vec::new(), None), Err(Error::Syntax { .. })));
        let odd = text.replace("/StrF /Identity", "/StrF /Odd");
        assert!(matches!(Security::from_dict(dict(&odd), Vec::new(), None), Err(Error::Unsupported(_))));
        // Version 4 with RC4 reads its key length from /Length.
        let rc4 = "<< /Filter /Standard /V 4 /R 4 /P -4 /Length 40 /O (x) /U (y) /CF << /StdCF << /CFM /V2 >> >> /StmF /StdCF /StrF /StdCF >>";
        assert_eq!(Security::from_dict(dict(rc4), Vec::new(), None).unwrap().params.key_bytes, 5);
        let no_length = rc4.replace("/Length 40 ", "");
        assert_eq!(Security::from_dict(dict(&no_length), Vec::new(), None).unwrap().params.key_bytes, 16);
    }

    #[test]
    fn which_crypt_filter_a_stream_names() {
        let d = dict("<< /Filter [/Crypt /FlateDecode] /DecodeParms [<< /Type /CryptFilterDecodeParms /Name /StdCF >> null] >>");
        assert_eq!(crypt_filter_name(&d), Some(Name::from("StdCF")));
        let d = dict("<< /Filter /Crypt >>");
        assert_eq!(crypt_filter_name(&d), Some(Name::from("Identity")));
        let d = dict("<< /Filter /Crypt /DecodeParms << /Name /Identity >> >>");
        assert_eq!(crypt_filter_name(&d), Some(Name::from("Identity")));
        assert_eq!(crypt_filter_name(&dict("<< /Filter /FlateDecode >>")), None);
        assert_eq!(crypt_filter_name(&dict("<< >>")), None);
    }
}

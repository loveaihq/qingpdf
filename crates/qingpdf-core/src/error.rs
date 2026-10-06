use std::fmt;

/// Everything that can go wrong while reading or writing a PDF.
///
/// The engine never panics on file data: every problem in the input ends up
/// as one of these.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// The file is malformed at `offset` (byte position in the file, when known).
    Syntax { offset: Option<u64>, message: String },
    /// A referenced object does not exist or could not be loaded.
    MissingObject { num: u32, generation: u16 },
    /// The file uses something this layer does not support (e.g. encryption).
    Unsupported(String),
    /// A limit on what a file may ask for was hit: too much to decode, too many
    /// pages, a reference cycle, and so on. The file may be fine; it is more
    /// than we are willing to do with it.
    Limit(String),
    /// One object nests arrays and dictionaries deeper than the parser goes
    /// (7.3.6 sets no limit; ours is [`crate::parser::MAX_NESTING`]). A fault
    /// of that one object, not of the file's size: a copy can leave the object
    /// out and say so, which it cannot do for a [`Error::Limit`].
    TooDeep(String),
    /// Invalid input from the caller (bad page range, unknown image format, ...).
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn syntax(offset: impl Into<Option<u64>>, message: impl Into<String>) -> Self {
        Error::Syntax { offset: offset.into(), message: message.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Syntax { offset: Some(o), message } => write!(f, "malformed PDF at byte {o}: {message}"),
            Error::Syntax { offset: None, message } => write!(f, "malformed PDF: {message}"),
            Error::MissingObject { num, generation } => write!(f, "object {num} {generation} R not found"),
            Error::Unsupported(m) => write!(f, "not supported yet: {m}"),
            Error::Limit(m) => write!(f, "limit reached: {m}"),
            Error::TooDeep(m) => write!(f, "nested too deeply: {m}"),
            Error::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

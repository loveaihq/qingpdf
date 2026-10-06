//! qingpdf-core: the PDF engine behind qingpdf.
//!
//! Spec references in comments point to ISO 32000-1:2008 unless noted.

pub mod error;
pub mod object;

pub use error::{Error, Result};
pub use object::{Dict, Name, ObjRef, Object, PdfString, Stream};

//! qingpdf-core: the PDF engine behind qingpdf.
//!
//! Spec references in comments point to ISO 32000-1:2008 unless noted.

pub mod document;
pub mod error;
pub mod filter;
pub mod lexer;
pub mod object;
pub mod parser;
pub mod repair;
pub mod xref;

#[cfg(test)]
mod testutil;

pub use document::{Document, Page};
pub use error::{Error, Result};
pub use object::{Dict, Name, ObjRef, Object, PdfString, Stream};

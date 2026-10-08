//! qingpdf-core: the PDF engine behind qingpdf.
//!
//! Spec references in comments point to ISO 32000-1:2008 unless noted.

mod cipher;
mod codecs;
mod dests;
pub mod document;
pub mod error;
pub mod filter;
pub mod image;
pub mod info;
pub mod lexer;
pub mod object;
pub mod ops;
pub mod parser;
mod prune;
pub mod render;
pub mod repair;
pub mod security;
pub mod text;
pub mod writer;
pub mod xref;

#[cfg(test)]
mod testutil;

pub use document::{Document, Page};
pub use error::{Error, Result};
pub use writer::{Builder, Warning};
pub use object::{Dict, Name, ObjRef, Object, PdfString, Stream};

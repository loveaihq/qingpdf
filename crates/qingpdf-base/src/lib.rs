//! qingpdf-base: the PDF engine behind qingpdf, without its editing and its reader's server (those are in `qingpdf-core`, which
//! re-exports this crate's modules under the same names).
//!
//! This crate holds the code that decides how fast things are: reading and writing files, decoding, text, drawing. It is built for
//! speed; `qingpdf-core` is built for size. What `qingpdf-core` needs of the inside of this crate is public here.
//!
//! Spec references in comments point to ISO 32000-1:2008 unless noted.

pub mod cipher;
pub mod codecs;
pub mod dests;
pub mod document;
pub mod error;
pub mod filter;
pub mod forms;
pub mod image;
pub mod info;
pub mod layout;
pub mod lexer;
pub mod object;
pub mod ops;
pub mod parser;
pub mod prune;
pub mod render;
pub mod repair;
pub mod security;
pub mod text;
pub mod writer;
pub mod xref;

#[cfg(any(test, feature = "testutil"))]
pub mod testutil;

pub use document::{Document, Page};
pub use error::{Error, Result};
pub use object::{Dict, Name, ObjRef, Object, PdfString, Stream};
pub use writer::{Builder, Warning};

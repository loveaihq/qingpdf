//! qingpdf-core: the PDF engine behind qingpdf.
//!
//! The reading, text, drawing and writing are in `qingpdf-base` (built for speed) and are re-exported here under their own names;
//! what this crate adds is the operations on pages ([`ops`], `prune`, [`image`]), the forms ([`forms`], [`layout`]: what a form
//! field looks like), the editing ([`edit`]) and the reader's engine ([`view`]), which are built for size.
//!
//! Spec references in comments point to ISO 32000-1:2008 unless noted.

pub mod bookmarks;
pub mod compress;
pub mod dests;
pub mod docx;
pub mod edit;
pub mod export;
pub mod forms;
pub mod image;
pub mod layout;
pub mod ops;
pub mod props;
pub mod prune;
pub mod squeeze;
mod tables;
pub mod view;
pub mod writer;
pub mod xlsx;
mod xml;
mod zip;

pub use qingpdf_base::{document, error, filter, idmap, info, lexer, object, order, parser, render, repair, security, text, xref};
#[cfg(test)]
use qingpdf_base::testutil;

pub use qingpdf_base::{Dict, Document, Error, Name, ObjRef, Object, Page, PdfString, Result, Stream};
pub use writer::{Builder, Warning};

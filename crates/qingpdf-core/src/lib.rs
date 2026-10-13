//! qingpdf-core: the PDF engine behind qingpdf.
//!
//! The reading, text, drawing and writing are in `qingpdf-base` (built for speed) and are re-exported here under their own names;
//! what this crate adds is the editing ([`edit`]) and the reader's engine ([`view`]), which are built for size.
//!
//! Spec references in comments point to ISO 32000-1:2008 unless noted.

pub mod docx;
pub mod edit;
pub mod view;

pub use qingpdf_base::{document, error, filter, image, info, lexer, object, ops, parser, render, repair, security, text, writer, xref};
// (Used inside this crate only.)
use qingpdf_base::{dests, forms, layout};

#[cfg(test)]
use qingpdf_base::testutil;

pub use qingpdf_base::{Builder, Dict, Document, Error, Name, ObjRef, Object, Page, PdfString, Result, Stream, Warning};

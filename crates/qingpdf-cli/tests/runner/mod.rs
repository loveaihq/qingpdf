//! The `qingpdf` program run as a user would, for the tests of this crate: arguments in, an output (the exit code, the text it
//! printed) out. Not every test file uses every function.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

pub fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_qingpdf")).args(args).output().expect("cannot run qingpdf")
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

pub fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

pub fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

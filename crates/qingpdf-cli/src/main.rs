use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("qingpdf {}: no commands yet", env!("CARGO_PKG_VERSION"));
    ExitCode::FAILURE
}

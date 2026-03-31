//! find-extract-iwork — extracts text from iWork files (.pages, .numbers, .key)
//! using Apache Tika.
//!
//! Usage: find-extract-iwork <tika-app.jar> <file>
//!
//! Runs: java -jar <tika-app.jar> --text <file>
//! Writes extracted text to stdout. Exits 0 on success, non-zero on error.
//!
//! Configure in server.toml:
//!   [scan.extractors]
//!   pages   = { mode = "stdout", bin = "/path/to/find-extract-iwork", args = ["/path/to/tika-app.jar", "{file}"] }
//!   numbers = { mode = "stdout", bin = "/path/to/find-extract-iwork", args = ["/path/to/tika-app.jar", "{file}"] }
//!   key     = { mode = "stdout", bin = "/path/to/find-extract-iwork", args = ["/path/to/tika-app.jar", "{file}"] }

use std::process;
use anyhow::{bail, Context, Result};

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        bail!("usage: find-extract-iwork <tika-app.jar> <file>");
    }
    let tika_jar = &args[1];
    let file = &args[2];

    let status = process::Command::new("java")
        .args(["-jar", tika_jar, "--text", file])
        .status()
        .context("failed to launch java — is java installed and in PATH?")?;

    if !status.success() {
        bail!("java -jar tika exited with status {status}");
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("find-extract-iwork: {e:#}");
        process::exit(1);
    }
}

//! `contracts-validate [CONTRACTS_DIR [REPO_ROOT]]`
//!
//! Defaults to the package this tool lives in (`packages/contracts`) and the repository
//! root two levels above it. Exits 1 when any rule is violated.

use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let contracts = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join(".."));
    let repo = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| contracts.join("../.."));
    let report = cfmeet_contracts_validator::validate(&contracts, &repo);
    if report.is_ok() {
        println!(
            "contracts: OK — {} contracts, {} examples, {} counter-examples, {} JSON files",
            report.contracts, report.examples, report.counter_examples, report.json_files
        );
        ExitCode::SUCCESS
    } else {
        for error in &report.errors {
            eprintln!("contracts: ERROR {error}");
        }
        eprintln!("contracts: {} error(s)", report.errors.len());
        ExitCode::FAILURE
    }
}

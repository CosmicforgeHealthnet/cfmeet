//! Contract tool.
//!
//!   admin-bridge-contract generate [DIR]          write docs/contract/ (default DIR)
//!   admin-bridge-contract check [DIR]             exit 1 if DIR differs from the catalog (drift)
//!   admin-bridge-contract compat --base BASE_DIR  exit 1 on a breaking change vs BASE_DIR
//!
//! DIR defaults to `docs/contract` relative to the crate root.

use std::{fs, path::PathBuf, process::ExitCode};

use admin_bridge::contract::{compat::breaking_changes, render::render};

fn default_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/contract")
}

fn list_files(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = format!("{prefix}{}", e.file_name().to_string_lossy());
        if e.path().is_dir() {
            list_files(&e.path(), &format!("{name}/"), out);
        } else {
            out.push(name);
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let files = render();
    match args.first().map(String::as_str) {
        Some("generate") => {
            let dir = args.get(1).map(PathBuf::from).unwrap_or_else(default_dir);
            let mut existing = Vec::new();
            list_files(&dir, "", &mut existing);
            for stale in existing.iter().filter(|p| !files.contains_key(*p)) {
                fs::remove_file(dir.join(stale)).expect("remove stale contract file");
            }
            for (path, bytes) in &files {
                let target = dir.join(path);
                fs::create_dir_all(target.parent().expect("parent")).expect("create dir");
                fs::write(&target, bytes).expect("write contract file");
            }
            println!("wrote {} contract files to {}", files.len(), dir.display());
            ExitCode::SUCCESS
        }
        Some("check") => {
            let dir = args.get(1).map(PathBuf::from).unwrap_or_else(default_dir);
            let mut problems = Vec::new();
            let mut existing = Vec::new();
            list_files(&dir, "", &mut existing);
            for p in existing.iter().filter(|p| !files.contains_key(*p)) {
                problems.push(format!("{p}: not produced by the catalog"));
            }
            for (path, bytes) in &files {
                match fs::read(dir.join(path)) {
                    Ok(disk) if disk == *bytes => {}
                    Ok(_) => problems.push(format!("{path}: differs from the catalog")),
                    Err(_) => problems.push(format!("{path}: missing")),
                }
            }
            if problems.is_empty() {
                println!("contract: {} files, no drift", files.len());
                ExitCode::SUCCESS
            } else {
                for p in &problems {
                    eprintln!("drift: {p}");
                }
                eprintln!(
                    "run `cargo run --bin admin-bridge-contract -- generate` and commit the result"
                );
                ExitCode::FAILURE
            }
        }
        Some("compat") if args.get(1).map(String::as_str) == Some("--base") && args.len() == 3 => {
            let base = PathBuf::from(&args[2]);
            let read = |p: PathBuf| {
                fs::read(p)
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
            };
            let (Some(bc), Some(bs)) = (
                read(base.join("capabilities.v1.json")),
                read(base.join("bridge-schemas.v1.json")),
            ) else {
                println!(
                    "compat: no base contract at {} (first publication)",
                    base.display()
                );
                return ExitCode::SUCCESS;
            };
            let parse = |k: &str| serde_json::from_slice(&files[k]).expect("rendered json");
            let breaks = breaking_changes(
                &bc,
                &bs,
                &parse("capabilities.v1.json"),
                &parse("bridge-schemas.v1.json"),
            );
            if breaks.is_empty() {
                println!("compat: no breaking change");
                ExitCode::SUCCESS
            } else {
                for b in &breaks {
                    eprintln!("breaking: {b}");
                }
                eprintln!("breaking changes need a major CONTRACT_VERSION bump agreed with HQ");
                ExitCode::FAILURE
            }
        }
        _ => {
            eprintln!("usage: admin-bridge-contract generate [DIR] | check [DIR] | compat --base BASE_DIR");
            ExitCode::from(2)
        }
    }
}

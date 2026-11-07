//! SEC-015: every Meeting Service migration binary refuses destructive subcommands
//! (down, fresh, refresh, reset) outside a non-production APP_ENV unless
//! CF_MIGRATION_ALLOW_DESTRUCTIVE names the subcommand, before any database connection.
//!
//! No database is needed: DATABASE_URL names a scheme no driver supports, so a binary that
//! got past the guard fails at connect time with a driver error instead of the refusal.

use std::path::PathBuf;
use std::process::{Command, Output};

const BINARIES: [&str; 5] = [
    env!("CARGO_BIN_EXE_migration"),
    env!("CARGO_BIN_EXE_account-security-migration"),
    env!("CARGO_BIN_EXE_meetings-calendar-migration"),
    env!("CARGO_BIN_EXE_profile-notifications-migration"),
    env!("CARGO_BIN_EXE_account-communications-migration"),
];

const UNSUPPORTED_DATABASE_URL: &str = "sec015-no-such-driver://guard-check/none";
const REFUSAL: &str = "SEC-015: refusing to run destructive migration subcommand";
const REFUSAL_EXIT_CODE: i32 = 2;

/// An empty working directory, so that no `.env` (the binaries read `../.env` and `.env`)
/// can add variables to the deliberately cleared environment.
fn empty_cwd() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("sec015-destructive-guard")
        .join("cwd");
    std::fs::create_dir_all(&dir).expect("create the empty working directory");
    dir
}

fn run(bin: &str, args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(bin);
    command
        .args(args)
        .current_dir(empty_cwd())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("DATABASE_URL", UNSUPPORTED_DATABASE_URL);
    if let Ok(root) = std::env::var("SystemRoot") {
        command.env("SystemRoot", root); // Windows process start-up needs it
    }
    for (k, v) in vars {
        command.env(k, v);
    }
    command.output().expect("spawn the migration binary")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_refused(bin: &str, args: &[&str], vars: &[(&str, &str)]) {
    let out = run(bin, args, vars);
    let err = stderr(&out);
    assert_eq!(
        out.status.code(),
        Some(REFUSAL_EXIT_CODE),
        "{bin} {args:?} {vars:?} must be refused; stderr:\n{err}"
    );
    assert!(err.contains(REFUSAL), "{bin} {args:?} {vars:?}:\n{err}");
    assert!(
        err.contains("no database connection was made"),
        "{bin} {args:?}:\n{err}"
    );
    assert!(
        !err.contains("sec015-no-such-driver"),
        "{bin} {args:?} reached the database layer:\n{err}"
    );
}

/// The binary passed the guard and failed only at connect time on the unsupported URL.
fn assert_passed_guard(bin: &str, args: &[&str], vars: &[(&str, &str)]) -> String {
    let out = run(bin, args, vars);
    let err = stderr(&out);
    assert!(
        !out.status.success(),
        "{bin} {args:?} cannot succeed without a database"
    );
    assert_ne!(
        out.status.code(),
        Some(REFUSAL_EXIT_CODE),
        "{bin} {args:?} {vars:?} was refused:\n{err}"
    );
    assert!(!err.contains(REFUSAL), "{bin} {args:?} {vars:?}:\n{err}");
    assert!(
        err.contains("sec015-no-such-driver"),
        "{bin} {args:?} {vars:?} should have failed only on the unsupported URL:\n{err}"
    );
    err
}

#[test]
fn destructive_subcommands_are_refused_in_production_like_environments() {
    for bin in BINARIES {
        for args in [
            &["reset"][..],
            &["fresh"],
            &["refresh"],
            &["down"],
            &["down", "-n", "1"],
            &["-u", UNSUPPORTED_DATABASE_URL, "reset"],
            &["--database-url", UNSUPPORTED_DATABASE_URL, "-v", "fresh"],
        ] {
            assert_refused(bin, args, &[]);
            assert_refused(bin, args, &[("APP_ENV", "production")]);
            assert_refused(bin, args, &[("APP_ENV", "staging")]);
        }
    }
}

#[test]
fn an_override_permits_only_the_subcommand_it_names() {
    for bin in BINARIES {
        assert_refused(
            bin,
            &["fresh"],
            &[
                ("APP_ENV", "production"),
                ("CF_MIGRATION_ALLOW_DESTRUCTIVE", "down"),
            ],
        );
        assert_refused(
            bin,
            &["reset"],
            &[("CF_MIGRATION_ALLOW_DESTRUCTIVE", "true")],
        );
        let err = assert_passed_guard(
            bin,
            &["down", "-n", "1"],
            &[
                ("APP_ENV", "production"),
                ("CF_MIGRATION_ALLOW_DESTRUCTIVE", "down"),
            ],
        );
        assert!(
            err.contains("CF_MIGRATION_ALLOW_DESTRUCTIVE=down is set"),
            "{bin}: the override use must be logged:\n{err}"
        );
    }
}

#[test]
fn non_production_environments_and_non_destructive_subcommands_pass_the_guard() {
    for bin in BINARIES {
        assert_passed_guard(bin, &["reset"], &[("APP_ENV", "test")]);
        assert_passed_guard(bin, &["fresh"], &[("APP_ENV", " Development ")]);
        assert_passed_guard(bin, &["status"], &[("APP_ENV", "production")]);
        assert_passed_guard(bin, &["status"], &[]);
    }
}

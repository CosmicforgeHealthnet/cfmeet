//! Destructive migration subcommand guard (Blueprint v1.0 hardening, SEC-015).
//!
//! Every service migration binary embeds the full sea-orm migration CLI, and releases run
//! `/app/<service>-migration up` inside production images. `fresh`, `refresh` and `reset`
//! drop or roll back the whole history and most `down()`s drop tables, so a destructive
//! command is one typo away. Each migration binary therefore calls [`enforce_from_env`] as
//! the first statement of `main`: a destructive subcommand (`down`, `fresh`, `refresh`,
//! `reset`) exits with status [`REFUSAL_EXIT_CODE`] before anything runs (no database
//! connection is made) unless
//!
//! * `APP_ENV` names a non-production environment ([`NON_PRODUCTION_APP_ENVS`]: `dev`,
//!   `development`, `local`, `test`, `ci`; trimmed, case-insensitive). An unset or any other
//!   value (including `staging` and `production`) is production-like, or
//! * the explicit override `CF_MIGRATION_ALLOW_DESTRUCTIVE=<subcommand>` names exactly the
//!   subcommand being run (`=down` permits `down` / `down -n N` only, never `fresh`). Its
//!   use is logged on stderr.
//!
//! Only the process environment is consulted: the guard runs before a binary loads any
//! `.env` file, so a stray `.env` can never unlock a destructive command.
//!
//! `up`, `status`, `generate`, `init`, `help`, `--help`, `--version` and no arguments at
//! all (which the CLI treats as `up`) are never affected.

use std::ffi::OsString;
use std::path::Path;

/// The explicit, per-invocation override: its value must equal the subcommand being run.
pub const OVERRIDE_VAR: &str = "CF_MIGRATION_ALLOW_DESTRUCTIVE";

/// The environment name variable shared with the services' own development policies.
pub const APP_ENV_VAR: &str = "APP_ENV";

/// The sea-orm migration CLI subcommands that drop or roll back schema and data.
pub const DESTRUCTIVE_SUBCOMMANDS: [&str; 4] = ["down", "fresh", "refresh", "reset"];

/// `APP_ENV` values that are not production-like. The Meeting Service JWT secret policy
/// (`validated_jwt_secret`) uses the same set plus `ci` here. `staging` is deliberately
/// absent: it holds real-ish data and uses the explicit override.
pub const NON_PRODUCTION_APP_ENVS: [&str; 5] = ["dev", "development", "local", "test", "ci"];

/// Exit status of a refused invocation (distinct from the CLI's own failure status 1).
pub const REFUSAL_EXIT_CODE: i32 = 2;

/// Global CLI options that take a value (`-u URL`, `--database-url URL`, `-s SCHEMA`, ...).
const VALUE_LONG_OPTIONS: [&str; 2] = ["database-url", "database-schema"];
const VALUE_SHORT_OPTIONS: [char; 2] = ['u', 's'];

/// What the guard decided for one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Not a destructive subcommand (`up`, `status`, `generate`, help, no subcommand...).
    NotDestructive,
    /// Destructive, permitted because `APP_ENV` names a non-production environment.
    AllowedNonProduction {
        subcommand: &'static str,
        app_env: String,
    },
    /// Destructive, permitted by the matching `CF_MIGRATION_ALLOW_DESTRUCTIVE` override.
    AllowedByOverride { subcommand: &'static str },
    /// Destructive and refused; `reason` says why in one sentence fragment.
    Refused {
        subcommand: &'static str,
        reason: String,
    },
}

/// The subcommand of a migration CLI invocation: the first argument that is neither an
/// option nor the value of a value-taking global option (`-u`, `-s` and their long forms,
/// separate or attached as `-uURL`, `-u=URL`, `--database-url=URL`). A token after `--` is
/// taken as the subcommand too, so that form cannot slip past the guard.
pub fn subcommand<S: AsRef<str>>(args: &[S]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_ref();
        if arg == "--" {
            return args.get(i + 1).map(AsRef::as_ref);
        }
        if let Some(long) = arg.strip_prefix("--") {
            if !long.contains('=') && VALUE_LONG_OPTIONS.contains(&long) {
                i += 1; // the option's value is the next argument
            }
        } else if let Some(shorts) = arg.strip_prefix('-').filter(|s| !s.is_empty()) {
            // A cluster such as `-v`, `-vu URL` or `-uURL`: the first value-taking option
            // consumes the rest of the cluster, or the next argument when nothing follows.
            if let Some((at, c)) = shorts
                .char_indices()
                .find(|(_, c)| VALUE_SHORT_OPTIONS.contains(c))
            {
                if at + c.len_utf8() == shorts.len() {
                    i += 1;
                }
            }
        } else {
            return Some(arg);
        }
        i += 1;
    }
    None
}

/// The destructive subcommand named by `args` (program name excluded), if any. Matching is
/// case-insensitive so that no spelling variant is ever waved through.
pub fn destructive_subcommand<S: AsRef<str>>(args: &[S]) -> Option<&'static str> {
    let sub = subcommand(args)?;
    DESTRUCTIVE_SUBCOMMANDS
        .into_iter()
        .find(|d| d.eq_ignore_ascii_case(sub))
}

/// Whether `app_env` names a non-production environment (unset is production-like).
pub fn is_non_production(app_env: Option<&str>) -> bool {
    app_env.is_some_and(|e| {
        let e = e.trim();
        NON_PRODUCTION_APP_ENVS
            .iter()
            .any(|n| n.eq_ignore_ascii_case(e))
    })
}

/// The pure decision: `args` without the program name, the `APP_ENV` value and the
/// `CF_MIGRATION_ALLOW_DESTRUCTIVE` value (`None` when unset).
pub fn decide<S: AsRef<str>>(
    args: &[S],
    app_env: Option<&str>,
    override_value: Option<&str>,
) -> Decision {
    let Some(subcommand) = destructive_subcommand(args) else {
        return Decision::NotDestructive;
    };
    if is_non_production(app_env) {
        return Decision::AllowedNonProduction {
            subcommand,
            app_env: app_env.unwrap_or_default().trim().to_string(),
        };
    }
    match override_value.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case(subcommand) => Decision::AllowedByOverride { subcommand },
        Some(v) => Decision::Refused {
            subcommand,
            reason: format!(
                "{OVERRIDE_VAR}={v:?} does not permit `{subcommand}` (the override must name \
                 exactly the subcommand being run)"
            ),
        },
        None => Decision::Refused {
            subcommand,
            reason: match app_env.map(str::trim) {
                None | Some("") => format!("{APP_ENV_VAR} is unset, which is production-like"),
                Some(e) => format!("{APP_ENV_VAR}={e:?} is not a non-production environment"),
            },
        },
    }
}

/// The stderr text of a refusal.
pub fn refusal_message(program: &str, subcommand: &str, reason: &str) -> String {
    format!(
        "SEC-015: refusing to run destructive migration subcommand `{subcommand}`: {reason}.\n\
         Nothing was executed and no database connection was made.\n\
         Destructive subcommands ({all}) run only when {APP_ENV_VAR} is one of {envs}, or \
         deliberately (after a verified backup) with the override naming this subcommand \
         for this invocation only:\n  \
         {OVERRIDE_VAR}={subcommand} {program} {subcommand} ...",
        all = DESTRUCTIVE_SUBCOMMANDS.join(", "),
        envs = NON_PRODUCTION_APP_ENVS.join(", "),
    )
}

/// The stderr notice printed when the override permits a destructive subcommand.
pub fn override_notice(program: &str, subcommand: &str) -> String {
    format!(
        "SEC-015: {OVERRIDE_VAR}={subcommand} is set: running destructive migration \
         subcommand `{subcommand}` of {program} outside a non-production {APP_ENV_VAR}."
    )
}

/// Applies the guard to this process: reads the arguments, `APP_ENV` and
/// `CF_MIGRATION_ALLOW_DESTRUCTIVE`, and exits with [`REFUSAL_EXIT_CODE`] when a
/// destructive subcommand is refused. Call it as the first statement of a migration
/// binary's `main`, before any `.env` file is loaded.
pub fn enforce_from_env() {
    let mut argv = std::env::args_os();
    let program = program_name(argv.next());
    let args: Vec<String> = argv.map(|a| a.to_string_lossy().into_owned()).collect();
    // A non-UTF-8 value reads as unset: production-like, and no override.
    let app_env = std::env::var(APP_ENV_VAR).ok();
    let override_value = std::env::var(OVERRIDE_VAR).ok();
    match decide(&args, app_env.as_deref(), override_value.as_deref()) {
        Decision::NotDestructive | Decision::AllowedNonProduction { .. } => {}
        Decision::AllowedByOverride { subcommand } => {
            eprintln!("{}", override_notice(&program, subcommand));
        }
        Decision::Refused { subcommand, reason } => {
            eprintln!("{}", refusal_message(&program, subcommand, &reason));
            std::process::exit(REFUSAL_EXIT_CODE);
        }
    }
}

fn program_name(argv0: Option<OsString>) -> String {
    argv0
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<migration>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(args: &[&str], app_env: Option<&str>, ov: Option<&str>) -> Decision {
        decide(args, app_env, ov)
    }

    fn refused(decision: &Decision) -> bool {
        matches!(decision, Decision::Refused { .. })
    }

    #[test]
    fn every_destructive_subcommand_is_refused_in_production() {
        for sub in DESTRUCTIVE_SUBCOMMANDS {
            for env in [
                None,
                Some(""),
                Some("production"),
                Some("prod"),
                Some("staging"),
            ] {
                let got = d(&[sub], env, None);
                assert!(refused(&got), "{sub} with APP_ENV={env:?}: {got:?}");
                let Decision::Refused { subcommand, .. } = got else {
                    unreachable!()
                };
                assert_eq!(subcommand, sub);
            }
        }
    }

    #[test]
    fn down_with_a_count_is_refused_and_named_down() {
        for args in [
            &["down", "-n", "3"][..],
            &["down", "--num", "3"],
            &["down", "-n3"],
        ] {
            assert_eq!(
                d(args, Some("production"), None),
                Decision::Refused {
                    subcommand: "down",
                    reason: "APP_ENV=\"production\" is not a non-production environment".into()
                }
            );
        }
    }

    #[test]
    fn unset_app_env_reason_says_so() {
        assert_eq!(
            d(&["reset"], None, None),
            Decision::Refused {
                subcommand: "reset",
                reason: "APP_ENV is unset, which is production-like".into()
            }
        );
        assert_eq!(
            d(&["reset"], Some("   "), None),
            Decision::Refused {
                subcommand: "reset",
                reason: "APP_ENV is unset, which is production-like".into()
            }
        );
    }

    #[test]
    fn non_destructive_invocations_always_pass() {
        for args in [
            &[][..],
            &["up"],
            &["up", "-n", "1"],
            &["status"],
            &["generate", "add_table"],
            &["init"],
            &["help"],
            &["help", "reset"],
            &["--help"],
            &["-h"],
            &["--version"],
            &["-V"],
            &["-v"],
            &["-u", "postgres://h/db"],
            &["-u", "reset"],
            &["--database-url", "down", "up"],
            &["-s", "fresh", "status"],
            &["--database-schema=fresh", "status"],
            &["-ureset", "up"],
            &["-vs", "refresh", "status"],
        ] {
            for env in [None, Some("production"), Some("staging"), Some("test")] {
                assert_eq!(
                    d(args, env, None),
                    Decision::NotDestructive,
                    "{args:?} with APP_ENV={env:?}"
                );
            }
        }
    }

    #[test]
    fn subcommand_is_found_after_global_flags() {
        let cases: [(&[&str], &str); 11] = [
            (&["-u", "postgres://h/db", "reset"], "reset"),
            (&["--database-url", "postgres://h/db", "fresh"], "fresh"),
            (&["--database-url=postgres://h/db", "refresh"], "refresh"),
            (&["-upostgres://h/db", "down"], "down"),
            (&["-u=postgres://h/db", "down"], "down"),
            (&["-s", "public", "reset"], "reset"),
            (&["--database-schema", "public", "-v", "reset"], "reset"),
            (&["-v", "reset"], "reset"),
            (
                &["--verbose", "-u", "x", "-s", "y", "down", "-n", "2"],
                "down",
            ),
            (&["-vu", "postgres://h/db", "fresh"], "fresh"),
            (&["--", "reset"], "reset"),
        ];
        for (args, want) in cases {
            assert_eq!(subcommand(args), Some(want), "{args:?}");
            assert!(refused(&d(args, Some("production"), None)), "{args:?}");
        }
    }

    #[test]
    fn subcommand_matching_ignores_case() {
        assert!(refused(&d(&["RESET"], None, None)));
        assert!(refused(&d(&["Down"], None, None)));
    }

    #[test]
    fn non_production_app_env_allows_every_destructive_subcommand() {
        for env in [
            "dev",
            "development",
            "local",
            "test",
            "ci",
            " TEST ",
            "Development",
            "CI\n",
        ] {
            for sub in DESTRUCTIVE_SUBCOMMANDS {
                assert_eq!(
                    d(&[sub], Some(env), None),
                    Decision::AllowedNonProduction {
                        subcommand: sub,
                        app_env: env.trim().to_string()
                    }
                );
            }
        }
    }

    #[test]
    fn production_like_app_env_values_are_not_non_production() {
        for env in [
            "production",
            "prod",
            "staging",
            "stage",
            "preview",
            "testing",
            "devel",
            "d ev",
            "",
        ] {
            assert!(!is_non_production(Some(env)), "{env:?}");
        }
        assert!(!is_non_production(None));
    }

    #[test]
    fn matching_override_allows_only_its_subcommand() {
        for sub in DESTRUCTIVE_SUBCOMMANDS {
            assert_eq!(
                d(&[sub], Some("production"), Some(sub)),
                Decision::AllowedByOverride { subcommand: sub }
            );
            assert_eq!(
                d(&[sub], None, Some(&format!("  {}\n", sub.to_uppercase()))),
                Decision::AllowedByOverride { subcommand: sub }
            );
            for other in DESTRUCTIVE_SUBCOMMANDS.into_iter().filter(|o| *o != sub) {
                let got = d(&[sub], Some("staging"), Some(other));
                assert!(refused(&got), "{other} must not permit {sub}: {got:?}");
            }
        }
        assert_eq!(
            d(
                &["-u", "postgres://h/db", "down", "-n", "1"],
                None,
                Some("down")
            ),
            Decision::AllowedByOverride { subcommand: "down" }
        );
    }

    #[test]
    fn override_mismatch_reason_names_both() {
        let Decision::Refused { subcommand, reason } = d(&["fresh"], None, Some("down")) else {
            panic!("expected a refusal")
        };
        assert_eq!(subcommand, "fresh");
        assert!(
            reason.contains("CF_MIGRATION_ALLOW_DESTRUCTIVE=\"down\""),
            "{reason}"
        );
        assert!(reason.contains("`fresh`"), "{reason}");
    }

    #[test]
    fn empty_or_unrelated_override_values_do_not_unlock() {
        for ov in [
            "",
            " ",
            "1",
            "true",
            "yes",
            "all",
            "*",
            "down,reset",
            "down reset",
        ] {
            for sub in DESTRUCTIVE_SUBCOMMANDS {
                assert!(refused(&d(&[sub], None, Some(ov))), "{ov:?} / {sub}");
            }
        }
    }

    #[test]
    fn override_is_irrelevant_to_non_destructive_commands() {
        assert_eq!(d(&["up"], None, Some("reset")), Decision::NotDestructive);
    }

    #[test]
    fn messages_name_the_subcommand_override_and_program() {
        let m = refusal_message(
            "organizations-migration",
            "down",
            "APP_ENV is unset, which is production-like",
        );
        assert!(m.starts_with("SEC-015: refusing to run destructive migration subcommand `down`"));
        assert!(m.contains("no database connection was made"));
        assert!(m.contains("CF_MIGRATION_ALLOW_DESTRUCTIVE=down organizations-migration down"));
        let n = override_notice("organizations-migration", "down");
        assert!(n.contains("CF_MIGRATION_ALLOW_DESTRUCTIVE=down is set"));
    }

    #[test]
    fn program_name_is_the_file_stem() {
        assert_eq!(
            program_name(Some("/app/calling-migration".into())),
            "calling-migration"
        );
        assert_eq!(
            program_name(Some(r"C:\t\debug\migration.exe".into())),
            if cfg!(windows) {
                "migration"
            } else {
                r"C:\t\debug\migration"
            }
        );
        assert_eq!(program_name(None), "<migration>");
    }
}

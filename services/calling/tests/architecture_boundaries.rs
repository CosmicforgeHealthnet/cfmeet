//! M / §16. Architecture boundaries (static checks over this service's sources):
//!
//! * Calling is independent of `services/meeting-service`: no path dependency, no source
//!   import, no Meeting Service crate names, no Meeting Service tables.
//! * The platform core contains no consumer-specific domain types or product-name
//!   branching.
//! * No Part C (calls / call legs / routes …) schema exists yet.
//! * Raw credential exposure is confined to the single CLI display site.
//!
//! Comments are stripped before matching so documentation can still discuss these
//! topics; string literals are kept, so `if app == "<product>"` is caught.

use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Source without `//` comments (doc text may legitimately name products or services).
fn code_of(path: &Path) -> String {
    strip_comments(&std::fs::read_to_string(path).unwrap())
}

fn strip_comments(source: &str) -> String {
    source
        .lines()
        .map(|l| match l.find("//") {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Production code only: comments stripped and every `#[cfg(test)]` item (unit-test
/// modules) removed — only the brace-balanced item itself, so code after it is still
/// scanned.
fn production_code_of(path: &Path) -> String {
    let mut code = strip_comments(&std::fs::read_to_string(path).unwrap());
    while let Some(start) = code.find("#[cfg(test)]") {
        let Some(open) = code[start..].find('{').map(|i| start + i) else {
            break;
        };
        let mut depth = 0usize;
        let mut end = code.len();
        for (i, ch) in code[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        code.replace_range(start..end, "");
    }
    code
}

fn production_sources() -> Vec<(String, String)> {
    let root = manifest_dir();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("migration/src"), &mut files);
    assert!(files.len() > 20, "source tree not found");
    files
        .into_iter()
        .map(|f| {
            (
                f.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                production_code_of(&f),
            )
        })
        .collect()
}

/// Whether `code` contains `path` as a path root (not as the suffix of an identifier).
fn uses_crate_path(code: &str, path: &str) -> bool {
    code.match_indices(path).any(|(i, _)| {
        code[..i]
            .chars()
            .last()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
    })
}

#[test]
fn calling_does_not_depend_on_meeting_service() {
    let root = manifest_dir();
    for manifest in ["Cargo.toml", "migration/Cargo.toml"] {
        let text = std::fs::read_to_string(root.join(manifest)).unwrap();
        for forbidden in ["meeting-service", "meeting_service", "../meeting"] {
            assert!(
                !text.contains(forbidden),
                "{manifest} references `{forbidden}`"
            );
        }
        // No dependency on the Meeting Service crates (`backend`, `migration`).
        for line in text.lines().map(str::trim) {
            let name = line.split('=').next().unwrap_or("").trim();
            assert!(
                name != "backend" && name != "migration",
                "{manifest} depends on Meeting Service crate `{name}`"
            );
        }
    }
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
    assert!(
        !lock.contains("name = \"backend\""),
        "Cargo.lock pulls in the Meeting Service crate"
    );

    for (file, code) in production_sources() {
        for forbidden in ["meeting_service", "meeting-service", "ActorContext"] {
            assert!(!code.contains(forbidden), "{file} references `{forbidden}`");
        }
    }

    // Neither production code nor tests import Meeting Service crates (`backend::…`,
    // `migration::…`); this service's own `calling_migration::…` is fine.
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("migration/src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    // (This guard necessarily names the forbidden paths; it is not scanned.)
    files.retain(|f| !f.ends_with("tests/architecture_boundaries.rs"));
    for file in files {
        let code = code_of(&file);
        for crate_path in ["backend::", "migration::"] {
            assert!(
                !uses_crate_path(&code, crate_path),
                "{} uses Meeting Service crate path `{crate_path}`",
                file.display()
            );
        }
        assert!(!code.contains("extern crate backend"));
    }
}

#[test]
fn calling_never_touches_meeting_service_tables() {
    for (file, code) in production_sources() {
        let lower = code.to_lowercase();
        for forbidden in [
            "from users",
            "join users",
            "references users",
            "into users",
            "update users",
            "organization_memberships",
            "from organizations",
            "join organizations",
            "meeting_spaces",
            "conference_participants",
            "conference_sessions",
            "from meetings",
            "join meetings",
            "api_keys",
        ] {
            assert!(
                !lower.contains(forbidden),
                "{file} touches Meeting Service table (`{forbidden}`)"
            );
        }
        // Every table this service names is one of its own.
        for keyword in [
            "create table ",
            "alter table ",
            "insert into ",
            "delete from ",
            "drop table if exists ",
        ] {
            for (i, _) in lower.match_indices(keyword) {
                let rest = &lower[i + keyword.len()..];
                let rest = rest.trim_start_matches("unlogged table ").trim_start();
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                assert!(
                    name.starts_with("platform_") || name.is_empty(),
                    "{file}: `{keyword}{name}` is not a Calling platform table"
                );
            }
        }
    }
}

#[test]
fn platform_core_has_no_consumer_specific_code() {
    for (file, code) in production_sources() {
        let lower = code.to_lowercase();
        for product in ["karryam", "healthnet", "health_net", "health-net"] {
            assert!(
                !lower.contains(product),
                "{file} contains consumer-specific `{product}`"
            );
        }
        for domain_type in ["Rider", "Driver", "Doctor", "Patient", "Clinician"] {
            assert!(
                !code.contains(domain_type),
                "{file} contains consumer-specific domain type `{domain_type}`"
            );
        }
    }
}

#[test]
fn no_part_c_schema_or_types_exist_yet() {
    for (file, code) in production_sources() {
        let lower = code.to_lowercase();
        for forbidden in [
            "create table calls",
            "call_participants",
            "call_sessions",
            "call_legs",
            "call_routes",
            "callparticipant",
            "callsession",
            "callleg",
            "callroute",
            "livekit",
            "pstn",
            "voicemail",
        ] {
            assert!(
                !lower.contains(forbidden),
                "{file} contains out-of-scope `{forbidden}`"
            );
        }
    }
}

#[test]
fn raw_credentials_are_exposed_only_at_the_issuance_display_site() {
    for (file, code) in production_sources() {
        if code.contains("expose_once()") {
            assert_eq!(
                file, "src/bin/platform_admin.rs",
                "{file} exposes a raw credential"
            );
        }
        // Secret containers never derive Serialize / Display.
        if file == "src/domain/secret.rs" {
            assert!(!code.contains("Serialize"));
            assert!(!code.contains("impl fmt::Display"));
            assert!(!code.contains("derive(Debug"));
        }
    }
    let admin = production_code_of(&manifest_dir().join("src/bin/platform_admin.rs"));
    assert_eq!(admin.matches("expose_once()").count(), 1);
}

#[test]
fn identity_types_stay_separate() {
    for (file, code) in production_sources() {
        for forbidden in [
            "impl From<PlatformAuthContext> for ResolvedPrincipal",
            "impl From<ResolvedPrincipal> for PlatformAuthContext",
            "impl From<&PlatformAuthContext> for ResolvedPrincipal",
        ] {
            assert!(!code.contains(forbidden), "{file}: `{forbidden}`");
        }
    }
    let context = code_of(&manifest_dir().join("src/platform/context.rs"));
    assert!(
        !context.contains("Deserialize"),
        "PlatformAuthContext must not be deserializable"
    );
    assert!(
        !context.contains("pub fn new"),
        "PlatformAuthContext must not be publicly constructible"
    );
}

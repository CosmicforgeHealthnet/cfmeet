//! Telephony architecture boundaries (Part E §2, §7–§8, §191): no other service's source,
//! database or tables; provider / SIP-stack specifics confined to their directories;
//! raw numbers only inside the execution boundary and never logged.

use std::{fs, path::Path};

fn sources() -> Vec<(String, String)> {
    let mut out = Vec::new();
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                // Production code only (negative test lists may name vendors).
                let text = fs::read_to_string(&path).unwrap();
                let production = text.split("#[cfg(test)]").next().unwrap_or("").to_string();
                out.push((rel, production));
            }
        }
    }
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    walk(&base.join("src"), base, &mut out);
    walk(&base.join("migration/src"), base, &mut out);
    out
}

fn contains_word(haystack: &str, word: &str) -> bool {
    haystack.match_indices(word).any(|(i, _)| {
        let before = haystack[..i].chars().last();
        let after = haystack[i + word.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        boundary(before) && boundary(after)
    })
}

#[test]
fn no_other_service_source_or_database() {
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    for other in ["calling", "privacy-relay", "meeting-service"] {
        for prefix in ["services/", "../"] {
            let dep = format!("{prefix}{other}");
            assert!(!manifest.contains(&dep), "{dep}");
        }
    }
    for (file, code) in sources() {
        let lower = code.to_lowercase();
        for import in [
            "calling::",
            "privacy_relay::",
            "meeting_service::",
            "backend::",
        ] {
            assert!(!code.contains(&format!("use {import}")), "{file}: {import}");
        }
        for owner in ["CALLING", "PRIVACY_RELAY", "MEETING"] {
            let var = format!("{owner}_{}", "DATABASE_URL");
            assert!(!code.contains(&var), "{file}: {var}");
        }
        assert!(
            !code.contains("CONTACT_VAULT_"),
            "{file}: Contact Vault configuration"
        );
        for table in [
            "calls",
            "call_participants",
            "call_sessions",
            "call_legs",
            "call_routes",
            "call_attempts",
            "communication_contexts",
            "privacy_relay_sessions",
            "protected_contacts",
            "contact_fingerprints",
            "relay_number_inventory",
            "relay_number_assignments",
            "relay_inbound_resolutions",
        ] {
            for verb in ["from ", "join ", "into ", "update ", "table "] {
                assert!(
                    !lower.contains(&format!("{verb}{table} "))
                        && !lower.contains(&format!("{verb}{table}\n")),
                    "{file}: SQL on another service's table {table}"
                );
            }
        }
    }
}

#[test]
fn provider_and_sip_stack_specifics_stay_in_their_directories() {
    for (file, code) in sources() {
        let lower = code.to_lowercase();
        let in_sip = file.starts_with("src/sip/");
        let in_providers = file.starts_with("src/providers/");
        if !in_sip {
            for term in ["asterisk", "/ari/", "stasis", "pjsip", "freeswitch", "esl"] {
                assert!(
                    !contains_word(&lower, term) || file.starts_with("src/main.rs"),
                    "{file}: SIP-stack term `{term}` outside src/sip/"
                );
            }
        }
        if !in_providers && !file.starts_with("src/bin/") && file != "src/main.rs" {
            for term in [
                "africastalking",
                "africas_talking",
                "twilio",
                "vonage",
                "deterministicprovider",
            ] {
                assert!(
                    !lower.contains(term),
                    "{file}: provider-specific `{term}` outside src/providers/"
                );
            }
        }
    }
}

#[test]
fn raw_numbers_exist_only_inside_the_execution_boundary_and_are_never_logged() {
    const MAY_EXPOSE: &[&str] = &[
        "src/providers/mod.rs",
        "src/providers/deterministic.rs",
        "src/providers/sip_trunk.rs",
        "src/sip/ari.rs",
        "src/ports.rs",
        "src/commands/executor.rs",
        "src/inbound.rs",
        "src/bin/telephony-admin.rs",
    ];
    for (file, code) in sources() {
        if code.contains(".expose()") {
            assert!(
                MAY_EXPOSE.contains(&file.as_str()),
                "{file} exposes a raw number"
            );
        }
        // No tracing macro ever carries a number.
        for (i, _) in code.match_indices("tracing::") {
            let end = code[i..].find(");").map(|e| i + e).unwrap_or(code.len());
            let call = &code[i..end];
            assert!(
                !call.contains("expose")
                    && !call.contains("e164")
                    && !call.contains("grant_secret"),
                "{file}: a log statement carries sensitive data: {call}"
            );
        }
    }
}

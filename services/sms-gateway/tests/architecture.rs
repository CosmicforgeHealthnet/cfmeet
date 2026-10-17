//! SMS Gateway architecture boundaries (Part IV M): no other service's source, database
//! or tables (Numbering is reached only over authenticated internal HTTP); no Numbering
//! authority replicated (assignments, KYC, provider-number lifecycle); provider specifics
//! confined to `src/providers/`; numbers and content never logged.

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
    for other in [
        "calling",
        "numbering",
        "telephony",
        "privacy-relay",
        "platform-access",
        "meeting-service",
    ] {
        for prefix in ["services/", "../"] {
            let dep = format!("{prefix}{other}");
            assert!(!manifest.contains(&dep), "{dep}");
        }
    }
    for (file, code) in sources() {
        let lower = code.to_lowercase();
        for import in [
            "calling::",
            "numbering::",
            "telephony::",
            "privacy_relay::",
            "platform_access::",
        ] {
            assert!(
                !code.contains(&format!("use {import}")) || import == "numbering::",
                "{file}: {import}"
            );
        }
        assert!(
            !code.contains("use numbering::") && !code.contains("numbering_migration"),
            "{file}: Numbering's crate"
        );
        for owner in [
            "CALLING",
            "NUMBERING",
            "TELEPHONY",
            "PRIVACY_RELAY",
            "PLATFORM_ACCESS",
        ] {
            let var = format!("{owner}_{}", "DATABASE_URL");
            assert!(!code.contains(&var), "{file}: {var}");
        }
        // Another service's tables never appear in SQL; every table here is `sms_`.
        for table in [
            "number_resources",
            "number_assignments",
            "number_routing",
            "number_grants",
            "number_inventory",
            "kyc_",
            "calls",
            "call_legs",
            "telephony_",
            "webhook_",
            "platform_",
        ] {
            for verb in ["from ", "join ", "into ", "update ", "table "] {
                assert!(
                    !lower.contains(&format!("{verb}{table}")),
                    "{file}: SQL on another service's table {table}"
                );
            }
        }
        if file.starts_with("migration/src/m") {
            for (i, _) in lower.match_indices("create table ") {
                let name = &lower[i + 13..];
                assert!(
                    name.starts_with("sms_"),
                    "{file}: table outside the sms_ namespace"
                );
            }
        }
    }
}

#[test]
fn numbering_authority_is_not_replicated() {
    // Ownership, assignment, KYC and lifecycle stay in Numbering: the gateway keeps the
    // facts Numbering answered for ONE message, never a copy of its records.
    for (file, code) in sources() {
        let lower = code.to_lowercase();
        for term in [
            "create table sms_number",
            "create table sms_assignment",
            "create table sms_kyc",
            "create table sms_sender_ids",
            "kyc_status",
            "verification_case",
            "provider_number_ref",
        ] {
            assert!(
                !lower.contains(term),
                "{file}: replicates Numbering authority ({term})"
            );
        }
    }
}

#[test]
fn provider_specifics_stay_in_src_providers() {
    for (file, code) in sources() {
        if file.starts_with("src/providers/") || file == "src/main.rs" {
            continue;
        }
        let lower = code.to_lowercase();
        for term in [
            "twilio",
            "vonage",
            "africastalking",
            "infobip",
            "sinch",
            "messagebird",
        ] {
            assert!(!lower.contains(term), "{file}: provider-specific `{term}`");
        }
        assert!(
            !code.contains("DeterministicSmsProvider"),
            "{file}: the test adapter outside src/providers/"
        );
    }
}

#[test]
fn numbers_and_content_are_never_logged() {
    for (file, code) in sources() {
        for (i, _) in code.match_indices("tracing::") {
            // Macro invocations only (`tracing::info!(…)`, `tracing::warn_span!(…)`).
            let name: String = code[i + 9..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !code[i + 9 + name.len()..].starts_with('!') {
                continue;
            }
            let end = code[i..].find(");").map(|e| i + e).unwrap_or(code.len());
            // Constant message literals are not data: check the fields / arguments only.
            let mut call = String::new();
            let mut in_literal = false;
            for c in code[i..end].to_lowercase().chars() {
                if c == '"' {
                    in_literal = !in_literal;
                } else if !in_literal {
                    call.push(c);
                }
            }
            let call = &call;
            for term in [
                "e164", "content", "text", "body", "from", "to =", "remote", "sender", "secret",
                "grant", "token", "open(",
            ] {
                let present = if term.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    contains_word(call, term)
                } else {
                    call.contains(term)
                };
                assert!(
                    !present,
                    "{file}: a log statement may carry sensitive data ({term}): {call}"
                );
            }
        }
        for macro_ in ["println!", "eprintln!", "dbg!"] {
            if file != "src/main.rs" && file != "migration/src/main.rs" {
                assert!(!code.contains(macro_), "{file}: {macro_}");
            }
        }
    }
}

#[test]
fn plaintext_sealed_values_are_opened_only_where_they_are_needed() {
    // Content and numbers are opened by the transport domain only (the public view for
    // the owner, the dispatcher for the provider, the callback path for resolution).
    for (file, code) in sources() {
        if code.contains("protection.open(") || code.contains(".open(Purpose") {
            assert!(
                file.starts_with("src/sms/") || file.starts_with("src/http/"),
                "{file}: opens sealed values"
            );
        }
    }
}

//! Developer Platform architecture boundaries (Part VI §§1–6): no other service's source
//! or database; Platform Access stays the ONLY identity / credential authority (no
//! credential verification, no identity hierarchy tables); webhook signing and sealing only
//! through the shared `cf_webhooks` primitives (no cryptography of its own); no
//! communications, E2EE or product-specific domain; no Part VII expansion.

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

const OTHER_SERVICES: &[&str] = &[
    "calling",
    "numbering",
    "telephony",
    "privacy-relay",
    "platform-access",
    "meeting-service",
    "messaging",
    "sms-gateway",
    "api-gateway",
];

#[test]
fn no_other_service_source_or_database() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    for manifest in ["Cargo.toml", "migration/Cargo.toml"] {
        let text = fs::read_to_string(base.join(manifest)).unwrap();
        for other in OTHER_SERVICES {
            for prefix in ["services/", "../"] {
                let dep = format!("{prefix}{other}");
                assert!(!text.contains(&format!("\"{dep}")), "{manifest}: {dep}");
            }
        }
    }
    for (file, code) in sources() {
        for other in OTHER_SERVICES {
            let krate = other.replace('-', "_");
            assert!(!code.contains(&format!("use {krate}::")), "{file}: {krate}");
            assert!(!code.contains(&format!("{krate}_migration")), "{file}");
            let var = format!("{}_DATABASE_URL", other.replace('-', "_").to_uppercase());
            assert!(!code.contains(&var), "{file}: {var}");
        }
    }
}

#[test]
fn platform_access_remains_the_only_identity_and_credential_authority() {
    for (file, code) in sources() {
        let lower = code.to_lowercase();
        // No credential verification machinery, no customer bearer authentication.
        for term in [
            "verifierkeyring",
            "credentialpepper",
            "verifier_message",
            "api_credential_pepper",
            "header::authorization",
            "argon2",
            "bcrypt",
        ] {
            assert!(
                !lower.contains(term),
                "{file}: credential authority ({term})"
            );
        }
        // No identity hierarchy of its own.
        if file.starts_with("migration/src/m") {
            for (i, _) in lower.match_indices("create table ") {
                let name = &lower[i + 13..];
                assert!(
                    name.starts_with("devplat_"),
                    "{file}: table outside devplat_"
                );
                for entity in [
                    "devplat_tenants",
                    "devplat_applications",
                    "devplat_environments ",
                    "devplat_service_accounts",
                    "devplat_credentials",
                    "devplat_api_keys",
                    "devplat_scopes",
                    "devplat_principals",
                ] {
                    assert!(!name.starts_with(entity), "{file}: duplicates {entity}");
                }
            }
        }
    }
}

#[test]
fn no_communications_e2ee_or_product_domain() {
    for (file, code) in sources() {
        let lower = code.to_lowercase();
        for term in [
            "hmac",
            "openmls",
            "decrypt",
            "plaintext",
            "messaging_crypto",
            "karryam",
            "healthnet",
            "payment",
            "carrier",
            "invoice",
        ] {
            assert!(
                !lower.contains(term),
                "{file}: `{term}` is outside the Developer Platform"
            );
        }
    }
}

/// Whether `term` occurs at a word start (`ring::` but not `String::`).
fn has_word(code: &str, term: &str) -> bool {
    code.match_indices(term).any(|(i, _)| {
        !code[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Section 53: webhook signing and the sealing of signing secrets come ONLY from the
/// shared `cf_webhooks` primitives (one signature scheme platform-wide); the Developer
/// Platform links no cryptographic primitive crate for them and implements none.
#[test]
fn webhook_cryptography_comes_only_from_cf_webhooks() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(base.join("Cargo.toml")).unwrap();
    let deps: String = manifest
        .split("[dev-dependencies]")
        .next()
        .unwrap()
        .to_string();
    assert!(deps.contains("cf-webhooks = { path = \"../../packages/webhooks\" }"));
    for krate in [
        "hmac",
        "aes-gcm",
        "aes",
        "ring",
        "chacha20poly1305",
        "openssl",
    ] {
        assert!(
            !deps
                .lines()
                .any(|l| l.trim_start().starts_with(&format!("{krate} "))
                    || l.trim_start().starts_with(&format!("{krate}="))),
            "Cargo.toml depends on the crypto primitive `{krate}`"
        );
    }
    let mut signs = false;
    let mut seals = false;
    for (file, code) in sources() {
        let lower = code.to_lowercase();
        for term in ["ring::", "aes_gcm", "aes::", "hmac", "sha2::"] {
            if file.starts_with("src/webhooks/") || file.starts_with("src/config/webhooks") {
                assert!(
                    !has_word(&lower, term),
                    "{file}: webhook crypto `{term}` outside cf_webhooks"
                );
            } else if term != "sha2::" {
                // sha2 digests (idempotency keys, invitation tokens) are not webhook crypto.
                assert!(!has_word(&lower, term), "{file}: `{term}`");
            }
        }
        if code.contains("cf_webhooks::signing::sign(") {
            signs = true;
            assert_eq!(
                file, "src/webhooks/worker.rs",
                "signing happens in the worker only"
            );
        }
        if code.contains(".seal(&secret") {
            seals = true;
            assert_eq!(file, "src/webhooks/mod.rs");
        }
        if file.starts_with("src/") && code.contains("cf_webhooks::") {
            assert!(
                file.starts_with("src/webhooks/") || file == "src/config/webhooks.rs",
                "{file}: cf_webhooks outside the webhook module"
            );
        }
    }
    assert!(
        signs && seals,
        "the webhook module signs and seals through cf_webhooks"
    );
}

#[test]
fn credentials_and_tokens_are_never_logged() {
    for (file, code) in sources() {
        for (i, _) in code.match_indices("tracing::") {
            let rest = &code[i + 9..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !rest[name.len()..].starts_with('!') {
                continue;
            }
            let end = code[i..].find(");").map(|e| i + e).unwrap_or(code.len());
            let mut call = String::new();
            let mut in_literal = false;
            for c in code[i..end].to_lowercase().chars() {
                if c == '"' {
                    in_literal = !in_literal;
                } else if !in_literal {
                    call.push(c);
                }
            }
            for term in [
                "token",
                "credential",
                "secret",
                "authorization",
                "delegated",
            ] {
                assert!(!call.contains(term), "{file}: logs `{term}`: {call}");
            }
        }
    }
}

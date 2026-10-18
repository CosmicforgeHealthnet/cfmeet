//! Messaging architecture boundaries (Part V A.3 / C.1): no other service's source,
//! database or tables; the client (private-key) half of the crypto core is never linked
//! into the backend; no plaintext-message parser; keys, tokens and content never logged.

use std::{fs, path::Path};

fn sources() -> Vec<(String, String)> {
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
    let mut out = Vec::new();
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    walk(&base.join("src"), base, &mut out);
    walk(&base.join("migration/src"), base, &mut out);
    out
}

fn manifest() -> toml_lite::Doc {
    toml_lite::Doc(
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap(),
    )
}

/// Minimal section reader (no TOML dependency needed for these checks).
mod toml_lite {
    pub struct Doc(pub String);
    impl Doc {
        pub fn section(&self, name: &str) -> String {
            let header = format!("[{name}]");
            let Some(start) = self.0.find(&header) else {
                return String::new();
            };
            let rest = &self.0[start + header.len()..];
            let end = rest.find("\n[").unwrap_or(rest.len());
            rest[..end].to_string()
        }
    }
}

#[test]
fn no_other_service_source_or_database() {
    let m = manifest().0;
    for other in [
        "calling",
        "numbering",
        "telephony",
        "privacy-relay",
        "platform-access",
        "meeting-service",
        "sms-gateway",
    ] {
        for prefix in ["services/", "../"] {
            assert!(!m.contains(&format!("{prefix}{other}")), "{prefix}{other}");
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
            "sms_gateway::",
            "meeting_service::",
        ] {
            assert!(!code.contains(&format!("use {import}")), "{file}: {import}");
        }
        for owner in [
            "CALLING",
            "NUMBERING",
            "TELEPHONY",
            "PRIVACY_RELAY",
            "PLATFORM_ACCESS",
            "MEETING",
            "SMS_GATEWAY",
        ] {
            assert!(
                !code.contains(&format!("{owner}_DATABASE_URL")),
                "{file}: {owner}"
            );
        }
        for table in [
            "platform_",
            "conference_",
            "chat_messages",
            "calls",
            "number_",
            "sms_",
            "users",
            "meetings",
            "participants",
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
                assert!(
                    lower[i + 13..].starts_with("messaging_"),
                    "{file}: table outside messaging_"
                );
            }
        }
    }
}

#[test]
fn the_backend_links_only_the_public_half_of_the_crypto_core() {
    let doc = manifest();
    let deps = doc.section("dependencies");
    let line = deps
        .lines()
        .find(|l| l.starts_with("cf-messaging-crypto"))
        .expect("crypto core dependency");
    assert!(
        !line.contains("features"),
        "the backend must not enable crypto features: {line}"
    );
    assert!(!deps.contains("openmls_basic_credential"));
    for (file, code) in sources() {
        for term in [
            "device::Device",
            "SignatureKeyPair",
            "KeyPackageBundle",
            "init_private_key",
            "encryption_private_key",
        ] {
            assert!(
                !code.contains(term),
                "{file}: private-key API `{term}` in the backend"
            );
        }
    }
}

#[test]
fn e2ee_content_is_opaque_to_the_backend() {
    for (file, code) in sources() {
        if file == "src/domain/profile.rs" {
            continue;
        }
        // Ciphertext is never turned into text, and no message-body parser exists.
        for term in [
            "String::from_utf8(ciphertext",
            "from_utf8(&ciphertext",
            "parse_message_body",
            "decrypt(",
        ] {
            assert!(!code.contains(term), "{file}: `{term}`");
        }
    }
    let profile =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/domain/profile.rs"))
            .unwrap();
    let production = profile.split("#[cfg(test)]").next().unwrap();
    assert!(!production.contains("impl fmt::Display for OpaqueCiphertext"));
    assert!(!production.contains("impl From<OpaqueCiphertext>"));
}

#[test]
fn keys_tokens_and_content_are_never_logged() {
    for (file, code) in sources() {
        for (i, _) in code.match_indices("tracing::") {
            let name: String = code[i + 9..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !code[i + 9 + name.len()..].starts_with('!') {
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
                "credential =",
                "key_package",
                "signature_key",
                "nonce",
                "ciphertext",
                "content",
                "body",
                "presented",
                "secret",
            ] {
                assert!(
                    !call.contains(term),
                    "{file}: logs `{term}`: {}",
                    &code[i..end]
                );
            }
        }
    }
}

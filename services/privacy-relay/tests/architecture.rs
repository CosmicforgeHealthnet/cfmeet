//! Architecture guards: Privacy Relay is a generic platform service with no consumer
//! product model, no other service's source or database, and none of the later sections
//! (Contact Vault, relay numbers, telephony) pre-built.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
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

/// Source without comments (documentation may explain the rules).
fn code_without_comments(source: &str) -> String {
    source
        .lines()
        .map(|l| match l.find("//") {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn production_sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(&root().join("src"), &mut files);
    rust_files(&root().join("migration/src"), &mut files);
    files
        .into_iter()
        .map(|f| {
            let rel = f.strip_prefix(root()).unwrap().display().to_string();
            let source = std::fs::read_to_string(&f).unwrap();
            // Unit-test modules (always last in a file) hold deliberate negative examples.
            let production = source.split("#[cfg(test)]").next().unwrap_or_default();
            (rel, code_without_comments(production))
        })
        .collect()
}

/// Consumer products and consumer business objects that must never become platform
/// concepts.
const FORBIDDEN_PRODUCT_TERMS: &[&str] = &["karryam", "healthnet", "health_net", "health-net"];
const FORBIDDEN_DOMAIN_TYPES: &[&str] = &[
    "TripContext",
    "DeliveryContext",
    "AppointmentContext",
    "OrderContext",
    "ViewingContext",
    "Rider",
    "Driver",
    "Doctor",
    "Patient",
    "Courier",
];

fn product_model_violations(file: &str, code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = code.to_lowercase();
    for term in FORBIDDEN_PRODUCT_TERMS {
        if lower.contains(term) {
            out.push(format!("{file}: consumer product `{term}`"));
        }
    }
    for ty in FORBIDDEN_DOMAIN_TYPES {
        if code.contains(ty) {
            out.push(format!("{file}: consumer business type `{ty}`"));
        }
    }
    if lower.contains("if application ==") || lower.contains("if tenant ==") {
        out.push(format!("{file}: product-name branching"));
    }
    out
}

#[test]
fn privacy_relay_has_no_consumer_specific_model() {
    let mut violations = Vec::new();
    for (file, code) in production_sources() {
        violations.extend(product_model_violations(&file, &code));
    }
    assert!(violations.is_empty(), "{violations:#?}");
}

#[test]
fn the_product_model_guard_detects_mutations_and_ignores_documentation() {
    let mutated = "pub struct KarryamTripContext { pub rider_id: Uuid }";
    let found = product_model_violations("src/context/aggregate.rs", mutated);
    assert!(found.iter().any(|v| v.contains("karryam")), "{found:?}");
    assert!(found.iter().any(|v| v.contains("TripContext")), "{found:?}");
    let doc = "//! Consumers (e.g. a Karryam delivery or a HealthNet appointment) map onto a\n//! generic CommunicationContext.\npub struct CommunicationContext;";
    assert!(product_model_violations("src/x.rs", &code_without_comments(doc)).is_empty());
    let branching = "fn f() { if application == \"karryam\" { } }";
    assert!(!product_model_violations("src/x.rs", branching).is_empty());
}

#[test]
fn no_other_service_source_or_database() {
    for manifest in ["Cargo.toml", "migration/Cargo.toml"] {
        let text = std::fs::read_to_string(root().join(manifest)).unwrap();
        // Fragments are joined so the repository guard does not see the literals here.
        for forbidden in [
            concat!("services/", "calling"),
            concat!("services/", "meeting-service"),
            concat!("../", "calling"),
            concat!("../", "meeting-service"),
            "calling-migration",
            "backend",
        ] {
            assert!(
                !text.contains(forbidden),
                "{manifest} references {forbidden}"
            );
        }
    }
    for (file, code) in production_sources() {
        for forbidden in [
            "use calling",
            "calling::",
            "meeting_service",
            concat!("CALLING", "_DATABASE_URL"),
            concat!("MEETING", "_DATABASE_URL"),
            "platform_api_credentials",
            "platform_principals",
            "platform_tenants",
            "platform_applications",
            "FROM calls",
            "JOIN calls",
            "call_participants",
        ] {
            assert!(!code.contains(forbidden), "{file} references `{forbidden}`");
        }
    }
}

#[test]
fn later_sections_are_not_prebuilt() {
    for (file, code) in production_sources() {
        // Database deny-patterns (`… !~* '(phone|e164|…)'`) name what they refuse.
        let lower: String = code
            .lines()
            .filter(|l| !l.contains("!~*"))
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        for forbidden in [
            "msisdn",
            "pstn",
            "sip_uri",
            "carrier_",
            "twilio",
            "vonage",
            "africastalking",
            "telnyx",
            "plivo",
            "phone_number",
        ] {
            // The Contact Vault's raw input member is `telephone_number`; it exists there only.
            if forbidden == "phone_number" && file.contains("src/contact_vault/") {
                continue;
            }
            assert!(!lower.contains(forbidden), "{file}: `{forbidden}`");
        }
    }
}

/// The temporary relay-number inventory (§34) is its own boundary: only
/// `src/relay_numbers/` (and the operator CLI through it) touches the inventory and its
/// ledger; the platform-owned public number is read nowhere else, and the provider
/// binding is only ever WRITTEN (on add) — no query selects it.
#[test]
fn the_relay_number_inventory_is_an_isolated_boundary() {
    let mut seen = false;
    for (file, code) in production_sources() {
        if file.contains("migration/src/") {
            continue;
        }
        let inside = file.contains("src/relay_numbers/");
        for needle in [
            "relay_number_inventory",
            "relay_number_events",
            "canonical_public_e164",
        ] {
            if code.contains(needle) {
                assert!(
                    inside,
                    "{file} uses `{needle}` outside the relay-number boundary"
                );
                seen = true;
            }
        }
        if code.contains("provider_binding_ref") {
            assert!(
                file.ends_with("src/relay_numbers/inventory.rs")
                    || file.ends_with("src/relay_numbers/mod.rs")
                    // The operator input path (it only passes the value to `add`).
                    || file.ends_with("src/bin/privacy-relay-admin.rs"),
                "{file} touches the provider binding"
            );
        }
    }
    assert!(seen, "the guard sees the inventory");
    let columns = std::fs::read_to_string(root().join("src/relay_numbers/mod.rs")).unwrap();
    let select = columns
        .split("INVENTORY_COLUMNS: &str =")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(
        !select.contains("provider_binding_ref"),
        "no query selects the provider binding"
    );
    // The relay number is never logged or formatted: no tracing field carries it.
    for (file, code) in production_sources() {
        assert!(
            !code.contains("canonical_public_e164 = %") && !code.contains("e164 = %"),
            "{file}"
        );
    }
}

/// The Contact Vault is an isolated code boundary: the only plaintext accessor
/// (`expose_e164`), the phone-number parser and the AES-GCM primitive are used inside
/// `src/contact_vault/` only, and no other module touches `protected_contacts`.
#[test]
fn the_contact_vault_is_an_isolated_code_boundary() {
    let mut seen_parser = false;
    for (file, code) in production_sources() {
        if file.contains("migration/src/") {
            continue; // the schema itself
        }
        let inside = file.contains("src/contact_vault/");
        for (needle, allowed) in [
            (
                "expose_e164",
                &[
                    "normalization.rs",
                    "crypto.rs",
                    "fingerprint.rs",
                    "resolution.rs",
                ][..],
            ),
            ("phonenumber::", &["normalization.rs"][..]),
            ("aes_gcm", &["crypto.rs", "keyring.rs"][..]),
            (
                "protected_contacts",
                &[
                    "repository.rs",
                    "service.rs",
                    "fingerprint.rs",
                    "verification.rs",
                    "resolution.rs",
                    "inbound.rs",
                ][..],
            ),
        ] {
            if code.contains(needle) {
                assert!(
                    inside && allowed.iter().any(|a| file.ends_with(a)),
                    "{file} uses `{needle}` outside the Contact Vault boundary"
                );
                seen_parser |= needle == "phonenumber::";
            }
        }
        // No raw value is ever logged or formatted from a request body.
        assert!(
            !code.contains("telephone_number = %") && !code.contains("telephone_number = ?"),
            "{file}"
        );
    }
    assert!(seen_parser, "the guard sees the parser");
}

#[test]
fn every_internal_route_checks_a_receiver_permission() {
    let code = std::fs::read_to_string(root().join("src/internal/mod.rs")).unwrap();
    let handlers = code.matches("caller: InternalCaller").count();
    let flat: String = code.split_whitespace().collect();
    let checks = flat.matches("authorize_caller(&caller").count()
        + flat.matches("reader(&caller").count()
        + flat.matches("contact_actor(&caller").count()
        + flat.matches("inbound_requester(&caller").count();
    assert!(handlers >= 10, "{handlers}");
    assert_eq!(
        handlers, checks,
        "one permission check per handler (the lifecycle macro has its own)"
    );
    // Every request body refuses unknown members.
    for (file, code) in production_sources() {
        if code.contains("Deserialize")
            && (file.contains("service.rs")
                || file.contains("authorization.rs")
                || file.contains("internal"))
        {
            for (i, _) in code.match_indices("#[derive(Debug, Clone, Serialize, Deserialize)]") {
                let window = &code[i..(i + 200).min(code.len())];
                assert!(
                    window.contains("deny_unknown_fields"),
                    "{file}: request body without deny_unknown_fields"
                );
            }
        }
    }
}

#[test]
fn no_phase_03_paths() {
    let mut files = Vec::new();
    rust_files(&root(), &mut files);
    assert!(files
        .iter()
        .all(|f| !f.to_string_lossy().contains("phase-03")));
}

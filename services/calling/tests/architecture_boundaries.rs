//! M / §16. Architecture boundaries (static checks over this service's sources):
//!
//! * Calling is independent of `services/meeting-service`: no path dependency, no source
//!   import, no Meeting Service crate names, no Meeting Service tables.
//! * The platform core contains no consumer-specific domain types or product-name
//!   branching.
//! * Part C §15–§19: the call domain is provider-independent (no provider SDK, provider
//!   names or provider / protocol states; no telephone-number fields), clients cannot set
//!   a state (the state column is written only by the transition path; the public call
//!   API is read-only), media identities are only server-generated, and no Privacy Relay /
//!   Telephony / Numbering service, table or database dependency exists yet.
//! * Raw credential exposure is confined to the single CLI display site.
//! * Section 13: Calling's only path dependencies are its own migration crate and shared
//!   `packages/`; it reads no other service's database URL.
//! * Section 11: internal service authentication never touches customer credentials, and
//!   the four identity/context types never convert into one another implicitly.
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

/// Tables owned by Calling: the platform foundation and the call domain.
fn is_calling_table(name: &str) -> bool {
    name.starts_with("platform_")
        || name.starts_with("call_")
        || name == "calls"
        || name == "calling_outbox_events"
}

/// Code with the contents of string literals removed (identifiers, types and syntax
/// only), so explicit deny-lists of forbidden words do not trip identifier guards.
fn strip_strings(code: &str) -> String {
    let bytes = code.as_bytes();
    let mut out = String::with_capacity(code.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        // Raw strings r"…" / r#"…"#.
        if c == 'r' && (i == 0 || !(bytes[i - 1] as char).is_ascii_alphanumeric()) {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'#' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let hashes = j - i - 1;
                let close = format!("\"{}", "#".repeat(hashes));
                let end = code[j + 1..]
                    .find(&close)
                    .map_or(code.len(), |e| j + 1 + e + close.len());
                out.push_str("\"\"");
                i = end;
                continue;
            }
        }
        if c == '"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'"' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            out.push_str("\"\"");
            i = j + 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
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
                    is_calling_table(&name) || name.is_empty(),
                    "{file}: `{keyword}{name}` is not a Calling table"
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

/// Provider / vendor names and provider-shaped identifiers that never belong in Calling's
/// canonical call domain (execution adapters live behind other boundaries).
const PROVIDER_TERMS: &[&str] = &[
    "livekit",
    "twilio",
    "africastalking",
    "africas_talking",
    "vonage",
    "nexmo",
    "plivo",
    "telnyx",
    "sinch",
    "agora",
    "room_sid",
    "carrier_call",
    "provider_call",
    "sip_dialog",
    "dialog_id",
    "pstn",
    "voicemail",
];

/// Identifiers that would hold a telephone number.
const TELEPHONE_FIELDS: &[&str] = &[
    "phone",
    "msisdn",
    "e164",
    "telephone_number",
    "caller_number",
    "callee_number",
    "dialed_number",
    "caller_id",
];

/// Tables of other services' domains (Part III §13) Calling must never create or name.
const FOREIGN_DOMAIN_TABLES: &[&str] = &[
    "communication_contexts",
    "communication_context_participants",
    "communication_context_channels",
    "communication_context_directions",
    "privacy_relay_sessions",
    "privacy_relay_session_participants",
    "privacy_relay_session_channels",
    "privacy_relay_session_directions",
    "relay_authorization_decisions",
    "relay_audit_events",
    "privacy_relay_outbox",
    "privacy_relay_commands",
    "privacy_relay_enforcement_jobs",
    "relay_sessions",
    "protected_contacts",
    "contact_fingerprints",
    "contact_access_audit",
    "contact_verification_events",
    "contact_resolution_grants",
    "relay_number_inventory",
    "relay_number_assignments",
    "relay_number_events",
    "relay_inbound_resolutions",
    "contact_vault",
    "provider_call_bindings",
    "provider_events",
    "telephony_commands",
    "telephony_routes",
    "carrier_events",
];

/// The media provider adapter (Part C §24): the only files that may name the current
/// provider. Everything else — the Call aggregate, orchestration, the provider trait,
/// models, migrations, HTTP — stays provider-independent.
const MEDIA_ADAPTER_FILES: &[&str] = &[
    "src/media/livekit.rs",
    "src/media/livekit_gateway.rs",
    "src/media/mod.rs",
];

/// Whether `haystack` contains `word` as a whole identifier (not inside a longer name).
fn contains_word(haystack: &str, word: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    haystack.match_indices(word).any(|(i, _)| {
        let before = haystack[..i].chars().next_back();
        let after = haystack[i + word.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

/// Violations of the call-domain provider / contact rules in one production file.
/// The canonical Part E route classes (APP_TO_PSTN, PSTN_TO_APP, PSTN_RELAY) are CF Meet
/// vocabulary, not provider code: only these exact variant names may contain `pstn`.
const CANONICAL_ROUTE_VARIANTS: &[&str] = &["apptopstn", "pstntoapp", "pstnrelay"];

fn call_domain_violations(file: &str, code: &str) -> Vec<String> {
    let mut identifiers = strip_strings(code).to_lowercase();
    for variant in CANONICAL_ROUTE_VARIANTS {
        identifiers = identifiers.replace(variant, "");
    }
    let mut out = Vec::new();
    for term in PROVIDER_TERMS {
        if *term == "livekit" && MEDIA_ADAPTER_FILES.contains(&file) {
            continue;
        }
        if identifiers.contains(term) {
            out.push(format!("{file}: provider-specific `{term}`"));
        }
    }
    let in_call_domain = file.starts_with("src/calls/") || file.starts_with("src/models/call");
    if in_call_domain {
        for field in TELEPHONE_FIELDS {
            if identifiers.contains(field) {
                out.push(format!("{file}: telephone-number field `{field}`"));
            }
        }
    }
    let lower = code.to_lowercase();
    for table in FOREIGN_DOMAIN_TABLES {
        if contains_word(&lower, table) {
            out.push(format!("{file}: other service's table `{table}`"));
        }
    }
    out
}

/// Violations of "clients request actions; only the transition path writes a state".
fn state_write_violations(file: &str, code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for setter in [
        "fn set_state",
        "fn set_call_state",
        "fn set_current_state",
        "fn force_state",
        "fn update_state(",
        "fn override_state",
    ] {
        if code.contains(setter) {
            out.push(format!("{file}: state setter `{setter}`"));
        }
    }
    let writes_state = code.contains("current_state: Set(")
        || code.contains("current_state = Set(")
        || code.to_lowercase().contains("set current_state");
    if writes_state && file != "src/calls/service/lifecycle.rs" {
        out.push(format!(
            "{file}: writes calls.current_state outside the transition path"
        ));
    }
    if file == "src/calls/http.rs" {
        // Read-only: no mutating methods, no request bodies, no deserialized input types.
        for method in ["post(", "put(", "patch(", "delete(", "Json<", "Deserialize"] {
            if code.contains(method) {
                out.push(format!("{file}: public call API is read-only (`{method}`)"));
            }
        }
    }
    out
}

/// Violations of "media identities are generated by the server only".
fn media_identity_violations(file: &str, code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let writes = code.matches("media_identity: Set(").count();
    let generated = code
        .matches("media_identity: Set(MediaIdentity::generate()")
        .count();
    if writes != generated {
        out.push(format!(
            "{file}: media identity not from MediaIdentity::generate()"
        ));
    }
    if code.contains("MediaIdentity::generate()") && file != "src/calls/service/participation.rs" {
        out.push(format!(
            "{file}: media identities are generated only when a session is created"
        ));
    }
    if code.contains("MediaIdentity::from_persisted(") && file != "src/calls/sessions.rs" {
        out.push(format!(
            "{file}: MediaIdentity built from non-persisted input"
        ));
    }
    out
}

#[test]
fn call_domain_is_provider_independent_and_holds_no_telephone_numbers() {
    let mut violations = Vec::new();
    for (file, code) in production_sources() {
        violations.extend(call_domain_violations(&file, &code));
    }
    assert!(violations.is_empty(), "{violations:#?}");
    // Canonical states and kinds are CF Meet vocabulary, not provider / protocol states
    // or consumer products.
    for state in calling::calls::CallState::ALL {
        let s = state.as_str().to_lowercase();
        for term in PROVIDER_TERMS
            .iter()
            .chain(&["sip", "joined", "200", "180", "carrier"])
        {
            assert!(!s.contains(term), "{state} looks provider-specific");
        }
    }
    assert_eq!(calling::calls::CallState::ALL.len(), 17);
    let kinds: Vec<_> = calling::calls::kinds::CallKind::ALL
        .iter()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(kinds, ["VOICE", "VIDEO"]);
    // No provider SDK in the dependency graph.
    let root = manifest_dir();
    for manifest in ["Cargo.toml", "migration/Cargo.toml", "Cargo.lock"] {
        let text = std::fs::read_to_string(root.join(manifest))
            .unwrap()
            .to_lowercase();
        for sdk in [
            "livekit",
            "twilio",
            "vonage",
            "africastalking",
            "plivo",
            "telnyx",
            "agora",
        ] {
            assert!(!text.contains(sdk), "{manifest} depends on `{sdk}`");
        }
    }
}

#[test]
fn clients_request_actions_and_only_the_transition_path_writes_a_state() {
    let mut violations = Vec::new();
    for (file, code) in production_sources() {
        violations.extend(state_write_violations(&file, &code));
    }
    assert!(violations.is_empty(), "{violations:#?}");
    // The transition path does write it (the guard is not vacuous).
    let lifecycle = production_code_of(&manifest_dir().join("src/calls/service/lifecycle.rs"));
    assert!(lifecycle.contains("current_state: Set(CallState::Requested)"));
    assert!(lifecycle.contains("update.current_state = Set(plan.to)"));
}

#[test]
fn media_identities_are_only_server_generated() {
    let mut violations = Vec::new();
    for (file, code) in production_sources() {
        violations.extend(media_identity_violations(&file, &code));
    }
    assert!(violations.is_empty(), "{violations:#?}");
    let participation =
        production_code_of(&manifest_dir().join("src/calls/service/participation.rs"));
    assert_eq!(
        participation
            .matches("media_identity: Set(MediaIdentity::generate()")
            .count(),
        1
    );
    // No public operation takes a media identity as input.
    for (file, code) in production_sources() {
        for signature in code.split("pub async fn ").skip(1) {
            let params = signature.split(')').next().unwrap_or_default();
            assert!(
                !params.contains("MediaIdentity") && !params.contains("media_identity"),
                "{file}: an operation accepts a media identity"
            );
        }
    }
}

/// Part D §31: Calling is never a raw-number processing service. It mints Contact Vault
/// access tokens but has no telephone-number field, parser, vault key or vault table.
#[test]
fn calling_never_handles_raw_contact_values() {
    for (file, code) in production_sources() {
        let lower = code.to_lowercase();
        // (Refusing phone-shaped principal references, `looks_like_telephone_number`, is
        // the opposite of handling a number and is allowed.)
        for forbidden in [
            "telephone_number:",
            "\"telephonenumber\"",
            "phonenumber::",
            "contact_vault_keyring",
            "contact_fingerprint_keyring",
            "aes_gcm",
            "resolve-for-execution",
            "resolve_for_execution",
        ] {
            assert!(!lower.contains(forbidden), "{file}: `{forbidden}`");
        }
    }
    let manifest = std::fs::read_to_string(manifest_dir().join("Cargo.toml")).unwrap();
    for dep in ["phonenumber", "aes-gcm"] {
        assert!(!manifest.contains(dep), "Calling depends on `{dep}`");
    }
    // The grant request body is closed: it can carry scopes and nothing else.
    let contacts = production_code_of(&manifest_dir().join("src/privacy_relay/contacts.rs"));
    assert!(contacts.contains("deny_unknown_fields"));
}

#[test]
fn privacy_relay_and_telephony_are_peer_services_and_later_services_do_not_exist_yet() {
    let services = manifest_dir().join("..");
    for future in ["numbering", "contact-vault"] {
        assert!(
            !services.join(future).exists(),
            "services/{future} must not exist before its implementation phase"
        );
    }
    // Privacy Relay (Part D) is its own deployable service: Calling reaches it only over
    // authenticated internal HTTP and never depends on its source or migrations.
    assert!(services.join("privacy-relay/Cargo.toml").exists());
    // Telephony (Part E) likewise: authenticated internal HTTP only, never its source,
    // migrations or database.
    assert!(services.join("telephony/Cargo.toml").exists());
    for manifest in ["Cargo.toml", "migration/Cargo.toml"] {
        let text = std::fs::read_to_string(manifest_dir().join(manifest)).unwrap();
        assert!(
            !text.contains("privacy-relay"),
            "{manifest} depends on privacy-relay"
        );
        assert!(
            !text.contains("telephony"),
            "{manifest} depends on telephony"
        );
    }
    for (file, code) in production_sources() {
        for forbidden in [
            "telephony_migration".to_string(),
            "extern crate telephony".to_string(),
            "use telephony::".to_string(),
            format!("{}_DATABASE_URL", "TELEPHONY"),
        ] {
            assert!(
                !code.contains(&forbidden),
                "{file} imports Telephony source or data (`{forbidden}`)"
            );
        }
    }
    for (file, code) in production_sources() {
        for forbidden in [
            "privacy_relay_migration",
            "extern crate privacy_relay",
            "use privacy_relay::",
        ] {
            assert!(
                !code.contains(forbidden),
                "{file} imports Privacy Relay source (`{forbidden}`)"
            );
        }
    }
    // Calling's migrations create only Calling tables.
    let mut files = Vec::new();
    rust_files(&manifest_dir().join("migration/src"), &mut files);
    for file in files {
        let lower = std::fs::read_to_string(&file).unwrap().to_lowercase();
        for (i, _) in lower.match_indices("create table ") {
            let name: String = lower[i + "create table ".len()..]
                .trim_start_matches("unlogged table ")
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            assert!(
                is_calling_table(&name),
                "{}: creates {name}",
                file.display()
            );
        }
    }
}

#[test]
fn call_domain_guards_detect_violations() {
    // Mutation tests: each guard fires on a synthetic violation and stays quiet on the
    // legitimate forms.
    let provider = call_domain_violations(
        "src/calls/legs.rs",
        "pub struct Leg { pub livekit_room_sid: String, pub phone_number: String }",
    );
    assert!(
        provider.iter().any(|v| v.contains("livekit")),
        "{provider:?}"
    );
    assert!(provider.iter().any(|v| v.contains("room_sid")));
    assert!(provider.iter().any(|v| v.contains("phone")));
    assert!(!call_domain_violations(
        "src/calls/x.rs",
        "const DENY: &[&str] = &[\"pstn\", \"twilio\"];"
    )
    .iter()
    .any(|v| v.contains("provider-specific")));
    assert!(
        !call_domain_violations("src/calls/x.rs", "SELECT * FROM provider_call_bindings")
            .is_empty()
    );
    assert!(!call_domain_violations("src/calls/x.rs", "TwilioCallId").is_empty());
    // The provider may be named only by its adapter; the trait, orchestration and the
    // Call aggregate stay neutral. Calling's own neutral evidence table is not a foreign
    // `provider_events` table, but that table name is.
    let adapter = "pub struct LiveKitAdapter; const ADAPTER_NAME: &str = \"livekit\";";
    assert!(call_domain_violations("src/media/livekit.rs", adapter).is_empty());
    for file in [
        "src/media/provider.rs",
        "src/calls/app/media.rs",
        "src/calls/aggregate.rs",
        "src/models/calls.rs",
    ] {
        assert!(
            call_domain_violations(file, adapter)
                .iter()
                .any(|v| v.contains("livekit")),
            "{file}"
        );
    }
    assert!(
        call_domain_violations("src/media/livekit.rs", "struct TwilioRoom;")
            .iter()
            .any(|v| v.contains("twilio"))
    );
    assert!(call_domain_violations(
        "src/calls/app/media.rs",
        "INSERT INTO call_media_provider_events"
    )
    .is_empty());
    assert!(
        !call_domain_violations("src/calls/app/media.rs", "INSERT INTO provider_events").is_empty()
    );

    assert!(!state_write_violations(
        "src/calls/service/x.rs",
        "pub async fn set_call_state(&self)"
    )
    .is_empty());
    assert!(!state_write_violations(
        "src/calls/service/routing.rs",
        "calls::ActiveModel { current_state: Set(s) }"
    )
    .is_empty());
    assert!(!state_write_violations(
        "src/calls/service/x.rs",
        "UPDATE calls SET current_state = $1"
    )
    .is_empty());
    assert!(state_write_violations(
        "src/calls/service/lifecycle.rs",
        "update.current_state = Set(plan.to)"
    )
    .is_empty());
    assert!(!state_write_violations(
        "src/calls/http.rs",
        ".route(\"/v1/calls\", post(create_call))"
    )
    .is_empty());
    assert!(!state_write_violations(
        "src/calls/http.rs",
        "#[derive(Deserialize)] struct SetState"
    )
    .is_empty());
    assert!(
        !state_write_violations("src/calls/http.rs", "async fn act(Json(body): Json<Value>)")
            .is_empty()
    );

    assert!(!media_identity_violations(
        "src/calls/service/participation.rs",
        "media_identity: Set(input.identity)"
    )
    .is_empty());
    assert!(
        !media_identity_violations("src/calls/http.rs", "MediaIdentity::from_persisted(body)")
            .is_empty()
    );
    assert!(!media_identity_violations(
        "src/calls/service/lifecycle.rs",
        "MediaIdentity::generate()"
    )
    .is_empty());
    assert!(media_identity_violations(
        "src/calls/service/participation.rs",
        "media_identity: Set(MediaIdentity::generate().as_str().to_string())"
    )
    .is_empty());

    assert!(is_calling_table("call_sessions") && is_calling_table("calls"));
    assert!(!is_calling_table("communication_contexts") && !is_calling_table("users"));
    assert_eq!(
        strip_strings("let a = \"twilio\"; let b = r#\"pstn\"#; c"),
        "let a = \"\"; let b = \"\"; c"
    );
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

#[test]
fn path_dependencies_are_own_crates_or_shared_packages_only() {
    let root = manifest_dir();
    let packages = root.join("../../packages").canonicalize().unwrap();
    for manifest in ["Cargo.toml", "migration/Cargo.toml"] {
        let manifest_path = root.join(manifest);
        let base = manifest_path.parent().unwrap();
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        for line in text.lines().map(str::trim).filter(|l| l.contains('{')) {
            let Some(at) = line.find("path = \"") else {
                continue;
            };
            let rel = &line[at + 8..];
            let rel = &rel[..rel.find('"').unwrap()];
            let target = base.join(rel).canonicalize().unwrap();
            assert!(
                target.starts_with(&root) || target.starts_with(&packages),
                "{manifest}: path dependency `{rel}` leaves this service and is not a shared package"
            );
        }
    }
}

#[test]
fn calling_reads_no_other_service_database_url() {
    for (file, code) in production_sources() {
        for (i, _) in code.match_indices("_DATABASE_URL") {
            let name: String = code[..i]
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            assert!(
                name == "TEST" || name.is_empty(),
                "{file} reads another service's database URL `{name}_DATABASE_URL`"
            );
        }
        let lower = code.to_lowercase();
        for helper in [
            "meeting_db",
            "meeting_database",
            "telephony_db",
            "privacy_relay_db",
        ] {
            assert!(
                !lower.contains(helper),
                "{file} contains cross-service helper `{helper}`"
            );
        }
    }
}

#[test]
fn internal_service_authentication_never_uses_customer_credentials() {
    let mut files = Vec::new();
    rust_files(&manifest_dir().join("src/internal_auth"), &mut files);
    assert!(files.len() >= 4, "internal_auth module not found");
    for file in files {
        let code = production_code_of(&file);
        for forbidden in [
            "platform::credentials",
            "platform::authentication",
            "VerifierKeyring",
            "API_CREDENTIAL_PEPPER",
            "platform_api_credentials",
            "cfmk_",
            "keyring",
        ] {
            assert!(
                !code.contains(forbidden),
                "{}: internal service auth references customer credential machinery `{forbidden}`",
                file.display()
            );
        }
    }
}

#[test]
fn calling_never_requests_plaintext_contact_resolution() {
    for (file, code) in production_sources() {
        for forbidden in [
            "ProtectedContactResolveForExecution",
            "protected-contact.resolve",
        ] {
            assert!(!code.contains(forbidden), "{file} references `{forbidden}`");
        }
    }
}

#[test]
fn context_types_have_no_implicit_conversions() {
    const TYPES: [&str; 5] = [
        "PlatformAuthContext",
        "ResolvedPrincipal",
        "InternalServiceContext",
        "DelegatedPlatformContext",
        "InternalCaller",
    ];
    for (file, code) in production_sources() {
        for from in TYPES {
            for to in TYPES.iter().filter(|t| **t != from) {
                for conversion in ["From", "TryFrom"] {
                    for form in [
                        format!("impl {conversion}<{from}> for {to}"),
                        format!("impl {conversion}<&{from}> for {to}"),
                        format!("impl<'a> {conversion}<&'a {from}> for {to}"),
                    ] {
                        assert!(!code.contains(&form), "{file}: `{form}`");
                    }
                }
            }
        }
        // Customer and internal contexts are never deserialized from requests.
        if file == "src/internal_auth/middleware.rs" {
            assert!(!code.contains("Deserialize"));
        }
    }
}

/// Fields a client request body must never carry (Part C §75, §127): scope, state,
/// authorization, routing, provider, room, media identity or permissions.
const FORBIDDEN_REQUEST_FIELDS: &[&str] = &[
    "tenant_id",
    "application_id",
    "environment_id",
    "state",
    "current_state",
    "call_state",
    "authorized",
    "authorization",
    "route",
    "provider",
    "room",
    "room_name",
    "media_identity",
    "identity",
    "permissions",
    "can_publish_audio",
    "can_publish_video",
    "can_publish_data",
    "can_subscribe",
    "grants",
    "phone_number",
];

/// Field names of every `Deserialize` struct in `code`.
fn request_body_fields(code: &str) -> Vec<String> {
    let mut fields = Vec::new();
    for (i, _) in code.match_indices("Deserialize") {
        let Some(start) = code[i..].find("struct ") else {
            continue;
        };
        let rest = &code[i + start..];
        let (Some(open), Some(close)) = (rest.find('{'), rest.find('}')) else {
            continue;
        };
        if open > close {
            continue;
        }
        for line in rest[open + 1..close].lines() {
            let line = line
                .trim()
                .trim_start_matches("pub ")
                .trim_start_matches("pub(crate) ");
            if let Some((name, _)) = line.split_once(':') {
                if !name.starts_with('#') && !name.is_empty() {
                    fields.push(name.trim().to_string());
                }
            }
        }
    }
    fields
}

fn request_body_violations(file: &str, code: &str) -> Vec<String> {
    request_body_fields(code)
        .into_iter()
        .filter(|f| FORBIDDEN_REQUEST_FIELDS.contains(&f.as_str()))
        .map(|f| format!("{file}: request body accepts `{f}`"))
        .collect()
}

#[test]
fn request_bodies_carry_cf_meet_concepts_only() {
    let mut violations = Vec::new();
    let mut checked = 0;
    for (file, code) in production_sources() {
        if file.ends_with("http.rs") {
            checked += request_body_fields(&code).len();
            violations.extend(request_body_violations(&file, &code));
            if code.contains("Deserialize") {
                assert!(
                    code.contains("deny_unknown_fields"),
                    "{file}: request bodies must refuse unknown fields"
                );
            }
        }
    }
    assert!(checked > 10, "request bodies were found ({checked} fields)");
    assert!(violations.is_empty(), "{violations:#?}");
    // Mutation: a client-chosen state, scope or permission is detected.
    for field in ["state", "tenant_id", "can_publish_video", "room"] {
        let code = format!(
            "#[derive(Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct Body {{\n    expected_version: i64,\n    {field}: String,\n}}"
        );
        assert_eq!(
            request_body_violations("src/x/http.rs", &code).len(),
            1,
            "{field}"
        );
    }
    assert!(request_body_violations(
        "src/x/http.rs",
        "#[derive(Deserialize)]\nstruct Body {\n    expected_version: i64,\n}"
    )
    .is_empty());
}

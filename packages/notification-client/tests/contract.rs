//! Conformance in both directions between `cf-notification-client` and the frozen
//! contract `cfmeet.notifications.event` (packages/contracts/notifications/events):
//! the Rust catalogue equals `x-cfmeet-catalogue`; every envelope the builder produces is
//! valid against the JSON Schema; every contract example passes the Rust validator and
//! every counter-example (forbidden keys, unsealed secrets, wrong producer, ...) is refused
//! by both.

use std::{fs, path::PathBuf};

use cf_notification_client::{
    catalogue::{LegacyKey, RecipientKind, SealedField, CATALOGUE, CONTROL_EVENT},
    envelope::{
        allowed_data_key, validate, Envelope, EnvelopeBuilder, EnvelopeError,
        FORBIDDEN_DATA_KEY_PARTS,
    },
    routing::RouteRef,
    seal::{generate_keypair, SealedPlaintext, SealingKey},
    Producer,
};
use jsonschema::{Registry, Resource, Validator};
use serde_json::{json, Value};
use uuid::Uuid;

const EVENTS: &str = "notifications/events/v1.schema.json";

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts")
}

fn read(rel: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(contracts().join(rel)).unwrap()).unwrap()
}

fn registry() -> Registry<'static> {
    let manifest = read("manifest.json");
    let mut builder = Registry::new();
    for c in manifest["contracts"].as_array().unwrap() {
        if c["kind"] == "json-schema" {
            let doc = read(c["path"].as_str().unwrap());
            let id = doc["$id"].as_str().unwrap().to_string();
            builder = builder.add(id, Resource::from_contents(doc)).unwrap();
        }
    }
    builder.prepare().unwrap()
}

fn validator() -> Validator {
    let registry = registry();
    jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(&read(EVENTS))
        .unwrap()
}

fn fixtures(kind: &str) -> Vec<(String, Value)> {
    let dir = contracts().join("notifications/events").join(kind);
    let mut out: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().to_string(),
                serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap(),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!out.is_empty());
    out
}

fn key() -> SealingKey {
    let (public, _private) = generate_keypair("notif-hpke-2026-11").unwrap();
    SealingKey::parse(&public).unwrap()
}

fn route() -> RouteRef {
    RouteRef {
        decision_id: Uuid::new_v4(),
        routing_version: 7,
        protocol: 1,
    }
}

const U: &str = "5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607";

/// A schema-valid sample value for each data key used by the catalogue.
fn sample(key: &str) -> Value {
    match key {
        "startsAt" | "changedAt" | "expiresAt" | "requestedAt" => json!("2026-11-20T09:00:00Z"),
        "purpose" => json!("CHANGE_EMAIL"),
        "side" => json!("OLD"),
        "requestClient" => json!("Mozilla/5.0 (X11; Linux x86_64) Firefox/133.0"),
        "requestNetwork" => json!("102.89.x.x"),
        "ttlMinutes" => json!(1440),
        "eventSequence" => json!(42),
        "sequence" => json!(41),
        "role" => json!("MEMBER"),
        "conversationKind" => json!("DIRECT"),
        "securityProfile" => json!("MANAGED"),
        "callKind" => json!("AUDIO"),
        "decision" => json!("REJECTED"),
        "actorRef" => json!(format!("cfmeet-user:{U}")),
        "alertName" => json!("NotificationsReplicaDown"),
        "severity" => json!("warning"),
        "status" => json!("resolved"),
        _ => json!(U),
    }
}

fn sample_idempotency_key(format: LegacyKey) -> String {
    match format {
        LegacyKey::DeviceLinked
        | LegacyKey::MeetingInvitation
        | LegacyKey::Welcome
        | LegacyKey::NewSignIn => format!("{}{U}", format.prefix()),
        LegacyKey::EmailChanged => format!("{}old:{U}", format.prefix()),
        LegacyKey::OrgInvitation => format!("{}{U}:3", format.prefix()),
        _ => format!("{}{U}:1", format.prefix()),
    }
}

/// Builds the minimal valid envelope of every catalogue type.
fn build(spec: &cf_notification_client::EventSpec, key: &SealingKey) -> Envelope {
    let producer = spec.producer.unwrap_or(Producer::MeetingService);
    let mut b = EnvelopeBuilder::new(producer, spec.event_type, "2026-11-15T12:00:00Z", route())
        .unwrap()
        .dedupe_key(format!("{}:{U}", spec.event_type));
    if spec.is_control() {
        b = b.cancel_key(format!("conference:{U}"));
    } else if producer == Producer::Alertmanager {
        b = b.recipient("operator-group:platform-operators");
    } else if spec.recipient == Some(RecipientKind::EmailPrincipal) {
        b = b.recipient(format!("meeting-email-principal:{U}"));
        // The sign-in code may be for an address without an account, and the email-in-use
        // notice must not reveal one: they name no subject.
        if !matches!(
            spec.event_type,
            "account.sign-in-code.requested" | "account.security.email-in-use"
        ) {
            b = b.subject(Uuid::parse_str(U).unwrap(), Some(1));
        }
    } else {
        b = b
            .recipient(format!("cfmeet-user:{U}"))
            .subject(Uuid::parse_str(U).unwrap(), Some(1));
    }
    for k in spec.required_data.iter().chain(spec.optional_data) {
        b = b.data(k, sample(k));
    }
    if let Some(format) = spec.idempotency_key {
        b = b.idempotency_key(sample_idempotency_key(format));
    }
    if spec.sealed_allowed() {
        let mut p = SealedPlaintext::default();
        for f in spec.sealed.iter().chain(spec.sealed_optional) {
            match f {
                SealedField::Code => p.code = Some("482913".into()),
                SealedField::Token => p.token = Some("tok_0123456789abcdefghij".into()),
                SealedField::Address => p.address = Some("invitee@example.com".into()),
                SealedField::MaskedAddress => p.masked_address = Some("n***@example.com".into()),
                SealedField::RequestClient => p.request_client = Some("TestBrowser/1.0".into()),
                SealedField::RequestNetwork => p.request_network = Some("102.89.x.x".into()),
            }
        }
        b.secret(p).build_sealed(key).unwrap()
    } else {
        b.build().unwrap()
    }
}

#[test]
fn the_rust_catalogue_equals_the_contract_catalogue() {
    let contract = read(EVENTS);
    let entries = contract["x-cfmeet-catalogue"].as_array().unwrap();
    assert_eq!(entries.len(), CATALOGUE.len());
    assert_eq!(CATALOGUE.iter().filter(|s| !s.is_control()).count(), 38);
    assert_eq!(CATALOGUE.iter().filter(|s| s.optional).count(), 1);
    for (e, s) in entries.iter().zip(CATALOGUE) {
        let what = s.event_type;
        assert_eq!(e["eventType"], s.event_type, "{what}");
        assert_eq!(e["number"], s.number, "{what}");
        assert_eq!(
            e["producer"],
            json!(s.producer.map(|p| p.as_str())),
            "{what}"
        );
        assert_eq!(e["priority"], s.priority.as_str(), "{what}");
        assert_eq!(e["lane"], s.lane().as_str(), "{what}");
        assert_eq!(e["category"], json!(s.category), "{what}");
        assert_eq!(e["subjectKind"], json!(s.subject_kind), "{what}");
        assert_eq!(e["optional"], s.optional, "{what}");
        let sealed: Option<Vec<&str>> =
            (!s.sealed.is_empty()).then(|| s.sealed.iter().map(|f| f.as_str()).collect());
        assert_eq!(e["sealed"], json!(sealed), "{what}");
        let optional: Option<Vec<&str>> = (!s.sealed_optional.is_empty())
            .then(|| s.sealed_optional.iter().map(|f| f.as_str()).collect());
        assert_eq!(e["sealedOptional"], json!(optional), "{what}");
        assert_eq!(
            e["idempotencyKey"],
            json!(s.idempotency_key.map(LegacyKey::name)),
            "{what}"
        );
        let mut keys: Vec<&str> = s
            .required_data
            .iter()
            .chain(s.optional_data)
            .copied()
            .collect();
        keys.sort_unstable();
        assert_eq!(e["dataKeys"], json!(keys), "{what}");
        assert_eq!(
            e["recipient"],
            json!(s.recipient.map(RecipientKind::as_str)),
            "{what}"
        );
    }
    // Closed data values agree with the per-type schema enums.
    let rules = contract["allOf"].as_array().unwrap();
    for s in CATALOGUE {
        for (key, values) in s.closed_data {
            let rule = rules
                .iter()
                .find(|r| r["if"]["properties"]["eventType"]["const"] == s.event_type)
                .unwrap();
            assert_eq!(
                rule["then"]["properties"]["data"]["properties"][key]["enum"],
                json!(values),
                "{} {key}",
                s.event_type
            );
        }
    }
    // The control event's producers.
    let control = entries
        .iter()
        .find(|e| e["eventType"] == CONTROL_EVENT)
        .unwrap();
    let producers: Vec<&str> = cf_notification_client::catalogue::CONTROL_PRODUCERS
        .iter()
        .map(|p| p.as_str())
        .collect();
    assert_eq!(control["producers"], json!(producers));
    // The schema's EventType / ProducerId enums agree too.
    let common = read("notifications/common/v1.schema.json");
    let types: Vec<&str> = CATALOGUE.iter().map(|s| s.event_type).collect();
    assert_eq!(common["$defs"]["EventType"]["enum"], json!(types));
    let producers: Vec<&str> = Producer::ALL.iter().map(|p| p.as_str()).collect();
    assert_eq!(common["$defs"]["ProducerId"]["enum"], json!(producers));
}

#[test]
fn legacy_key_formats_agree_with_the_contract_patterns() {
    let contract = read(EVENTS);
    let patterns = contract["x-cfmeet-legacy-idempotency-keys"]
        .as_object()
        .unwrap();
    let all = LegacyKey::ALL.iter().copied();
    assert_eq!(patterns.len(), LegacyKey::ALL.len());
    let samples = |f: LegacyKey| {
        vec![
            sample_idempotency_key(f),
            f.prefix().to_string(),
            format!("{}{U}:x", f.prefix()),
            format!("{}a b", f.prefix()),
            format!("{}{}", f.prefix(), "x".repeat(141)),
            format!("other:{U}"),
            format!("{}{U}:1234567890", f.prefix()),
        ]
    };
    for f in all {
        let pattern = patterns[f.name()].as_str().unwrap();
        let v =
            jsonschema::validator_for(&json!({ "type": "string", "pattern": pattern })).unwrap();
        assert!(f.matches(&sample_idempotency_key(f)), "{}", f.name());
        for s in samples(f) {
            assert_eq!(
                f.matches(&s),
                v.is_valid(&json!(s)),
                "{} vs pattern on {s:?}",
                f.name()
            );
        }
    }
}

#[test]
fn every_built_envelope_conforms_to_the_contract() {
    let v = validator();
    let key = key();
    for spec in CATALOGUE {
        let env = build(spec, &key);
        let value = serde_json::to_value(&env).unwrap();
        let errors: Vec<String> = v
            .iter_errors(&value)
            .map(|e| format!("{e} at {}", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{}: {errors:?}\n{value:#}",
            spec.event_type
        );
        assert_eq!(
            validate(&env, Some(&env.producer)).unwrap().event_type,
            spec.event_type
        );
        assert_eq!(
            env.sealed.is_some(),
            spec.sealed_allowed(),
            "{}",
            spec.event_type
        );
    }
}

#[test]
fn contract_examples_pass_and_counter_examples_fail_in_rust() {
    for (name, value) in fixtures("examples") {
        let env: Envelope = serde_json::from_value(value).unwrap_or_else(|e| panic!("{name}: {e}"));
        validate(&env, Some(&env.producer)).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    for (name, value) in fixtures("counter-examples") {
        let refused = match serde_json::from_value::<Envelope>(value) {
            Err(_) => true,
            Ok(env) => validate(&env, None).is_err(),
        };
        assert!(
            refused,
            "counter-example {name} is accepted by the Rust validator"
        );
    }
}

#[test]
fn forbidden_data_keys_are_refused_in_every_case_by_rust_and_by_the_schema() {
    let v = validator();
    let key = key();
    let base = build(
        cf_notification_client::spec("organizations.ownership.transferred").unwrap(),
        &key,
    );
    for word in FORBIDDEN_DATA_KEY_PARTS {
        let mut capital = word.to_string();
        capital[..1].make_ascii_uppercase();
        for k in [
            word.to_string(),
            format!("my{capital}"),
            format!("x{}", word.to_ascii_uppercase()),
            format!("a{capital}Id"),
        ] {
            assert!(!allowed_data_key(&k), "{k}");
            let mut env = base.clone();
            env.data.insert(k.clone(), json!("x"));
            assert!(
                matches!(validate(&env, None), Err(EnvelopeError::DataKey(_))),
                "{k}"
            );
            assert!(
                !v.is_valid(&serde_json::to_value(&env).unwrap()),
                "schema accepts {k}"
            );
        }
    }
}

#[test]
fn producers_priorities_and_sealed_allowance_are_enforced() {
    let key = key();
    // A producer outside the type's allowlist cannot even start the envelope.
    assert_eq!(
        EnvelopeBuilder::new(
            Producer::Organizations,
            "account.verification-code.requested",
            "2026-11-15T12:00:00Z",
            route()
        )
        .unwrap_err(),
        EnvelopeError::ProducerNotAllowed
    );
    // CRITICAL only for account.* from the Meeting Service.
    for spec in CATALOGUE {
        if spec.priority == cf_notification_client::Priority::Critical {
            assert!(spec.event_type.starts_with("account."));
            assert_eq!(spec.producer, Some(Producer::MeetingService));
        }
    }
    let env = build(
        cf_notification_client::spec("account.verification-code.requested").unwrap(),
        &key,
    );
    // The I1 issuer must equal the producer.
    assert_eq!(
        validate(&env, Some("cfmeet.organizations")).unwrap_err(),
        EnvelopeError::ProducerMismatch
    );
    // Unsealed code type refused; sealed part on a non-sealed type refused.
    let mut unsealed = env.clone();
    unsealed.sealed = None;
    assert_eq!(
        validate(&unsealed, None).unwrap_err(),
        EnvelopeError::SealedRequired("code")
    );
    let mut missed = build(
        cf_notification_client::spec("calling.call.participant-missed").unwrap(),
        &key,
    );
    missed.sealed = env.sealed.clone();
    assert_eq!(
        validate(&missed, None).unwrap_err(),
        EnvelopeError::SealedNotAllowed
    );
    // A token in a code-only type, or a missing address for org invitations, is refused.
    let b = |t: &str| {
        EnvelopeBuilder::new(Producer::MeetingService, t, "2026-11-15T12:00:00Z", route())
            .unwrap()
            .recipient(format!("cfmeet-user:{U}"))
            .dedupe_key("d")
    };
    let wrong = b("account.verification-code.requested")
        .data("ttlMinutes", 10)
        .idempotency_key("otp:verify:1")
        .secret(SealedPlaintext::default().with_token("tok_0123456789abcdefghij"))
        .build_sealed(&key);
    assert_eq!(wrong.unwrap_err(), EnvelopeError::SealedRequired("code"));
    let no_address = b("meeting-service.organization-invitation.email-requested")
        .data("organizationId", U)
        .data("invitationId", U)
        .idempotency_key(format!("org-invitation:{U}:1"))
        .secret(SealedPlaintext::default().with_token("tok_0123456789abcdefghij"))
        .build_sealed(&key);
    assert_eq!(
        no_address.unwrap_err(),
        EnvelopeError::SealedRequired("address")
    );
    // A secret can never be built into an unsealed envelope.
    let no_key = b("account.verification-code.requested")
        .data("ttlMinutes", 10)
        .idempotency_key("otp:verify:1")
        .secret(SealedPlaintext::default().with_code("123456"))
        .build();
    assert!(no_key.is_err());
    // Legacy key formats are mandatory where they exist today.
    let bad_key = b("account.password-reset-code.requested")
        .data("ttlMinutes", 10)
        .idempotency_key("reset-1")
        .secret(SealedPlaintext::default().with_code("123456"))
        .build_sealed(&key);
    assert_eq!(bad_key.unwrap_err(), EnvelopeError::IdempotencyKeyFormat);
}

#[test]
fn size_recipient_and_string_limits_are_enforced() {
    let key = key();
    let mut env = build(
        cf_notification_client::spec("calling.voicemail.ready").unwrap(),
        &key,
    );
    env.recipients = (0..201)
        .map(|_| cf_notification_client::envelope::Recipient {
            reference: format!("cfmeet-user:{U}"),
        })
        .collect();
    assert_eq!(
        validate(&env, None).unwrap_err(),
        EnvelopeError::Invalid("recipients")
    );
    env.recipients.truncate(200);
    assert_eq!(
        validate(&env, None).unwrap_err(),
        EnvelopeError::TooLarge(4096)
    );
    let mut env = build(
        cf_notification_client::spec("calling.voicemail.ready").unwrap(),
        &key,
    );
    env.recipients[0].reference = "cfmeet-user:not-a-uuid".into();
    assert_eq!(
        validate(&env, None).unwrap_err(),
        EnvelopeError::Invalid("recipients")
    );
    let mut env = build(
        cf_notification_client::spec("organizations.invitation.created").unwrap(),
        &key,
    );
    env.data.insert("role".into(), json!("a\u{7}b"));
    assert_eq!(
        validate(&env, None).unwrap_err(),
        EnvelopeError::DataValue("role".into())
    );
    // Unknown data keys (even harmless names) are refused per type.
    let mut env = build(
        cf_notification_client::spec("organizations.invitation.created").unwrap(),
        &key,
    );
    env.data.insert("note".into(), json!("x"));
    assert_eq!(
        validate(&env, None).unwrap_err(),
        EnvelopeError::DataKey("note".into())
    );
}

/// Each counter-example is refused for the reason it was written for (a counter-example
/// refused for another reason proves nothing; this caught `voicemailId`, which contains
/// the forbidden part `email`).
#[test]
fn counter_examples_are_refused_for_their_intended_reason() {
    use EnvelopeError::*;
    let expected: &[(&str, Option<EnvelopeError>)] = &[
        (
            "code-in-data.json",
            Some(DataKey("verificationCode".into())),
        ),
        (
            "control-character-in-data.json",
            Some(DataValue("role".into())),
        ),
        (
            "critical-from-wrong-producer.json",
            Some(ProducerNotAllowed),
        ),
        (
            "critical-priority-on-standard-type.json",
            Some(PriorityMismatch),
        ),
        (
            "email-changed-side-mismatch.json",
            Some(DataValue("side".into())),
        ),
        ("email-in-data.json", Some(DataKey("inviteeEmail".into()))),
        // No `route`: not even an envelope.
        ("missing-route.json", None),
        ("no-recipients.json", Some(Invalid("recipients"))),
        ("operator-alert-to-user.json", Some(Invalid("recipients"))),
        (
            "request-network-in-data.json",
            Some(DataKey("requestNetwork".into())),
        ),
        ("sealed-not-allowed.json", Some(SealedNotAllowed)),
        ("sign-in-code-to-account.json", Some(Invalid("recipients"))),
        (
            "step-up-unknown-purpose.json",
            Some(DataValue("purpose".into())),
        ),
        ("token-in-data.json", Some(DataKey("inviteToken".into()))),
        ("undo-token-in-data.json", Some(DataKey("undoToken".into()))),
        ("unknown-event-type.json", Some(UnknownEventType)),
        ("unsealed-code.json", Some(SealedRequired("code"))),
        (
            "uppercase-secret-key.json",
            Some(DataKey("clientSECRETValue".into())),
        ),
        ("wrong-legacy-key-format.json", Some(IdempotencyKeyFormat)),
    ];
    let fixtures = fixtures("counter-examples");
    assert_eq!(fixtures.len(), expected.len());
    for ((name, value), (want_name, want)) in fixtures.into_iter().zip(expected) {
        assert_eq!(&name, want_name);
        match (serde_json::from_value::<Envelope>(value), want) {
            (Err(_), None) => {}
            (Ok(env), Some(want)) => assert_eq!(&validate(&env, None).unwrap_err(), want, "{name}"),
            (got, want) => panic!("{name}: parsed={} expected={want:?}", got.is_ok()),
        }
    }
}

/// The passwordless account codes (contract 1.1.0): each names exactly one recipient of its
/// kind, carries exactly its sealed fields, and the step-up purpose is a closed enum — in
/// Rust and in the schema.
#[test]
fn passwordless_account_codes_follow_their_catalogue_rules() {
    let v = validator();
    let key = key();
    let conforms = |env: &Envelope| v.is_valid(&serde_json::to_value(env).unwrap());
    let sign_in = build(
        cf_notification_client::spec("account.sign-in-code.requested").unwrap(),
        &key,
    );
    assert!(conforms(&sign_in));
    assert!(sign_in.subject.is_none());
    // An account recipient, a second recipient, or a subject on the sign-in code.
    for refs in [
        vec![format!("cfmeet-user:{U}")],
        vec![
            format!("meeting-email-principal:{U}"),
            "meeting-email-principal:6f1d2c3b-4a59-4e6f-8a7b-9c0d1e2f3a4b".to_string(),
        ],
    ] {
        let mut env = sign_in.clone();
        env.recipients = refs
            .into_iter()
            .map(|reference| cf_notification_client::envelope::Recipient { reference })
            .collect();
        assert_eq!(
            validate(&env, None).unwrap_err(),
            EnvelopeError::Invalid("recipients")
        );
        assert!(!conforms(&env));
    }
    let mut with_subject = sign_in.clone();
    with_subject.subject = Some(cf_notification_client::envelope::Subject {
        kind: "ACCOUNT".into(),
        id: Uuid::parse_str(U).unwrap(),
        version: None,
    });
    assert!(
        !conforms(&with_subject),
        "the sign-in code names no account"
    );
    // The sealed fields are exactly the declared ones.
    let b = |t: &str, r: &str| {
        EnvelopeBuilder::new(Producer::MeetingService, t, "2026-11-15T12:00:00Z", route())
            .unwrap()
            .recipient(r.to_string())
            .dedupe_key("d")
            .data("ttlMinutes", 10)
    };
    let principal = format!("meeting-email-principal:{U}");
    let no_token = b("account.sign-in-code.requested", &principal)
        .data("requestedAt", "2026-11-15T12:00:00Z")
        .idempotency_key("otp:signin:a:b")
        .secret(
            SealedPlaintext::default()
                .with_code("123456")
                .with_address("a@example.com"),
        )
        .build_sealed(&key);
    assert_eq!(
        no_token.unwrap_err(),
        EnvelopeError::SealedRequired("token")
    );
    let no_address = b("account.email-change-code.requested", &principal)
        .idempotency_key("otp:emailchange:a:b")
        .secret(SealedPlaintext::default().with_code("123456"))
        .build_sealed(&key);
    assert_eq!(
        no_address.unwrap_err(),
        EnvelopeError::SealedRequired("address")
    );
    let wrong_key = b("account.email-change-code.requested", &principal)
        .idempotency_key("otp:verify:a:b")
        .secret(
            SealedPlaintext::default()
                .with_code("123456")
                .with_address("a@example.com"),
        )
        .build_sealed(&key);
    assert_eq!(wrong_key.unwrap_err(), EnvelopeError::IdempotencyKeyFormat);
    // The step-up purpose: only the closed values, as strings.
    let account = format!("cfmeet-user:{U}");
    for (purpose, ok) in [
        (json!("DEACTIVATE_ACCOUNT"), true),
        (json!("CHANGE_EMAIL"), true),
        (json!("change_email"), false),
        (json!(1), false),
        (Value::Null, false),
    ] {
        let env = b("account.step-up-code.requested", &account)
            .data("purpose", purpose.clone())
            .idempotency_key("otp:stepup:a:b")
            .subject(Uuid::parse_str(U).unwrap(), None)
            .secret(SealedPlaintext::default().with_code("123456"))
            .build_sealed(&key);
        assert_eq!(env.is_ok(), ok, "{purpose}");
        if let Ok(env) = env {
            assert!(conforms(&env));
        }
    }
    let to_principal = b("account.step-up-code.requested", &principal)
        .data("purpose", "CHANGE_EMAIL")
        .idempotency_key("otp:stepup:a:b")
        .secret(SealedPlaintext::default().with_code("123456"))
        .build_sealed(&key);
    assert_eq!(
        to_principal.unwrap_err(),
        EnvelopeError::Invalid("recipients")
    );
}

/// The account notices (contract 1.3.0): one email principal with its address sealed;
/// what the legacy email shows beyond ids and times (the undo token, the masked new
/// address, the request's client label and network) only in the sealed part, never in
/// data; `side` closed and agreeing with the legacy key — in Rust and in the schema.
#[test]
fn account_notices_follow_their_catalogue_rules() {
    let v = validator();
    let key = key();
    let conforms = |env: &Envelope| v.is_valid(&serde_json::to_value(env).unwrap());
    let principal = format!("meeting-email-principal:{U}");
    let b = |t: &str| {
        EnvelopeBuilder::new(Producer::MeetingService, t, "2026-11-15T12:00:00Z", route())
            .unwrap()
            .recipient(principal.clone())
            .dedupe_key("d")
    };
    let account = Uuid::parse_str(U).unwrap();
    let address = || SealedPlaintext::default().with_address("ada@example.com");

    // Welcome: the address only; an account recipient is refused.
    let welcome = b("account.welcome.requested")
        .subject(account, None)
        .idempotency_key(format!("welcome:{U}"))
        .secret(address())
        .build_sealed(&key)
        .unwrap();
    assert!(conforms(&welcome));
    let mut to_account = welcome.clone();
    to_account.recipients[0].reference = format!("cfmeet-user:{U}");
    assert_eq!(
        validate(&to_account, None).unwrap_err(),
        EnvelopeError::Invalid("recipients")
    );
    assert!(!conforms(&to_account));
    let mut no_subject = welcome.clone();
    no_subject.subject = None;
    assert!(!conforms(&no_subject), "the welcome names its account");
    let with_token = b("account.welcome.requested")
        .subject(account, None)
        .idempotency_key(format!("welcome:{U}"))
        .secret(address().with_token("tok_0123456789abcdefghij"))
        .build_sealed(&key);
    assert_eq!(with_token.unwrap_err(), EnvelopeError::SealedNotAllowed);

    // New sign-in: the request details are optional and sealed; never data.
    for (client, network) in [
        (Some("TestBrowser/1.0"), Some("102.89.x.x")),
        (None, Some("2001:db8:x:x")),
        (Some("curl/8"), None),
        (None, None),
    ] {
        let mut p = address();
        if let Some(c) = client {
            p = p.with_request_client(c);
        }
        if let Some(n) = network {
            p = p.with_request_network(n);
        }
        let env = b("account.security.new-sign-in")
            .subject(account, None)
            .idempotency_key(format!("new-sign-in:{U}"))
            .data("requestedAt", "2026-11-15T12:00:00Z")
            .secret(p)
            .build_sealed(&key)
            .unwrap();
        assert!(conforms(&env));
    }
    for (k, val) in [
        ("requestClient", "TestBrowser/1.0"),
        ("requestNetwork", "102.89.x.x"),
    ] {
        let clear = b("account.security.new-sign-in")
            .subject(account, None)
            .idempotency_key(format!("new-sign-in:{U}"))
            .data("requestedAt", "2026-11-15T12:00:00Z")
            .data(k, val)
            .secret(address())
            .build_sealed(&key);
        assert_eq!(clear.unwrap_err(), EnvelopeError::DataKey(k.into()));
    }
    // A full network address is not a masked one.
    let full = b("account.security.new-sign-in")
        .subject(account, None)
        .idempotency_key(format!("new-sign-in:{U}"))
        .data("requestedAt", "2026-11-15T12:00:00Z")
        .secret(address().with_request_network("102.89.10.11"))
        .build_sealed(&key);
    assert!(full.is_err());

    // Email in use: no subject in the schema (the event never names the account).
    let in_use = b("account.security.email-in-use")
        .idempotency_key("email-in-use:6b86b273:2026111512")
        .secret(address())
        .build_sealed(&key)
        .unwrap();
    assert!(conforms(&in_use));
    let mut named = in_use.clone();
    named.subject = Some(cf_notification_client::envelope::Subject {
        kind: "ACCOUNT".into(),
        id: account,
        version: None,
    });
    assert!(!conforms(&named));

    // Email changed: OLD seals the masked new address (+ the undo token of a self-service
    // change); NEW neither; the side agrees with the key.
    let changed = |side: &str, key_side: &str, p: SealedPlaintext| {
        b("account.security.email-changed")
            .subject(account, None)
            .idempotency_key(format!("email-changed:{key_side}:{U}"))
            .data("changedAt", "2026-11-15T12:00:00Z")
            .data("side", side)
            .secret(p)
            .build_sealed(&key)
    };
    let masked = || address().with_masked_address("n***@example.com");
    for p in [masked().with_token("tok_0123456789abcdefghij"), masked()] {
        let env = changed("OLD", "old", p).unwrap();
        assert!(conforms(&env));
        assert!(!serde_json::to_string(&env).unwrap().contains("n***"));
    }
    assert!(conforms(&changed("NEW", "new", address()).unwrap()));
    assert_eq!(
        changed("OLD", "old", address()).unwrap_err(),
        EnvelopeError::SealedRequired("maskedAddress")
    );
    assert_eq!(
        changed("NEW", "new", masked()).unwrap_err(),
        EnvelopeError::SealedNotAllowed
    );
    assert_eq!(
        changed(
            "NEW",
            "new",
            address().with_token("tok_0123456789abcdefghij")
        )
        .unwrap_err(),
        EnvelopeError::SealedNotAllowed
    );
    assert_eq!(
        changed("NEW", "old", address()).unwrap_err(),
        EnvelopeError::DataValue("side".into())
    );
    assert_eq!(
        changed("BOTH", "old", masked()).unwrap_err(),
        EnvelopeError::DataValue("side".into())
    );
    // A full address is not a masked one.
    assert!(changed(
        "OLD",
        "old",
        address().with_masked_address("nadia@example.com")
    )
    .is_err());
    // The undo token in clear data is refused, whatever the key is called.
    for k in ["undoToken", "token", "undoLink"] {
        let clear = b("account.security.email-changed")
            .subject(account, None)
            .idempotency_key(format!("email-changed:old:{U}"))
            .data("changedAt", "2026-11-15T12:00:00Z")
            .data("side", "OLD")
            .data(k, "tok_0123456789abcdefghij")
            .secret(masked())
            .build_sealed(&key);
        assert!(
            matches!(clear, Err(EnvelopeError::DataKey(_))),
            "{k}: {clear:?}"
        );
    }
    // The sealed plaintext of a notice never shows in Debug.
    let p = masked()
        .with_token("tok_0123456789abcdefghij")
        .with_request_client("TestBrowser/1.0")
        .with_request_network("102.89.x.x");
    let shown = format!("{p:?}");
    for secret in ["ada@", "n***", "tok_", "TestBrowser", "102.89"] {
        assert!(!shown.contains(secret), "{secret}");
    }
}

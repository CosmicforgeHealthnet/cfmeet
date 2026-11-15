//! Conformance in both directions between `cf-notification-client` and the frozen
//! contract `cfmeet.notifications.event` (packages/contracts/notifications/events):
//! the Rust catalogue equals `x-cfmeet-catalogue`; every envelope the builder produces is
//! valid against the JSON Schema; every contract example passes the Rust validator and
//! every counter-example (forbidden keys, unsealed secrets, wrong producer, ...) is refused
//! by both.

use std::{fs, path::PathBuf};

use cf_notification_client::{
    catalogue::{LegacyKey, SealedField, CATALOGUE, CONTROL_EVENT},
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
        "startsAt" | "changedAt" | "expiresAt" => json!("2026-11-20T09:00:00Z"),
        "ttlMinutes" => json!(1440),
        "eventSequence" => json!(42),
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
        LegacyKey::DeviceLinked | LegacyKey::MeetingInvitation => format!("{}{U}", format.prefix()),
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
        for f in spec.sealed {
            match f {
                SealedField::Code => p.code = Some("482913".into()),
                SealedField::Token => p.token = Some("tok_0123456789abcdefghij".into()),
                SealedField::Address => p.address = Some("invitee@example.com".into()),
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
    assert_eq!(CATALOGUE.iter().filter(|s| !s.is_control()).count(), 31);
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
    let all = [
        LegacyKey::OtpVerify,
        LegacyKey::OtpReset,
        LegacyKey::DeviceStepUp,
        LegacyKey::PasswordChanged,
        LegacyKey::DeviceLinked,
        LegacyKey::MeetingInvitation,
        LegacyKey::OrgInvitation,
        LegacyKey::MeetingNotification,
        LegacyKey::DevplatAccess,
    ];
    assert_eq!(patterns.len(), all.len());
    let samples = |f: LegacyKey| {
        vec![
            sample_idempotency_key(f),
            format!("{}", f.prefix()),
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
        ("email-in-data.json", Some(DataKey("inviteeEmail".into()))),
        // No `route`: not even an envelope.
        ("missing-route.json", None),
        ("no-recipients.json", Some(Invalid("recipients"))),
        ("operator-alert-to-user.json", Some(Invalid("recipients"))),
        ("sealed-not-allowed.json", Some(SealedNotAllowed)),
        ("token-in-data.json", Some(DataKey("inviteToken".into()))),
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

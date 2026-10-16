//! Part H §80: retention and minimization — contact erasure after the configured delay
//! (never before, never under a hold), purge of verification events / grants / access
//! audit / relay evidence / outbox / relay-number ledger after their bounds, worker-only
//! deletes, concurrent workers, and no plaintext after an erasure.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use cf_service_auth::{ApplicationEnvironmentKind, CorrelationId, RequestId, ServiceId};
use chrono::Duration;
use common::{at, exec, iso, rows, scalar, Consumer, Harness};
use privacy_relay::{
    clock::Clock,
    contact_vault::{
        keyring::{
            activate_key_version, add_key_version, generate_keyring_json, FileKeyring, KeyPurpose,
            KeyVersion,
        },
        repository,
        resolution::ResolveRequest,
        ContactActor,
    },
    retention::{
        holds::{self, HoldReason, HoldScope},
        worker::RetentionWorker,
        DataClass, RetentionSettings,
    },
    scope::RelayScope,
};
use serde_json::{json, Value};
use uuid::Uuid;

const NUMBER: &str = "+39 02 1234 5678";
const E164: &str = "+390212345678";
const DIGITS: &str = "0212345678";

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

fn scope_of(c: &Consumer) -> RelayScope {
    RelayScope::of_delegated(&c.delegated())
}

fn worker(h: &Harness) -> RetentionWorker {
    RetentionWorker::new(
        h.conn().clone(),
        Arc::new(h.clock.clone()),
        RetentionSettings::default(),
    )
}

fn days(n: i64) -> Duration {
    Duration::days(n)
}

async fn contact_row(
    h: &Harness,
    c: &Consumer,
    id: &str,
) -> privacy_relay::contact_vault::model::ContactRow {
    repository::find(h.conn(), scope_of(c), id.parse().unwrap(), false)
        .await
        .unwrap()
        .unwrap()
}

async fn get_contact(h: &Harness, c: &Consumer, id: &str) -> common::Response {
    h.contact_call(
        Method::GET,
        &format!("/v1/privacy-relay/contacts/{id}"),
        Some(&h.writer(c)),
        None,
    )
    .await
}

async fn disable(h: &Harness, c: &Consumer, id: &str) {
    let current = get_contact(h, c, id).await.json["contact"]["version"].clone();
    let r = h
        .contact_call(
            Method::POST,
            &format!("/v1/privacy-relay/contacts/{id}/disable"),
            Some(&h.writer(c)),
            Some(json!({ "expectedVersion": current })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

/// A verified contact in a PUBLIC_RELAY session: (contact, context, session, principal).
struct Flow {
    contact: String,
    context: String,
    session: String,
    principal: Uuid,
}

async fn flow(h: &Harness, c: &Consumer, number: &str) -> Flow {
    let principal = Uuid::new_v4();
    let contact = h.create_contact(c, number).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    h.verify_contact(c, &contact, "VERIFIED", None).await;
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] = json!(["PUBLIC_RELAY_ALLOWED"]);
    let ctx = h.create_context(c, req).await;
    let context = ctx["contextId"].as_str().unwrap().to_string();
    let session = h
        .create_session(
            c,
            &context,
            json!({ "relayStrategy": "PUBLIC_RELAY_ALLOWED" }),
        )
        .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    Flow {
        contact,
        context,
        session,
        principal,
    }
}

async fn authorize(h: &Harness, c: &Consumer, f: &Flow) -> Value {
    let r = h
        .post(
            "/internal/v1/communication/authorize",
            c,
            json!({ "relaySessionId": f.session, "source": { "principalId": f.principal },
                    "targets": [{ "contactReferenceId": f.contact }], "channel": "VOICE",
                    "route": "PUBLIC_RELAY", "purpose": "CALL_CREATE" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["decision"], "AUTHORIZED", "{}", r.text);
    r.json
}

async fn grant(h: &Harness, c: &Consumer, f: &Flow, decision: &Value) -> (Value, Uuid, Uuid) {
    let (call, leg) = (Uuid::new_v4(), Uuid::new_v4());
    let g = h
        .post(
            &format!("/internal/v1/contacts/{}/resolution-grants", f.contact),
            c,
            json!({ "relaySessionId": f.session, "decisionId": decision["decisionId"], "callId": call,
                    "callLegId": leg, "purpose": "PUBLIC_NETWORK_CALL_LEG" }),
        )
        .await;
    assert_eq!(g.status, StatusCode::CREATED, "{}", g.text);
    (g.json["grant"].clone(), call, leg)
}

async fn resolve(
    h: &Harness,
    c: &Consumer,
    f: &Flow,
    g: &Value,
    call: Uuid,
    leg: Uuid,
) -> common::Response {
    let telephony = h.ids.telephony.clone();
    h.send_as(
        &telephony,
        Method::POST,
        &format!("/internal/v1/contacts/{}/resolve-for-execution", f.contact),
        Some(c),
        Some(
            json!({ "grantId": g["grantId"], "grantSecret": g["grantSecret"], "callId": call,
                     "callLegId": leg, "executionAttemptId": Uuid::new_v4() }),
        ),
    )
    .await
}

#[tokio::test]
async fn a_disabled_contact_is_erased_after_the_delay_and_stays_an_opaque_reference() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let f = flow(&h, &c, NUMBER).await;
    let kept = h.create_contact(&c, "+44 20 7946 0958").await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    disable(&h, &c, &f.contact).await;
    let before = contact_row(&h, &c, &f.contact).await;
    assert!(before.ciphertext.is_some() && !before.is_erased());
    let w = worker(&h);
    // Not before the delay (29 days).
    h.clock.set(at(0) + days(29));
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!(r.contacts_erased, 0);
    assert!(contact_row(&h, &c, &f.contact).await.ciphertext.is_some());
    // After the delay: value gone, reference kept.
    h.clock.set(at(0) + days(31));
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!(r.contacts_erased, 1);
    let erased = contact_row(&h, &c, &f.contact).await;
    assert!(erased.is_erased());
    assert_eq!(
        (
            erased.ciphertext.as_ref(),
            erased.nonce.as_ref(),
            erased.encryption_key_version.as_deref()
        ),
        (None, None, None)
    );
    assert_eq!(erased.masked_display, "+39••••••");
    assert_eq!(erased.version, before.version + 1);
    assert!(erased.sealed().is_err(), "no ciphertext to open");
    // The enabled contact is untouched.
    assert!(contact_row(&h, &c, &kept).await.ciphertext.is_some());
    // Calling-style GET: still resolvable as an opaque, disabled reference without digits.
    let got = get_contact(&h, &c, &f.contact).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.text);
    assert_eq!(got.json["contact"]["contactReference"], f.contact);
    assert_eq!(got.json["contact"]["status"], "DISABLED");
    assert_eq!(got.json["contact"]["maskedDisplay"], "+39••••••");
    assert!(!got.text.contains("5678"), "{}", got.text);
    // Audit: one ERASE row per contact (SYSTEM, no number) and one retention batch row.
    let audit = rows(
        h.conn(),
        &format!("SELECT operation, outcome, actor_kind, purpose FROM contact_access_audit WHERE contact_reference = '{}' AND operation = 'ERASE'", f.contact),
    )
    .await;
    assert_eq!(
        audit,
        vec![vec![
            "ERASE".to_string(),
            "SUCCEEDED".into(),
            "SYSTEM".into(),
            "DATA_RETENTION".into()
        ]]
    );
    assert_eq!(
        rows(
            h.conn(),
            "SELECT data_class, action, batch_count::text, policy, actor, outcome FROM privacy_relay_retention_audit"
        )
        .await,
        vec![vec![
            "PROTECTED_CONTACT_VALUE".to_string(),
            "MINIMIZE".into(),
            "1".into(),
            "CONTACT_VAULT_ERASE_DISABLED_AFTER_DAYS=30".into(),
            "SYSTEM".into(),
            "SUCCEEDED".into()
        ]]
    );
    // Idempotent: a second tick finds nothing.
    assert_eq!(w.retention_tick(500).await.unwrap().contacts_erased, 0);
    assert_eq!(
        contact_row(&h, &c, &f.contact).await.version,
        erased.version
    );
    // An erased contact cannot be updated or re-verified for use; its number can be
    // protected again as a NEW contact (no equality identity survives).
    let r = h
        .contact_call(
            Method::PATCH,
            &format!("/v1/privacy-relay/contacts/{}", f.contact),
            Some(&h.writer(&c)),
            Some(json!({ "expectedVersion": erased.version, "telephoneNumber": NUMBER })),
        )
        .await;
    assert_eq!(r.code(), "CONTACT_DISABLED");
    let again = h.create_contact(&c, NUMBER).await;
    assert_ne!(again["contactReference"], json!(f.contact));
    // Nothing in the database holds the number; the erased row holds no bytes at all.
    let dump = common::dump_database(h.conn()).await;
    assert!(!dump.contains(E164) && !dump.contains(DIGITS));
    assert_eq!(
        scalar(h.conn(), &format!("SELECT count(*) FROM protected_contacts WHERE id = '{}' AND ciphertext IS NULL AND nonce IS NULL AND encryption_key_version IS NULL AND erased_at IS NOT NULL", f.contact)).await,
        1
    );
    // Erasure happens once: the database refuses to un-erase or re-erase.
    assert!(exec(
        h.conn(),
        &format!(
            "UPDATE protected_contacts SET erased_at = now() WHERE id = '{}'",
            f.contact
        )
    )
    .await
    .is_err());
    assert!(exec(h.conn(), &format!("UPDATE protected_contacts SET erased_at = NULL, ciphertext = '\\x00', nonce = '\\x00', encryption_key_version = 'x' WHERE id = '{}'", f.contact)).await.is_err());
}

#[tokio::test]
async fn an_erased_contact_never_resolves_even_with_a_live_grant_and_is_audited() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let f = flow(&h, &c, NUMBER).await;
    let d = authorize(&h, &c, &f).await;
    let (g, call, leg) = grant(&h, &c, &f, &d).await;
    // Race: disable, then a grant request, then the erasure — all within the grant's TTL.
    disable(&h, &c, &f.contact).await;
    let denied = h
        .post(
            &format!("/internal/v1/contacts/{}/resolution-grants", f.contact),
            &c,
            json!({ "relaySessionId": f.session, "decisionId": d["decisionId"], "callId": Uuid::new_v4(),
                    "callLegId": Uuid::new_v4(), "purpose": "PUBLIC_NETWORK_CALL_LEG" }),
        )
        .await;
    assert_eq!(denied.code(), "CONTACT_DISABLED");
    let now = h.clock.now();
    let report =
        privacy_relay::contact_vault::service::erase_disabled_contacts(h.conn(), now, now, 10)
            .await
            .unwrap();
    assert_eq!(report.erased, vec![f.contact.parse::<Uuid>().unwrap()]);
    // The still-live grant yields no plaintext: DENIED, audited as CONTACT_ERASED.
    let r = resolve(&h, &c, &f, &g, call, leg).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.code(), "CONTACT_DISABLED");
    assert!(!r.text.contains(E164) && !r.text.contains(DIGITS));
    assert_eq!(
        scalar(h.conn(), &format!("SELECT count(*) FROM contact_access_audit WHERE contact_reference = '{}' AND operation = 'RESOLVE_FOR_EXECUTION' AND outcome = 'DENIED' AND reason_code = 'CONTACT_ERASED'", f.contact)).await,
        1
    );
    // Through the library too (a service actor): the same refusal.
    let actor = ContactActor::service(
        scope_of(&c),
        ServiceId::telephony(),
        None,
        RequestId::generate(),
        CorrelationId::generate(),
    );
    let request: ResolveRequest = serde_json::from_value(json!({
        "grantId": g["grantId"], "grantSecret": g["grantSecret"], "callId": call, "callLegId": leg,
        "executionAttemptId": Uuid::new_v4() }))
    .unwrap();
    let err = h
        .contacts
        .resolve_for_execution(&actor, f.contact.parse().unwrap(), request)
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CONTACT_DISABLED");
    assert!(!format!("{err:?}").contains(DIGITS));
    let dump = common::dump_database(h.conn()).await;
    assert!(!dump.contains(E164) && !dump.contains(DIGITS));
    let _ = f.context;
}

#[tokio::test]
async fn held_contacts_are_not_erased_and_a_cross_scope_hold_does_not_protect() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (a, b) = (consumer(), consumer());
    let held = h.create_contact(&a, NUMBER).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    let other_a = h.create_contact(&a, "+44 20 7946 0958").await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    let of_b = h.create_contact(&b, NUMBER).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    for (c, id) in [(&a, &held), (&a, &other_a), (&b, &of_b)] {
        disable(&h, c, id).await;
    }
    let now = h.clock.now();
    // A hold on the resource, and a hold on tenant B's whole scope for every class.
    let hold_resource = holds::place(
        h.conn(),
        &HoldScope::resource(held.parse().unwrap()).class(DataClass::ProtectedContactValue),
        HoldReason::Dispute,
        "operator-1",
        now,
    )
    .await
    .unwrap();
    let hold_scope = holds::place(
        h.conn(),
        &HoldScope::scope(scope_of(&b)),
        HoldReason::SecurityInvestigation,
        "operator-1",
        now,
    )
    .await
    .unwrap();
    // Invalid operator references and partial scopes are refused.
    assert!(holds::place(
        h.conn(),
        &HoldScope::everything(),
        HoldReason::Other,
        "bad operator!",
        now
    )
    .await
    .is_err());
    assert!(holds::place(
        h.conn(),
        &HoldScope {
            data_class: None,
            resource_id: None,
            tenant_id: None,
            application_id: Some(Uuid::new_v4()),
            environment_id: None
        },
        HoldReason::Other,
        "operator-1",
        now
    )
    .await
    .is_err());
    h.clock.set(at(0) + days(31));
    let w = worker(&h);
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!(r.contacts_erased, 1, "only the unheld contact of tenant A");
    assert!(contact_row(&h, &a, &held).await.ciphertext.is_some());
    assert!(contact_row(&h, &b, &of_b).await.ciphertext.is_some());
    assert!(contact_row(&h, &a, &other_a).await.is_erased());
    // Holds are listed, append-only and audited.
    let active = holds::list(h.conn(), true).await.unwrap();
    assert_eq!(active.len(), 2);
    assert!(exec(h.conn(), "DELETE FROM privacy_relay_retention_holds")
        .await
        .is_err());
    assert!(exec(
        h.conn(),
        "UPDATE privacy_relay_retention_holds SET reason_code = 'OTHER'"
    )
    .await
    .is_err());
    assert!(exec(h.conn(), "DELETE FROM privacy_relay_retention_audit")
        .await
        .is_err());
    assert!(exec(h.conn(), "TRUNCATE privacy_relay_retention_audit")
        .await
        .is_err());
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM privacy_relay_retention_audit WHERE action = 'HOLD' AND actor = 'OPERATOR'").await,
        2
    );
    // Released holds protect nothing any more.
    assert!(holds::release(h.conn(), hold_resource, h.clock.now())
        .await
        .unwrap());
    assert!(
        !holds::release(h.conn(), hold_resource, h.clock.now())
            .await
            .unwrap(),
        "idempotent"
    );
    assert!(holds::release(h.conn(), hold_scope, h.clock.now())
        .await
        .unwrap());
    assert_eq!(holds::list(h.conn(), true).await.unwrap().len(), 0);
    assert_eq!(holds::list(h.conn(), false).await.unwrap().len(), 2);
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!(r.contacts_erased, 2);
    assert!(contact_row(&h, &a, &held).await.is_erased());
    assert!(contact_row(&h, &b, &of_b).await.is_erased());
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM privacy_relay_retention_audit WHERE action = 'RELEASE'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn evidence_purges_after_its_bound_and_not_before_and_only_the_worker_may_delete() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let f = flow(&h, &c, NUMBER).await;
    h.add_relay_number("+44 20 7946 0500", "pb-ret").await;
    assert_eq!(
        h.assign_relay_number(&c, &f.session).await.status,
        StatusCode::CREATED
    );
    let d = authorize(&h, &c, &f).await;
    let (g, call, leg) = grant(&h, &c, &f, &d).await;
    let r = resolve(&h, &c, &f, &g, call, leg).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // End the authority: the context is REVOKED (terminal), enforcement applied by the
    // fake Calling, outbox rows published (marked here: no publisher in the harness).
    let v = h.version_of(&c, &f.context).await;
    assert_eq!(
        h.lifecycle(&c, &f.context, "revoke", v).await.status,
        StatusCode::OK
    );
    // The relay number's cooldown completes (the assignment reaches its terminal state).
    h.clock.advance(Duration::hours(2));
    assert_eq!(h.relay.complete_relay_cooldowns(100).await.unwrap(), 1);
    assert_eq!(scalar(h.conn(), "SELECT count(*) FROM relay_number_assignments WHERE status NOT IN ('ACTIVE', 'COOLDOWN')").await, 1);
    let now = h.clock.now();
    exec(h.conn(), &format!("UPDATE privacy_relay_outbox SET status = 'PUBLISHED', published_at = '{}', updated_at = '{0}'", iso(now))).await.unwrap();
    async fn counts(h: &Harness) -> (i64, i64, i64, i64, i64, i64) {
        (
            scalar(h.conn(), "SELECT count(*) FROM contact_verification_events").await,
            scalar(h.conn(), "SELECT count(*) FROM contact_resolution_grants").await,
            scalar(h.conn(), "SELECT count(*) FROM contact_access_audit").await,
            scalar(h.conn(), "SELECT count(*) FROM communication_contexts").await,
            scalar(h.conn(), "SELECT count(*) FROM privacy_relay_outbox").await,
            scalar(h.conn(), "SELECT count(*) FROM relay_number_events").await,
        )
    }
    let (ve, gr, aa, ctx, ob, ne) = counts(&h).await;
    assert!(
        ve >= 1 && gr == 1 && aa >= 3 && ctx == 1 && ob >= 3 && ne >= 2,
        "{:?}",
        (ve, gr, aa, ctx, ob, ne)
    );
    // Plain deletes (and TRUNCATE) are refused outside a retention transaction.
    for sql in [
        "DELETE FROM contact_verification_events",
        "DELETE FROM contact_resolution_grants",
        "DELETE FROM contact_access_audit",
        "DELETE FROM relay_audit_events",
        "DELETE FROM relay_authorization_decisions",
        "DELETE FROM privacy_relay_outbox",
        "DELETE FROM relay_number_events",
        "DELETE FROM communication_contexts",
        "DELETE FROM privacy_relay_sessions",
        "TRUNCATE contact_access_audit",
        "TRUNCATE relay_audit_events",
        "TRUNCATE contact_verification_events",
        "TRUNCATE relay_authorization_decisions",
        "TRUNCATE protected_contacts",
    ] {
        assert!(exec(h.conn(), sql).await.is_err(), "{sql}");
    }
    assert_eq!(counts(&h).await, (ve, gr, aa, ctx, ob, ne));
    let w = worker(&h);
    // Day 29: nothing ages out yet (the outbox bound is 30 days).
    h.clock.set(now + days(29));
    assert_eq!(w.retention_tick(500).await.unwrap().total(), 0);
    assert_eq!(counts(&h).await, (ve, gr, aa, ctx, ob, ne));
    // Day 31: published outbox rows only.
    h.clock.set(now + days(31));
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!((r.outbox_deleted as i64, r.total() as i64), (ob, ob));
    assert_eq!(counts(&h).await, (ve, gr, aa, ctx, 0, ne));
    // Day 91: the consumed grant.
    h.clock.set(now + days(91));
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!((r.grants_deleted, r.total()), (1, 1));
    // Day 366: verification events and the relay-number ledger.
    h.clock.set(now + days(364));
    assert_eq!(w.retention_tick(500).await.unwrap().total(), 0);
    h.clock.set(now + days(366));
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!(r.verification_events_deleted as i64, ve);
    assert_eq!(r.number_events_deleted as i64, ne);
    assert_eq!(r.total() as i64, ve + ne);
    // Day 729: the context, its evidence and the access audit are still there.
    h.clock.set(now + days(729));
    assert_eq!(w.retention_tick(500).await.unwrap().total(), 0);
    assert_eq!(counts(&h).await, (0, 0, aa, 1, 0, 0));
    // A hold on the context keeps everything of it.
    let hold = holds::place(
        h.conn(),
        &HoldScope::resource(f.context.parse().unwrap()).class(DataClass::RelayEvidence),
        HoldReason::PolicyReview,
        "operator-2",
        h.clock.now(),
    )
    .await
    .unwrap();
    h.clock.set(now + days(731));
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!(r.contexts_purged, 0);
    assert!(
        r.access_audit_deleted as i64 >= 1,
        "the audit of this run's age is not held"
    );
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM communication_contexts").await,
        1
    );
    holds::release(h.conn(), hold, h.clock.now()).await.unwrap();
    // Day 731, no hold: the context and every child row are gone in one transaction.
    let r = w.retention_tick(500).await.unwrap();
    assert_eq!(r.contexts_purged, 1);
    for table in [
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
        "privacy_relay_enforcement_jobs",
        "privacy_relay_outbox",
        "relay_number_assignments",
        "relay_inbound_resolutions",
        "contact_resolution_grants",
        "privacy_relay_commands",
    ] {
        assert_eq!(
            scalar(h.conn(), &format!("SELECT count(*) FROM {table}")).await,
            0,
            "{table}"
        );
    }
    // The inventory (platform-owned) and the protected contact stay.
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM relay_number_inventory").await,
        1
    );
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM protected_contacts").await,
        1
    );
    // The access audit older than its bound is gone; the retention audit tells what happened.
    assert_eq!(
        scalar(
            h.conn(),
            &format!(
                "SELECT count(*) FROM contact_access_audit WHERE created_at <= '{}'",
                iso(now)
            )
        )
        .await,
        0
    );
    let classes = rows(
        h.conn(),
        "SELECT DISTINCT data_class FROM privacy_relay_retention_audit WHERE actor = 'SYSTEM' ORDER BY data_class",
    )
    .await;
    assert_eq!(
        classes,
        vec![
            vec!["CONTACT_ACCESS_AUDIT".to_string()],
            vec!["CONTACT_RESOLUTION_GRANTS".to_string()],
            vec!["CONTACT_VERIFICATION_EVENTS".to_string()],
            vec!["RELAY_EVIDENCE".to_string()],
            vec!["RELAY_NUMBER_EVENTS".to_string()],
            vec!["RELAY_OUTBOX".to_string()],
        ]
    );
    // (The platform-owned relay number itself stays in the inventory; it is no contact.)
    let dump = common::dump_database(h.conn()).await;
    assert!(!dump.contains(E164) && !dump.contains(DIGITS));
    // Everything left is idempotent.
    assert_eq!(w.retention_tick(500).await.unwrap().total(), 0);
}

#[tokio::test]
async fn a_live_context_and_an_unfinished_one_are_never_purged() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let f = flow(&h, &c, NUMBER).await;
    // A second, revoked context whose enforcement is still PENDING (Calling unreachable).
    h.calling.fail_with(Some(
        privacy_relay::enforcement::DeliveryFailure::Unavailable,
    ));
    let other = flow(&h, &c, "+44 20 7946 0958").await;
    let v = h.version_of(&c, &other.context).await;
    assert_eq!(
        h.lifecycle(&c, &other.context, "revoke", v).await.status,
        StatusCode::OK
    );
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE status = 'PENDING'"
        )
        .await,
        1
    );
    h.clock.set(at(0) + days(800));
    let r = worker(&h).retention_tick(500).await.unwrap();
    assert_eq!(
        r.contexts_purged, 0,
        "an ACTIVE context and a context with pending enforcement stay"
    );
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM communication_contexts").await,
        2
    );
    let _ = (f.context, other.session);
}

#[tokio::test]
async fn concurrent_workers_erase_each_contact_exactly_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let mut ids = Vec::new();
    for i in 0..10 {
        let id = h.create_contact(&c, &format!("+39 02 1234 56{i:02}")).await["contactReference"]
            .as_str()
            .unwrap()
            .to_string();
        disable(&h, &c, &id).await;
        ids.push(id);
    }
    h.clock.set(at(0) + days(31));
    let (w1, w2) = (worker(&h), worker(&h));
    let (a, b) = tokio::join!(w1.retention_tick(3), w2.retention_tick(3));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(
        a.contacts_erased + b.contacts_erased,
        6,
        "SKIP LOCKED: disjoint batches"
    );
    let (a, b) = tokio::join!(w1.retention_tick(500), w2.retention_tick(500));
    assert_eq!(a.unwrap().contacts_erased + b.unwrap().contacts_erased, 4);
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM protected_contacts WHERE erased_at IS NOT NULL"
        )
        .await,
        10
    );
    assert_eq!(
        scalar(
            h.conn(),
            "SELECT count(*) FROM contact_access_audit WHERE operation = 'ERASE'"
        )
        .await,
        10,
        "one ERASE row per contact"
    );
    assert_eq!(
        scalar(h.conn(), "SELECT coalesce(sum(batch_count), 0)::bigint FROM privacy_relay_retention_audit WHERE data_class = 'PROTECTED_CONTACT_VALUE'").await,
        10
    );
}

#[tokio::test]
async fn rotation_skips_erased_rows_and_they_reference_no_key_version() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let v1 = KeyVersion::parse("ck-v1").unwrap();
    let v2 = KeyVersion::parse("ck-v2").unwrap();
    let doc1 = generate_keyring_json(KeyPurpose::Encryption, &v1);
    let base = h.vault_keys.clone();
    let keys = |doc: &str| {
        base.with_encryption(Arc::new(
            FileKeyring::from_json(doc, KeyPurpose::Encryption).unwrap(),
        ))
        .unwrap()
    };
    h.set_vault_keys(keys(&doc1));
    let c = consumer();
    let erased = h.create_contact(&c, NUMBER).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    let live = h.create_contact(&c, "+44 20 7946 0958").await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    disable(&h, &c, &erased).await;
    h.clock.set(at(0) + days(31));
    assert_eq!(
        worker(&h)
            .retention_tick(500)
            .await
            .unwrap()
            .contacts_erased,
        1
    );
    // Rotate to v2: only the live row is re-encrypted; the erased row is never selected.
    let doc2 = activate_key_version(
        &add_key_version(&doc1, KeyPurpose::Encryption, &v2).unwrap(),
        KeyPurpose::Encryption,
        &v2,
    )
    .unwrap();
    h.set_vault_keys(keys(&doc2));
    assert_eq!(
        h.contacts.key_version_counts().await.unwrap(),
        vec![("ck-v1".into(), 1)]
    );
    let r = h.contacts.rewrap_batch(10).await.unwrap();
    assert_eq!((r.rewrapped, r.failed, r.remaining), (1, 0, 0));
    assert_eq!(
        h.contacts.key_version_counts().await.unwrap(),
        vec![("ck-v2".into(), 1)]
    );
    assert!(contact_row(&h, &c, &erased).await.is_erased());
    assert_eq!(
        contact_row(&h, &c, &live)
            .await
            .encryption_key_version
            .as_deref(),
        Some("ck-v2")
    );
    // The erased row references nothing: v1 can be retired, v2 is still referenced.
    let status = h.contacts.rotation_status().await.unwrap();
    assert_eq!(status.referenced_encryption, vec!["ck-v2".to_string()]);
    assert!(status.unreadable_encryption.is_empty());
    h.contacts.run_sweep().await.unwrap();
}

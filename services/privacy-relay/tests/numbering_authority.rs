//! Part IV Part P §§119–123: the relay-number authority moves to Numbering without
//! changing what Privacy Relay owns.
//!
//! * LOCAL (default) behaves exactly as Part III; NUMBERING lends numbers only after the
//!   idempotent backfill imported the inventory and adopted every ACTIVE mapping, and the
//!   compatibility check proved Numbering's view equals Privacy Relay's (same assignment,
//!   same generation) — until then new assignments are refused (fail closed);
//! * after cutover every new assignment holds a Numbering lease issued to that very
//!   assignment; a number Numbering refuses is skipped; Numbering unavailable refuses the
//!   assignment; session ids, assignment ids, generations and the inbound mapping are
//!   unchanged (pre- / post-cutover routing equivalence);
//! * an assignment's end makes its lease RELEASE_PENDING atomically and the sweep releases
//!   it at Numbering — also after a rollback to LOCAL.
//!
//! Numbering is an in-memory authority here (the real one, over HTTP, is certified by
//! Numbering's `communications_e2e` suite).

mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind;
use chrono::{DateTime, Utc};
use common::{at, iso, rows, Consumer, Harness};
use privacy_relay::{
    relay_numbers::authority::{
        AuthorityError, AuthorityMode, GrantedLease, ImportRow, NumberAuthority,
        NumberingResolution,
    },
    state::AppState,
};
use serde_json::{json, Value};
use uuid::Uuid;

const RELAY_A: &str = "+44 20 7946 0500";
const RELAY_B: &str = "+44 20 7946 0501";
const RELAY_C: &str = "+44 20 7946 0502";
const CONTACT: &str = "+44 20 7946 0123";

#[derive(Default)]
struct Fake {
    imported: Mutex<BTreeMap<Uuid, String>>,
    /// relay ref → (lease id, holder, generation, status)
    leases: Mutex<BTreeMap<Uuid, (Uuid, Uuid, i64, String)>>,
    refused: Mutex<BTreeSet<Uuid>>,
    unavailable: Mutex<bool>,
    released: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl NumberAuthority for Fake {
    async fn import(&self, row: &ImportRow) -> Result<(), AuthorityError> {
        if *self.unavailable.lock().unwrap() {
            return Err(AuthorityError::Unavailable);
        }
        self.imported
            .lock()
            .unwrap()
            .insert(row.relay_inventory_ref, row.e164.clone());
        Ok(())
    }
    async fn lease(
        &self,
        r: Uuid,
        holder: Uuid,
        generation: i64,
        _expires_at: DateTime<Utc>,
    ) -> Result<GrantedLease, AuthorityError> {
        if *self.unavailable.lock().unwrap() {
            return Err(AuthorityError::Unavailable);
        }
        if self.refused.lock().unwrap().contains(&r)
            || !self.imported.lock().unwrap().contains_key(&r)
        {
            return Err(AuthorityError::Refused("RELAY_NUMBER_NOT_LEASABLE".into()));
        }
        let mut leases = self.leases.lock().unwrap();
        match leases.get(&r) {
            Some((id, h, _, s)) if s == "ACTIVE" && *h == holder => Ok(GrantedLease {
                lease_id: *id,
                generation,
            }),
            Some((_, _, _, s)) if s == "ACTIVE" => {
                Err(AuthorityError::Refused("RELAY_NUMBER_LEASED".into()))
            }
            _ => {
                let id = Uuid::new_v4();
                leases.insert(r, (id, holder, generation, "ACTIVE".into()));
                Ok(GrantedLease {
                    lease_id: id,
                    generation,
                })
            }
        }
    }
    async fn release(&self, lease_id: Uuid, _reason: &str) -> Result<(), AuthorityError> {
        if *self.unavailable.lock().unwrap() {
            return Err(AuthorityError::Unavailable);
        }
        for v in self.leases.lock().unwrap().values_mut() {
            if v.0 == lease_id {
                v.3 = "RELEASED".into();
            }
        }
        self.released.lock().unwrap().push(lease_id);
        Ok(())
    }
    async fn resolve(&self, r: Uuid) -> Result<NumberingResolution, AuthorityError> {
        if *self.unavailable.lock().unwrap() {
            return Err(AuthorityError::Unavailable);
        }
        if !self.imported.lock().unwrap().contains_key(&r) {
            return Err(AuthorityError::Refused("NOT_FOUND".into()));
        }
        let lease = self
            .leases
            .lock()
            .unwrap()
            .get(&r)
            .filter(|l| l.3 == "ACTIVE")
            .cloned();
        Ok(NumberingResolution {
            lifecycle_state: "AVAILABLE".into(),
            leasable: lease.is_none(),
            lease,
        })
    }
}

fn consumer() -> Consumer {
    Consumer::new(ApplicationEnvironmentKind::Production)
}

/// Re-deploys the harness's service in `mode` with `authority`.
fn deploy(h: &mut Harness, mode: AuthorityMode, authority: Option<Arc<Fake>>) {
    let relay = h
        .relay
        .clone()
        .with_number_authority(mode, authority.map(|a| a as Arc<dyn NumberAuthority>));
    h.relay = relay.clone();
    h.state = AppState::new(relay, h.ids.runtime.clone())
        .with_contact_vault(h.contacts.clone(), Some(h.ids.contact_verifier()));
}

/// A CALLBACK_RELAY context with a VERIFIED protected contact and a session; returns the
/// session id.
async fn session(h: &Harness, c: &Consumer) -> String {
    let principal = Uuid::new_v4();
    let contact = h.create_contact(c, CONTACT).await["contactReference"]
        .as_str()
        .unwrap()
        .to_string();
    let state = h
        .contact_call(
            axum::http::Method::GET,
            &format!("/v1/privacy-relay/contacts/{contact}"),
            Some(&h.writer(c)),
            None,
        )
        .await;
    if state.json["contact"]["verificationState"] != "VERIFIED" {
        h.verify_contact(c, &contact, "VERIFIED", None).await;
    }
    let mut req = common::context_request(&[principal], "BIDIRECTIONAL", &[]);
    req["participants"] = json!([{ "principalId": principal }, { "contactReferenceId": contact }]);
    req["allowedRelayStrategies"] =
        json!(["APP_ONLY", "PUBLIC_RELAY_ALLOWED", "CALLBACK_RELAY_ALLOWED"]);
    req["startsAt"] = json!(iso(at(0)));
    req["expiresAt"] = json!(iso(at(600)));
    let ctx = h.create_context(c, req).await;
    h.create_session(
        c,
        ctx["contextId"].as_str().unwrap(),
        json!({ "relayStrategy": "CALLBACK_RELAY_ALLOWED" }),
    )
    .await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn inbound(h: &Harness, number: &str) -> Value {
    let r = h
        .telephony(
            "/internal/v1/relay-routing/resolve-inbound",
            json!({
                "calledRelayIdentity": number,
                "providerEventReference": format!("evt-{}", Uuid::new_v4().simple()),
                "providerObservedAt": iso(h.relay_now()),
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{number}: {}", r.text);
    let mut res = r.json["resolution"].clone();
    // Per-call fields.
    res["decisionId"] = Value::Null;
    res
}

/// The number (id, E.164) an assignment holds.
async fn held(h: &Harness, assignment: &Value) -> (Uuid, String) {
    let r = rows(
        h.conn(),
        &format!(
            "SELECT i.id::text, i.canonical_public_e164 FROM relay_number_assignments a \
               JOIN relay_number_inventory i ON i.id = a.relay_number_id WHERE a.id = '{}'",
            assignment["assignmentId"].as_str().unwrap()
        ),
    )
    .await;
    (Uuid::parse_str(&r[0][0]).unwrap(), r[0][1].clone())
}

trait Now {
    fn relay_now(&self) -> DateTime<Utc>;
}

impl Now for Harness {
    fn relay_now(&self) -> DateTime<Utc> {
        use privacy_relay::clock::Clock;
        self.clock.now()
    }
}

#[tokio::test]
async fn cutover_needs_a_verified_backfill_and_preserves_every_existing_mapping() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    h.add_relay_number(RELAY_A, "pb-cut-a").await;
    h.add_relay_number(RELAY_B, "pb-cut-b").await;
    h.clock.set(at(1));
    // Part III behaviour (LOCAL): an assignment before cutover.
    let s1 = session(&h, &c).await;
    let before = h.assign_relay_number(&c, &s1).await;
    assert_eq!(before.status, StatusCode::CREATED, "{}", before.text);
    let a1 = before.json["assignment"].clone();
    let (a, first) = held(&h, &a1).await;
    h.clock.set(at(5));
    let routed_before = inbound(&h, &first).await;
    assert_eq!(routed_before["outcome"], "RESOLVED");

    // NUMBERING, not yet verified: nothing new is lent (fail closed); the existing
    // mapping keeps routing.
    let fake = Arc::new(Fake::default());
    deploy(&mut h, AuthorityMode::Numbering, Some(fake.clone()));
    let s2 = session(&h, &c).await;
    let refused = h.assign_relay_number(&c, &s2).await;
    assert_eq!(
        refused.code(),
        "RELAY_NUMBER_UNAVAILABLE",
        "{}",
        refused.text
    );
    assert_eq!(inbound(&h, &first).await, routed_before);

    // Numbering unavailable during preparation: still closed.
    *fake.unavailable.lock().unwrap() = true;
    let report = h.relay.prepare_numbering_cutover().await.unwrap();
    assert!(!report.compatible());
    assert!(!h.relay.numbering_cutover_verified());
    *fake.unavailable.lock().unwrap() = false;

    // The backfill imports every number and adopts the live mapping (its own assignment
    // id and generation); the check proves equality; a re-run changes nothing.
    let report = h.relay.prepare_numbering_cutover().await.unwrap();
    assert!(report.compatible(), "{report:?}");
    assert_eq!((report.active_assignments, report.matching_leases), (1, 1));
    assert_eq!(fake.imported.lock().unwrap().len(), 2);
    let (_, holder, generation, _) = fake.leases.lock().unwrap().get(&a).cloned().unwrap();
    assert_eq!(holder.to_string(), a1["assignmentId"].as_str().unwrap());
    assert_eq!(generation, a1["assignmentGeneration"].as_i64().unwrap());
    let again = h.relay.backfill_numbering().await.unwrap();
    assert_eq!((again.adopted, again.already_leased), (0, 1));

    // Post-cutover: the next assignment is leased to itself; ids, binding and inbound
    // routing of the pre-cutover mapping are exactly as before.
    let after = h.assign_relay_number(&c, &s2).await;
    assert_eq!(after.status, StatusCode::CREATED, "{}", after.text);
    let a2 = &after.json["assignment"];
    let leased: Vec<Vec<String>> = rows(
        h.conn(),
        "SELECT assignment_id::text, status FROM relay_number_leases ORDER BY acquired_at",
    )
    .await;
    assert_eq!(leased.len(), 2);
    assert!(leased
        .iter()
        .any(|r| r[0] == a2["assignmentId"].as_str().unwrap() && r[1] == "ACTIVE"));
    assert_eq!(inbound(&h, &first).await, routed_before);
    let (_, second) = held(&h, a2).await;
    assert_ne!(second, first);
    h.clock.set(at(6));
    let routed_b = inbound(&h, &second).await;
    assert_eq!(routed_b["relaySessionId"].as_str(), Some(s2.as_str()));
    let s = h.get(&format!("/internal/v1/sessions/{s1}"), &c).await;
    assert_eq!(s.json["relayIdentityBindingRef"], a1["assignmentId"]);
    // The replay of an existing session's assignment needs nothing from Numbering.
    *fake.unavailable.lock().unwrap() = true;
    let replay = h.assign_relay_number(&c, &s1).await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(
        replay.json["assignment"]["assignmentId"],
        a1["assignmentId"]
    );
    // A new assignment while Numbering is unavailable: refused, nothing half-done.
    h.add_relay_number(RELAY_C, "pb-cut-c").await;
    let s3 = session(&h, &c).await;
    let r = h.assign_relay_number(&c, &s3).await;
    assert_eq!(r.code(), "SERVICE_UNAVAILABLE", "{}", r.text);
    assert_eq!(
        h.count("SELECT count(*) FROM relay_number_assignments")
            .await,
        2
    );
}

#[tokio::test]
async fn refused_numbers_are_skipped_and_ended_leases_are_released_durably_even_after_rollback() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let c = consumer();
    let a = h.add_relay_number(RELAY_A, "pb-rel-a").await;
    let b = h.add_relay_number(RELAY_B, "pb-rel-b").await;
    h.clock.set(at(1));
    let fake = Arc::new(Fake::default());
    deploy(&mut h, AuthorityMode::Numbering, Some(fake.clone()));
    assert!(h
        .relay
        .prepare_numbering_cutover()
        .await
        .unwrap()
        .compatible());
    // Numbering took RELAY_A out of service: the allocation moves on to RELAY_B.
    fake.refused.lock().unwrap().insert(a);
    let s1 = session(&h, &c).await;
    let r = h.assign_relay_number(&c, &s1).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(held(&h, &r.json["assignment"]).await.0, b);
    h.clock.set(at(5));
    assert_eq!(
        inbound(&h, RELAY_B).await["relaySessionId"].as_str(),
        Some(s1.as_str())
    );
    let s2 = session(&h, &c).await;
    assert_eq!(
        h.assign_relay_number(&c, &s2).await.code(),
        "RELAY_NUMBER_UNAVAILABLE"
    );
    // The assignment ends: its lease is RELEASE_PENDING in the same transaction.
    let released = h
        .post(
            &format!("/internal/v1/sessions/{s1}/relay-number-assignment/release"),
            &c,
            json!({}),
        )
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.text);
    let status = |h: &Harness| {
        let conn = h.conn().clone();
        async move { rows(&conn, "SELECT status FROM relay_number_leases").await[0][0].clone() }
    };
    assert_eq!(status(&h).await, "RELEASE_PENDING");
    // Numbering unavailable: retried, never dropped.
    *fake.unavailable.lock().unwrap() = true;
    h.relay.run_sweep().await.unwrap();
    assert_eq!(status(&h).await, "RELEASE_PENDING");
    // Rollback to LOCAL: the sweep still releases what was leased.
    *fake.unavailable.lock().unwrap() = false;
    deploy(&mut h, AuthorityMode::Local, Some(fake.clone()));
    let report = h.relay.run_sweep().await.unwrap();
    assert_eq!(report.numbering_leases_released, 1);
    assert_eq!(status(&h).await, "RELEASED");
    assert_eq!(fake.released.lock().unwrap().len(), 1);
    // LOCAL lends without Numbering again (Part III behaviour): RELAY_A, which Numbering
    // had refused, is Privacy Relay's own decision once more.
    *fake.unavailable.lock().unwrap() = true;
    let r = h.assign_relay_number(&c, &s2).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        inbound(&h, RELAY_A).await["relaySessionId"].as_str(),
        Some(s2.as_str())
    );
    assert_eq!(h.count("SELECT count(*) FROM relay_number_leases").await, 1);
    // The lease ledger is append-only in substance.
    assert!(common::exec(h.conn(), "DELETE FROM relay_number_leases")
        .await
        .is_err());
}

//! ADR 0068 phase A4: owner-delivered organization exports.
//!
//! The admin bridge's `organizations.organization.export` (R4, two people,
//! `Idempotency-Key`, `organization_admin_operations`) builds the same document as the
//! existing export, keeps it encrypted for 7 days and queues one single-use download link
//! per ACTIVE OWNER (a notice whose token only travels sealed). HQ gets a reference only;
//! the owner downloads once after sign-in; expiry and purge leave references only. The
//! bridge never receives the document from the operator export route either.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use common::{s, Harness};
use organizations::{
    exports::{self, notice_event_id, ExportDelivery, ExportNotice, ExportNotices, ExportVault},
    notification_publications::{IngestResult, NotificationEventSink, PublishFailure},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const STAFF: &str = "staff-0001";
const APPROVER: &str = "staff-0002";
const INVITEE: &str = "invitee.person@example.com";

fn command(org: &str) -> String {
    format!("/internal/v1/admin/organizations/{org}/exports")
}
fn list(org: &str) -> String {
    format!("/internal/v1/operator/organizations/{org}/exports")
}
fn detail(org: &str, export: &str) -> String {
    format!("/internal/v1/operator/organizations/{org}/exports/{export}")
}
fn download(org: &str, export: &str) -> String {
    format!("/internal/v1/organizations/{org}/exports/{export}/download")
}
fn operation(key: &str) -> String {
    format!("/internal/v1/operator/operations/{key}")
}
fn admin_export(org: &str) -> String {
    format!("/internal/v1/admin/organizations/{org}/export")
}

fn key() -> String {
    Uuid::new_v4().to_string()
}

/// The bridge's export command for `org` (fresh key, approved).
async fn send(h: &Harness, org: &str) -> common::Response {
    h.by(&h.peers.bridge)
        .operator(STAFF)
        .approved(APPROVER)
        .idem(&key())
        .post(&command(org), json!({}))
        .await
}

/// The notice builder of the tests: records each link's token (as only the Notification
/// Service would see it, sealed) and returns a stand-in envelope.
#[derive(Default)]
struct Notices {
    refuse: bool,
    /// (delivery id, recipient, token).
    sent: Mutex<Vec<(Uuid, Uuid, String)>>,
}

impl Notices {
    fn token_for(&self, user: Uuid) -> String {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(_, r, _)| *r == user)
            .map(|(_, _, t)| t.clone())
            .expect("a link for this owner")
    }
}

impl ExportNotices for Notices {
    fn available(&self) -> Result<(), &'static str> {
        if self.refuse {
            Err("NOT_IN_CATALOGUE")
        } else {
            Ok(())
        }
    }

    fn notice(&self, n: &ExportNotice<'_>) -> Result<Value, &'static str> {
        self.sent
            .lock()
            .unwrap()
            .push((n.delivery_id, n.recipient, n.token.to_string()));
        Ok(json!({
            "eventId": notice_event_id(n.delivery_id),
            "eventType": exports::EXPORT_READY_TYPE,
            "sealed": { "kid": "notif-hpke-2026-11", "enc": "stand-in" },
        }))
    }
}

fn vault() -> ExportVault {
    let k = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([42u8; 32]);
    ExportVault::parse(&format!("exp-test-1:{k}")).unwrap()
}

async fn harness(notices: Arc<Notices>) -> Option<Harness> {
    Harness::with_exports(Some(Arc::new(ExportDelivery {
        vault: vault(),
        notices,
    })))
    .await
}

/// An organization with one OWNER, one ADMIN and a pending invitation (an address in
/// the document).
async fn org_with_invitation(h: &Harness, owner: Uuid, admin: Uuid, slug: &str) -> String {
    let org = s(&h.create_org(owner, slug).await["id"]);
    h.add_member(&org, owner, admin, "ADMIN").await;
    h.invite(&org, owner, INVITEE, "MEMBER").await;
    org
}

/// Nothing a bridge answer or read may carry.
#[track_caller]
fn reference_only(v: &Value) {
    let text = v.to_string();
    for forbidden in [
        "@",
        "email",
        "token",
        "http",
        "url",
        "sealed",
        "ciphertext",
        "invitations",
        "userId",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} in {v:#}");
    }
}

#[tokio::test]
async fn the_export_command_follows_the_owner_contract_and_answers_a_reference() {
    let notices = Arc::new(Notices::default());
    let Some(h) = harness(notices.clone()).await else {
        return;
    };
    let (owner, admin) = (Uuid::new_v4(), Uuid::new_v4());
    let org = org_with_invitation(&h, owner, admin, "export-contract").await;
    let bridge = || h.by(&h.peers.bridge).operator(STAFF);

    // Refusals build and store nothing.
    bridge()
        .approved(APPROVER)
        .post(&command(&org), json!({}))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    bridge()
        .idem(&key())
        .post(&command(&org), json!({}))
        .await
        .expect_error(403, "APPROVAL_REQUIRED");
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(
            &command(&org),
            json!({ "recipient": "someone@example.com" }),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    bridge()
        .approved(APPROVER)
        .idem(&key())
        .post(&command(&Uuid::new_v4().to_string()), json!({}))
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
    for signer in [&h.peers.operator, &h.peers.meeting] {
        h.by(signer)
            .idem(&key())
            .post(&command(&org), json!({}))
            .await
            .expect_error(403, "ACTOR_NOT_ALLOWED");
    }
    h.by(&h.peers.messaging)
        .post(&command(&org), json!({}))
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
    assert_eq!(
        h.count("SELECT count(*) FROM organization_exports").await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_admin_operations")
            .await,
        0
    );

    // Applied: a reference only, recorded with both people in the same transaction.
    let k = key();
    let r = bridge()
        .approved(APPROVER)
        .idem(&k)
        .post(&command(&org), json!({}))
        .await;
    let v = r.expect(200).clone();
    assert_eq!(v.as_object().unwrap().len(), 3, "{v:#}");
    assert_eq!(v["status"], "READY");
    assert_eq!(v["version"], "1");
    reference_only(&v);
    let res = &v["resource"];
    let export = s(&res["exportId"]);
    assert_eq!(res["organizationId"], json!(org));
    assert_eq!(res["state"], "READY");
    assert_eq!(res["delivery"], "OWNER_DELIVERED");
    assert_eq!(res["deliveryChannel"], "DOWNLOAD_LINK_EMAIL");
    assert!(res["recipientMasked"].is_null());
    assert_eq!(res["recipientCount"], 1, "the ADMIN is not a recipient");
    assert_eq!(res["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(res["deliveryRef"], res["deliveries"][0]["deliveryRef"]);
    assert_eq!(res["deliveries"][0]["state"], "PENDING");
    assert!(res["sizeBytes"].as_i64().unwrap() > 0);
    let sha = s(&res["sha256"]);
    assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()));
    let created = chrono::DateTime::parse_from_rfc3339(res["createdAt"].as_str().unwrap()).unwrap();
    let expires = chrono::DateTime::parse_from_rfc3339(res["expiresAt"].as_str().unwrap()).unwrap();
    assert_eq!(expires - created, chrono::Duration::days(7));
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_admin_operations WHERE idempotency_key = '{k}' \
             AND command = 'organization.export' AND subject_kind = 'ORGANIZATION_EXPORT' AND subject_id = '{export}' \
             AND organization_id = '{org}' AND operator_ref = 'hq:{STAFF}' AND approver_ref = 'hq:{APPROVER}' \
             AND approval_ref = 'apr-0001' AND resulting_state = 'READY' AND resulting_version = 1"
        ))
        .await,
        1
    );
    assert_eq!(
        bridge().get(&operation(&k)).await.expect(200).clone(),
        json!({ "idempotency_key": k, "outcome": "APPLIED", "source_status": "READY", "source_version": "1" })
    );

    // At rest: the copy is encrypted and only the token's digest is stored.
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_exports WHERE id = '{export}' AND ciphertext IS NOT NULL \
             AND position(convert_to('{INVITEE}', 'UTF8') in ciphertext) = 0 AND key_id = 'exp-test-1'"
        ))
        .await,
        1
    );
    let token = notices.token_for(owner);
    let digest = hex::encode(Sha256::digest(token.as_bytes()));
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_export_deliveries WHERE export_id = '{export}' \
             AND recipient_user_id = '{owner}' AND token_hash = '{digest}' AND channel = 'DOWNLOAD_LINK_EMAIL'"
        ))
        .await,
        1
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_export_deliveries WHERE notice::text LIKE '%{token}%'"
        ))
        .await,
        0
    );
    // The audit trail names the export, never an address.
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_audit_events WHERE organization_id = '{org}' \
             AND action = 'organization.export.requested' AND target_id = '{export}' \
             AND actor_kind = 'OPERATOR' AND summary::text NOT LIKE '%@%'"
        ))
        .await,
        1
    );

    // A repeated key answers the same export and builds nothing.
    let again = bridge()
        .approved(APPROVER)
        .idem(&k)
        .post(&command(&org), json!({}))
        .await;
    assert_eq!(again.expect(200)["resource"]["exportId"], json!(export));
    assert_eq!(
        h.count("SELECT count(*) FROM organization_exports").await,
        1
    );
    assert_eq!(notices.sent.lock().unwrap().len(), 1);
    // The same key elsewhere is a conflict.
    let other = s(&h.create_org(owner, "export-contract-2").await["id"]);
    bridge()
        .approved(APPROVER)
        .idem(&k)
        .post(&command(&other), json!({}))
        .await
        .expect_error(409, "IDEMPOTENCY_CONFLICT");

    // The bridge never receives the document from the operator export either; the
    // console keeps it.
    bridge()
        .get(&admin_export(&org))
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
    h.by(&h.peers.operator)
        .get(&admin_export(&org))
        .await
        .expect(200);
}

#[tokio::test]
async fn every_active_owner_gets_their_own_link() {
    let notices = Arc::new(Notices::default());
    let Some(h) = harness(notices.clone()).await else {
        return;
    };
    let (owner, admin, second) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let org = org_with_invitation(&h, owner, admin, "export-owners").await;
    h.by(&h.peers.operator)
        .post(
            &format!("/internal/v1/admin/organizations/{org}/owners"),
            json!({ "userId": second, "reason": "second owner" }),
        )
        .await
        .expect(200);
    let v = h
        .by(&h.peers.bridge)
        .operator(STAFF)
        .approved(APPROVER)
        .idem(&key())
        .post(&command(&org), json!({}))
        .await
        .expect(200)
        .clone();
    assert_eq!(v["resource"]["recipientCount"], 2);
    assert_eq!(v["resource"]["deliveries"].as_array().unwrap().len(), 2);
    let tokens = (notices.token_for(owner), notices.token_for(second));
    assert_ne!(tokens.0, tokens.1);
    // One owner's token is not the other owner's link.
    let export = s(&v["resource"]["exportId"]);
    h.user(second)
        .post(&download(&org, &export), json!({ "token": tokens.0 }))
        .await
        .expect_error(404, "EXPORT_LINK_INVALID");
    let r = h
        .user(second)
        .post(&download(&org, &export), json!({ "token": tokens.1 }))
        .await;
    r.expect(200);
    // Delivered once one owner downloaded; the other link stays usable until expiry.
    let read = h
        .by(&h.peers.bridge)
        .operator(STAFF)
        .get(&detail(&org, &export))
        .await
        .expect(200)
        .clone();
    assert_eq!(read["state"], "DELIVERED");
    assert_eq!(read["version"], 2);
    h.user(owner)
        .post(&download(&org, &export), json!({ "token": tokens.0 }))
        .await
        .expect(200);
    let read = h
        .by(&h.peers.bridge)
        .operator(STAFF)
        .get(&detail(&org, &export))
        .await
        .expect(200)
        .clone();
    assert_eq!(read["version"], 2);
}

#[tokio::test]
async fn the_command_is_refused_when_delivery_is_unavailable_or_no_owner_can_receive_it() {
    // Not configured (the default).
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "export-off").await["id"]);
    send(&h, &org)
        .await
        .expect_error(409, "EXPORT_DELIVERY_UNAVAILABLE");
    assert_eq!(
        h.count("SELECT count(*) FROM organization_exports").await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_admin_operations")
            .await,
        0
    );
    // The download needs the feature too.
    h.user(owner)
        .post(
            &download(&org, &Uuid::new_v4().to_string()),
            json!({ "token": "x".repeat(43) }),
        )
        .await
        .expect_error(409, "EXPORT_DELIVERY_UNAVAILABLE");

    // Configured, but the notice cannot be built (the catalogue lacks the type).
    let refusing = Arc::new(Notices {
        refuse: true,
        ..Notices::default()
    });
    let Some(h) = harness(refusing).await else {
        return;
    };
    let org = s(&h.create_org(owner, "export-no-notice").await["id"]);
    send(&h, &org)
        .await
        .expect_error(409, "EXPORT_DELIVERY_UNAVAILABLE");
    assert_eq!(
        h.count("SELECT count(*) FROM organization_exports").await,
        0
    );

    // No ACTIVE OWNER to deliver to.
    let Some(h) = harness(Arc::new(Notices::default())).await else {
        return;
    };
    let org = s(&h.create_org(owner, "export-no-owner").await["id"]);
    h.exec(&format!(
        "UPDATE organization_memberships SET status = 'SUSPENDED' WHERE organization_id = '{org}'"
    ))
    .await
    .unwrap();
    send(&h, &org)
        .await
        .expect_error(409, "EXPORT_NO_RECIPIENT");
    assert_eq!(
        h.count("SELECT count(*) FROM organization_exports").await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM organization_admin_operations")
            .await,
        0
    );
}

#[tokio::test]
async fn the_owner_downloads_the_document_once_after_sign_in() {
    let notices = Arc::new(Notices::default());
    let Some(h) = harness(notices.clone()).await else {
        return;
    };
    let (owner, admin) = (Uuid::new_v4(), Uuid::new_v4());
    let org = org_with_invitation(&h, owner, admin, "export-download").await;
    let v = h
        .by(&h.peers.bridge)
        .operator(STAFF)
        .approved(APPROVER)
        .idem(&key())
        .post(&command(&org), json!({}))
        .await
        .expect(200)
        .clone();
    let export = s(&v["resource"]["exportId"]);
    let sha = s(&v["resource"]["sha256"]);
    let token = notices.token_for(owner);
    let path = download(&org, &export);

    // Only the recipient owner, signed in, with the link's token.
    h.user(owner)
        .post(&path, json!({ "token": "w".repeat(43) }))
        .await
        .expect_error(404, "EXPORT_LINK_INVALID");
    h.user(admin)
        .post(&path, json!({ "token": token }))
        .await
        .expect_error(404, "EXPORT_LINK_INVALID");
    h.user(owner)
        .post(
            &download(&org, &Uuid::new_v4().to_string()),
            json!({ "token": token }),
        )
        .await
        .expect_error(404, "EXPORT_LINK_INVALID");
    h.by(&h.peers.meeting)
        .post(&path, json!({ "token": token }))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    h.by(&h.peers.bridge)
        .operator(STAFF)
        .post(&path, json!({ "token": token }))
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");

    // The owner gets the document exactly as stored (the same as the existing export).
    let r = h.user(owner).post(&path, json!({ "token": token })).await;
    let doc = r.expect(200).clone();
    assert_eq!(r.headers.get("cache-control").unwrap(), "no-store");
    assert_eq!(r.headers.get("x-cf-export-sha256").unwrap(), sha.as_str());
    assert_eq!(
        hex::encode(Sha256::digest(serde_json::to_vec(&doc).unwrap())),
        sha
    );
    assert_eq!(doc["schemaVersion"], 1);
    assert_eq!(doc["organization"]["id"], json!(org));
    assert_eq!(doc["invitations"][0]["email_normalized"], INVITEE);
    assert!(doc["invitations"][0].get("token_hash").is_none());

    // Single use.
    h.user(owner)
        .post(&path, json!({ "token": token }))
        .await
        .expect_error(404, "EXPORT_LINK_INVALID");
    let read = h
        .by(&h.peers.bridge)
        .operator(STAFF)
        .get(&detail(&org, &export))
        .await
        .expect(200)
        .clone();
    reference_only(&read);
    assert_eq!(read["state"], "DELIVERED");
    assert_eq!(read["version"], 2);
    assert_eq!(read["deliveries"][0]["state"], "CLAIMED");
    assert!(read["deliveredAt"].is_string());
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_audit_events WHERE action = 'organization.export.downloaded' \
             AND target_id = '{export}' AND actor_user_id = '{owner}'"
        ))
        .await,
        1
    );

    // Every link settled: the encrypted copy is purged right away.
    let report = exports::maintain(&h.db.conn, 100).await.unwrap();
    assert_eq!(report.purged, 1);
    let read = h
        .by(&h.peers.bridge)
        .operator(STAFF)
        .get(&detail(&org, &export))
        .await
        .expect(200)
        .clone();
    assert_eq!(read["state"], "DELIVERED");
    assert!(read["purgedAt"].is_string());
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_exports WHERE id = '{export}' AND ciphertext IS NULL AND nonce IS NULL AND key_id IS NULL"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn expired_exports_are_purged_and_old_rows_deleted() {
    let notices = Arc::new(Notices::default());
    let Some(h) = harness(notices.clone()).await else {
        return;
    };
    let (owner, admin) = (Uuid::new_v4(), Uuid::new_v4());
    let org = org_with_invitation(&h, owner, admin, "export-expiry").await;
    let v = h
        .by(&h.peers.bridge)
        .operator(STAFF)
        .approved(APPROVER)
        .idem(&key())
        .post(&command(&org), json!({}))
        .await
        .expect(200)
        .clone();
    let export = s(&v["resource"]["exportId"]);
    let token = notices.token_for(owner);
    h.exec(&format!(
        "UPDATE organization_exports SET expires_at = now() - interval '1 second' WHERE id = '{export}'"
    ))
    .await
    .unwrap();
    h.exec(&format!(
        "UPDATE organization_export_deliveries SET expires_at = now() - interval '1 second' WHERE export_id = '{export}'"
    ))
    .await
    .unwrap();
    // Past expiry: the link is refused and HQ already reads EXPIRED.
    h.user(owner)
        .post(&download(&org, &export), json!({ "token": token }))
        .await
        .expect_error(410, "EXPORT_LINK_EXPIRED");
    let bridge = || h.by(&h.peers.bridge).operator(STAFF);
    assert_eq!(
        bridge().get(&detail(&org, &export)).await.expect(200)["state"],
        "EXPIRED"
    );
    let report = exports::maintain(&h.db.conn, 100).await.unwrap();
    assert_eq!(report.expired, 2, "{report:?}");
    assert_eq!(report.purged, 1, "{report:?}");
    let read = bridge()
        .get(&detail(&org, &export))
        .await
        .expect(200)
        .clone();
    assert_eq!(read["state"], "EXPIRED");
    assert_eq!(read["version"], 2);
    assert_eq!(read["deliveries"][0]["state"], "EXPIRED");
    assert!(read["purgedAt"].is_string());
    h.user(owner)
        .post(&download(&org, &export), json!({ "token": token }))
        .await
        .expect_error(410, "EXPORT_LINK_EXPIRED");
    // Nothing is resent: a second pass changes nothing.
    assert_eq!(
        exports::maintain(&h.db.conn, 100).await.unwrap(),
        exports::ExportReport::default()
    );
    // References are deleted after the retention period.
    h.exec(&format!(
        "UPDATE organization_exports SET created_at = now() - interval '91 days' WHERE id = '{export}'"
    ))
    .await
    .unwrap();
    assert_eq!(exports::maintain(&h.db.conn, 100).await.unwrap().deleted, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM organization_export_deliveries")
            .await,
        0
    );
    bridge()
        .get(&detail(&org, &export))
        .await
        .expect_error(404, "NOT_FOUND");
}

/// The Notification Service I1 stand-in: every notice gets `outcome`.
struct Sink {
    outcome: Result<&'static str, PublishFailure>,
    posted: Mutex<Vec<Value>>,
}

#[async_trait]
impl NotificationEventSink for Sink {
    async fn ingest(&self, events: &[Value]) -> Result<Vec<IngestResult>, PublishFailure> {
        self.posted.lock().unwrap().extend(events.iter().cloned());
        let outcome = self.outcome?;
        Ok(events
            .iter()
            .map(|e| IngestResult {
                event_id: e["eventId"].as_str().unwrap().to_string(),
                outcome: outcome.to_string(),
                reason: (outcome == "REJECTED").then(|| "RECIPIENT_UNKNOWN".to_string()),
            })
            .collect())
    }
}

#[tokio::test]
async fn notices_are_relayed_and_an_export_nobody_can_receive_fails_and_is_purged() {
    let notices = Arc::new(Notices::default());
    let Some(h) = harness(notices.clone()).await else {
        return;
    };
    let owner = Uuid::new_v4();
    let mut exports_made = Vec::new();
    for slug in ["export-relay-a", "export-relay-b"] {
        let org = s(&h.create_org(owner, slug).await["id"]);
        let v = h
            .by(&h.peers.bridge)
            .operator(STAFF)
            .approved(APPROVER)
            .idem(&key())
            .post(&command(&org), json!({}))
            .await
            .expect(200)
            .clone();
        exports_made.push((org, s(&v["resource"]["exportId"])));
    }
    let bridge = || h.by(&h.peers.bridge).operator(STAFF);

    // Unavailable: retried later, nothing settled.
    let down = Sink {
        outcome: Err(PublishFailure::Unavailable),
        posted: Mutex::default(),
    };
    let r = exports::relay_notices(&h.db.conn, &down, 1).await.unwrap();
    assert_eq!((r.sent, r.retried, r.failed), (0, 1, 0));

    // Accepted: SENT, and the sealed notice is dropped.
    let (org_a, export_a) = &exports_made[0];
    let (org_b, export_b) = &exports_made[1];
    h.exec(&format!(
        "UPDATE organization_export_deliveries SET next_attempt_at = now() WHERE export_id = '{export_a}'"
    ))
    .await
    .unwrap();
    h.exec(&format!(
        "UPDATE organization_export_deliveries SET next_attempt_at = now() + interval '1 hour' WHERE export_id = '{export_b}'"
    ))
    .await
    .unwrap();
    let up = Sink {
        outcome: Ok("ACCEPTED"),
        posted: Mutex::default(),
    };
    let r = exports::relay_notices(&h.db.conn, &up, 1).await.unwrap();
    assert_eq!(r.sent, 1, "{r:?}");
    let read = bridge()
        .get(&detail(org_a, export_a))
        .await
        .expect(200)
        .clone();
    assert_eq!(read["state"], "READY");
    assert_eq!(read["deliveries"][0]["state"], "SENT");
    assert!(read["deliveries"][0]["sentAt"].is_string());
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_export_deliveries WHERE export_id = '{export_a}' AND notice IS NULL"
        ))
        .await,
        1
    );

    // Rejected: the only owner cannot be told, so the export fails and is purged.
    h.exec(&format!(
        "UPDATE organization_export_deliveries SET next_attempt_at = now() WHERE export_id = '{export_b}'"
    ))
    .await
    .unwrap();
    let refusing = Sink {
        outcome: Ok("REJECTED"),
        posted: Mutex::default(),
    };
    let r = exports::relay_notices(&h.db.conn, &refusing, 10)
        .await
        .unwrap();
    assert_eq!((r.failed, r.purged), (1, 1), "{r:?}");
    let read = bridge()
        .get(&detail(org_b, export_b))
        .await
        .expect(200)
        .clone();
    assert_eq!(read["state"], "FAILED");
    assert_eq!(read["version"], 2);
    assert_eq!(read["deliveries"][0]["state"], "FAILED");
    assert!(read["purgedAt"].is_string());
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM organization_export_deliveries WHERE export_id = '{export_b}' \
             AND last_error = 'RECIPIENT_UNKNOWN' AND notice IS NULL"
        ))
        .await,
        1
    );
    // A failed export's link no longer works.
    h.user(owner)
        .post(
            &download(org_b, export_b),
            json!({ "token": notices.token_for(owner) }),
        )
        .await
        .expect_error(404, "EXPORT_LINK_INVALID");
}

#[tokio::test]
async fn operator_reads_list_and_show_export_references() {
    let notices = Arc::new(Notices::default());
    let Some(h) = harness(notices).await else {
        return;
    };
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, "export-reads").await["id"]);
    let mut ids = Vec::new();
    for _ in 0..3 {
        let v = h
            .by(&h.peers.bridge)
            .operator(STAFF)
            .approved(APPROVER)
            .idem(&key())
            .post(&command(&org), json!({}))
            .await
            .expect(200)
            .clone();
        ids.push(s(&v["resource"]["exportId"]));
    }
    let bridge = || h.by(&h.peers.bridge).operator(STAFF);
    let page = bridge()
        .get(&format!("{}?limit=2", list(&org)))
        .await
        .expect(200)
        .clone();
    reference_only(&page);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["exportId"], json!(ids[2]), "newest first");
    assert_eq!(page["items"][0]["version"], 1);
    let cursor = s(&page["nextCursor"]);
    let rest = bridge()
        .get(&format!("{}?limit=2&cursor={cursor}", list(&org)))
        .await
        .expect(200)
        .clone();
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);
    assert_eq!(rest["items"][0]["exportId"], json!(ids[0]));
    assert!(rest["nextCursor"].is_null());

    bridge()
        .get(&format!("{}?limit=101", list(&org)))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    bridge()
        .get(&format!("{}?cursor=nope", list(&org)))
        .await
        .expect_error(400, "VALIDATION_ERROR");
    bridge()
        .get(&list(&Uuid::new_v4().to_string()))
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
    bridge()
        .get(&detail(&Uuid::new_v4().to_string(), &ids[0]))
        .await
        .expect_error(404, "ORGANIZATION_NOT_FOUND");
    bridge()
        .get(&detail(&org, &Uuid::new_v4().to_string()))
        .await
        .expect_error(404, "NOT_FOUND");
    // An export of another organization is not found under this one.
    let other = s(&h.create_org(owner, "export-reads-2").await["id"]);
    bridge()
        .get(&detail(&other, &ids[0]))
        .await
        .expect_error(404, "NOT_FOUND");
    // Bridge attribution and operator-read only.
    h.by(&h.peers.bridge)
        .get(&detail(&org, &ids[0]))
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
    h.by(&h.peers.operator)
        .get(&list(&org))
        .await
        .expect_error(403, "INTERNAL_PERMISSION_DENIED");
}

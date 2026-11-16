//! N3 / N5 / N8–N10 / N12 pipeline tests against a real database: ingest outcomes, shadow
//! processing without provider calls, template parity, the email ledger and its lanes,
//! the CRITICAL lease protocol, suppression, the inbox, preferences, push, cancellation,
//! reroute and the call fast lane.

mod common;

use std::time::Duration;

use axum::http::Method;
use cf_notification_client::{Producer, SealedPlaintext};
use chrono::Utc;
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const U: &str = "5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607";

fn org_removed(recipient: &str) -> Value {
    envelope(
        Producer::Organizations,
        "organizations.membership.removed",
        &[recipient.to_string()],
        &[("membershipId", json!(U)), ("organizationId", json!(U))],
        None,
    )
}

fn verification(h: &Harness, recipient: &str, code: &str) -> Value {
    sealed_envelope(
        &h.sealing,
        Producer::MeetingService,
        "account.verification-code.requested",
        &[recipient.to_string()],
        &[("ttlMinutes", json!(1440))],
        &format!("otp:verify:{}:1", Uuid::new_v4().simple()),
        SealedPlaintext::default().with_code(code),
    )
}

#[tokio::test]
async fn ingest_outcomes_follow_the_contract() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_category("ORGANIZATION_UPDATES", "ACTIVE").await;
    let org = h.peers.organizations.clone();
    let ev = org_removed(&alice());
    let id = ev["eventId"].as_str().unwrap().to_string();
    assert_eq!(
        h.ingest(&org, vec![ev.clone()]).await.expect(200)["results"][0]["outcome"],
        "ACCEPTED"
    );
    assert_eq!(
        h.ingest(&org, vec![ev.clone()]).await.expect(200)["results"][0]["outcome"],
        "DUPLICATE"
    );
    // Same id, different body.
    let mut changed = ev.clone();
    changed["occurredAt"] = json!("2026-11-16T12:00:01Z");
    let r = h.ingest(&org, vec![changed]).await;
    assert_eq!(r.json["results"][0]["outcome"], "REJECTED");
    assert_eq!(r.json["results"][0]["reason"], "DIGEST_MISMATCH");
    // Producer allowlist: Organizations may not publish an Account code.
    let code = verification(&h, &alice(), "123456");
    let r = h.ingest(&org, vec![code]).await;
    assert_eq!(r.json["results"][0]["reason"], "PRODUCER_MISMATCH");
    // Forbidden data key: refused, and the body is never stored.
    let mut bad = org_removed(&alice());
    bad["data"]["emailAddress"] = json!("leak@example.com");
    let r = h.ingest(&org, vec![bad]).await;
    assert_eq!(r.json["results"][0]["outcome"], "REJECTED");
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_event_inbox WHERE envelope::text LIKE '%leak@example.com%'").await, 0);
    // Unsupported routing protocol: retry another replica.
    let mut future = org_removed(&alice());
    future["route"]["protocol"] = json!(2);
    h.ingest(&org, vec![future])
        .await
        .expect_error(503, "PROTOCOL_UNSUPPORTED");
    // dryRun probe (§9.5).
    let probe = h
        .signed(
            &org,
            Method::POST,
            "/internal/v1/notification-events",
            Some(json!({"events": [ev, org_removed(&bob())], "dryRun": true})),
            &[],
        )
        .await;
    let res = probe.expect(200)["results"].as_array().unwrap().clone();
    assert_eq!(res[0]["outcome"], "DUPLICATE");
    assert_eq!(res[0]["eventId"], id);
    assert_eq!(res[1]["outcome"], "UNKNOWN");
    // A decision id is unique per producer.
    let mut reuse = org_removed(&bob());
    reuse["route"] = h.text(&format!("SELECT json_build_object('decisionId', route_decision_id, 'routingVersion', 1, 'protocol', 1)::text FROM notif_event_inbox WHERE event_id = '{id}'")).await.map(|s| serde_json::from_str::<Value>(&s).unwrap()).unwrap();
    assert_eq!(
        h.ingest(&org, vec![reuse]).await.json["results"][0]["reason"],
        "DECISION_REUSED"
    );
}

#[tokio::test]
async fn shadow_processing_records_parity_digests_and_never_sends() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_mode("AUTH_VERIFICATION", "EMAIL", "SHADOW").await;
    let meeting = h.peers.meeting.clone();
    let ev = verification(&h, &alice(), "482913");
    assert_eq!(
        h.ingest(&meeting, vec![ev]).await.expect(200)["results"][0]["outcome"],
        "ACCEPTED"
    );
    h.process_all().await;
    let worker = h.email_worker("CRITICAL");
    worker.tick(Utc::now()).await.unwrap();
    assert!(h.provider.sent().is_empty());
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_email_messages")
            .await,
        0
    );
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_deliveries WHERE status = 'SHADOW' AND channel = 'EMAIL'").await, 1);
    let digest = h
        .text("SELECT render_digest FROM notif_shadow_observations WHERE side = 'NS'")
        .await
        .unwrap();
    // Same subject + text as the Meeting template for the same input.
    let legacy = notifications::templates::email::verification_email("Alice", "482913", "24 hours");
    let expected = notifications::templates::RenderedEmail {
        subject: "Your CF Meet verification code".into(),
        html: legacy.html,
        text: legacy.text,
    };
    assert_eq!(digest, expected.digest());
    // The sealed part is erased from the stored envelope once processed.
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_event_inbox WHERE envelope ? 'sealed'")
            .await,
        0
    );
    assert!(h.owners.leases_claimed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn producer_shadow_copies_are_never_sent_even_when_active() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_category("ORGANIZATION_UPDATES", "ACTIVE").await;
    let org = h.peers.organizations.clone();
    let r = h
        .signed(
            &org,
            Method::POST,
            "/internal/v1/notification-events",
            Some(json!({"events": [org_removed(&alice())], "shadow": true})),
            &[],
        )
        .await;
    assert_eq!(r.expect(200)["results"][0]["outcome"], "ACCEPTED");
    h.process_all().await;
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_inbox_items")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_event_inbox WHERE state = 'SHADOWED'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_shadow_observations WHERE side = 'LEGACY'")
            .await,
        1
    );
    let op = h.peers.operator.clone();
    let report = h
        .signed(
            &op,
            Method::GET,
            "/internal/v1/admin/shadow-report",
            None,
            &[],
        )
        .await;
    let rows = report.expect(200)["rows"].as_array().unwrap().clone();
    assert!(rows
        .iter()
        .any(|r| r["category"] == "ORGANIZATION_UPDATES" && r["legacy"].as_i64() == Some(1)));
}

#[tokio::test]
async fn critical_codes_are_sent_only_under_an_account_lease_and_completed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_mode("AUTH_VERIFICATION", "EMAIL", "ACTIVE").await;
    let meeting = h.peers.meeting.clone();
    let ev = verification(&h, &alice(), "482913");
    let decision = Uuid::parse_str(ev["route"]["decisionId"].as_str().unwrap()).unwrap();
    h.ingest(&meeting, vec![ev]).await.expect(200);
    h.process_all().await;
    // Nothing plaintext at rest: address and code are sealed.
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_email_messages WHERE lane = 'CRITICAL' AND status = 'QUEUED'").await, 1);
    assert_eq!(h.count(&format!("SELECT count(*)::bigint FROM notif_email_messages WHERE concat_ws(' ', recipient_sealed, subject_sealed, body_sealed) LIKE '%{ALICE_EMAIL}%' OR body_sealed LIKE '%482913%'")).await, 0);
    let r = h.email_worker("CRITICAL").tick(Utc::now()).await.unwrap();
    assert_eq!(r.accepted, 1);
    let sent = h.provider.sent();
    assert_eq!(sent[0].1, ALICE_EMAIL);
    assert!(sent[0].3.contains("482913"));
    assert_eq!(
        h.provider.from.lock().unwrap()[0],
        "CF Meet <no-reply@cfmeet.com>"
    );
    assert_eq!(*h.owners.leases_claimed.lock().unwrap(), vec![decision]);
    assert_eq!(*h.owners.completed.lock().unwrap(), vec![decision]);
    // Content erased at the final state.
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_email_messages WHERE body_sealed IS NOT NULL")
            .await,
        0
    );
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_deliveries WHERE channel = 'EMAIL' AND status = 'SENT'").await, 1);
}

#[tokio::test]
async fn a_revoked_lease_is_never_sent_and_an_unreachable_account_sends_nothing() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_mode("AUTH_VERIFICATION", "EMAIL", "ACTIVE").await;
    let meeting = h.peers.meeting.clone();
    let ev = verification(&h, &alice(), "111111");
    let decision = Uuid::parse_str(ev["route"]["decisionId"].as_str().unwrap()).unwrap();
    h.ingest(&meeting, vec![ev]).await.expect(200);
    h.process_all().await;
    *h.owners.unavailable.lock().unwrap() = true;
    let r = h.email_worker("CRITICAL").tick(Utc::now()).await.unwrap();
    assert_eq!((r.accepted, r.lease_refused), (0, 1));
    *h.owners.unavailable.lock().unwrap() = false;
    h.owners
        .lease_outcomes
        .lock()
        .unwrap()
        .insert(decision, "REVOKED".into());
    h.exec("UPDATE notif_email_messages SET next_attempt_at = now()")
        .await
        .unwrap();
    h.email_worker("CRITICAL").tick(Utc::now()).await.unwrap();
    assert!(h.provider.sent().is_empty());
    assert_eq!(
        h.text("SELECT last_error_class FROM notif_email_messages")
            .await
            .as_deref(),
        Some("ROUTE_REVOKED")
    );
}

#[tokio::test]
async fn the_critical_lane_is_not_starved_by_a_standard_backlog() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_category("DEVELOPER_PLATFORM", "ACTIVE").await;
    h.set_mode("AUTH_VERIFICATION", "EMAIL", "ACTIVE").await;
    let dev = h.peers.developer.clone();
    let events: Vec<Value> = (0..40)
        .map(|_| {
            envelope(
                Producer::DeveloperPlatform,
                "developer-platform.production-access.decided",
                &[bob()],
                &[
                    ("decision", json!("APPROVED")),
                    ("requestId", json!(Uuid::new_v4().to_string())),
                ],
                Some(&format!("devplat-access:{}:1", Uuid::new_v4())),
            )
        })
        .collect();
    h.ingest(&dev, events).await.expect(200);
    let meeting = h.peers.meeting.clone();
    h.ingest(&meeting, vec![verification(&h, &alice(), "777777")])
        .await
        .expect(200);
    // W1 claims CRITICAL first.
    h.processor().tick(Utc::now()).await.unwrap();
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_email_messages WHERE lane = 'CRITICAL'")
            .await,
        1
    );
    h.process_all().await;
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_email_messages WHERE lane = 'STANDARD'")
            .await,
        40
    );
    let r = h.email_worker("CRITICAL").tick(Utc::now()).await.unwrap();
    assert_eq!(r.accepted, 1);
    assert!(h.provider.sent()[0].3.contains("777777"));
}

#[tokio::test]
async fn two_workers_with_a_slow_provider_send_each_message_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_category("DEVELOPER_PLATFORM", "ACTIVE").await;
    let dev = h.peers.developer.clone();
    let events: Vec<Value> = (0..5)
        .map(|_| {
            envelope(
                Producer::DeveloperPlatform,
                "developer-platform.credential.expired",
                &[bob()],
                &[
                    ("applicationId", json!(U)),
                    ("credentialId", json!(Uuid::new_v4().to_string())),
                    ("expiresAt", json!("2026-11-20T09:00:00Z")),
                ],
                None,
            )
        })
        .collect();
    h.ingest(&dev, events).await.expect(200);
    h.process_all().await;
    h.provider.slow(Duration::from_millis(300));
    let a = h.email_worker("STANDARD").with_lease_secs(1);
    let b = h.email_worker("STANDARD").with_lease_secs(1);
    let (ra, rb) = tokio::join!(
        async {
            let mut n = 0;
            for _ in 0..6 {
                n += a.tick(Utc::now()).await.unwrap().accepted;
            }
            n
        },
        async {
            let mut n = 0;
            for _ in 0..6 {
                n += b.tick(Utc::now()).await.unwrap().accepted;
            }
            n
        },
    );
    assert_eq!(ra + rb, 5);
    let sent = h.provider.sent();
    let mut ids: Vec<_> = sent.iter().map(|s| s.0).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 5, "duplicate sends: {}", sent.len());
}

#[tokio::test]
async fn provider_webhooks_drive_delivery_evidence_and_suppression() {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    let raw = base64::engine::general_purpose::STANDARD.encode(b"0123456789abcdef0123456789abcdef");
    let mut email = notifications::config::EmailConfig::disabled();
    email.resend_webhook_secret = Some(secrecy::SecretString::from(format!("whsec_{raw}")));
    let Some(h) = Harness::with_email(email).await else {
        return;
    };
    h.set_category("DEVELOPER_PLATFORM", "ACTIVE").await;
    h.set_mode("AUTH_VERIFICATION", "EMAIL", "ACTIVE").await;
    let dev = h.peers.developer.clone();
    h.ingest(
        &dev,
        vec![envelope(
            Producer::DeveloperPlatform,
            "developer-platform.credential.expired",
            &[bob()],
            &[
                ("applicationId", json!(U)),
                ("credentialId", json!(U)),
                ("expiresAt", json!("2026-11-20T09:00:00Z")),
            ],
            None,
        )],
    )
    .await
    .expect(200);
    h.process_all().await;
    h.email_worker("STANDARD").tick(Utc::now()).await.unwrap();
    let provider_id = format!("rec_{}", h.provider.sent()[0].0.simple());
    // The recording provider stands in for Resend (events are matched per provider).
    h.exec("UPDATE notif_email_messages SET provider = 'resend'")
        .await
        .unwrap();
    let post = |event_id: &str, ty: &str| {
        let body = json!({ "type": ty, "created_at": "2026-11-16T12:00:00Z", "data": { "email_id": provider_id } }).to_string();
        let ts = Utc::now().timestamp().to_string();
        let key = base64::engine::general_purpose::STANDARD
            .decode(&raw)
            .unwrap();
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&key).unwrap();
        mac.update(format!("{event_id}.{ts}.").as_bytes());
        mac.update(body.as_bytes());
        let sig = format!(
            "v1,{}",
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        );
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("svix-id", event_id.parse().unwrap());
        headers.insert("svix-timestamp", ts.parse().unwrap());
        headers.insert("svix-signature", sig.parse().unwrap());
        (headers, serde_json::from_str::<Value>(&body).unwrap())
    };
    let (hd, body) = post("msg_1", "email.delivered");
    h.raw(
        Method::POST,
        "/api/v1/notifications/provider-events/resend",
        hd,
        Some(body),
    )
    .await
    .expect(200);
    assert_eq!(
        h.text("SELECT status FROM notif_email_messages")
            .await
            .as_deref(),
        Some("DELIVERED")
    );
    assert_eq!(
        h.text("SELECT status FROM notif_deliveries WHERE channel = 'EMAIL'")
            .await
            .as_deref(),
        Some("DELIVERED")
    );
    // Forged signature.
    let (mut hd, body) = post("msg_2", "email.complained");
    hd.insert("svix-signature", "v1,AAAA".parse().unwrap());
    h.raw(
        Method::POST,
        "/api/v1/notifications/provider-events/resend",
        hd,
        Some(body),
    )
    .await
    .expect_error(401, "AUTHENTICATION_REQUIRED");
    // A complaint suppresses STANDARD mail, never the CRITICAL lane.
    let (hd, body) = post("msg_3", "email.complained");
    h.raw(
        Method::POST,
        "/api/v1/notifications/provider-events/resend",
        hd,
        Some(body),
    )
    .await
    .expect(200);
    assert_eq!(
        h.text("SELECT status FROM notif_email_messages")
            .await
            .as_deref(),
        Some("COMPLAINED")
    );
    h.ingest(
        &dev,
        vec![envelope(
            Producer::DeveloperPlatform,
            "developer-platform.credential.expiring-soon",
            &[bob()],
            &[
                ("applicationId", json!(U)),
                ("credentialId", json!(U)),
                ("expiresAt", json!("2026-11-20T09:00:00Z")),
            ],
            None,
        )],
    )
    .await
    .expect(200);
    let meeting = h.peers.meeting.clone();
    h.ingest(&meeting, vec![verification(&h, &bob(), "246810")])
        .await
        .expect(200);
    h.process_all().await;
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_email_messages WHERE status = 'SUPPRESSED' AND lane = 'STANDARD'").await, 1);
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_email_messages WHERE status = 'QUEUED' AND lane = 'CRITICAL'").await, 1);
    // Unknown provider message ids are recorded and ignored.
    let (hd, mut body) = post("msg_4", "email.delivered");
    body["data"]["email_id"] = json!("someone-elses-message");
    let (hd2, _) = (hd, ());
    let _ = h
        .raw(
            Method::POST,
            "/api/v1/notifications/provider-events/resend",
            hd2,
            Some(body),
        )
        .await;
    let op = h.peers.operator.clone();
    let s = h
        .signed(
            &op,
            Method::GET,
            "/internal/v1/admin/email-suppressions",
            None,
            &[],
        )
        .await;
    assert_eq!(s.expect(200)["items"][0]["reason"], "COMPLAINED");
    assert!(!s.json.to_string().contains(BOB_EMAIL));
}

#[tokio::test]
async fn organization_invitation_sender_is_configuration() {
    let mut email = notifications::config::EmailConfig::disabled();
    email.sender_profiles = vec![(
        "organization-invitation".into(),
        "CF Meet Teams <no-reply@cfmeet.com>".into(),
    )];
    let Some(h) = Harness::with_email(email).await else {
        return;
    };
    h.set_category("ORGANIZATION_INVITATION", "ACTIVE").await;
    let meeting = h.peers.meeting.clone();
    let ev = sealed_envelope(
        &h.sealing,
        Producer::MeetingService,
        "meeting-service.organization-invitation.email-requested",
        &[format!("meeting-email-principal:{U}")],
        &[("invitationId", json!(U)), ("organizationId", json!(U))],
        &format!("org-invitation:{U}:1"),
        SealedPlaintext::default()
            .with_token("tok_0123456789abcdefghij")
            .with_address("invitee@example.com"),
    );
    h.ingest(&meeting, vec![ev]).await.expect(200);
    h.process_all().await;
    h.email_worker("STANDARD").tick(Utc::now()).await.unwrap();
    let sent = h.provider.sent();
    assert_eq!(sent[0].1, "invitee@example.com");
    assert!(sent[0]
        .3
        .contains("/organizations/invitations/tok_0123456789abcdefghij"));
    assert_eq!(
        h.provider.from.lock().unwrap()[0],
        "CF Meet Teams <no-reply@cfmeet.com>"
    );
    assert_eq!(
        h.text("SELECT idempotency_key FROM notif_email_messages")
            .await
            .unwrap(),
        format!("org-invitation:{U}:1")
    );
}

#[tokio::test]
async fn the_inbox_is_private_versioned_and_counts_unread() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_category("ORGANIZATION_UPDATES", "ACTIVE").await;
    let org = h.peers.organizations.clone();
    h.ingest(
        &org,
        vec![
            org_removed(&alice()),
            org_removed(&alice()),
            org_removed(&bob()),
        ],
    )
    .await
    .expect(200);
    h.process_all().await;
    let list = h.user(&alice(), "GET", "", Some("limit=1"), None).await;
    let v = list.expect(200).clone();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["unreadCount"], 2);
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["items"][0]["title"], "Removed from organization");
    let next = v["nextCursor"].as_str().unwrap().to_string();
    let page2 = h
        .user(
            &alice(),
            "GET",
            "",
            Some(&format!("limit=1&cursor={next}")),
            None,
        )
        .await;
    assert_eq!(page2.json["items"].as_array().unwrap().len(), 1);
    assert!(page2.json["nextCursor"].is_null());
    let id = v["items"][0]["id"].as_str().unwrap().to_string();
    // Bob cannot see or change Alice's item.
    h.user(
        &bob(),
        "PATCH",
        &format!("/{id}"),
        None,
        Some(json!({"read": true, "expectedVersion": 1})),
    )
    .await
    .expect_error(404, "NOT_FOUND");
    let upd = h
        .user(
            &alice(),
            "PATCH",
            &format!("/{id}"),
            None,
            Some(json!({"read": true, "expectedVersion": 1})),
        )
        .await;
    assert!(upd.expect(200)["readAt"].is_string());
    h.user(
        &alice(),
        "PATCH",
        &format!("/{id}"),
        None,
        Some(json!({"read": true, "expectedVersion": 1})),
    )
    .await
    .expect_error(409, "VERSION_CONFLICT");
    let c = h.user(&alice(), "GET", "/unread-count", None, None).await;
    assert_eq!(c.expect(200)["unreadCount"], 1);
    assert!(c.headers.get("etag").is_some());
    let all = h
        .user(
            &alice(),
            "POST",
            "/read",
            None,
            Some(json!({"upTo": Utc::now()})),
        )
        .await;
    assert_eq!(all.expect(200)["unreadCount"], 0);
    let d = h
        .user(
            &alice(),
            "POST",
            "/dismiss",
            None,
            Some(json!({"ids": [id]})),
        )
        .await;
    assert_eq!(d.expect(200)["dismissed"], 1);
    assert_eq!(
        h.user(&alice(), "GET", "", None, None).await.json["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        h.user(&bob(), "GET", "", None, None).await.json["unreadCount"],
        1
    );
    // Mutations need an Idempotency-Key; unknown paths are 404.
    let no_key = json!({"method": "POST", "path": "/read", "account": alice(), "body": {"upTo": Utc::now()}});
    let meeting = h.peers.meeting.clone();
    h.signed(
        &meeting,
        Method::POST,
        "/internal/v1/notifications/dispatch",
        Some(no_key),
        &[],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    h.user(&alice(), "GET", "/admin", None, None)
        .await
        .expect_error(404, "NOT_FOUND");
}

#[tokio::test]
async fn preferences_are_versioned_and_drive_silent_items_and_channels() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let doc = h.user(&alice(), "GET", "/preferences", None, None).await;
    let v = doc.expect(200).clone();
    assert_eq!(v["version"], 0);
    assert_eq!(v["categories"]["contactRequests"], true);
    assert_eq!(v["securityEvents"], true);
    assert_eq!(v["timeZone"], "UTC");
    let upd = h
        .user(&alice(), "PUT", "/preferences", None, Some(json!({"expectedVersion": 0, "categories": {"contactRequests": false}, "quietHours": {"enabled": true, "start": "22:00", "end": "07:00", "days": []}})))
        .await;
    assert_eq!(upd.expect(200)["version"], 1);
    h.user(
        &alice(),
        "PUT",
        "/preferences",
        None,
        Some(json!({"expectedVersion": 0, "categories": {"voicemail": false}})),
    )
    .await
    .expect_error(409, "VERSION_CONFLICT");
    h.user(
        &alice(),
        "PUT",
        "/preferences",
        None,
        Some(json!({"expectedVersion": 1, "theme": "dark"})),
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_preference_change_log")
            .await,
        1
    );
    // Category off: the in-app item is silent and nothing is pushed.
    h.set_category("CONTACT_REQUESTS", "ACTIVE").await;
    let hub = h.peers.communications.clone();
    h.ingest(
        &hub,
        vec![envelope(
            Producer::Communications,
            "communications.connection.requested",
            &[alice()],
            &[("actorRef", json!(bob())), ("connectionId", json!(U))],
            None,
        )],
    )
    .await
    .expect(200);
    h.process_all().await;
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_inbox_items WHERE silent")
            .await,
        1
    );
    assert_eq!(
        h.user(&alice(), "GET", "/unread-count", None, None)
            .await
            .json["unreadCount"],
        0
    );
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_deliveries WHERE channel = 'PUSH' AND status NOT IN ('SUPPRESSED')").await, 0);
}

#[tokio::test]
async fn push_registration_dispatch_and_invalid_handles() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let device = Uuid::new_v4();
    // Not the caller's device: 404.
    h.user(
        &alice(),
        "PUT",
        &format!("/devices/{device}/push"),
        None,
        Some(json!({"platform": "LOCAL_TEST", "handle": "handle-1"})),
    )
    .await
    .expect_error(404, "NOT_FOUND");
    h.owners
        .contacts
        .lock()
        .unwrap()
        .get_mut(&alice())
        .unwrap()
        .devices
        .push(notifications::authorities::AccountDevice {
            account_device_ref: device.to_string(),
            state: "ACTIVE".into(),
        });
    let r = h
        .user(
            &alice(),
            "PUT",
            &format!("/devices/{device}/push"),
            None,
            Some(json!({"platform": "LOCAL_TEST", "handle": "handle-1"})),
        )
        .await;
    let v = r.expect(200).clone();
    assert!(v["pushTargetRef"].is_string());
    assert!(!v.to_string().contains("handle-1"));
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_push_endpoints WHERE handle_sealed LIKE '%handle-1%'").await, 0);
    h.set_category("MISSED_CALLS", "ACTIVE").await;
    let calling = h.peers.calling.clone();
    h.ingest(
        &calling,
        vec![envelope(
            Producer::Calling,
            "calling.call.participant-missed",
            &[alice()],
            &[("callId", json!(U)), ("callKind", json!("AUDIO"))],
            None,
        )],
    )
    .await
    .expect(200);
    h.process_all().await;
    let t = h.push_worker().tick(Utc::now()).await.unwrap();
    assert_eq!(t.sent, 1);
    let sent = h.push.sent.lock().unwrap().clone();
    assert_eq!(sent[0].0, "handle-1");
    assert!(cf_push_transport::adapters::metadata(&sent[0].1).is_some());
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_inbox_items")
            .await,
        1
    );
    // Removal is idempotent.
    h.user(
        &alice(),
        "DELETE",
        &format!("/devices/{device}/push"),
        Some("platform=WEB_PUSH"),
        None,
    )
    .await
    .expect(200);
    h.user(
        &alice(),
        "DELETE",
        &format!("/devices/{device}/push"),
        None,
        None,
    )
    .await
    .expect(200);
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_push_endpoints")
            .await,
        0
    );
}

#[tokio::test]
async fn cancel_events_stop_pending_deliveries() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let device = Uuid::new_v4();
    h.owners
        .contacts
        .lock()
        .unwrap()
        .get_mut(&alice())
        .unwrap()
        .devices
        .push(notifications::authorities::AccountDevice {
            account_device_ref: device.to_string(),
            state: "ACTIVE".into(),
        });
    h.user(
        &alice(),
        "PUT",
        &format!("/devices/{device}/push"),
        None,
        Some(json!({"platform": "LOCAL_TEST", "handle": "h2"})),
    )
    .await
    .expect(200);
    h.set_category("VOICEMAIL", "ACTIVE").await;
    let calling = h.peers.calling.clone();
    let mut ev = envelope(
        Producer::Calling,
        "calling.voicemail.ready",
        &[alice()],
        &[("callId", json!(U)), ("voiceMessageId", json!(U))],
        None,
    );
    ev["delivery"]["cancelKey"] = json!(format!("call:{U}"));
    h.ingest(&calling, vec![ev]).await.expect(200);
    h.process_all().await;
    let cancel = cf_notification_client::EnvelopeBuilder::new(
        Producer::Calling,
        "notifications.cancel",
        NOW,
        route(),
    )
    .unwrap()
    .dedupe_key(format!("cancel:{U}"))
    .cancel_key(format!("call:{U}"))
    .build()
    .unwrap();
    h.ingest(&calling, vec![serde_json::to_value(cancel).unwrap()])
        .await
        .expect(200);
    h.process_all().await;
    assert_eq!(h.push_worker().tick(Utc::now()).await.unwrap().sent, 0);
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_deliveries WHERE channel = 'PUSH' AND status = 'CANCELLED'").await, 1);
}

#[tokio::test]
async fn reroute_cancellation_never_reroutes_in_flight_messages() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_category("DEVELOPER_PLATFORM", "ACTIVE").await;
    let dev = h.peers.developer.clone();
    let a = envelope(
        Producer::DeveloperPlatform,
        "developer-platform.credential.expired",
        &[bob()],
        &[
            ("applicationId", json!(U)),
            ("credentialId", json!(U)),
            ("expiresAt", json!("2026-11-20T09:00:00Z")),
        ],
        None,
    );
    let b = envelope(
        Producer::DeveloperPlatform,
        "developer-platform.credential.expiring-soon",
        &[bob()],
        &[
            ("applicationId", json!(U)),
            ("credentialId", json!(U)),
            ("expiresAt", json!("2026-11-20T09:00:00Z")),
        ],
        None,
    );
    let (da, db_) = (
        a["route"]["decisionId"].clone(),
        b["route"]["decisionId"].clone(),
    );
    h.ingest(&dev, vec![a]).await.expect(200);
    h.process_all().await;
    h.email_worker("STANDARD").tick(Utc::now()).await.unwrap();
    h.ingest(&dev, vec![b]).await.expect(200);
    let op = h.peers.operator.clone();
    let r = h
        .signed(
            &op,
            Method::POST,
            "/internal/v1/admin/reroute/cancel",
            Some(json!({"decisionIds": [da, db_, Uuid::new_v4()]})),
            &[("idempotency-key", "reroute-0001")],
        )
        .await;
    let res = r.expect(200)["results"].as_array().unwrap().clone();
    assert_eq!(res[0]["outcome"], "IN_FLIGHT_OR_SENT");
    assert_eq!(res[1]["outcome"], "CANCELLED");
    assert_eq!(res[2]["outcome"], "UNKNOWN");
    h.process_all().await;
    h.email_worker("STANDARD").tick(Utc::now()).await.unwrap();
    assert_eq!(h.provider.sent().len(), 1);
}

#[tokio::test]
async fn the_call_fast_lane_is_bounded_and_off_until_activated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let device = Uuid::new_v4();
    h.owners
        .contacts
        .lock()
        .unwrap()
        .get_mut(&alice())
        .unwrap()
        .devices
        .push(notifications::authorities::AccountDevice {
            account_device_ref: device.to_string(),
            state: "ACTIVE".into(),
        });
    let r = h
        .user(
            &alice(),
            "PUT",
            &format!("/devices/{device}/push"),
            None,
            Some(json!({"platform": "LOCAL_TEST", "handle": "voip-1"})),
        )
        .await;
    let target = r.json["pushTargetRef"].clone();
    let calling = h.peers.calling.clone();
    let body = json!({"pushTargetRefs": [target], "payload": {"v": 1}, "expiresAt": (Utc::now() + chrono::Duration::seconds(30)).to_rfc3339()});
    let off = h
        .signed(
            &calling,
            Method::POST,
            "/internal/v1/push/call-invitations",
            Some(body.clone()),
            &[],
        )
        .await;
    assert_eq!(off.expect(200)["results"][0]["outcome"], "NOT_CONFIGURED");
    h.set_category("CALL_INVITATIONS", "ACTIVE").await;
    let on = h
        .signed(
            &calling,
            Method::POST,
            "/internal/v1/push/call-invitations",
            Some(body),
            &[],
        )
        .await;
    assert_eq!(on.expect(200)["results"][0]["outcome"], "SENT");
    // Only Calling holds the permission.
    let org = h.peers.organizations.clone();
    h.signed(
        &org,
        Method::POST,
        "/internal/v1/push/call-invitations",
        Some(json!({"pushTargetRefs": [], "payload": {}, "expiresAt": NOW})),
        &[],
    )
    .await
    .expect_error(403, "FORBIDDEN");
}

#[tokio::test]
async fn ingest_never_logs_or_stores_codes_in_clear() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_mode("AUTH_PASSWORD_RESET", "EMAIL", "ACTIVE").await;
    let meeting = h.peers.meeting.clone();
    let ev = sealed_envelope(
        &h.sealing,
        Producer::MeetingService,
        "account.password-reset-code.requested",
        &[alice()],
        &[("ttlMinutes", json!(60))],
        &format!("otp:reset:{}:1", Uuid::new_v4().simple()),
        SealedPlaintext::default().with_code("938271"),
    );
    h.ingest(&meeting, vec![ev]).await.expect(200);
    h.process_all().await;
    let leaks = h
        .count("SELECT count(*)::bigint FROM (SELECT envelope::text t FROM notif_event_inbox UNION ALL SELECT params::text FROM notif_notifications UNION ALL SELECT policy_snapshot::text FROM notif_notifications UNION ALL SELECT concat_ws(' ', recipient_sealed, subject_sealed, body_sealed) FROM notif_email_messages) x WHERE t LIKE '%938271%' OR t LIKE '%alice@example.com%'")
        .await;
    assert_eq!(leaks, 0);
}

#[tokio::test]
async fn retention_expiry_and_erasure_run_cleanly() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_category("ORGANIZATION_UPDATES", "ACTIVE").await;
    h.set_category("DEVELOPER_PLATFORM", "ACTIVE").await;
    let org = h.peers.organizations.clone();
    h.ingest(&org, vec![org_removed(&alice())])
        .await
        .expect(200);
    let dev = h.peers.developer.clone();
    h.ingest(
        &dev,
        vec![envelope(
            Producer::DeveloperPlatform,
            "developer-platform.credential.expired",
            &[alice()],
            &[
                ("applicationId", json!(U)),
                ("credentialId", json!(U)),
                ("expiresAt", json!("2026-11-20T09:00:00Z")),
            ],
            None,
        )],
    )
    .await
    .expect(200);
    h.process_all().await;
    h.email_worker("STANDARD").purge().await.unwrap();
    notifications::workers::expire_overdue(&h.db.conn)
        .await
        .unwrap();
    notifications::workers::retention(&h.db.conn).await.unwrap();
    // Queued mail past its expiry is expired by the purge (no content lingers).
    h.exec("UPDATE notif_email_messages SET expires_at = now() - interval '1 minute'")
        .await
        .unwrap();
    h.email_worker("STANDARD").purge().await.unwrap();
    assert_eq!(h.count("SELECT count(*)::bigint FROM notif_email_messages WHERE status = 'EXPIRED' AND body_sealed IS NULL").await, 1);
    notifications::workers::erase_account(&h.db.conn, &alice())
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_inbox_items")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_notifications")
            .await,
        0
    );
    assert_eq!(
        h.count(
            "SELECT count(*)::bigint FROM notif_email_messages WHERE recipient_sealed IS NOT NULL"
        )
        .await,
        0
    );
}

//! Account and meeting reads through the full chain (real mTLS, signed HQ request) against
//! a fake Meeting Service that records every call: paths, query mapping (search term,
//! window, cursors), the operator attribution, strict translation, code normalization and
//! minimization. The fake owner deliberately leaks fields (email, labels, keys, secrets,
//! IP addresses, guest identities, audit metadata) that must never reach HQ.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const USER: &str = "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a";
const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const T0: &str = "2026-11-01T10:00:00.123456+00:00";
const EMAIL: &str = "ada.lovelace@example.com";
const LEAKS: &[&str] = &[
    EMAIL,
    "Chrome on my laptop",
    "deadbeef-key",
    "JBSWY3DPEHPK3PXP",
    "203.0.113.9",
    "guest-token",
    "secret-meta",
];

/// (owner, path, query, operator) of one owner call.
type Recorded = (Owner, String, Vec<(String, String)>, String);

#[derive(Default)]
struct FakeMeetingService {
    calls: Mutex<Vec<Recorded>>,
}

fn page(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "nextCursor": next})
}

fn user_row() -> Value {
    json!({"userId": USER, "username": "ada", "displayName": "Ada Lovelace", "role": "admin",
           "status": "pending_verification", "createdAt": T0, "lastLoginAt": null, "email": EMAIL})
}

fn conference_row() -> Value {
    json!({"conferenceId": ID, "spaceId": USER, "title": "Weekly sync", "kind": "SCHEDULED", "status": "LIVE",
           "scheduledStartAt": T0, "scheduledEndAt": null, "startedAt": T0, "endedAt": null,
           "terminationReason": null, "locked": false, "recordingState": "idle", "transcriptionState": "idle",
           "participantsAdmitted": 3, "participantsTotal": 4, "createdAt": T0, "version": 2,
           "rtcRoomName": "secret-meta"})
}

#[async_trait]
impl OwnerPort for FakeMeetingService {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError> {
        self.calls.lock().unwrap().push((
            owner,
            path.to_string(),
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            operator.to_string(),
        ));
        let p = path.trim_start_matches("/internal/v1/operator");
        Ok(match p {
            "/accounts/users" => page(vec![user_row()], json!(USER)),
            x if x == format!("/accounts/users/{USER}") => {
                let mut u = user_row();
                u["emailMasked"] = json!("a***@e***.com");
                u["emailVerified"] = json!(true);
                u["authType"] = json!("local");
                u["locale"] = json!("en");
                u["timezone"] = json!("Africa/Lagos");
                u["passwordHash"] = json!("deadbeef-key");
                u
            }
            x if x.ends_with("/sessions") => page(
                vec![
                    json!({"sessionId": ID, "authMethod": "PASSWORD", "authenticatedAt": T0, "createdAt": T0,
                    "lastRefreshedAt": null, "expiresAt": T0, "revokedAt": null, "revokeReason": null,
                    "clientLabel": "Chrome on my laptop", "ipAddress": "203.0.113.9"}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/devices") => page(
                vec![
                    json!({"deviceId": ID, "deviceClass": "MOBILE", "platform": "IOS", "trust": "TRUSTED",
                    "trustedVia": "BOOTSTRAP", "trustedAt": T0, "state": "ACTIVE", "createdAt": T0, "lastSeenAt": T0,
                    "revokedAt": null, "revokeReason": null, "publicKey": "deadbeef-key", "label": "Chrome on my laptop"}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/mfa") => {
                json!({"userId": USER, "totp": {"state": "ENABLED", "enrolledAt": T0,
                "confirmedAt": T0, "secret": "JBSWY3DPEHPK3PXP"}, "recoveryCodesRemaining": 8})
            }
            x if x.ends_with("/security-events") => page(
                vec![
                    json!({"eventId": ID, "sequence": 41, "eventType": "SIGN_IN", "outcome": "SUCCESS",
                    "actorKind": "USER", "actorUserId": USER, "sessionId": ID, "occurredAt": T0,
                    "detail": {"ip": "203.0.113.9"}}),
                ],
                json!("41"),
            ),
            "/meetings/spaces" => page(
                vec![
                    json!({"spaceId": USER, "meetingCode": "abc-defg-hij", "title": "Weekly sync",
                    "meetingType": "STANDARD", "status": "ACTIVE", "ownerUserId": USER, "organizationId": null,
                    "createdAt": T0, "updatedAt": T0, "archivedAt": null, "version": 1, "passcodeHash": "deadbeef-key"}),
                ],
                Value::Null,
            ),
            "/meetings/conferences" | "/meetings/live" => page(vec![conference_row()], Value::Null),
            x if x == format!("/meetings/conferences/{ID}") => conference_row(),
            x if x.ends_with("/participants") => page(
                vec![
                    json!({"participantId": ID, "identityType": "user", "userId": USER, "displayName": "Ada",
                    "role": "host", "admissionState": "ADMITTED", "requestedAt": T0, "admittedAt": T0,
                    "removedAt": null, "bannedAt": null, "guestIdentity": "guest-token"}),
                ],
                Value::Null,
            ),
            "/meetings/audit-events" => page(
                vec![
                    json!({"eventId": ID, "spaceId": USER, "conferenceId": ID, "actorType": "PARTICIPANT",
                    "action": "SCREEN_SHARE_STOP", "targetType": "PARTICIPANT", "occurredAt": T0,
                    "metadata": {"note": "secret-meta"}, "actorRef": "guest-token"}),
                ],
                json!("1795000000000000.6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b"),
            ),
            _ => return Err(OwnerError::NotFound),
        })
    }
}

async fn harness() -> (Harness, Arc<FakeMeetingService>) {
    let fake = Arc::new(FakeMeetingService::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn last(fake: &FakeMeetingService) -> Recorded {
    fake.calls
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an owner call")
}

fn assert_no_leaks(v: &Value) {
    let text = v.to_string();
    for leak in LEAKS {
        assert!(!text.contains(leak), "{leak} must never reach HQ: {text}");
    }
}

fn window() -> String {
    "&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z".into()
}

#[tokio::test]
async fn meetings_user_search_never_returns_an_address() {
    let (h, fake) = harness().await;
    let c = call(
        "/accounts/users",
        &format!("{}&limit=10&q=ada", scoped("support-review")),
        "accounts.users.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (owner, path, query, operator) = last(&fake);
    assert_eq!(
        (owner, path.as_str(), operator.as_str()),
        (
            Owner::MeetingService,
            "/internal/v1/operator/accounts/users",
            "hq:staff-0001"
        )
    );
    assert_eq!(
        query,
        vec![("q".into(), "ada".into()), ("limit".into(), "10".into())]
    );
    let u = &v["result"]["items"][0];
    assert_eq!(
        (u["role"].as_str(), u["status"].as_str()),
        (Some("ADMIN"), Some("PENDING_VERIFICATION")),
        "codes are upper-cased"
    );
    assert_eq!(v["result"]["next_cursor"], USER);
    assert_no_leaks(&v);
}

#[tokio::test]
async fn meetings_user_detail_masks_the_address() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/accounts/users/{USER}"),
        &scoped("support-review"),
        "accounts.users.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        last(&fake).1,
        format!("/internal/v1/operator/accounts/users/{USER}")
    );
    let r = &v["result"];
    assert_eq!(r["email_masked"], "a***@e***.com");
    assert_eq!(r["auth_type"], "LOCAL");
    assert_no_leaks(&v);
    // a malformed id names nothing and never reaches the owner
    let calls = fake.calls.lock().unwrap().len();
    let bad = call(
        "/accounts/users/not-a-uuid",
        &scoped("support-review"),
        "accounts.users.read",
        &["cfmeet.support"],
    );
    assert!(matches!(
        code(&send(&h, &bad).await.1),
        "CAPABILITY_NOT_FOUND" | "RESOURCE_NOT_FOUND"
    ));
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn meetings_account_lists_translate_strictly() {
    let (h, fake) = harness().await;
    for (suffix, perm) in [
        ("sessions", "accounts.sessions.read"),
        ("devices", "accounts.devices.read"),
    ] {
        let c = call(
            &format!("/accounts/users/{USER}/{suffix}"),
            &format!("{}&limit=5", scoped("support-review")),
            perm,
            &["cfmeet.support"],
        );
        let (status, v) = send(&h, &c).await;
        assert_eq!(status, 200, "{suffix}: {v}");
        assert_eq!(
            last(&fake).1,
            format!("/internal/v1/operator/accounts/users/{USER}/{suffix}")
        );
        assert_eq!(v["result"]["items"].as_array().unwrap().len(), 1);
        assert_no_leaks(&v);
    }
    // a non-uuid cursor is the caller's error, never sent
    let bad = call(
        &format!("/accounts/users/{USER}/sessions"),
        &format!("{}&cursor=abc", scoped("support-review")),
        "accounts.sessions.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &bad).await.1), "QUERY_INVALID");
}

#[tokio::test]
async fn meetings_mfa_reports_state_only() {
    let (h, _) = harness().await;
    let c = call(
        &format!("/accounts/users/{USER}/mfa"),
        &scoped("support-review"),
        "accounts.mfa.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["totp_state"], "ENABLED");
    assert_eq!(v["result"]["recovery_codes_remaining"], 8);
    assert_no_leaks(&v);
}

#[tokio::test]
async fn meetings_security_history_maps_window_and_sequence_cursor() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/accounts/users/{USER}/security-history"),
        &format!("{}&limit=5&cursor=42{}", scoped("support-review"), window()),
        "accounts.security-history.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last(&fake);
    assert_eq!(
        path,
        format!("/internal/v1/operator/accounts/users/{USER}/security-events")
    );
    let q: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        q,
        [
            ("since", "2026-11-01T00:00:00Z"),
            ("until", "2026-11-10T00:00:00Z"),
            ("limit", "5"),
            ("cursor", "42")
        ]
    );
    assert_eq!(v["result"]["next_cursor"], "41");
    assert_no_leaks(&v);
}

#[tokio::test]
async fn meetings_space_and_meeting_search() {
    let (h, fake) = harness().await;
    let c = call(
        "/meetings/spaces",
        &format!("{}&q=weekly", scoped("support-review")),
        "meetings.spaces.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(last(&fake).1, "/internal/v1/operator/meetings/spaces");
    assert_eq!(v["result"]["items"][0]["meeting_code"], "abc-defg-hij");
    assert_no_leaks(&v);
    let c = call(
        "/meetings/meetings",
        &format!("{}&q=weekly", scoped("support-review")),
        "meetings.meetings.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(last(&fake).1, "/internal/v1/operator/meetings/conferences");
    let m = &v["result"]["items"][0];
    assert_eq!(
        (m["meeting_id"].as_str(), m["recording_state"].as_str()),
        (Some(ID), Some("IDLE"))
    );
    assert_no_leaks(&v);
}

#[tokio::test]
async fn meetings_detail_and_participants() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/meetings/meetings/{ID}"),
        &scoped("support-review"),
        "meetings.meetings.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["participants_admitted"], 3);
    assert_no_leaks(&v);
    let c = call(
        &format!("/meetings/meetings/{ID}/participants"),
        &scoped("support-review"),
        "meetings.participants.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        last(&fake).1,
        format!("/internal/v1/operator/meetings/conferences/{ID}/participants")
    );
    let p = &v["result"]["items"][0];
    assert_eq!(
        (p["identity_type"].as_str(), p["role"].as_str()),
        (Some("USER"), Some("HOST"))
    );
    assert_no_leaks(&v);
    // a meeting id that is not a UUID names nothing, no owner call
    let calls = fake.calls.lock().unwrap().len();
    let bad = call(
        "/meetings/meetings/abc-defg",
        &scoped("support-review"),
        "meetings.meetings.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &bad).await.1), "RESOURCE_NOT_FOUND");
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn meetings_live_list() {
    let (h, fake) = harness().await;
    let c = call(
        "/meetings/live",
        &scoped("ops-incident"),
        "meetings.live.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(last(&fake).1, "/internal/v1/operator/meetings/live");
    assert_eq!(v["result"]["items"][0]["status"], "LIVE");
}

#[tokio::test]
async fn meetings_audit_maps_window_and_time_cursor() {
    let (h, fake) = harness().await;
    let c = call(
        "/meetings/audit",
        &format!(
            "{}&cursor=1795000000000001.{ID}{}",
            scoped("ops-incident"),
            window()
        ),
        "meetings.audit.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last(&fake);
    assert_eq!(path, "/internal/v1/operator/meetings/audit-events");
    assert!(query.iter().any(|(k, v)| k == "cursor" && v.ends_with(ID)));
    assert_eq!(v["result"]["items"][0]["action"], "SCREEN_SHARE_STOP");
    assert_no_leaks(&v);
    let bad = call(
        "/meetings/audit",
        &format!("{}&cursor=12345{}", scoped("ops-incident"), window()),
        "meetings.audit.read",
        &["cfmeet.platform_ops"],
    );
    assert_eq!(code(&send(&h, &bad).await.1), "QUERY_INVALID");
}

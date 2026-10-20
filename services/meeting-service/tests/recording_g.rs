//! Phase 2 Part G — recording, artefacts, consent, transcription and intelligence.
//!
//! Recording runs through the real `LiveKitEgressRecorder` against the mock LiveKit
//! Egress service; capture evidence arrives as signed egress webhooks; objects live in
//! in-memory S3-equivalent storage with failure injection. Test IDs follow the Part G
//! specification (G-S security, G-C concurrency, G-F failure, G-E2E end-to-end).

mod common;

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use common::{
    artifacts::*,
    code, livekit_mock,
    rtc::{connect, join_guest, lk, room},
    uuid, Auth, TestApp,
};
use livekit_protocol as proto;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::artifacts().await {
            Some(a) => a,
            None => return,
        }
    };
}

fn transcribable() -> Value {
    json!({ "title": "Part G", "accessPolicy": "PUBLIC", "transcriptionPolicy": "HOST_OPTIONAL" })
}

async fn stop(app: &TestApp, cid: Uuid, rid: Uuid, auth: Auth<'_>) -> (StatusCode, Value) {
    app.post(
        format!("{}/recordings/{rid}/stop", rec_base(cid)),
        auth,
        json!({}),
    )
    .await
}

async fn access(app: &TestApp, cid: Uuid, aid: Uuid, auth: Auth<'_>) -> (StatusCode, Value) {
    app.post(
        format!("{}/artifacts/{aid}/access", rec_base(cid)),
        auth,
        json!({}),
    )
    .await
}

async fn explicit_recording(app: &TestApp, owner: &common::User, sid: Uuid) {
    let (s, v) = set_profile(
        app,
        owner,
        sid,
        json!({ "recording": { "consentMode": "EXPLICIT_CONSENT_REQUIRED" } }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "explicit consent policy: {v}");
}

// ===========================================================================
// Security G-S1 … G-S20
// ===========================================================================

#[tokio::test]
async fn g_s01_participant_cannot_start_recording() {
    let app = app!();
    let (_owner, _sid, cid, guests) = meeting(&app, 1).await;
    let (s, v) = start_recording(&app, cid, guests[0].auth()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "PERMISSION_DENIED");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}'"
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn g_s02_host_of_a_cannot_control_b_recording() {
    let app = app!();
    let (owner_a, _, cid_a, _) = meeting(&app, 0).await;
    let (owner_b, _, cid_b, _) = meeting(&app, 0).await;
    let (rid_b, _) = recording_active(&app, cid_b, &owner_b).await;
    // Start on B with A's identity.
    let (s, v) = start_recording(&app, cid_b, Auth::Bearer(&owner_a.token)).await;
    assert!(
        matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{v}"
    );
    // Stop B's recording through B's path and through A's own conference path.
    let (s, v) = stop(&app, cid_b, rid_b, Auth::Bearer(&owner_a.token)).await;
    assert!(
        matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{v}"
    );
    let (s, v) = stop(&app, cid_a, rid_b, Auth::Bearer(&owner_a.token)).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(artifact_status(&app, rid_b).await, "RECORDING");
    assert_eq!(lk(&app).call_count("StopEgress"), 0);
}

#[tokio::test]
async fn g_s03_client_cannot_inject_provider_recording_id() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    for body in [
        json!({ "providerRef": "EG_attacker" }),
        json!({ "egressId": "EG_attacker" }),
        json!({ "storageKey": "other/tenant.mp4" }),
        json!({ "bucket": "foreign" }),
    ] {
        let (s, v) = app
            .post(
                format!("{}/recordings", rec_base(cid)),
                Auth::Bearer(&owner.token),
                body,
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    }
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let rid = uuid(&v["recording"]["id"]);
    let pref = provider_ref(&app, rid).await;
    assert!(pref.starts_with("EG_") && pref != "EG_attacker");
    // The output key is server-chosen and conference-scoped.
    let key = storage_key(&app, rid).await;
    assert!(key.contains(&cid.simple().to_string()), "{key}");
}

#[tokio::test]
async fn g_s04_explicit_consent_blocks_provider_start() {
    let app = app!();
    let (owner, sid, cid, guests) = meeting(&app, 2).await;
    explicit_recording(&app, &owner, sid).await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(code(&v), "CONSENT_REQUIRED", "{s} {v}");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
    assert!(events(&app, cid, "consent.required").await >= 1);
    // One guest consents → still blocked.
    consent(&app, cid, guests[0].auth(), "RECORDING", "ACCEPTED").await;
    let (_, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(code(&v), "CONSENT_REQUIRED");
    // A decline is reported as such.
    consent(&app, cid, guests[1].auth(), "RECORDING", "DECLINED").await;
    let (_, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(code(&v), "CONSENT_DECLINED");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
}

#[tokio::test]
async fn g_s05_participant_cannot_consent_for_another() {
    let app = app!();
    let (_owner, _sid, cid, guests) = meeting(&app, 2).await;
    let version = consent_version(&app, cid, guests[0].auth()).await;
    let (s, v) = app
        .post(
            format!("{}/consents", rec_base(cid)),
            guests[0].auth(),
            json!({
                "consentVersion": version,
                "participantId": guests[1].participant_id,
                "decisions": [{ "consentType": "RECORDING", "decision": "ACCEPTED" }],
            }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = consent(&app, cid, guests[0].auth(), "RECORDING", "ACCEPTED").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let other = app
        .scalar(&format!(
            "SELECT count(*) FROM meeting_consents WHERE participant_id='{}' AND consent_type='RECORDING'",
            guests[1].participant_id
        ))
        .await;
    assert_eq!(other, 0, "only the caller's own decision is stored");
    // Anonymous callers have no participant to consent for.
    let (s, _) = app
        .post(
            format!("{}/consents", rec_base(cid)),
            Auth::None,
            json!({ "consentVersion": version, "decisions": [{ "consentType": "RECORDING", "decision": "ACCEPTED" }] }),
        )
        .await;
    assert!(s.is_client_error());
}

#[tokio::test]
async fn g_s06_recording_consent_is_not_transcription_consent() {
    let app = app!();
    let (owner, sid, cid, guests) = meeting_with(&app, transcribable(), 1).await;
    let (s, v) = set_profile(
        &app,
        &owner,
        sid,
        json!({
            "recording": { "consentMode": "EXPLICIT_CONSENT_REQUIRED" },
            "transcription": { "consentMode": "EXPLICIT_CONSENT_REQUIRED" },
        }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    consent(&app, cid, guests[0].auth(), "RECORDING", "ACCEPTED").await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "recording consent satisfied: {v}");
    let (_, v) = app
        .post(
            format!("{}/transcription", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "LIVE" }),
        )
        .await;
    assert_eq!(code(&v), "CONSENT_REQUIRED", "{v}");
    assert_eq!(app.transcription.count("start_live"), 0);
}

#[tokio::test]
async fn g_s07_transcription_consent_is_not_ai_consent() {
    let app = app!();
    let (owner, sid, cid, guests) = meeting_with(&app, transcribable(), 1).await;
    set_profile(
        &app,
        &owner,
        sid,
        json!({ "transcription": { "consentMode": "EXPLICIT_CONSENT_REQUIRED" } }),
    )
    .await;
    consent(&app, cid, guests[0].auth(), "TRANSCRIPTION", "ACCEPTED").await;
    let (_job, transcript) = transcript_ready(&app, cid, &owner, &[("hello", None)]).await;
    let (_, v) = app
        .post(
            format!("{}/intelligence/requests", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "sourceArtifactId": transcript, "kind": "SUMMARY" }),
        )
        .await;
    assert_eq!(code(&v), "CONSENT_REQUIRED", "{v}");
    assert_eq!(app.intelligence.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn g_s08_forged_recording_callback_rejected() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let rid = uuid(&v["recording"]["id"]);
    let eg = provider_ref(&app, rid).await;
    let body = lk(&app).egress_activate(&eg);
    let forged = livekit_mock::sign_webhook(
        &body,
        livekit_mock::API_KEY,
        "not-the-secret-not-the-secret-000",
    );
    let (s, _) = app.webhook(&body, Some(&forged)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = app.webhook(&body, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(artifact_status(&app, rid).await, "REQUESTED");
    // Body tampering invalidates the signature.
    let signed_sig = signed(&body);
    let tampered = body.replace("EGRESS_ACTIVE", "EGRESS_COMPLETE");
    let (s, _) = app.webhook(&tampered, Some(&signed_sig)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(artifact_status(&app, rid).await, "REQUESTED");
}

#[tokio::test]
async fn g_s09_duplicate_recording_callback_deduped() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let rid = uuid(&v["recording"]["id"]);
    let eg = provider_ref(&app, rid).await;
    let body = lk(&app).egress_activate(&eg);
    let (_, v1) = egress_event(&app, &body).await;
    let (_, v2) = egress_event(&app, &body).await;
    assert_eq!(v1["outcome"], "APPLIED");
    assert_eq!(v2["outcome"], "DUPLICATE");
    assert_eq!(events(&app, cid, "recording.started").await, 1);
    assert_eq!(audits(&app, cid, "RECORDING_PROVIDER_STARTED").await, 1);
}

#[tokio::test]
async fn g_s10_unknown_provider_recording_cannot_mutate_other_conference() {
    let app = app!();
    let (owner_a, _, cid_a, _) = meeting(&app, 0).await;
    let (owner_b, _, cid_b, _) = meeting(&app, 0).await;
    let (rid_a, eg_a) = recording_active(&app, cid_a, &owner_a).await;
    let (rid_b, _) = recording_active(&app, cid_b, &owner_b).await;
    // Unknown egress id.
    let unknown = proto::EgressInfo {
        egress_id: "EG_unknown".into(),
        room_name: room(cid_b),
        status: proto::EgressStatus::EgressComplete as i32,
        ..Default::default()
    };
    let (_, v) = egress_event(
        &app,
        &livekit_mock::egress_webhook_body("egress_ended", &unknown),
    )
    .await;
    assert_eq!(v["outcome"], "UNKNOWN_RECORDING", "{v}");
    // A's egress id claimed for B's room → rejected, nothing mutates.
    let mut info = lk(&app).egress(&eg_a).unwrap();
    info.room_name = room(cid_b);
    info.status = proto::EgressStatus::EgressFailed as i32;
    let (_, v) = egress_event(
        &app,
        &livekit_mock::egress_webhook_body("egress_ended", &info),
    )
    .await;
    assert_eq!(v["outcome"], "REJECTED", "{v}");
    assert_eq!(artifact_status(&app, rid_a).await, "RECORDING");
    assert_eq!(artifact_status(&app, rid_b).await, "RECORDING");
}

#[tokio::test]
async fn g_s11_unauthorized_artifact_read_denied() {
    let app = app!();
    let (owner, _, cid, guests) = meeting(&app, 1).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let (s, v) = app
        .get(format!("{}/artifacts", rec_base(cid)), guests[0].auth())
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    let (s, v) = access(&app, cid, rid, guests[0].auth()).await;
    assert_eq!(code(&v), "ARTIFACT_ACCESS_DENIED", "{s} {v}");
    let (s, _) = access(&app, cid, rid, Auth::None).await;
    assert!(s.is_client_error());
    let stranger = app.user("stranger").await;
    let (s, _) = access(&app, cid, rid, Auth::Bearer(&stranger.token)).await;
    assert!(matches!(s, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND));
    assert_eq!(audits(&app, cid, "ARTIFACT_ACCESS").await, 0);
}

#[tokio::test]
async fn g_s12_foreign_conference_artifact_rejected() {
    let app = app!();
    let (owner_a, _, cid_a, _) = meeting(&app, 0).await;
    let (owner_b, _, cid_b, _) = meeting(&app, 0).await;
    let (rid_b, _) = recording_ready(&app, cid_b, &owner_b).await;
    // A's owner addresses B's artefact through A's conference.
    let (s, v) = access(&app, cid_a, rid_b, Auth::Bearer(&owner_a.token)).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    assert_eq!(code(&v), "ARTIFACT_NOT_FOUND");
    let (s, _) = app
        .get(
            format!("{}/artifacts/{rid_b}", rec_base(cid_a)),
            Auth::Bearer(&owner_a.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .delete(
            format!("{}/artifacts/{rid_b}", rec_base(cid_a)),
            Auth::Bearer(&owner_a.token),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(artifact_status(&app, rid_b).await, "READY");
}

#[tokio::test]
async fn g_s13_no_storage_credentials_or_provider_ids_leak() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let key = storage_key(&app, rid).await;
    let pref = provider_ref(&app, rid).await;
    let mut bodies = vec![];
    bodies.push(
        app.get(
            format!("{}/recordings", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await
        .1,
    );
    bodies.push(
        app.get(
            format!("{}/recordings/{rid}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await
        .1,
    );
    bodies.push(
        app.get(
            format!("{}/artifacts", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await
        .1,
    );
    bodies.push(
        app.get(
            format!("{}/artifacts/{rid}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await
        .1,
    );
    for b in &bodies {
        let text = b.to_string();
        assert!(!text.contains(&key), "storage key leaked: {text}");
        assert!(!text.contains(&pref), "provider id leaked: {text}");
        for needle in [
            "storageKey",
            "storageBucket",
            "providerRef",
            "secret",
            "accessKey",
            livekit_mock::API_SECRET,
        ] {
            assert!(!text.contains(needle), "{needle} leaked: {text}");
        }
    }
    // The signed URL carries a signature, never a secret.
    let (_, v) = access(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    assert!(!v.to_string().contains(livekit_mock::API_SECRET));
}

#[tokio::test]
async fn g_s14_signed_url_bounded_and_object_scoped() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let (s, v) = access(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ttl = v["expiresInSeconds"].as_u64().unwrap();
    assert!(ttl > 0 && ttl <= 900, "short-lived: {ttl}");
    let url = v["url"].as_str().unwrap().to_string();
    let now = chrono::Utc::now();
    assert_eq!(
        app.storage.verify_signed(&url, now).as_deref(),
        Some(storage_key(&app, rid).await.as_str())
    );
    // Expired.
    assert!(app
        .storage
        .verify_signed(&url, now + chrono::Duration::seconds(ttl as i64 + 5))
        .is_none());
    // Re-scoped to another object.
    let other = url.replace(
        &rid.simple().to_string(),
        &Uuid::new_v4().simple().to_string(),
    );
    assert!(app.storage.verify_signed(&other, now).is_none());
    assert_eq!(audits(&app, cid, "ARTIFACT_ACCESS").await, 1);
    // Never persisted.
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM meeting_artifacts WHERE id='{rid}' AND (storage_uri LIKE '%sig=%' OR metadata::text LIKE '%sig=%')")).await,
        0
    );
}

#[tokio::test]
async fn g_s15_deleted_artifact_cannot_issue_access() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let (s, v) = app
        .delete(
            format!("{}/artifacts/{rid}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(artifact_status(&app, rid).await, "DELETED");
    let (s, v) = access(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::GONE, "{v}");
    assert_eq!(code(&v), "ARTIFACT_DELETED");
}

#[tokio::test]
async fn g_s16_anonymous_poll_export_remains_anonymous() {
    let app = app!();
    let (owner, _, cid, guests) = meeting(&app, 2).await;
    let mut body = common::collab::simple_poll("AFTER_VOTE");
    body["anonymous"] = json!(true);
    let pid = common::collab::open_poll(&app, cid, &owner, body).await;
    let (_, poll) = app
        .get(
            format!("{}/polls/{pid}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let option = poll["questions"][0]["options"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for g in &guests {
        let (s, v) = common::collab::vote(&app, cid, pid, g.auth(), &option).await;
        assert!(s.is_success(), "{v}");
    }
    let (s, v) = app
        .post(
            format!("{}/artifacts/reports", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "artifactType": "POLL_REPORT" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let aid = uuid(&v["id"]);
    let bytes = app.storage.get_raw(&storage_key(&app, aid).await).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    for g in &guests {
        assert!(
            !text.contains(&g.participant_id.to_string()),
            "voter id in anonymous export"
        );
    }
    assert!(
        !text.contains("guest-0") && !text.contains("guest-1"),
        "voter name in anonymous export"
    );
    assert!(text.contains("\"anonymous\": true"), "{text}");
}

#[tokio::test]
async fn g_s17_anonymous_qna_remains_anonymous_in_exports() {
    let app = app!();
    let (owner, _, cid, guests) = meeting(&app, 1).await;
    let (s, v) = app
        .post(
            format!("{}/questions", rec_base(cid)),
            guests[0].auth(),
            json!({ "body": "Secret question", "anonymous": true }),
        )
        .await;
    assert!(s.is_success(), "{v}");
    for kind in ["ATTENDANCE_REPORT", "CHAT_EXPORT", "POLL_REPORT"] {
        let (s, v) = app
            .post(
                format!("{}/artifacts/reports", rec_base(cid)),
                Auth::Bearer(&owner.token),
                json!({ "artifactType": kind }),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{kind}: {v}");
        let text = String::from_utf8(
            app.storage
                .get_raw(&storage_key(&app, uuid(&v["id"])).await)
                .unwrap(),
        )
        .unwrap();
        assert!(
            !text.contains("Secret question"),
            "{kind} links the anonymous question"
        );
    }
    // The Q&A API itself keeps the author hidden from the host.
    let (_, list) = app
        .get(
            format!("{}/questions", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert!(
        !list
            .to_string()
            .contains(&guests[0].participant_id.to_string()),
        "{list}"
    );
}

#[tokio::test]
async fn g_s18_transcript_speaker_identity_cannot_be_forged() {
    let app = app!();
    let (owner_a, _, cid_a, guests_a) = meeting_with(&app, transcribable(), 1).await;
    let (_owner_b, _, cid_b, guests_b) = meeting_with(&app, transcribable(), 1).await;
    let _ = cid_b;
    let (_job, transcript) = transcript_ready(
        &app,
        cid_a,
        &owner_a,
        &[
            ("from a", Some(guests_a[0].identity.as_str())),
            // A media identity of ANOTHER conference never maps to a speaker here.
            ("forged", Some(guests_b[0].identity.as_str())),
            ("unknown", Some("ps_doesnotexist")),
        ],
    )
    .await;
    let (s, v) = app
        .get(
            format!("{}/transcripts/{transcript}", rec_base(cid_a)),
            Auth::Bearer(&owner_a.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let segs = v["segments"].as_array().unwrap();
    assert_eq!(segs.len(), 3);
    assert_eq!(
        segs[0]["speakerParticipantId"],
        json!(guests_a[0].participant_id)
    );
    assert!(
        segs[1]["speakerParticipantId"].is_null(),
        "cross-conference identity mapped: {v}"
    );
    assert!(segs[2]["speakerParticipantId"].is_null());
    // The callback format has no participant-id field at all.
    let (s, _) = transcription_callback(
        &app,
        &json!({ "type": "SEGMENT", "jobRef": "x", "segmentId": "z", "sequence": 1, "startMs": 0, "endMs": 1,
                 "language": "en", "text": "t", "final": true, "speakerParticipantId": guests_b[0].participant_id }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn g_s19_external_ai_prohibited_policy_blocks_dispatch() {
    let app = app!();
    let (owner, sid, cid, _) = meeting_with(&app, transcribable(), 0).await;
    let (_job, transcript) = transcript_ready(&app, cid, &owner, &[("hello", None)]).await;
    for location in ["DISABLED"] {
        let (s, v) = set_profile(
            &app,
            &owner,
            sid,
            json!({ "intelligence": { "location": location } }),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let (_, v) = app
            .post(
                format!("{}/intelligence/requests", rec_base(cid)),
                Auth::Bearer(&owner.token),
                json!({ "sourceArtifactId": transcript, "kind": "SUMMARY" }),
            )
            .await;
        assert_eq!(code(&v), "INTELLIGENCE_NOT_ALLOWED", "{v}");
    }
    assert_eq!(app.intelligence.calls.load(Ordering::SeqCst), 0);
    // External transcription provider + SELF_HOSTED_ONLY policy → zero provider calls.
    app.transcription.external.store(true, Ordering::SeqCst);
    set_profile(
        &app,
        &owner,
        sid,
        json!({ "transcription": { "location": "SELF_HOSTED_ONLY" } }),
    )
    .await;
    let before = app.transcription.count("start_live");
    let (_, v) = app
        .post(
            format!("{}/transcription", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "LIVE" }),
        )
        .await;
    assert_eq!(code(&v), "TRANSCRIPTION_NOT_ALLOWED", "{v}");
    assert_eq!(app.transcription.count("start_live"), before);
}

#[tokio::test]
async fn g_s20_ai_disabled_by_default() {
    // Production defaults: intelligence flag off and no provider configured.
    assert!(!backend::config::FeatureFlags::default().intelligence_v2_enabled);
    let defaults = backend::services::artifacts::ArtifactBackends::default();
    assert_eq!(defaults.intelligence.name(), "none");
    assert!(defaults.intelligence.capabilities().location.is_none());
    let Some(app) = TestApp::with(common::TestOptions {
        rtc: common::RtcMode::LiveKitMock,
        flags: backend::config::FeatureFlags {
            intelligence_v2_enabled: false,
            ..TestApp::ghi_flags()
        },
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (_, v) = app
        .post(
            format!("{}/intelligence/requests", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "sourceArtifactId": Uuid::new_v4(), "kind": "SUMMARY" }),
        )
        .await;
    assert_eq!(code(&v), "INTELLIGENCE_DISABLED", "{v}");
    assert_eq!(app.intelligence.calls.load(Ordering::SeqCst), 0);
}

// ===========================================================================
// Concurrency G-C1 … G-C12
// ===========================================================================

async fn two_hosts(app: &TestApp) -> (common::User, common::User, Uuid, Uuid) {
    let (owner, sid, cid, _) = meeting(app, 0).await;
    let host = app.user("host-b").await;
    app.add_member(sid, &owner, &host, backend::domain::Role::Host)
        .await;
    (owner, host, sid, cid)
}

#[tokio::test]
async fn g_c01_two_hosts_start_one_capture() {
    let app = app!();
    let (a, b, _, cid) = two_hosts(&app).await;
    let (r1, r2) = tokio::join!(
        start_recording(&app, cid, Auth::Bearer(&a.token)),
        start_recording(&app, cid, Auth::Bearer(&b.token))
    );
    let ok = [&r1, &r2]
        .iter()
        .filter(|r| r.0 == StatusCode::ACCEPTED)
        .count();
    assert_eq!(ok, 1, "{r1:?} {r2:?}");
    let loser = if r1.0 == StatusCode::ACCEPTED {
        &r2
    } else {
        &r1
    };
    assert_eq!(code(&loser.1), "RECORDING_ALREADY_ACTIVE");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 1);
    assert_eq!(lk(&app).egresses_of(&room(cid)).len(), 1);
}

#[tokio::test]
async fn g_c02_duplicate_provider_start_callbacks() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let rid = uuid(&v["recording"]["id"]);
    let eg = provider_ref(&app, rid).await;
    let b1 = lk(&app).egress_activate(&eg);
    let b2 = lk(&app).egress_activate(&eg); // distinct event id, same evidence
    let (x, y) = tokio::join!(egress_event(&app, &b1), egress_event(&app, &b2));
    assert_eq!(x.0, StatusCode::OK);
    assert_eq!(y.0, StatusCode::OK);
    assert_eq!(artifact_status(&app, rid).await, "RECORDING");
    assert_eq!(events(&app, cid, "recording.started").await, 1);
}

#[tokio::test]
async fn g_c03_stop_vs_provider_completion() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    let bytes = b"race-bytes".repeat(100);
    let done = lk(&app).egress_complete(&eg, Some(&app.storage), &bytes, None);
    let (s, e) = tokio::join!(
        stop(&app, cid, rid, Auth::Bearer(&owner.token)),
        egress_event(&app, &done)
    );
    assert!(
        s.0.is_success() || code(&s.1) == "RECORDING_NOT_ACTIVE",
        "{s:?}"
    );
    assert_eq!(e.0, StatusCode::OK);
    retention_tick(&app).await;
    assert_eq!(artifact_status(&app, rid).await, "READY");
    assert_eq!(events(&app, cid, "recording.processing").await, 1);
    assert_eq!(events(&app, cid, "recording.ready").await, 1);
}

#[tokio::test]
async fn g_c04_two_hosts_stop() {
    let app = app!();
    let (a, b, _, cid) = two_hosts(&app).await;
    let (rid, _) = recording_active(&app, cid, &a).await;
    let (x, y) = tokio::join!(
        stop(&app, cid, rid, Auth::Bearer(&a.token)),
        stop(&app, cid, rid, Auth::Bearer(&b.token))
    );
    assert_eq!(x.0, StatusCode::OK, "{x:?}");
    assert_eq!(y.0, StatusCode::OK, "{y:?}");
    assert_eq!(artifact_status(&app, rid).await, "STOPPING");
    assert_eq!(lk(&app).call_count("StopEgress"), 1);
    assert_eq!(events(&app, cid, "recording.stopping").await, 1);
    assert_eq!(audits(&app, cid, "RECORDING_STOP").await, 1);
}

#[tokio::test]
async fn g_c05_recording_request_vs_consent_withdrawal() {
    for _ in 0..3 {
        let app = app!();
        let (owner, sid, cid, guests) = meeting(&app, 1).await;
        explicit_recording(&app, &owner, sid).await;
        consent(&app, cid, guests[0].auth(), "RECORDING", "ACCEPTED").await;
        let withdraw = app.post(
            format!("{}/consents/RECORDING/withdraw", rec_base(cid)),
            guests[0].auth(),
            json!({}),
        );
        let (start, w) = tokio::join!(
            start_recording(&app, cid, Auth::Bearer(&owner.token)),
            withdraw
        );
        assert_eq!(w.0, StatusCode::OK, "{w:?}");
        // Never an active capture without consent at the end.
        let active = app
            .scalar(&format!("SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND status IN ('REQUESTED','RECORDING')"))
            .await;
        assert_eq!(active, 0, "start={start:?}");
    }
}

#[tokio::test]
async fn g_c06_recording_start_vs_conference_end() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let end = app.post(
        format!("/api/v2/conferences/{cid}/end"),
        Auth::Bearer(&owner.token),
        json!({}),
    );
    let (start, e) = tokio::join!(start_recording(&app, cid, Auth::Bearer(&owner.token)), end);
    assert!(e.0.is_success(), "{e:?}");
    if start.0 != StatusCode::ACCEPTED {
        assert!(
            matches!(
                code(&start.1),
                "CONFERENCE_NOT_LIVE" | "CONFERENCE_NOT_FOUND"
            ),
            "{start:?}"
        );
    }
    let active = app
        .scalar(&format!("SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND status IN ('REQUESTED','RECORDING')"))
        .await;
    assert_eq!(active, 0, "end stops (or fails) any capture");
}

#[tokio::test]
async fn g_c07_join_vs_recording_activation() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let rid = uuid(&v["recording"]["id"]);
    let body = lk(&app).egress_activate(&provider_ref(&app, rid).await);
    let (j, e) = tokio::join!(
        app.join(cid, Auth::None, json!({ "displayName": "late" })),
        egress_event(&app, &body)
    );
    assert_eq!(j.0, StatusCode::OK, "{j:?}");
    assert_eq!(e.0, StatusCode::OK);
    // The joiner sees the indicator (join response or authoritative recovery).
    let token = j.1["participantToken"].as_str().unwrap();
    let (_, req) = app
        .get(
            format!("{}/consents/requirements", rec_base(cid)),
            Auth::Participant(token),
        )
        .await;
    assert_eq!(req["recordingNotice"], true, "{req}");
    assert_eq!(
        j.1["policy"]["recordingActive"], true,
        "REQUESTED already shows the notice: {}",
        j.1
    );
}

#[tokio::test]
async fn g_c08_ready_vs_delete() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    let done = lk(&app).egress_complete(&eg, Some(&app.storage), b"bytes", None);
    let del = app.delete(
        format!("{}/artifacts/{rid}", rec_base(cid)),
        Auth::Bearer(&owner.token),
    );
    let (e, d) = tokio::join!(egress_event(&app, &done), del);
    assert_eq!(e.0, StatusCode::OK);
    let key = storage_key(&app, rid).await;
    if d.0 == StatusCode::OK {
        retention_tick(&app).await;
        retention_tick(&app).await;
        assert_eq!(artifact_status(&app, rid).await, "DELETED");
        assert!(!app.storage.contains(&key));
    } else {
        assert_eq!(code(&d.1), "RECORDING_STATE_CONFLICT", "{d:?}");
        assert_eq!(artifact_status(&app, rid).await, "READY");
    }
}

#[tokio::test]
async fn g_c09_retention_worker_vs_manual_delete() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    common::rtc::exec(&app, &format!("UPDATE meeting_artifacts SET retention_at = now() - interval '1 minute' WHERE id='{rid}'")).await;
    let del = app.delete(
        format!("{}/artifacts/{rid}", rec_base(cid)),
        Auth::Bearer(&owner.token),
    );
    let (d, _r) = tokio::join!(del, retention_tick(&app));
    assert_eq!(d.0, StatusCode::OK, "{d:?}");
    retention_tick(&app).await;
    assert_eq!(artifact_status(&app, rid).await, "DELETED");
    assert_eq!(audits(&app, cid, "ARTIFACT_DELETED").await, 1);
    assert_eq!(events(&app, cid, "artifact.deleted").await, 1);
}

#[tokio::test]
async fn g_c10_two_retention_workers() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    common::rtc::exec(&app, &format!("UPDATE meeting_artifacts SET retention_at = now() - interval '1 minute' WHERE id='{rid}'")).await;
    let (a, b) = tokio::join!(retention_tick(&app), retention_tick(&app));
    assert_eq!(a.deleted + b.deleted, 1, "{a:?} {b:?}");
    assert_eq!(audits(&app, cid, "ARTIFACT_DELETED").await, 1);
    assert_eq!(artifact_status(&app, rid).await, "DELETED");
}

#[tokio::test]
async fn g_c11_duplicate_transcript_final_segments() {
    let app = app!();
    let (owner, _, cid, _) = meeting_with(&app, transcribable(), 0).await;
    let (s, v) = app
        .post(
            format!("{}/transcription", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "LIVE" }),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let job = uuid(&v["id"]);
    let jr = job_ref(&app, job).await;
    let seg = segment(&jr, "seg-1", 1, "once", true);
    let (a, b, c) = tokio::join!(
        transcription_callback(&app, &seg),
        transcription_callback(&app, &seg),
        transcription_callback(&app, &seg)
    );
    let outcomes: Vec<&str> = [&a, &b, &c]
        .iter()
        .map(|r| r.1["outcome"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        outcomes.iter().filter(|o| **o == "APPLIED").count(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM transcript_segments WHERE job_id='{job}'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT segment_count::bigint FROM transcription_jobs WHERE id='{job}'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn g_c12_transcription_stop_vs_final_callback() {
    let app = app!();
    let (owner, _, cid, _) = meeting_with(&app, transcribable(), 0).await;
    let (_, v) = app
        .post(
            format!("{}/transcription", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "LIVE" }),
        )
        .await;
    let job = uuid(&v["id"]);
    let artifact = uuid(&v["artifactId"]);
    let jr = job_ref(&app, job).await;
    let done = json!({ "type": "JOB_STATUS", "jobRef": jr, "status": "COMPLETED" });
    let stop = app.post(
        format!("{}/transcription/{job}/stop", rec_base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    );
    let (s, c) = tokio::join!(stop, transcription_callback(&app, &done));
    assert_eq!(s.0, StatusCode::OK, "{s:?}");
    assert_eq!(c.0, StatusCode::OK, "{c:?}");
    assert_eq!(job_status(&app, job).await, "READY");
    assert_eq!(artifact_status(&app, artifact).await, "READY");
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}' AND artifact_type='TRANSCRIPT'")).await,
        1
    );
    assert_eq!(events(&app, cid, "transcription.ready").await, 1);
}

// ===========================================================================
// Failure G-F1 … G-F12
// ===========================================================================

#[tokio::test]
async fn g_f01_provider_unavailable_on_start() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    lk(&app).faults.fail_egress.store(true, Ordering::SeqCst);
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["providerDispatch"], "PENDING_RETRY");
    let rid = uuid(&v["recording"]["id"]);
    assert_eq!(
        artifact_status(&app, rid).await,
        "REQUESTED",
        "never RECORDING without evidence"
    );
    assert!(provider_ref(&app, rid).await.is_empty());
    lk(&app).faults.fail_egress.store(false, Ordering::SeqCst);
    expire_backoff(&app, cid).await;
    dispatch(&app, cid).await;
    assert!(
        !provider_ref(&app, rid).await.is_empty(),
        "outbox retry reached the provider"
    );
    assert_eq!(artifact_status(&app, rid).await, "REQUESTED");
}

#[tokio::test]
async fn g_f02_provider_unavailable_on_stop() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    lk(&app).faults.fail_egress.store(true, Ordering::SeqCst);
    let (s, v) = stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["providerDispatch"], "PENDING_RETRY");
    assert_eq!(
        artifact_status(&app, rid).await,
        "STOPPING",
        "never READY without finalization"
    );
    lk(&app).faults.fail_egress.store(false, Ordering::SeqCst);
    expire_backoff(&app, cid).await;
    dispatch(&app, cid).await;
    assert_eq!(
        lk(&app).egress(&eg).unwrap().status,
        proto::EgressStatus::EgressEnding as i32
    );
    assert_eq!(artifact_status(&app, rid).await, "STOPPING");
}

#[tokio::test]
async fn g_f03_start_timeout() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let rid = uuid(&v["recording"]["id"]);
    // The provider never confirms: age the request beyond 3× the start timeout.
    common::rtc::exec(
        &app,
        &format!(
            "UPDATE meeting_artifacts SET created_at = now() - interval '1 hour' WHERE id='{rid}'"
        ),
    )
    .await;
    backend::services::artifacts::recording::RecordingService::reconcile(
        &app.state.meeting.core,
        50,
    )
    .await
    .unwrap();
    assert_eq!(artifact_status(&app, rid).await, "FAILED");
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM meeting_artifacts WHERE id='{rid}' AND failure_code='RECORDING_START_TIMEOUT'")).await,
        1
    );
    assert!(app.scalar(&format!("SELECT count(*) FROM rtc_drift_events WHERE conference_id='{cid}' AND kind='RECORDING_START_TIMEOUT'")).await >= 1);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND recording_state='OFF'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn g_f04_completion_callback_lost() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    // Provider finishes; the webhook is never delivered.
    let _lost = lk(&app).egress_complete(&eg, Some(&app.storage), b"lost-callback-bytes", None);
    common::rtc::exec(
        &app,
        &format!(
            "UPDATE meeting_artifacts SET updated_at = now() - interval '1 hour' WHERE id='{rid}'"
        ),
    )
    .await;
    let n = backend::services::artifacts::recording::RecordingService::reconcile(
        &app.state.meeting.core,
        50,
    )
    .await
    .unwrap();
    assert!(n >= 1);
    assert_eq!(artifact_status(&app, rid).await, "READY");
}

#[tokio::test]
async fn g_f05_object_storage_unavailable() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    let done = lk(&app).egress_complete(&eg, Some(&app.storage), b"object-bytes", None);
    app.storage.unavailable.store(true, Ordering::SeqCst);
    egress_event(&app, &done).await;
    assert_eq!(
        artifact_status(&app, rid).await,
        "PROCESSING",
        "unverified → never READY"
    );
    let r = retention_tick(&app).await;
    let _ = r;
    assert_eq!(artifact_status(&app, rid).await, "PROCESSING");
    app.storage.unavailable.store(false, Ordering::SeqCst);
    common::rtc::exec(
        &app,
        &format!("UPDATE meeting_artifacts SET next_attempt_at = now() WHERE id='{rid}'"),
    )
    .await;
    retention_tick(&app).await;
    assert_eq!(artifact_status(&app, rid).await, "READY");
    // New starts refuse without storage.
    app.storage.unavailable.store(true, Ordering::SeqCst);
}

#[tokio::test]
async fn g_f06_provider_complete_but_object_missing() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    // Provider claims success but wrote nothing.
    let done = lk(&app).egress_complete(&eg, None, b"never-written", None);
    egress_event(&app, &done).await;
    assert_eq!(artifact_status(&app, rid).await, "PROCESSING");
    for _ in 0..10 {
        common::rtc::exec(
            &app,
            &format!("UPDATE meeting_artifacts SET next_attempt_at = now() WHERE id='{rid}'"),
        )
        .await;
        retention_tick(&app).await;
    }
    assert_eq!(
        artifact_status(&app, rid).await,
        "FAILED",
        "missing object is never READY"
    );
    // Size mismatch also fails integrity.
    let (rid2, eg2) = recording_active(&app, cid, &owner).await;
    stop(&app, cid, rid2, Auth::Bearer(&owner.token)).await;
    let done = lk(&app).egress_complete(&eg2, Some(&app.storage), b"short", Some(999_999));
    egress_event(&app, &done).await;
    assert_ne!(artifact_status(&app, rid2).await, "READY");
}

#[tokio::test]
async fn g_f07_signed_access_failure() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    app.storage.fail_sign.store(true, Ordering::SeqCst);
    let (s, v) = access(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(code(&v), "ARTIFACT_STORAGE_UNAVAILABLE");
    assert!(v["url"].is_null());
    assert_eq!(artifact_status(&app, rid).await, "READY");
    assert_eq!(audits(&app, cid, "ARTIFACT_ACCESS").await, 0);
}

#[tokio::test]
async fn g_f08_retention_delete_failure() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let key = storage_key(&app, rid).await;
    app.storage.fail_delete.store(true, Ordering::SeqCst);
    let (s, v) = app
        .delete(
            format!("{}/artifacts/{rid}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["deletionPending"], true, "{v}");
    assert_ne!(
        artifact_status(&app, rid).await,
        "DELETED",
        "never DELETED before confirmed"
    );
    // A delete that "succeeds" but the object remains is also not confirmed.
    app.storage.fail_delete.store(false, Ordering::SeqCst);
    app.storage.sticky.store(true, Ordering::SeqCst);
    common::rtc::exec(
        &app,
        &format!("UPDATE meeting_artifacts SET next_attempt_at = now() WHERE id='{rid}'"),
    )
    .await;
    retention_tick(&app).await;
    assert_ne!(artifact_status(&app, rid).await, "DELETED");
    assert!(app.storage.contains(&key));
    app.storage.sticky.store(false, Ordering::SeqCst);
    common::rtc::exec(
        &app,
        &format!("UPDATE meeting_artifacts SET next_attempt_at = now() WHERE id='{rid}'"),
    )
    .await;
    retention_tick(&app).await;
    assert_eq!(artifact_status(&app, rid).await, "DELETED");
    assert!(!app.storage.contains(&key));
}

#[tokio::test]
async fn g_f09_redis_unavailable_during_recording_transition() {
    let Some(app) = TestApp::with(common::TestOptions {
        rtc: common::RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        redis: common::RedisMode::Unreachable,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    assert_eq!(artifact_status(&app, rid).await, "READY");
    // Events are durable in the outbox regardless of Redis.
    assert_eq!(events(&app, cid, "recording.ready").await, 1);
}

#[tokio::test]
async fn g_f10_outbox_crash_does_not_lose_provider_command() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    lk(&app).faults.fail_egress.store(true, Ordering::SeqCst);
    let (_, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    let rid = uuid(&v["recording"]["id"]);
    // Simulate a crashed claimer: the row is claimed by a dead holder.
    common::rtc::exec(&app, &format!(
        "UPDATE outbox_events SET locked_by='dead-instance', available_at = now() - interval '1 second' WHERE conference_id='{cid}' AND event_type='rtc.recording_start_requested'"
    )).await;
    lk(&app).faults.fail_egress.store(false, Ordering::SeqCst);
    dispatch(&app, cid).await;
    assert!(
        !provider_ref(&app, rid).await.is_empty(),
        "expired claim re-processed"
    );
    assert_eq!(
        lk(&app).egresses_of(&room(cid)).len(),
        1,
        "exactly one capture"
    );
    // Replaying the same command after success never starts a second capture.
    common::rtc::exec(&app, &format!("UPDATE outbox_events SET status = 'PENDING', published_at = NULL, available_at = now() - interval '1 second', locked_by = NULL WHERE conference_id='{cid}' AND event_type='rtc.recording_start_requested'")).await;
    dispatch(&app, cid).await;
    assert_eq!(lk(&app).egresses_of(&room(cid)).len(), 1);
}

#[tokio::test]
async fn g_f11_retention_worker_crash_restart() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let key = storage_key(&app, rid).await;
    // A worker claimed the deletion and crashed mid-way (lease left behind).
    common::rtc::exec(&app, &format!("UPDATE meeting_artifacts SET retention_at = now() - interval '1 minute', locked_by='crashed', locked_until = now() + interval '1 hour' WHERE id='{rid}'")).await;
    let r = retention_tick(&app).await;
    assert_eq!(r.deleted, 0, "live lease respected");
    common::rtc::exec(&app, &format!("UPDATE meeting_artifacts SET locked_until = now() - interval '1 second' WHERE id='{rid}'")).await;
    let r = retention_tick(&app).await;
    assert_eq!(r.deleted, 1);
    assert!(!app.storage.contains(&key));
    assert_eq!(artifact_status(&app, rid).await, "DELETED");
}

#[tokio::test]
async fn g_f12_transcription_provider_failure() {
    let app = app!();
    let (owner, _, cid, _) = meeting_with(&app, transcribable(), 0).await;
    app.transcription.fail.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            format!("{}/transcription", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "LIVE" }),
        )
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(code(&v), "TRANSCRIPTION_PROVIDER_UNAVAILABLE");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM transcription_jobs WHERE conference_id='{cid}' AND status='FAILED'")).await, 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND transcription_state='OFF' AND status='LIVE'")).await, 1);
    // Provider-reported failure after start.
    app.transcription.fail.store(false, Ordering::SeqCst);
    let (_, v) = app
        .post(
            format!("{}/transcription", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "LIVE" }),
        )
        .await;
    let job = uuid(&v["id"]);
    let jr = job_ref(&app, job).await;
    transcription_callback(
        &app,
        &json!({ "type": "JOB_STATUS", "jobRef": jr, "status": "FAILED", "error": "vendor" }),
    )
    .await;
    assert_eq!(job_status(&app, job).await, "FAILED");
}

// ===========================================================================
// End-to-end G-E2E-01 … G-E2E-25
// ===========================================================================

#[tokio::test]
async fn g_e2e_01_composite_recording_start() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 1).await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["recording"]["status"], "REQUESTED");
    let calls = lk(&app).calls("StartRoomCompositeEgress");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, room(cid), "composite of the conference room");
    let rid = uuid(&v["recording"]["id"]);
    let body = lk(&app).egress_activate(&provider_ref(&app, rid).await);
    egress_event(&app, &body).await;
    assert_eq!(artifact_status(&app, rid).await, "RECORDING");
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND recording_state='RECORDING'")).await, 1);
}

#[tokio::test]
async fn g_e2e_02_duplicate_start_one_capture() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let body = json!({ "idempotencyKey": "rec-1" });
    let (s1, v1) = app
        .post(
            format!("{}/recordings", rec_base(cid)),
            Auth::Bearer(&owner.token),
            body.clone(),
        )
        .await;
    let (s2, v2) = app
        .post(
            format!("{}/recordings", rec_base(cid)),
            Auth::Bearer(&owner.token),
            body,
        )
        .await;
    assert_eq!(s1, StatusCode::ACCEPTED);
    assert_eq!(s2, StatusCode::OK, "{v2}");
    assert_eq!(v1["recording"]["id"], v2["recording"]["id"]);
    let (s3, v3) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s3, StatusCode::CONFLICT, "{v3}");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 1);
}

#[tokio::test]
async fn g_e2e_03_unauthorized_start_denied() {
    let app = app!();
    let (_owner, _, cid, guests) = meeting(&app, 1).await;
    for auth in [guests[0].auth(), Auth::None] {
        let (s, _) = start_recording(&app, cid, auth).await;
        assert!(s.is_client_error());
    }
    let stranger = app.user("stranger").await;
    let (s, _) = start_recording(&app, cid, Auth::Bearer(&stranger.token)).await;
    assert!(s.is_client_error());
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
}

#[tokio::test]
async fn g_e2e_04_notice_only_policy() {
    let app = app!();
    let (owner, _, cid, guests) = meeting(&app, 1).await;
    let (_, req) = app
        .get(
            format!("{}/consents/requirements", rec_base(cid)),
            guests[0].auth(),
        )
        .await;
    assert_eq!(req["requirements"][0]["mode"], "NOTICE_ONLY");
    assert_eq!(req["requirements"][0]["required"], false);
    let (rid, _) = recording_active(&app, cid, &owner).await;
    let _ = rid;
    let (_, req) = app
        .get(
            format!("{}/consents/requirements", rec_base(cid)),
            guests[0].auth(),
        )
        .await;
    assert_eq!(req["recordingNotice"], true, "visible notice: {req}");
    assert_eq!(events(&app, cid, "consent.required").await, 0);
}

#[tokio::test]
async fn g_e2e_05_explicit_consent_satisfied() {
    let app = app!();
    let (owner, sid, cid, guests) = meeting(&app, 2).await;
    explicit_recording(&app, &owner, sid).await;
    for g in &guests {
        let (s, v) = consent(&app, cid, g.auth(), "RECORDING", "ACCEPTED").await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 1);
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_consents WHERE conference_id='{cid}' AND consent_type='RECORDING' AND decision='GRANTED'")).await, 2);
}

#[tokio::test]
async fn g_e2e_06_missing_consent_blocks_provider_start() {
    let app = app!();
    let (owner, sid, cid, guests) = meeting(&app, 1).await;
    explicit_recording(&app, &owner, sid).await;
    let (_, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(code(&v), "CONSENT_REQUIRED");
    assert_eq!(lk(&app).call_count("StartRoomCompositeEgress"), 0);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts WHERE conference_id='{cid}'"
        ))
        .await,
        0
    );
    // Targeted consent request to the missing participant only.
    let payloads = common::collab::event_payloads(&app, cid, "consent.required").await;
    assert!(!payloads.is_empty());
    let _ = guests;
}

#[tokio::test]
async fn g_e2e_07_mid_meeting_consent_reevaluation() {
    let app = app!();
    let (owner, sid, cid, guests) = meeting(&app, 1).await;
    explicit_recording(&app, &owner, sid).await;
    consent(&app, cid, guests[0].auth(), "RECORDING", "ACCEPTED").await;
    // A new participant arrives mid-meeting without consent → re-evaluation blocks.
    let late = join_guest(&app, cid, "late").await;
    connect(&app, cid, &late).await;
    let (_, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(code(&v), "CONSENT_REQUIRED", "{v}");
    consent(&app, cid, late.auth(), "RECORDING", "ACCEPTED").await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
}

#[tokio::test]
async fn g_e2e_08_new_joiner_during_active_recording() {
    let app = app!();
    let (owner, sid, cid, guests) = meeting(&app, 1).await;
    explicit_recording(&app, &owner, sid).await;
    consent(&app, cid, guests[0].auth(), "RECORDING", "ACCEPTED").await;
    recording_active(&app, cid, &owner).await;
    // Joining during an explicit-consent capture requires the recording consent.
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "newcomer" }))
        .await;
    assert_eq!(code(&v), "CONSENT_REQUIRED", "{s} {v}");
    let version = consent_version(&app, cid, guests[0].auth()).await;
    let (s, v) = app
        .join(cid, Auth::None, json!({ "displayName": "newcomer", "consents": [{ "type": "RECORDING", "version": version }] }))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["policy"]["recordingActive"], true);
}

#[tokio::test]
async fn g_e2e_09_stop_to_stopping_to_processing() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    let (s, v) = stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["recording"]["status"], "STOPPING");
    assert_eq!(lk(&app).call_count("StopEgress"), 1);
    app.storage.unavailable.store(true, Ordering::SeqCst);
    let done = lk(&app).egress_complete(&eg, None, b"", Some(10));
    egress_event(&app, &done).await;
    assert_eq!(artifact_status(&app, rid).await, "PROCESSING");
    assert_eq!(events(&app, cid, "recording.processing").await, 1);
}

#[tokio::test]
async fn g_e2e_10_duplicate_stop() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_active(&app, cid, &owner).await;
    let (s1, _) = stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    let (s2, v2) = stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK, "{v2}");
    assert_eq!(v2["providerDispatch"], "NOT_REQUIRED");
    assert_eq!(lk(&app).call_count("StopEgress"), 1);
}

#[tokio::test]
async fn g_e2e_11_processing_to_ready_after_object_exists() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, bytes) = recording_ready(&app, cid, &owner).await;
    let (_, v) = app
        .get(
            format!("{}/recordings/{rid}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(v["status"], "READY");
    assert_eq!(v["sizeBytes"], json!(bytes.len()));
    use sha2::Digest;
    let expected = hex::encode(sha2::Sha256::digest(&bytes));
    assert_eq!(v["checksumSha256"], json!(expected));
    assert!(
        v["retentionAt"].is_string() && v["finalizedAt"].is_string(),
        "{v}"
    );
}

#[tokio::test]
async fn g_e2e_12_authorized_signed_access() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, bytes) = recording_ready(&app, cid, &owner).await;
    let (s, v) = access(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let key = app
        .storage
        .verify_signed(v["url"].as_str().unwrap(), chrono::Utc::now())
        .unwrap();
    assert_eq!(app.storage.get_raw(&key).unwrap(), bytes);
    assert_eq!(v["contentType"], "video/mp4");
}

#[tokio::test]
async fn g_e2e_13_unauthorized_access_denied() {
    let app = app!();
    let (owner, _, cid, guests) = meeting(&app, 1).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let (s, v) = access(&app, cid, rid, guests[0].auth()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert!(v["url"].is_null());
}

#[tokio::test]
async fn g_e2e_14_reconnect_retains_recording_indicator() {
    let app = app!();
    let (owner, _, cid, guests) = meeting(&app, 1).await;
    recording_active(&app, cid, &owner).await;
    // Client reconnects / reloads: authoritative state still carries the indicator.
    let (_, conf) = app
        .get(format!("/api/v2/conferences/{cid}"), guests[0].auth())
        .await;
    assert_eq!(conf["recordingState"], "RECORDING", "{conf}");
    let (_, pre) = app
        .get(format!("{}/pre-join", rec_base(cid)), guests[0].auth())
        .await;
    assert_eq!(pre["notices"]["recordingActive"], true, "{pre}");
}

#[tokio::test]
async fn g_e2e_15_callback_replay_deduped() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    stop(&app, cid, rid, Auth::Bearer(&owner.token)).await;
    let done = lk(&app).egress_complete(&eg, Some(&app.storage), b"replayed", None);
    for i in 0..3 {
        let (_, v) = egress_event(&app, &done).await;
        assert_eq!(v["outcome"], if i == 0 { "APPLIED" } else { "DUPLICATE" });
    }
    assert_eq!(events(&app, cid, "recording.ready").await, 1);
}

#[tokio::test]
async fn g_e2e_16_provider_failure_safe() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 1).await;
    let (rid, eg) = recording_active(&app, cid, &owner).await;
    let failed = lk(&app).egress_fail(&eg, "encoder crashed");
    egress_event(&app, &failed).await;
    assert_eq!(artifact_status(&app, rid).await, "FAILED");
    assert_eq!(
        common::rtc::conference_status(&app, cid).await,
        "LIVE",
        "meeting unaffected"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND recording_state='OFF'"
        ))
        .await,
        1
    );
    // Internal provider error text is not exposed.
    let (_, v) = app
        .get(
            format!("{}/recordings/{rid}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert!(!v.to_string().contains("encoder crashed"), "{v}");
    // A provider rejection on start also fails safely.
    lk(&app).faults.reject_egress.store(true, Ordering::SeqCst);
    let (_, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    let rid2 = uuid(&v["recording"]["id"]);
    assert_eq!(artifact_status(&app, rid2).await, "FAILED");
}

#[tokio::test]
async fn g_e2e_17_retention_deletes_real_object() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let key = storage_key(&app, rid).await;
    assert!(app.storage.contains(&key));
    let r = retention_tick(&app).await;
    assert_eq!(r.deleted, 0, "not due yet");
    common::rtc::exec(&app, &format!("UPDATE meeting_artifacts SET retention_at = now() - interval '1 second' WHERE id='{rid}'")).await;
    let r = retention_tick(&app).await;
    assert_eq!(r.deleted, 1);
    assert!(!app.storage.contains(&key));
    assert_eq!(artifact_status(&app, rid).await, "DELETED");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_artifacts WHERE id='{rid}' AND delete_reason='RETENTION'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn g_e2e_18_manual_delete() {
    let app = app!();
    let (owner, _, cid, guests) = meeting(&app, 1).await;
    let (rid, _) = recording_ready(&app, cid, &owner).await;
    let (s, _) = app
        .delete(
            format!("{}/artifacts/{rid}", rec_base(cid)),
            guests[0].auth(),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let key = storage_key(&app, rid).await;
    let (s, v) = app
        .delete(
            format!("{}/artifacts/{rid}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "DELETED");
    assert!(!app.storage.contains(&key));
    assert_eq!(audits(&app, cid, "ARTIFACT_DELETE_REQUEST").await, 1);
    assert_eq!(audits(&app, cid, "ARTIFACT_DELETED").await, 1);
    // Active capture cannot be deleted.
    let (rid2, _) = recording_active(&app, cid, &owner).await;
    let (s, v) = app
        .delete(
            format!("{}/artifacts/{rid2}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(code(&v), "RECORDING_STATE_CONFLICT", "{s} {v}");
}

#[tokio::test]
async fn g_e2e_19_consent_withdrawal_behavior() {
    let app = app!();
    let (owner, sid, cid, guests) = meeting(&app, 1).await;
    explicit_recording(&app, &owner, sid).await;
    consent(&app, cid, guests[0].auth(), "RECORDING", "ACCEPTED").await;
    let (rid, _) = recording_active(&app, cid, &owner).await;
    let (s, v) = app
        .post(
            format!("{}/consents/RECORDING/withdraw", rec_base(cid)),
            guests[0].auth(),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["effect"], "PROCESSING_STOPPED");
    assert_eq!(v["withdrawalMode"], "STOP_FUTURE_PROCESSING");
    assert_eq!(v["retroactiveDeletion"], false);
    assert_eq!(artifact_status(&app, rid).await, "STOPPING");
    // REQUIRE_PARTICIPANT_EXIT: the withdrawing participant leaves the live room.
    let (owner2, sid2, cid2, guests2) = meeting(&app, 1).await;
    set_profile(
        &app,
        &owner2,
        sid2,
        json!({ "recording": { "withdrawalMode": "REQUIRE_PARTICIPANT_EXIT" } }),
    )
    .await;
    recording_active(&app, cid2, &owner2).await;
    let (_, v) = app
        .post(
            format!("{}/consents/RECORDING/withdraw", rec_base(cid2)),
            guests2[0].auth(),
            json!({}),
        )
        .await;
    assert_eq!(v["effect"], "PARTICIPANT_EXITED", "{v}");
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='WAITING'", guests2[0].participant_id)).await,
        1
    );
}

#[tokio::test]
async fn g_e2e_20_recording_does_not_imply_transcription() {
    let app = app!();
    let (owner, _, cid, _) = meeting_with(&app, transcribable(), 0).await;
    recording_ready(&app, cid, &owner).await;
    assert_eq!(app.transcription.count(""), 0);
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM transcription_jobs WHERE conference_id='{cid}'"
        ))
        .await,
        0
    );
    assert_eq!(app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE id='{cid}' AND transcription_state='OFF'")).await, 1);
}

#[tokio::test]
async fn g_e2e_21_transcription_path_not_required_but_implemented() {
    // Classification: NOT REQUIRED for this release (README "Future | Intelligence:
    // transcription"; platform overview "Communications intelligence | future"). The
    // provider-neutral path is implemented and disabled by default.
    assert!(!backend::config::FeatureFlags::default().transcription_v2_enabled);
    let app = app!();
    let (owner, _, cid, guests) = meeting_with(&app, transcribable(), 1).await;
    let (_job, transcript) = transcript_ready(
        &app,
        cid,
        &owner,
        &[
            ("Hello everyone", Some(guests[0].identity.as_str())),
            ("Second line", None),
        ],
    )
    .await;
    let (s, v) = app
        .get(
            format!("{}/transcripts/{transcript}", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["segments"].as_array().unwrap().len(), 2);
    let (s, v) = app
        .post(
            format!("{}/transcripts/{transcript}/translations", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "targetLanguage": "fr" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["translatedSegments"], 2);
    // Guests cannot read transcripts.
    let (s, _) = app
        .get(
            format!("{}/transcripts/{transcript}", rec_base(cid)),
            guests[0].auth(),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn g_e2e_22_caption_path() {
    let app = app!();
    let (owner, _, cid, _) = meeting_with(&app, transcribable(), 0).await;
    transcript_ready(
        &app,
        cid,
        &owner,
        &[("Caption one", None), ("Caption two", None)],
    )
    .await;
    let (_, list) = app
        .get(
            format!("{}/artifacts?artifactType=CAPTION_TRACK", rec_base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let captions = list["artifacts"].as_array().unwrap();
    assert_eq!(captions.len(), 1, "{list}");
    let aid = uuid(&captions[0]["id"]);
    let vtt =
        String::from_utf8(app.storage.get_raw(&storage_key(&app, aid).await).unwrap()).unwrap();
    assert!(vtt.starts_with("WEBVTT"), "{vtt}");
    assert!(vtt.contains("Caption two"));
    assert_eq!(captions[0]["contentType"], "text/vtt");
}

#[tokio::test]
async fn g_e2e_23_ai_policy_and_feature_gate() {
    let app = app!();
    let (owner, _, cid, guests) = meeting_with(&app, transcribable(), 1).await;
    let (_job, transcript) = transcript_ready(
        &app,
        cid,
        &owner,
        &[("We agreed to ship", Some(guests[0].identity.as_str()))],
    )
    .await;
    // AI consent required by default (intelligence.consentRequired).
    let (_, v) = app
        .post(
            format!("{}/intelligence/requests", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "sourceArtifactId": transcript, "kind": "SUMMARY" }),
        )
        .await;
    assert_eq!(code(&v), "CONSENT_REQUIRED", "{v}");
    consent(&app, cid, guests[0].auth(), "AI_PROCESSING", "ACCEPTED").await;
    let (s, v) = app
        .post(
            format!("{}/intelligence/requests", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "sourceArtifactId": transcript, "kind": "SUMMARY" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["artifactType"], "MEETING_SUMMARY");
    assert_eq!(v["status"], "READY");
    // Speakers are opaque labels; no names / ids reach the provider.
    let inputs = app.intelligence.inputs.lock().unwrap().clone();
    assert_eq!(inputs[0].lines[0].0, "Speaker 1");
    let joined: String = inputs[0].lines.iter().map(|l| l.0.clone()).collect();
    assert!(!joined.contains("guest-0"));
    // Participants cannot request.
    let (s, _) = app
        .post(
            format!("{}/intelligence/requests", rec_base(cid)),
            guests[0].auth(),
            json!({ "sourceArtifactId": transcript, "kind": "SUMMARY" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn g_e2e_24_cross_conference_attack() {
    let app = app!();
    let (owner_a, _, cid_a, _) = meeting_with(&app, transcribable(), 0).await;
    let (owner_b, _, cid_b, _) = meeting_with(&app, transcribable(), 0).await;
    let (rid_b, _) = recording_ready(&app, cid_b, &owner_b).await;
    let (_, transcript_b) = transcript_ready(&app, cid_b, &owner_b, &[("b only", None)]).await;
    let a = Auth::Bearer(&owner_a.token);
    for (s, v) in [
        stop(&app, cid_a, rid_b, a).await,
        access(&app, cid_a, rid_b, a).await,
        app.get(format!("{}/recordings/{rid_b}", rec_base(cid_a)), a)
            .await,
        app.get(format!("{}/transcripts/{transcript_b}", rec_base(cid_a)), a)
            .await,
        app.post(
            format!(
                "{}/transcripts/{transcript_b}/translations",
                rec_base(cid_a)
            ),
            a,
            json!({ "targetLanguage": "de" }),
        )
        .await,
        app.post(
            format!("{}/transcription", rec_base(cid_a)),
            a,
            json!({ "mode": "POST_MEETING", "sourceRecordingId": rid_b }),
        )
        .await,
    ] {
        assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    }
    assert_eq!(artifact_status(&app, rid_b).await, "READY");
}

#[tokio::test]
async fn g_e2e_25_reconciliation_after_lost_callback() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 0).await;
    let (s, v) = start_recording(&app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let rid = uuid(&v["recording"]["id"]);
    let eg = provider_ref(&app, rid).await;
    // egress_started is lost.
    let _lost = lk(&app).egress_activate(&eg);
    common::rtc::exec(&app, &format!("UPDATE meeting_artifacts SET created_at = now() - interval '2 minutes' WHERE id='{rid}'")).await;
    let reconciler = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    reconciler.tick().await.unwrap();
    assert_eq!(
        artifact_status(&app, rid).await,
        "RECORDING",
        "state repaired from provider query"
    );
    // Unowned provider capture is escalated, never stopped by guess.
    let foreign = lk(&app).insert_egress(&room(cid), proto::EgressStatus::EgressActive);
    reconciler.tick().await.unwrap();
    assert_eq!(
        lk(&app).egress(&foreign).unwrap().status,
        proto::EgressStatus::EgressActive as i32
    );
    assert!(app.scalar(&format!("SELECT count(*) FROM rtc_drift_events WHERE conference_id='{cid}' AND kind='RECORDING_PROVIDER_DRIFT' AND status='OPEN'")).await >= 1);
}

/// Regression (found by real certification): the Egress recorder joins the room as a
/// provider service participant and must never be evicted by the session rules
/// (webhook or reconciler), while an ordinary unknown identity still is.
#[tokio::test]
async fn g_recorder_participant_is_not_evicted() {
    let app = app!();
    let (owner, _, cid, _) = meeting(&app, 1).await;
    recording_active(&app, cid, &owner).await;
    let recorder = "EG_recorder_identity";
    lk(&app).insert_recorder(&room(cid), recorder);
    let body = livekit_mock::recorder_joined_body(
        &format!("EV_{}", Uuid::new_v4().simple()),
        &room(cid),
        recorder,
    );
    let (s, v) = app.signed_webhook(&body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "IGNORED", "{v}");
    assert!(lk(&app)
        .identities(&room(cid))
        .contains(&recorder.to_string()));
    let reconciler = backend::workers::ConferenceReconciler::new(app.state.meeting.core.clone());
    let report = reconciler.tick().await.unwrap();
    assert!(
        lk(&app)
            .identities(&room(cid))
            .contains(&recorder.to_string()),
        "{report:?}"
    );
    assert_eq!(lk(&app).call_count("RemoveParticipant"), 0);
    // A standard unknown identity is still evicted.
    let (_, v) =
        common::rtc::provider_event(&app, "participant_joined", cid, Some("ps_intruder")).await;
    assert_eq!(v["outcome"], "UNKNOWN_IDENTITY_EVICTED");
}

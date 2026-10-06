//! Part G test backends: recording through the mock LiveKit Egress (the real
//! `LiveKitEgressRecorder`), in-memory object storage, and provider-neutral test
//! transcription / translation / intelligence providers with failure injection.
#![allow(dead_code)]

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use backend::services::{
    artifacts::{
        providers::{
            IntelligenceCapabilities, IntelligenceInput, IntelligenceProvider,
            LiveTranscriptionRequest, PostMeetingTranscriptionRequest, ProviderError, ProviderJob,
            ProviderJobStatus, ProviderResult, TranscriptionCapabilities, TranscriptionProvider,
            TranslationProvider,
        },
        transcription::sign_callback,
    },
    profiles::ProcessingLocation,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{livekit_mock, uuid, Auth, TestApp, User};

pub const TRANSCRIPTION_SECRET: &str = "test-transcription-secret-0123456789";

/// Test transcription provider (records calls; jobs are `TJ_<n>`).
#[derive(Default)]
pub struct TestTranscription {
    pub calls: Mutex<Vec<String>>,
    pub fail: AtomicBool,
    pub external: AtomicBool,
    seq: AtomicU64,
}

impl TestTranscription {
    fn next(&self) -> String {
        format!("TJ_{}", self.seq.fetch_add(1, Ordering::SeqCst))
    }
    fn check(&self) -> ProviderResult<()> {
        if self.fail.load(Ordering::SeqCst) {
            Err(ProviderError::Unavailable("injected".into()))
        } else {
            Ok(())
        }
    }
    pub fn count(&self, prefix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
}

#[async_trait]
impl TranscriptionProvider for TestTranscription {
    fn name(&self) -> &'static str {
        "test-transcription"
    }
    fn capabilities(&self) -> TranscriptionCapabilities {
        TranscriptionCapabilities {
            live: true,
            post_meeting: true,
            location: Some(if self.external.load(Ordering::SeqCst) {
                ProcessingLocation::ExternalAllowed
            } else {
                ProcessingLocation::SelfHostedOnly
            }),
        }
    }
    async fn start_live_transcription(
        &self,
        r: &LiveTranscriptionRequest,
    ) -> ProviderResult<ProviderJob> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("start_live:{}", r.room_name));
        self.check()?;
        Ok(ProviderJob {
            provider_job_ref: self.next(),
            status: ProviderJobStatus::Active,
        })
    }
    async fn stop_live_transcription(&self, job: &str) -> ProviderResult<ProviderJob> {
        self.calls.lock().unwrap().push(format!("stop:{job}"));
        self.check()?;
        Ok(ProviderJob {
            provider_job_ref: job.into(),
            status: ProviderJobStatus::Completed,
        })
    }
    async fn submit_post_meeting_transcription(
        &self,
        r: &PostMeetingTranscriptionRequest,
    ) -> ProviderResult<ProviderJob> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("submit_post:{}", r.source_url));
        self.check()?;
        Ok(ProviderJob {
            provider_job_ref: self.next(),
            status: ProviderJobStatus::Pending,
        })
    }
    async fn get_job(&self, job: &str) -> ProviderResult<Option<ProviderJob>> {
        self.check()?;
        Ok(Some(ProviderJob {
            provider_job_ref: job.into(),
            status: ProviderJobStatus::Active,
        }))
    }
    async fn cancel_job(&self, job: &str) -> ProviderResult<()> {
        self.calls.lock().unwrap().push(format!("cancel:{job}"));
        self.check()
    }
}

#[derive(Default)]
pub struct TestTranslation {
    pub calls: AtomicU64,
    pub fail: AtomicBool,
}

#[async_trait]
impl TranslationProvider for TestTranslation {
    fn name(&self) -> &'static str {
        "test-translation"
    }
    fn location(&self) -> Option<ProcessingLocation> {
        Some(ProcessingLocation::SelfHostedOnly)
    }
    async fn translate(&self, text: &str, _src: &str, target: &str) -> ProviderResult<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable("injected".into()));
        }
        Ok(format!("[{target}] {text}"))
    }
}

#[derive(Default)]
pub struct TestIntelligence {
    pub calls: AtomicU64,
    pub fail: AtomicBool,
    pub inputs: Mutex<Vec<IntelligenceInput>>,
}

#[async_trait]
impl IntelligenceProvider for TestIntelligence {
    fn name(&self) -> &'static str {
        "test-intelligence"
    }
    fn capabilities(&self) -> IntelligenceCapabilities {
        IntelligenceCapabilities {
            summarize: true,
            action_items: true,
            location: Some(ProcessingLocation::SelfHostedOnly),
        }
    }
    async fn summarize(&self, input: &IntelligenceInput) -> ProviderResult<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().unwrap().push(input.clone());
        if self.fail.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable("injected".into()));
        }
        Ok(format!("Summary of {} lines", input.lines.len()))
    }
    async fn extract_action_items(&self, input: &IntelligenceInput) -> ProviderResult<Vec<String>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(ProviderError::Unavailable("injected".into()));
        }
        Ok(vec![format!("Follow up ({} lines)", input.lines.len())])
    }
}

// ---------------------------------------------------------------------------
// Scenario helpers
// ---------------------------------------------------------------------------

pub fn rec_base(cid: Uuid) -> String {
    format!("/api/v2/conferences/{cid}")
}

/// Start a recording as `by`; returns (status, body).
pub async fn start_recording(app: &TestApp, cid: Uuid, auth: Auth<'_>) -> (StatusCode, Value) {
    app.post(format!("{}/recordings", rec_base(cid)), auth, json!({}))
        .await
}

/// The provider egress id recorded for a recording artefact.
pub async fn provider_ref(app: &TestApp, rid: Uuid) -> String {
    use sea_orm::ConnectionTrait;
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT coalesce(provider_ref,'') FROM meeting_artifacts WHERE id='{rid}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0).unwrap()
}

pub async fn artifact_status(app: &TestApp, id: Uuid) -> String {
    use sea_orm::ConnectionTrait;
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT status FROM meeting_artifacts WHERE id='{id}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0).unwrap()
}

pub async fn storage_key(app: &TestApp, id: Uuid) -> String {
    use sea_orm::ConnectionTrait;
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT coalesce(storage_key,'') FROM meeting_artifacts WHERE id='{id}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0).unwrap()
}

/// Deliver a signed egress webhook body.
pub async fn egress_event(app: &TestApp, body: &str) -> (StatusCode, Value) {
    app.signed_webhook(body).await
}

/// Start + provider-activate a recording. Returns (recording id, egress id).
pub async fn recording_active(app: &TestApp, cid: Uuid, owner: &User) -> (Uuid, String) {
    let (s, v) = start_recording(app, cid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::ACCEPTED, "start recording: {v}");
    let rid = uuid(&v["recording"]["id"]);
    let eg = provider_ref(app, rid).await;
    assert!(!eg.is_empty(), "provider ref stored after dispatch: {v}");
    let body = super::rtc::lk(app).egress_activate(&eg);
    let (s, v) = egress_event(app, &body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(artifact_status(app, rid).await, "RECORDING");
    (rid, eg)
}

/// Stop (API) + provider completes + storage verified → READY.
pub async fn recording_ready(app: &TestApp, cid: Uuid, owner: &User) -> (Uuid, Vec<u8>) {
    let (rid, eg) = recording_active(app, cid, owner).await;
    let (s, v) = app
        .post(
            format!("{}/recordings/{rid}/stop", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "stop: {v}");
    let bytes = b"\x00\x00\x00\x18ftypmp42 mock composite recording bytes".repeat(64);
    let body = super::rtc::lk(app).egress_complete(&eg, Some(&app.storage), &bytes, None);
    let (s, v) = egress_event(app, &body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(artifact_status(app, rid).await, "READY");
    (rid, bytes)
}

/// Signed transcription callback.
pub async fn transcription_callback(app: &TestApp, body: &Value) -> (StatusCode, Value) {
    let raw = body.to_string();
    let ts = chrono::Utc::now().timestamp().to_string();
    let sig = sign_callback(TRANSCRIPTION_SECRET, &ts, &raw);
    app.call_with_headers(
        axum::http::Method::POST,
        "/internal/transcription/events",
        &[
            ("x-cf-timestamp", ts.as_str()),
            ("x-cf-signature", sig.as_str()),
        ],
        raw,
    )
    .await
}

pub fn segment(job_ref: &str, seg: &str, seq: i64, text: &str, is_final: bool) -> Value {
    json!({
        "type": "SEGMENT",
        "jobRef": job_ref,
        "segmentId": seg,
        "sequence": seq,
        "startMs": seq * 1000,
        "endMs": seq * 1000 + 900,
        "language": "en",
        "text": text,
        "final": is_final,
    })
}

/// Signed egress webhook with the mock's API key (helper used by livekit_mock users).
pub fn signed(body: &str) -> String {
    livekit_mock::sign_webhook(body, livekit_mock::API_KEY, livekit_mock::API_SECRET)
}

/// LIVE conference (PUBLIC space) with `n` connected guests. Returns
/// (owner, space id, conference id, guests).
pub async fn meeting(app: &TestApp, n: usize) -> (User, Uuid, Uuid, Vec<super::rtc::Joined>) {
    meeting_with(
        app,
        json!({ "title": "Part G", "accessPolicy": "PUBLIC" }),
        n,
    )
    .await
}

pub async fn meeting_with(
    app: &TestApp,
    body: Value,
    n: usize,
) -> (User, Uuid, Uuid, Vec<super::rtc::Joined>) {
    let owner = app.user("owner").await;
    let (sid, cid) = app.live_conference(&owner, body).await;
    let mut guests = vec![];
    for i in 0..n {
        let g = super::rtc::join_guest(app, cid, &format!("guest-{i}")).await;
        super::rtc::connect(app, cid, &g).await;
        guests.push(g);
    }
    (owner, sid, cid, guests)
}

/// PATCH the space profile override (reads the current version first).
pub async fn set_profile(
    app: &TestApp,
    owner: &User,
    sid: Uuid,
    changes: Value,
) -> (StatusCode, Value) {
    let (s, v) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "profile policy: {v}");
    app.patch(
        format!("/api/v2/meeting-spaces/{sid}/profile-policy"),
        Auth::Bearer(&owner.token),
        json!({ "profilePolicyVersion": v["profilePolicyVersion"], "changes": changes }),
    )
    .await
}

pub async fn consent_version(app: &TestApp, cid: Uuid, auth: Auth<'_>) -> String {
    let (s, v) = app
        .get(format!("{}/consents/requirements", rec_base(cid)), auth)
        .await;
    assert_eq!(s, StatusCode::OK, "requirements: {v}");
    v["requirements"][0]["consentVersion"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Record one consent decision for the caller.
pub async fn consent(
    app: &TestApp,
    cid: Uuid,
    auth: Auth<'_>,
    kind: &str,
    decision: &str,
) -> (StatusCode, Value) {
    let version = consent_version(app, cid, auth).await;
    app.post(
        format!("{}/consents", rec_base(cid)),
        auth,
        json!({ "consentVersion": version, "decisions": [{ "consentType": kind, "decision": decision }] }),
    )
    .await
}

pub async fn audits(app: &TestApp, cid: Uuid, action: &str) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action='{action}'"
    ))
    .await
}

pub async fn events(app: &TestApp, cid: Uuid, event_type: &str) -> i64 {
    super::rtc::events(app, cid, event_type).await
}

/// Run one artefact worker pass (finalization retries + deletions).
pub async fn retention_tick(
    app: &TestApp,
) -> backend::services::artifacts::retention::RetentionReport {
    backend::services::artifacts::retention::ArtifactRetentionWorker::new(
        app.state.meeting.core.clone(),
    )
    .tick()
    .await
    .unwrap()
}

/// Run the provider-command dispatcher for a conference (outbox retries).
pub async fn dispatch(app: &TestApp, cid: Uuid) {
    backend::services::rtc_control::RtcControl::new(app.state.meeting.core.clone())
        .dispatch_for_conference(cid)
        .await;
}

/// Make due provider commands claimable now (skips retry backoff).
pub async fn expire_backoff(app: &TestApp, cid: Uuid) {
    super::rtc::exec(
        app,
        &format!("UPDATE outbox_events SET available_at = now() - interval '1 second' WHERE conference_id='{cid}' AND status = 'PENDING'"),
    )
    .await;
}

/// Start + activate + stop + complete a live transcription with `n` final segments.
/// Returns (job id, transcript artefact id).
pub async fn transcript_ready(
    app: &TestApp,
    cid: Uuid,
    owner: &User,
    segments: &[(&str, Option<&str>)],
) -> (Uuid, Uuid) {
    let (s, v) = app
        .post(
            format!("{}/transcription", rec_base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "mode": "LIVE", "language": "en" }),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "start transcription: {v}");
    let job = uuid(&v["id"]);
    let artifact = uuid(&v["artifactId"]);
    let job_ref = job_ref(app, job).await;
    for (i, (text, speaker)) in segments.iter().enumerate() {
        let mut seg = segment(&job_ref, &format!("s{i}"), i as i64, text, true);
        if let Some(sp) = speaker {
            seg["speakerIdentity"] = json!(sp);
        }
        let (s, v) = transcription_callback(app, &seg).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (s, v) = transcription_callback(
        app,
        &json!({ "type": "JOB_STATUS", "jobRef": job_ref, "status": "COMPLETED" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(artifact_status(app, artifact).await, "READY");
    (job, artifact)
}

pub async fn job_ref(app: &TestApp, job: Uuid) -> String {
    use sea_orm::ConnectionTrait;
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT coalesce(provider_job_ref,'') FROM transcription_jobs WHERE id='{job}'"
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0).unwrap()
}

pub async fn job_status(app: &TestApp, job: Uuid) -> String {
    use sea_orm::ConnectionTrait;
    let row = app
        .db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT status FROM transcription_jobs WHERE id='{job}'"),
        ))
        .await
        .unwrap()
        .unwrap();
    row.try_get_by_index::<String>(0).unwrap()
}

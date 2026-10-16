//! Application (RTC) call-audio capture through LiveKit Egress: the execution adapter
//! behind the neutral [`CallMediaCapture`] interface (§60 voicemail, §61 recording).
//!
//! * Voicemail: a TRACK egress of exactly one authorized source — the caller's own
//!   microphone track (no other participant, no video, no room mix).
//! * Call recording: an AUDIO-ONLY room-composite egress of the Call's own media
//!   resource (the mixed call audio; never video).
//!
//! Calling decides everything (authorization, consent, retention, access); this adapter
//! only starts / stops / observes the egress and hands the finished media over. It uses
//! the Call's existing media resource (`MediaResourceName::for_call`) and never creates
//! a room. The egress writes to a PRIVATE staging key of the protected artifact bucket
//! (`{prefix}/{capture_id}.ogg`, the storage credentials live in the Egress deployment's
//! injected configuration — Calling never sends one); Calling reads it back with its own
//! store, verifies it and stores the artifact under its canonical protected key through
//! the same path as SIP captures, then releases the staging object. No egress identifier
//! leaves this module: captures are addressed by Calling's own capture id.

use std::{sync::Arc, time::Duration as StdDuration};

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{livekit::LiveKitAdapter, MediaProviderError, MediaResourceName};
use crate::{
    calls::app::enterprise::{
        CallMediaCapture, CapturePurpose, CaptureRequest, CaptureStatus, CapturedMedia,
        CAPTURE_CONTENT_TYPE_OGG, MAX_CAPTURE_BYTES,
    },
    calls::ids::CallId,
    config::ConfigError,
};

/// How long a voicemail start waits for the caller's microphone track to be published.
const SOURCE_WAIT: StdDuration = StdDuration::from_secs(5);
const SOURCE_POLL: StdDuration = StdDuration::from_millis(250);

/// `CALLING_CAPTURE_PROVIDER` = `livekit-egress` | `none` (default `none`), and
/// `CALLING_CAPTURE_STAGING_PREFIX` (default `capture-staging`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSettings {
    pub egress: bool,
    pub staging_prefix: String,
}

impl CaptureSettings {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let egress = match get("CALLING_CAPTURE_PROVIDER")
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .as_deref()
        {
            None | Some("none") => false,
            Some("livekit-egress") => true,
            Some(_) => {
                return Err(ConfigError::invalid(
                    "CALLING_CAPTURE_PROVIDER",
                    "must be `livekit-egress` or `none`".to_string(),
                ))
            }
        };
        let staging_prefix = get("CALLING_CAPTURE_STAGING_PREFIX")
            .map(|v| v.trim().trim_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "capture-staging".to_string());
        if staging_prefix.len() > 64
            || !staging_prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/'))
        {
            return Err(ConfigError::invalid(
                "CALLING_CAPTURE_STAGING_PREFIX",
                "must be a short path of [A-Za-z0-9_-/]".to_string(),
            ));
        }
        Ok(Self {
            egress,
            staging_prefix,
        })
    }
}

/// Egress status as LiveKit reports it (protojson name or number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EgressState {
    /// Starting or active: still capturing.
    Running,
    /// Stopped, still finalizing / uploading.
    Ending,
    Complete,
    Failed,
}

fn egress_state(v: &Value) -> EgressState {
    match v.get("status") {
        Some(Value::String(s)) => match s.as_str() {
            "EGRESS_COMPLETE" | "EGRESS_LIMIT_REACHED" => EgressState::Complete,
            "EGRESS_FAILED" | "EGRESS_ABORTED" => EgressState::Failed,
            "EGRESS_ENDING" => EgressState::Ending,
            _ => EgressState::Running,
        },
        Some(Value::Number(n)) => match n.as_i64() {
            Some(2) => EgressState::Ending,
            Some(3 | 6) => EgressState::Complete,
            Some(4 | 5) => EgressState::Failed,
            _ => EgressState::Running,
        },
        // A status-less item is starting.
        _ => EgressState::Running,
    }
}

fn int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// The output files an egress item names (request outputs and results alike).
fn egress_files(item: &Value) -> Vec<&str> {
    let mut out = Vec::new();
    for result in item
        .get("file_results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(item.get("file"))
    {
        if let Some(name) = result.get("filename").and_then(Value::as_str) {
            out.push(name);
        }
    }
    if let Some(path) = item
        .get("track")
        .and_then(|t| t.get("file"))
        .and_then(|f| f.get("filepath"))
        .and_then(Value::as_str)
    {
        out.push(path);
    }
    for output in item
        .get("room_composite")
        .and_then(|r| r.get("file_outputs"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(path) = output.get("filepath").and_then(Value::as_str) {
            out.push(path);
        }
    }
    out
}

/// The capture's recorded length (ns in LiveKit's file result), in ms.
fn duration_ms(item: &Value) -> i32 {
    item.get("file_results")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .or_else(|| item.get("file"))
        .and_then(|f| int(f.get("duration")))
        .map(|ns| (ns / 1_000_000).clamp(0, 3_600_000) as i32)
        .unwrap_or(0)
}

/// The LiveKit Egress capture adapter.
pub struct LiveKitEgressCapture {
    livekit: Arc<LiveKitAdapter>,
    store: Arc<dyn cf_object_storage::ObjectStore>,
    prefix: String,
}

impl LiveKitEgressCapture {
    pub fn new(
        livekit: Arc<LiveKitAdapter>,
        store: Arc<dyn cf_object_storage::ObjectStore>,
        settings: &CaptureSettings,
    ) -> Self {
        Self {
            livekit,
            store,
            prefix: settings.staging_prefix.clone(),
        }
    }

    /// The private staging key of one capture (Calling's capture id only).
    fn staging_key(&self, capture_id: Uuid) -> String {
        format!("{}/{}.ogg", self.prefix, capture_id.simple())
    }

    fn room(call_id: Uuid) -> String {
        MediaResourceName::for_call(CallId::from_uuid(call_id))
            .as_str()
            .to_string()
    }

    async fn egress(&self, method: &str, body: Value) -> Result<Value, MediaProviderError> {
        self.livekit
            .twirp("Egress", method, json!({ "roomRecord": true }), body)
            .await
    }

    /// Every egress of the Call's resource writing this capture's staging key (normally
    /// one; a crashed start may have left another).
    async fn egresses_of(
        &self,
        call_id: Uuid,
        capture_id: Uuid,
    ) -> Result<Vec<Value>, MediaProviderError> {
        let key = self.staging_key(capture_id);
        let items = match self
            .egress("ListEgress", json!({ "room_name": Self::room(call_id) }))
            .await
        {
            Ok(body) => body.get("items").cloned().unwrap_or(Value::Null),
            Err(MediaProviderError::NotFound) => Value::Null,
            Err(e) => return Err(e),
        };
        Ok(items
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| egress_files(item).contains(&key.as_str()))
            .cloned()
            .collect())
    }

    /// The published microphone track of the first authorized source.
    async fn source_track(&self, room: &str, sources: &[String]) -> Option<String> {
        let deadline = tokio::time::Instant::now() + SOURCE_WAIT;
        loop {
            if let Ok(body) = self
                .livekit
                .twirp(
                    "RoomService",
                    "ListParticipants",
                    json!({ "roomAdmin": true, "room": room }),
                    json!({ "room": room }),
                )
                .await
            {
                for source in sources {
                    let track = body
                        .get("participants")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter(|p| p.get("identity").and_then(Value::as_str) == Some(source))
                        .flat_map(|p| p.get("tracks").and_then(Value::as_array).into_iter().flatten())
                        .find(|t| {
                            let audio = matches!(t.get("type"), Some(Value::String(s)) if s == "AUDIO")
                                || matches!(t.get("type"), Some(Value::Number(n)) if n.as_i64() == Some(0));
                            let mic = matches!(t.get("source"), Some(Value::String(s)) if s == "MICROPHONE")
                                || matches!(t.get("source"), Some(Value::Number(n)) if n.as_i64() == Some(2));
                            audio && mic
                        })
                        .and_then(|t| t.get("sid").and_then(Value::as_str));
                    if let Some(sid) = track {
                        return Some(sid.to_string());
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(SOURCE_POLL).await;
        }
    }

    async fn stop_running(&self, call_id: Uuid, capture_id: Uuid) -> Result<(), &'static str> {
        let items = self
            .egresses_of(call_id, capture_id)
            .await
            .map_err(|_| "CAPTURE_STOP_FAILED")?;
        for item in items {
            if egress_state(&item) != EgressState::Running {
                continue;
            }
            let Some(id) = item.get("egress_id").and_then(Value::as_str) else {
                continue;
            };
            match self.egress("StopEgress", json!({ "egress_id": id })).await {
                // Already ending / ended: the stop is done.
                Ok(_) | Err(MediaProviderError::Rejected | MediaProviderError::NotFound) => {}
                Err(_) => return Err("CAPTURE_STOP_FAILED"),
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CallMediaCapture for LiveKitEgressCapture {
    fn available(&self) -> bool {
        self.store.configured()
    }

    async fn start(&self, request: CaptureRequest) -> Result<(), &'static str> {
        let room = Self::room(request.call_id);
        // Idempotent: an existing (running or finished) egress of this capture is it.
        match self.egresses_of(request.call_id, request.capture_id).await {
            Ok(items) if items.iter().any(|i| egress_state(i) != EgressState::Failed) => {
                return Ok(())
            }
            Ok(_) => {}
            Err(_) => return Err("CAPTURE_UNAVAILABLE"),
        }
        let filepath = self.staging_key(request.capture_id);
        let (method, body) = match request.purpose {
            // ONLY the authorized source's microphone: never the room, never video.
            CapturePurpose::Voicemail => {
                if request.sources.is_empty() {
                    return Err("CAPTURE_SOURCE_UNAVAILABLE");
                }
                let Some(track) = self.source_track(&room, &request.sources).await else {
                    return Err("CAPTURE_SOURCE_UNAVAILABLE");
                };
                (
                    "StartTrackEgress",
                    json!({
                        "room_name": room,
                        "track_id": track,
                        "file": { "filepath": filepath, "disable_manifest": true },
                    }),
                )
            }
            // The mixed call audio of the Call's own resource; audio only.
            CapturePurpose::Recording => (
                "StartRoomCompositeEgress",
                json!({
                    "room_name": room,
                    "audio_only": true,
                    "file_outputs": [{
                        "file_type": "OGG",
                        "filepath": filepath,
                        "disable_manifest": true,
                    }],
                }),
            ),
        };
        match self.egress(method, body).await {
            Ok(_) => {
                tracing::info!(capture_id = %request.capture_id, purpose = request.purpose.as_str(),
                               "application capture started");
                Ok(())
            }
            Err(e) => {
                // The request may have been executed anyway: never leave a capture running
                // that Calling considers failed.
                let _ = self.stop_running(request.call_id, request.capture_id).await;
                tracing::warn!(capture_id = %request.capture_id, failure = %e, "application capture not started");
                Err(match e {
                    MediaProviderError::Timeout => "CAPTURE_START_TIMEOUT",
                    MediaProviderError::Rejected | MediaProviderError::NotFound => {
                        "CAPTURE_REJECTED"
                    }
                    _ => "CAPTURE_UNAVAILABLE",
                })
            }
        }
    }

    async fn stop(&self, call_id: Uuid, capture_id: Uuid) -> Result<(), &'static str> {
        self.stop_running(call_id, capture_id).await
    }

    async fn poll(&self, call_id: Uuid, capture_id: Uuid) -> CaptureStatus {
        let items = match self.egresses_of(call_id, capture_id).await {
            Ok(items) => items,
            Err(_) => return CaptureStatus::Pending,
        };
        if items
            .iter()
            .any(|i| egress_state(i) == EgressState::Running)
        {
            return CaptureStatus::Running;
        }
        if items.iter().any(|i| egress_state(i) == EgressState::Ending) {
            return CaptureStatus::Pending;
        }
        let complete = items
            .iter()
            .find(|i| egress_state(i) == EgressState::Complete);
        let failed = !items.is_empty() && complete.is_none();
        let key = self.staging_key(capture_id);
        let bytes = match self.store.get(&key, MAX_CAPTURE_BYTES).await {
            Ok(Some(bytes)) if !bytes.is_empty() => bytes,
            Ok(_) if failed => return CaptureStatus::Failed("CAPTURE_FAILED"),
            // Not uploaded yet (or the egress is unknown): the delivery window decides.
            Ok(_) => return CaptureStatus::Pending,
            Err(cf_object_storage::StorageError::Integrity) => {
                return CaptureStatus::Failed("CAPTURE_TOO_LARGE")
            }
            Err(_) => return CaptureStatus::Pending,
        };
        let duration_ms = complete.or(items.first()).map(duration_ms).unwrap_or(0);
        CaptureStatus::Finished(CapturedMedia {
            content_type: CAPTURE_CONTENT_TYPE_OGG.to_string(),
            duration_ms,
            sha256: cf_object_storage::sha256_hex(&bytes),
            bytes,
        })
    }

    async fn release(&self, _call_id: Uuid, capture_id: Uuid) -> Result<(), &'static str> {
        self.store
            .delete(&self.staging_key(capture_id))
            .await
            .map_err(|_| "CAPTURE_RELEASE_FAILED")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_off_and_refuse_unknown_values() {
        let none = CaptureSettings::from_lookup(&|_: &str| None).unwrap();
        assert!(!none.egress);
        assert_eq!(none.staging_prefix, "capture-staging");
        let on = CaptureSettings::from_lookup(&|k: &str| match k {
            "CALLING_CAPTURE_PROVIDER" => Some("livekit-egress".into()),
            "CALLING_CAPTURE_STAGING_PREFIX" => Some("/stage/x/".into()),
            _ => None,
        })
        .unwrap();
        assert!(on.egress);
        assert_eq!(on.staging_prefix, "stage/x");
        for (k, v) in [
            ("CALLING_CAPTURE_PROVIDER", "meeting-egress"),
            ("CALLING_CAPTURE_STAGING_PREFIX", "../x"),
        ] {
            assert!(
                CaptureSettings::from_lookup(&|q: &str| (q == k).then(|| v.to_string())).is_err()
            );
        }
    }

    #[test]
    fn egress_items_are_read_by_status_file_and_duration() {
        let item = json!({
            "egress_id": "EG_x", "status": "EGRESS_COMPLETE",
            "track": { "file": { "filepath": "capture-staging/a.ogg" } },
            "file_results": [{ "filename": "capture-staging/a.ogg", "duration": "5994433951" }],
        });
        assert_eq!(egress_state(&item), EgressState::Complete);
        assert!(egress_files(&item).contains(&"capture-staging/a.ogg"));
        assert_eq!(duration_ms(&item), 5994);
        assert_eq!(egress_state(&json!({ "status": 1 })), EgressState::Running);
        assert_eq!(
            egress_state(&json!({ "status": "EGRESS_ENDING" })),
            EgressState::Ending
        );
        assert_eq!(
            egress_state(&json!({ "status": "EGRESS_ABORTED" })),
            EgressState::Failed
        );
        assert_eq!(egress_state(&json!({ "status": 6 })), EgressState::Complete);
        let composite =
            json!({ "room_composite": { "file_outputs": [{ "filepath": "p/b.ogg" }] } });
        assert_eq!(egress_files(&composite), vec!["p/b.ogg"]);
    }
}

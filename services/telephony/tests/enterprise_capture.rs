//! Part F §60 edge media capture on an internal enterprise SIP leg (scripted edge; the
//! real Asterisk path is certified by Calling's `enterprise_sip_certification`): CAPTURE
//! is accepted only for enterprise legs behind ENTERPRISE_SIP_ENABLED, the edge records
//! into its transient spool under Telephony's own name, the finished audio is delivered
//! to Calling with size / duration / SHA-256 and the spool copy is deleted; Calling
//! outages retry with backoff, refusals dead-letter (incident) and still delete the spool;
//! failures and lost end events are delivered as bounded failures.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::{all_on, edge::ScriptedEdge, Harness, CALLING_PERMISSIONS};
use serde_json::{json, Value};
use telephony::{
    domain::LegStatus,
    sip::{EdgeEnterpriseDialog, EdgeEvent, EdgeEventKind},
};
use uuid::Uuid;

/// Asserts `value` conforms to `packages/contracts/<reference>`.
fn conforms(reference: &str, value: &Value) {
    use jsonschema::{Registry, Resource};
    const B: &str = "https://schemas.cfmeet.com/";
    let root =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/contracts");
    let read = |rel: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(root.join(rel)).unwrap()).unwrap()
    };
    let mut builder = Registry::new();
    for c in read("manifest.json")["contracts"].as_array().unwrap() {
        let doc = read(c["path"].as_str().unwrap());
        let id = if c["kind"] == "json-schema" {
            doc["$id"].as_str().unwrap().to_string()
        } else {
            format!("{B}{}", c["path"].as_str().unwrap())
        };
        builder = builder.add(id, Resource::from_contents(doc)).unwrap();
    }
    let registry = builder.prepare().unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry)
        .should_validate_formats(true)
        .build(
            &json!({ "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "$ref": format!("{B}{reference}") }),
        )
        .unwrap();
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{reference}: {errors:?}");
}

/// A 16-bit 8 kHz mono WAV of `ms` milliseconds.
fn wav(ms: u32) -> Vec<u8> {
    let data_len = ms * 16;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8000u32.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend((0..data_len).map(|i| (i % 7) as u8));
    out
}

struct World {
    h: Harness,
    edge: ScriptedEdge,
}

impl World {
    async fn new() -> Option<Self> {
        let edge = ScriptedEdge::default();
        let mut permissions = CALLING_PERMISSIONS.to_vec();
        permissions.push("telephony-leg.media");
        let h = Harness::build(
            &permissions,
            all_on().with_enterprise_sip(true),
            Some(Arc::new(edge.clone())),
        )
        .await?;
        let now = h.clock_now();
        h.sql(&format!(
            "INSERT INTO telephony_enterprise_devices (id, device_ref, edge_endpoint, tenant_id, application_id, \
               environment_id, status, created_at, updated_at) \
             VALUES ('{}', 'dev-front-desk-01', 'frontdesk', '{}', '{}', '{}', 'ENABLED', '{now}', '{now}')",
            Uuid::new_v4(),
            h.consumer.tenant_id,
            h.consumer.application_id,
            h.consumer.environment_id,
        ))
        .await;
        Some(Self { h, edge })
    }

    fn event(&self, channel: &str, kind: EdgeEventKind) -> EdgeEvent {
        EdgeEvent {
            event_id: format!("t-{}", Uuid::new_v4().simple()),
            channel: channel.to_string(),
            application_media: false,
            correlation_key: None,
            kind,
            at: self.h.now(),
        }
    }

    /// An enterprise dialog offered to (fake) Calling: (telephony leg, channel).
    async fn dialog(&self) -> (Uuid, String) {
        let channel = format!("chan-{}", Uuid::new_v4().simple());
        self.edge.add_live(&channel, LegStatus::Ringing);
        self.h
            .svc
            .handle_edge_event(self.event(
                &channel,
                EdgeEventKind::EnterpriseInbound(EdgeEnterpriseDialog {
                    endpoint: "frontdesk".into(),
                    extension: "6000".into(),
                }),
            ))
            .await
            .unwrap();
        let leg = self
            .h
            .text(&format!(
                "SELECT id::text FROM telephony_legs WHERE edge_channel_ref = '{channel}'"
            ))
            .await
            .expect("enterprise leg");
        (leg.parse().unwrap(), channel)
    }

    async fn command(&self, leg: Uuid, action: &str, extra: Value) -> common::Response {
        let mut body = json!({
            "schemaVersion": "1",
            "commandId": Uuid::new_v4(),
            "idempotencyKey": format!("{action}-{}", Uuid::new_v4().simple()),
            "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
        });
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        self.h
            .calling(
                Method::POST,
                &format!("/internal/v1/legs/{leg}/{action}"),
                Some(body),
            )
            .await
    }

    async fn capture(&self, leg: Uuid, capture: Uuid) -> common::Response {
        self.command(
            leg,
            "capture",
            json!({ "capture": { "captureId": capture, "purpose": "VOICEMAIL",
                                  "maxDurationSeconds": 60, "beep": true } }),
        )
        .await
    }

    async fn state(&self, capture: Uuid) -> String {
        self.h
            .text(&format!(
                "SELECT status || ':' || COALESCE(failure_code, '') || ':' || COALESCE(last_error, '') \
                   FROM telephony_captures WHERE id = '{capture}'"
            ))
            .await
            .unwrap_or_default()
    }

    async fn finish(&self, channel: &str, capture: Uuid, audio: Option<Vec<u8>>) {
        let name = telephony::capture::spool_name(capture);
        if let Some(bytes) = audio {
            self.edge.put_spool(&name, bytes);
        }
        self.h
            .svc
            .handle_edge_event(self.event(channel, EdgeEventKind::CaptureFinished { name }))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn capture_is_recorded_delivered_and_the_spool_is_removed() {
    let Some(w) = World::new().await else {
        return;
    };
    let (leg, channel) = w.dialog().await;
    let r = w.command(leg, "answer", json!({})).await;
    assert!(r.status.is_success(), "{}", r.text);
    let capture = Uuid::new_v4();
    let r = w.capture(leg, capture).await;
    assert!(r.status.is_success(), "{}", r.text);
    w.h.run().await;
    // The edge records under Telephony's own spool name.
    let name = telephony::capture::spool_name(capture);
    let started = w.edge.recordings();
    assert_eq!(started.get(&name), Some(&(channel.clone(), 60, true)));
    assert_eq!(w.state(capture).await, "RECORDING::");
    // The recording ends; the audio is delivered with its integrity metadata.
    let audio = wav(1500);
    w.finish(&channel, capture, Some(audio.clone())).await;
    assert_eq!(w.state(capture).await, "FINISHED::");
    w.h.run().await;
    assert_eq!(w.state(capture).await, "DELIVERED::");
    let reports = w.h.calling.captures();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    conforms(
        "telephony/capture/v1.schema.json#/$defs/CaptureReport",
        report,
    );
    assert_eq!(report["outcome"], "COMPLETED");
    assert_eq!(report["captureId"], capture.to_string());
    assert_eq!(report["telephonyLegId"], leg.to_string());
    assert_eq!(report["media"]["durationMs"], 1500);
    assert_eq!(report["media"]["sizeBytes"], audio.len());
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(report["media"]["base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, audio);
    use sha2::Digest;
    assert_eq!(
        report["media"]["sha256"],
        hex::encode(sha2::Sha256::digest(&audio))
    );
    // Never a spool name, channel or path in what Calling receives.
    let text = report.to_string();
    assert!(!text.contains(&name) && !text.contains(&channel), "{text}");
    // The transient spool copy is gone; the row keeps only metadata.
    assert!(w.edge.spool_files().is_empty());
    assert_eq!(
        w.h.scalar(&format!(
            "SELECT count(*) FROM telephony_captures WHERE id = '{capture}' AND spool_deleted_at IS NOT NULL \
               AND duration_ms = 1500"
        ))
        .await,
        1
    );
    // Re-sending the same CAPTURE is idempotent (nothing recorded twice).
    let r = w.capture(leg, capture).await;
    assert!(r.status.is_success(), "{}", r.text);
    w.h.run().await;
    assert_eq!(w.h.calling.captures().len(), 1);
}

#[tokio::test]
async fn calling_outages_retry_and_refusals_dead_letter() {
    let Some(w) = World::new().await else {
        return;
    };
    let (leg, channel) = w.dialog().await;
    let capture = Uuid::new_v4();
    assert!(w.capture(leg, capture).await.status.is_success());
    w.h.run().await;
    w.finish(&channel, capture, Some(wav(800))).await;
    // Calling (or its artifact storage) is down: the spool is kept, retried with backoff.
    w.h.calling.set_capture_mode("UNAVAILABLE");
    w.h.run().await;
    assert_eq!(w.state(capture).await, "FINISHED::CALLING_UNAVAILABLE");
    assert_eq!(w.edge.spool_files().len(), 1);
    w.h.run().await;
    assert_eq!(
        w.h.scalar(&format!(
            "SELECT delivery_attempts::bigint FROM telephony_captures WHERE id = '{capture}'"
        ))
        .await,
        1,
        "not retried before its backoff"
    );
    w.h.calling.set_capture_mode("OK");
    w.h.advance(3);
    w.h.run().await;
    assert_eq!(w.state(capture).await, "DELIVERED::");
    assert!(w.edge.spool_files().is_empty());

    // Calling refuses for good: DEAD, an incident, and the spool is deleted anyway.
    let (leg, channel) = w.dialog().await;
    let capture = Uuid::new_v4();
    assert!(w.capture(leg, capture).await.status.is_success());
    w.h.run().await;
    w.finish(&channel, capture, Some(wav(800))).await;
    w.h.calling.set_capture_mode("DENIED");
    w.h.run().await;
    assert_eq!(w.state(capture).await, "DEAD::CALLING_REFUSED");
    assert!(w.edge.spool_files().is_empty());
    assert_eq!(
        w.h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'CAPTURE_DELIVERY_DEAD'"
        )
        .await,
        1
    );

    // The spool cannot be deleted right now: retried by the next pass.
    w.h.calling.set_capture_mode("OK");
    let (leg, channel) = w.dialog().await;
    let capture = Uuid::new_v4();
    assert!(w.capture(leg, capture).await.status.is_success());
    w.h.run().await;
    w.finish(&channel, capture, Some(wav(400))).await;
    w.edge.set_spool_unavailable(true);
    w.h.run().await;
    assert_eq!(w.state(capture).await, "FINISHED::EDGE_UNAVAILABLE");
    w.edge.set_spool_unavailable(false);
    w.h.advance(3);
    w.h.run().await;
    assert_eq!(w.state(capture).await, "DELIVERED::");
    assert!(w.edge.spool_files().is_empty());
}

#[tokio::test]
async fn failures_and_lost_end_events_are_reported() {
    let Some(w) = World::new().await else {
        return;
    };
    // The edge fails the recording: a bounded failure reaches Calling.
    let (leg, channel) = w.dialog().await;
    let capture = Uuid::new_v4();
    assert!(w.capture(leg, capture).await.status.is_success());
    w.h.run().await;
    w.h.svc
        .handle_edge_event(w.event(
            &channel,
            EdgeEventKind::CaptureFailed {
                name: telephony::capture::spool_name(capture),
            },
        ))
        .await
        .unwrap();
    w.h.run().await;
    let reports = w.h.calling.captures();
    conforms(
        "telephony/capture/v1.schema.json#/$defs/CaptureReport",
        reports.last().unwrap(),
    );
    assert_eq!(reports.last().unwrap()["outcome"], "FAILED");
    assert_eq!(
        reports.last().unwrap()["failureCode"],
        "EDGE_RECORDING_FAILED"
    );
    assert!(reports.last().unwrap().get("media").is_none());

    // The end event is lost and no file exists: after its bound, EDGE_RECORDING_MISSING.
    let (leg, _) = w.dialog().await;
    let capture = Uuid::new_v4();
    assert!(w.capture(leg, capture).await.status.is_success());
    w.h.run().await;
    w.h.advance(60 + 121);
    w.h.run().await;
    let reports = w.h.calling.captures();
    assert_eq!(reports.last().unwrap()["captureId"], capture.to_string());
    assert_eq!(
        reports.last().unwrap()["failureCode"],
        "EDGE_RECORDING_MISSING"
    );
    assert_eq!(w.state(capture).await, "DELIVERED:EDGE_RECORDING_MISSING:");

    // The caller is gone before the recording starts: CALLER_GONE.
    let (leg, channel) = w.dialog().await;
    let capture = Uuid::new_v4();
    assert!(w.capture(leg, capture).await.status.is_success());
    w.edge
        .end_remote(&channel, telephony::domain::EndReason::Completed, true);
    w.h.run().await;
    w.h.run().await;
    assert_eq!(w.state(capture).await, "DELIVERED:CALLER_GONE:");
}

#[tokio::test]
async fn capture_is_refused_outside_enterprise_legs_and_behind_the_switch() {
    let Some(w) = World::new().await else {
        return;
    };
    let (leg, _) = w.dialog().await;
    // Strict bodies: purpose, bound, no unknown fields (never a path or URL).
    for capture in [
        json!({ "captureId": Uuid::new_v4(), "purpose": "SNAPSHOT", "maxDurationSeconds": 60, "beep": true }),
        json!({ "captureId": Uuid::new_v4(), "purpose": "VOICEMAIL", "maxDurationSeconds": 0, "beep": true }),
        json!({ "captureId": Uuid::new_v4(), "purpose": "VOICEMAIL", "maxDurationSeconds": 601, "beep": true }),
        json!({ "captureId": Uuid::new_v4(), "purpose": "VOICEMAIL", "maxDurationSeconds": 60, "beep": true,
                "path": "/var/spool/asterisk/recording/x" }),
    ] {
        let r = w
            .command(leg, "capture", json!({ "capture": capture }))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    }
    let r = w.command(leg, "capture", json!({})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = w
        .command(
            leg,
            "prompt",
            json!({ "prompt": "WELCOME", "capture": { "captureId": Uuid::new_v4(), "purpose": "VOICEMAIL",
                                                      "maxDurationSeconds": 60, "beep": true } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // A public-network leg is never captured here.
    let (public, _) = w.h.originate().await;
    let r = w.capture(public, Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.code(), "LEG_NOT_ACTIVE");
    // The kill switch stops new captures; delivery / cleanup of existing ones continues.
    let (leg2, channel2) = w.dialog().await;
    let capture = Uuid::new_v4();
    assert!(w.capture(leg2, capture).await.status.is_success());
    w.h.run().await;
    w.h.flags.enterprise_sip.set(false);
    let r = w.capture(leg, Uuid::new_v4()).await;
    assert_eq!(r.code(), "LEG_NOT_ACTIVE", "{}", r.text);
    w.finish(&channel2, capture, Some(wav(300))).await;
    w.h.run().await;
    assert_eq!(w.state(capture).await, "DELIVERED::");
    assert!(w.edge.spool_files().is_empty());
}

/// Part F §61: a call RECORDING records the leg's bridge (the mixed call audio) when the
/// leg is bridged, else its channel; CAPTURE_STOP ends it early and is never blocked by a
/// kill switch; bounds differ per purpose.
#[tokio::test]
async fn recording_captures_the_bridge_and_stops_early() {
    let Some(w) = World::new().await else {
        return;
    };
    let (leg, channel) = w.dialog().await;
    let recording = |capture: Uuid, max: i64| {
        json!({ "capture": { "captureId": capture, "purpose": "RECORDING",
                              "maxDurationSeconds": max, "beep": false } })
    };
    // Bounds: a recording up to 1440 s, a voicemail up to 600 s.
    let r = w
        .command(leg, "capture", recording(Uuid::new_v4(), 1441))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = w
        .command(
            leg,
            "capture",
            json!({ "capture": { "captureId": Uuid::new_v4(), "purpose": "VOICEMAIL",
                                  "maxDurationSeconds": 601, "beep": true } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Not bridged: the channel is recorded.
    let first = Uuid::new_v4();
    let r = w.command(leg, "capture", recording(first, 1440)).await;
    assert!(r.status.is_success(), "{}", r.text);
    w.h.run().await;
    let name = telephony::capture::spool_name(first);
    assert_eq!(
        w.edge.recordings().get(&name),
        Some(&(channel.clone(), 1440, false))
    );
    // CAPTURE_STOP stops it at the edge — even with every kill switch off.
    w.h.flags.enterprise_sip.set(false);
    w.h.flags.telephony.set(false);
    let r = w.command(leg, "capture-stop", recording(first, 1440)).await;
    assert!(r.status.is_success(), "{}", r.text);
    w.h.run().await;
    assert_eq!(w.edge.stopped(), std::slice::from_ref(&name));
    w.finish(&channel, first, Some(wav(2000))).await;
    w.h.run().await;
    assert_eq!(w.state(first).await, "DELIVERED::");
    w.h.flags.enterprise_sip.set(true);
    w.h.flags.telephony.set(true);
    // Bridged: the bridge's mixed audio is recorded.
    let now = w.h.clock_now();
    let bridge = Uuid::new_v4();
    w.h.sql(&format!(
        "INSERT INTO telephony_bridges (id, calling_call_id, tenant_id, application_id, environment_id, kind, \
           status, media_join_code, edge_bridge_ref, created_at, established_at, updated_at) \
         SELECT '{bridge}', calling_call_id, tenant_id, application_id, environment_id, 'APPLICATION_MEDIA', \
           'ACTIVE', 'cfmj{}', 'cfmb-test-1', '{now}', '{now}', '{now}' FROM telephony_legs WHERE id = '{leg}'",
        Uuid::new_v4().simple()
    ))
    .await;
    w.h.sql(&format!(
        "INSERT INTO telephony_bridge_legs (id, bridge_id, member_kind, telephony_leg_id, edge_channel_ref, \
           status, created_at, joined_at, updated_at) \
         VALUES ('{}', '{bridge}', 'PUBLIC_LEG', '{leg}', '{channel}', 'JOINED', '{now}', '{now}', '{now}')",
        Uuid::new_v4()
    ))
    .await;
    let second = Uuid::new_v4();
    let r = w.command(leg, "capture", recording(second, 600)).await;
    assert!(r.status.is_success(), "{}", r.text);
    w.h.run().await;
    assert_eq!(
        w.edge
            .recordings()
            .get(&telephony::capture::spool_name(second)),
        Some(&("bridge:cfmb-test-1".to_string(), 600, false))
    );
    // Stopping a capture that already finished is a no-op success.
    let r = w
        .command(leg, "capture-stop", recording(Uuid::new_v4(), 60))
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    w.h.run().await;
    assert_eq!(
        w.h.scalar(
            "SELECT count(*) FROM telephony_commands WHERE kind = 'CAPTURE_STOP' AND status = 'SUCCEEDED'"
        )
        .await,
        2
    );
}

/// Part F: an enterprise device's leg (no provider binding — its edge channel is bound
/// directly) is bridged to application media: the edge answers it, the application side
/// joins with the single-use code, the bridge is ACTIVE and the leg CONNECTED; a call
/// RECORDING of it then records the bridge.
#[tokio::test]
async fn an_enterprise_leg_bridges_to_application_media() {
    let Some(w) = World::new().await else {
        return;
    };
    let (leg, channel) = w.dialog().await;
    let call: Uuid =
        w.h.text(&format!(
            "SELECT calling_call_id::text FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap()
        .parse()
        .unwrap();
    let r =
        w.h.calling(
            Method::POST,
            "/internal/v1/bridges",
            Some(json!({
                "schemaVersion": "1",
                "commandId": Uuid::new_v4(),
                "idempotencyKey": format!("bridge-{}", Uuid::new_v4().simple()),
                "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
                "callId": call,
                "kind": "APPLICATION_MEDIA",
                "telephonyLegIds": [leg],
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let code = r.json["bridge"]["mediaJoinCode"]
        .as_str()
        .expect("join code")
        .to_string();
    w.h.run().await;
    // The edge answered the ringing device leg and put its channel in the bridge.
    assert_eq!(w.h.leg_status(leg).await, "ANSWERED");
    assert_eq!(
        w.h.scalar(
            "SELECT count(*) FROM telephony_bridge_legs WHERE member_kind = 'PUBLIC_LEG' AND status = 'JOINED'"
        )
        .await,
        1
    );
    // The application media side joins with its code: ACTIVE, the leg CONNECTED.
    let media = "app-media-1".to_string();
    w.edge.add_live(&media, LegStatus::Connected);
    assert!(w
        .h
        .svc
        .application_media_joined(&code, &media)
        .await
        .unwrap());
    assert_eq!(
        w.h.text("SELECT status FROM telephony_bridges")
            .await
            .unwrap(),
        "ACTIVE"
    );
    assert_eq!(w.h.leg_status(leg).await, "CONNECTED");
    // A recording of the bridged leg records the bridge.
    let capture = Uuid::new_v4();
    let r = w
        .command(
            leg,
            "capture",
            json!({ "capture": { "captureId": capture, "purpose": "RECORDING",
                                  "maxDurationSeconds": 600, "beep": false } }),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    w.h.run().await;
    let target = w
        .edge
        .recordings()
        .get(&telephony::capture::spool_name(capture))
        .map(|r| r.0.clone())
        .unwrap();
    assert!(
        target.starts_with("bridge:"),
        "{target} (channel {channel})"
    );
}

/// The application media side may present its join code before the bridge command has
/// created the edge bridge (LiveKit SIP dials as soon as Calling has the code): the channel
/// is held, not refused, and the bridge command attaches it — exactly once. A second
/// presentation of the same code is refused.
#[tokio::test]
async fn application_media_arriving_before_the_edge_bridge_is_held_and_attached() {
    let Some(w) = World::new().await else {
        return;
    };
    let (leg, _channel) = w.dialog().await;
    let call: Uuid =
        w.h.text(&format!(
            "SELECT calling_call_id::text FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap()
        .parse()
        .unwrap();
    let r =
        w.h.calling(
            Method::POST,
            "/internal/v1/bridges",
            Some(json!({
                "schemaVersion": "1",
                "commandId": Uuid::new_v4(),
                "idempotencyKey": format!("bridge-{}", Uuid::new_v4().simple()),
                "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
                "callId": call,
                "kind": "APPLICATION_MEDIA",
                "telephonyLegIds": [leg],
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let code = r.json["bridge"]["mediaJoinCode"]
        .as_str()
        .unwrap()
        .to_string();
    // Before the command ran: no edge bridge yet. The media channel is held.
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM telephony_bridges WHERE edge_bridge_ref IS NULL")
            .await,
        1
    );
    let media = "app-media-early".to_string();
    w.edge.add_live(&media, LegStatus::Connected);
    assert!(w
        .h
        .svc
        .application_media_joined(&code, &media)
        .await
        .unwrap());
    assert_eq!(
        w.h.text("SELECT status FROM telephony_bridges")
            .await
            .unwrap(),
        "PENDING"
    );
    // The same code presented again (another channel) is refused.
    let other = "app-media-second".to_string();
    w.edge.add_live(&other, LegStatus::Connected);
    assert!(!w
        .h
        .svc
        .application_media_joined(&code, &other)
        .await
        .unwrap());
    // The bridge command creates the edge bridge and attaches the held channel.
    w.h.run().await;
    assert_eq!(
        w.h.text("SELECT status FROM telephony_bridges")
            .await
            .unwrap(),
        "ACTIVE"
    );
    assert_eq!(
        w.h.text(
            "SELECT edge_channel_ref FROM telephony_bridge_legs WHERE member_kind = 'APPLICATION_MEDIA' AND status = 'JOINED'"
        )
        .await
        .unwrap(),
        media
    );
    assert_eq!(w.h.leg_status(leg).await, "CONNECTED");
    // Used: the code admits nothing more.
    assert!(!w
        .h
        .svc
        .application_media_joined(&code, &other)
        .await
        .unwrap());
}

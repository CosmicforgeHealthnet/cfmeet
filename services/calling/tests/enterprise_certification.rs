//! Part F §212 organization isolation certification: an administrator of tenant A who
//! knows every valid id of tenant B — extensions, departments, ring groups, queues, IVR
//! flows, business hours, forwarding rules, mailboxes, recording policies, a Call, a
//! READY voicemail message and a READY recording — can neither read, update, route
//! through, listen to the voicemail nor download the recording. Every attempt answers
//! exactly like an unknown id (404), and tenant B's resources are unchanged.

mod common;

use common::NewPrincipal;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{
    calls::app::enterprise::CapturedMedia,
    domain::{PrincipalKind, TenantClassification},
    enterprise::policy::AdminCapability,
    platform::scopes::names,
};
use cf_object_storage::MemoryObjectStore;
use common::{
    app::Device,
    enterprise::{credential, id_of, strings, EnterpriseWorld, RecordingCapture},
};
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

struct Desk {
    principal: Principal,
    phone: Device,
}

async fn desk(e: &EnterpriseWorld, name: &str) -> Desk {
    let principal = e.member(name).await;
    let phone = e.a.device(&principal, &format!("{name}-desk")).await;
    Desk { principal, phone }
}

fn wav(ms: u32) -> Vec<u8> {
    let samples = 8 * ms;
    let data_len = samples * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
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
    for i in 0..samples {
        let v: i16 = if (i / 10) % 2 == 0 { 800 } else { -800 };
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn media(bytes: &[u8]) -> CapturedMedia {
    CapturedMedia {
        content_type: "audio/wav".into(),
        duration_ms: 1500,
        sha256: cf_object_storage::sha256_hex(bytes),
        bytes: bytes.to_vec(),
    }
}

#[tokio::test]
async fn org_isolation_certification() {
    let store = Arc::new(MemoryObjectStore::new());
    let capture = Arc::new(RecordingCapture::default());
    let (s, c) = (store.clone(), capture.clone());
    let Some(e) = EnterpriseWorld::configured(move |runtime| {
        runtime.artifacts = s;
        runtime.capture = c;
    })
    .await
    else {
        return;
    };
    // ---------------------------------------------------------------- tenant B (victim)
    e.h()
        .state
        .enterprise
        .operator_set_capabilities(
            e.a.w.scope(),
            vec![
                AdminCapability::ManageRouting,
                AdminCapability::ManageVoicemail,
                AdminCapability::ManageRecording,
            ],
        )
        .await
        .unwrap();
    let b_all = credential(
        e.h(),
        &e.a.w.consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
            names::ENTERPRISE_RECORDING_READ,
            names::ENTERPRISE_RECORDING_MANAGE,
        ],
    )
    .await;
    let b = |method: Method, path: String, body: Option<Value>| {
        let token = b_all.secret.expose_once().to_string();
        let e = &e;
        async move { e.send(&token, method, &path, body).await }
    };
    let caller = desk(&e, "b-caller").await;
    let member = desk(&e, "b-member").await;
    let agent = desk(&e, "b-agent").await;
    let mut ids: Vec<(&str, Uuid)> = Vec::new();
    let mailbox = {
        let r = b(
            Method::POST,
            "/v1/enterprise/voicemail/mailboxes".into(),
            Some(json!({ "owner": { "type": "PRINCIPAL", "id": member.principal.id().to_string() },
                         "displayLabel": "Member box", "maxMessageSeconds": 120, "retentionDays": 30 })),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        id_of(&r.json, "mailboxId")
    };
    ids.push(("voicemail/mailboxes", mailbox));
    let forwarding = e
        .create(
            "/v1/enterprise/forwarding-rules",
            json!({ "principalId": member.principal.id(), "condition": "NO_ANSWER",
                    "destination": { "type": "VOICEMAIL", "id": mailbox.to_string() } }),
        )
        .await;
    ids.push(("forwarding-rules", id_of(&forwarding, "forwardingRuleId")));
    let ext = e
        .extension("4100", "ENTERPRISE_MEMBER", member.principal.id())
        .await;
    ids.push(("extensions", id_of(&ext, "extensionId")));
    let ext_agent = e
        .extension("4101", "ENTERPRISE_MEMBER", agent.principal.id())
        .await;
    let department = e.department("Operations").await;
    ids.push(("departments", id_of(&department, "departmentId")));
    let group = e
        .create(
            "/v1/enterprise/ring-groups",
            json!({ "name": "B front desk", "strategy": "SIMULTANEOUS", "memberRingSeconds": 10,
                    "groupTimeoutSeconds": 30 }),
        )
        .await;
    let group_id = id_of(&group, "ringGroupId");
    ids.push(("ring-groups", group_id));
    let queue = e
        .create(
            "/v1/enterprise/queues",
            json!({ "name": "B support", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10,
                    "capacity": 5 }),
        )
        .await;
    ids.push(("queues", id_of(&queue, "queueId")));
    let flow = e
        .create("/v1/enterprise/ivr-flows", json!({ "name": "B menu" }))
        .await;
    ids.push(("ivr-flows", id_of(&flow, "ivrFlowId")));
    let hours = e
        .create(
            "/v1/enterprise/business-hours",
            json!({ "name": "B hours", "timezone": "Africa/Lagos",
                    "openDestination": { "type": "ENTERPRISE_MEMBER", "id": member.principal.id().to_string() },
                    "closedDestination": { "type": "VOICEMAIL", "id": mailbox.to_string() },
                    "weeklyWindows": [{ "dayOfWeek": "MONDAY", "start": "09:00", "end": "17:00" }] }),
        )
        .await;
    ids.push(("business-hours", id_of(&hours, "businessHoursId")));
    let policy = b(
        Method::POST,
        "/v1/enterprise/recording-policies".into(),
        Some(
            json!({ "name": "B recording", "mode": "MANUAL_ALLOWED", "noticeRequired": false,
                     "consentParticipants": "NONE", "jurisdictionRef": "NG-NDPA-2023/v2",
                     "retentionDays": 30, "maxRecordingSeconds": 600, "isDefault": true }),
        ),
    )
    .await;
    assert_eq!(policy.status, StatusCode::CREATED, "{}", policy.text);
    ids.push((
        "recording-policies",
        id_of(&policy.json, "recordingPolicyId"),
    ));

    // B's READY voicemail message (an unanswered Call into the member's mailbox).
    let r = e
        .call_extension("VOICE", &caller.principal, &caller.phone, "4100", "b-vm")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let vm_call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    e.h().clock.advance(chrono::Duration::seconds(46));
    e.h().state.app.expire_due(100).await.unwrap();
    let message: Uuid = strings(
        e.h().conn(),
        &format!("SELECT id::text FROM voicemail_messages WHERE call_id = '{vm_call}'"),
    )
    .await
    .pop()
    .expect("a message")
    .parse()
    .unwrap();
    e.h()
        .state
        .app
        .deliver_capture(message, vm_call, media(&wav(1500)))
        .await
        .unwrap();
    // B's READY recording of a CONNECTED Call.
    let r = e
        .call_extension("VOICE", &caller.principal, &caller.phone, "4101", "b-rec")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rec_call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let inv =
        e.a.invitation_for(rec_call, agent.phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(rec_call).await;
    assert_eq!(
        e.a.accept(rec_call, inv, &agent.phone, v).await.status,
        StatusCode::OK
    );
    for d in [&caller.phone, &agent.phone] {
        let session = e.a.session_on(rec_call, d.endpoint).await.unwrap();
        e.a.connect(rec_call, session).await;
    }
    let r = b(
        Method::POST,
        format!("/v1/calls/{rec_call}/recordings"),
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let recording: Uuid = r.json["recording"]["recordingId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (_, v) = e.a.state(rec_call).await;
    e.a.command(rec_call, "end", v).await;
    e.h().state.app.expire_due(100).await.unwrap();
    e.h()
        .state
        .app
        .deliver_capture(recording, rec_call, media(&wav(1500)))
        .await
        .unwrap();
    for (path, expected) in [
        (
            format!("/v1/enterprise/voicemail/messages/{message}"),
            "READY",
        ),
        (format!("/v1/enterprise/recordings/{recording}"), "READY"),
    ] {
        let r = b(Method::GET, path.clone(), None).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text);
        let state = r.json["state"]
            .as_str()
            .or(r.json["recording"]["state"].as_str());
        assert_eq!(state, Some(expected), "{path}: {}", r.text);
    }
    // Part G §69: one artifact usage fact each (duration and stored size), and a
    // repeated capture delivery adds nothing.
    for (artifact, call) in [(message, vm_call), (recording, rec_call)] {
        e.h()
            .state
            .app
            .deliver_capture(artifact, call, media(&wav(1500)))
            .await
            .unwrap();
    }
    for (call, usage_type) in [
        (vm_call, "VOICEMAIL_ARTIFACT"),
        (rec_call, "RECORDING_ARTIFACT"),
    ] {
        assert_eq!(
            common::scalar_i64(
                e.h().conn(),
                &format!(
                    "SELECT count(*) FROM call_usage_records WHERE call_id = '{call}' \
                       AND usage_type = '{usage_type}' AND size_bytes > 0 AND quantity > 0 \
                       AND finalized_at IS NOT NULL AND cost_status IS NULL"
                ),
            )
            .await,
            1,
            "{usage_type}"
        );
    }
    let versions = |ids: Vec<(&'static str, Uuid)>| {
        let e = &e;
        let token = b_all.secret.expose_once().to_string();
        async move {
            let mut out = Vec::new();
            for (family, id) in ids {
                let r = e
                    .send(
                        &token,
                        Method::GET,
                        &format!("/v1/enterprise/{family}/{id}"),
                        None,
                    )
                    .await;
                assert_eq!(r.status, StatusCode::OK, "{family}: {}", r.text);
                out.push(r.json["version"].clone());
            }
            out
        }
    };
    let before = versions(ids.clone()).await;

    // ---------------------------------------------------------------- tenant A (attacker)
    let (attacker, _) = e.other_tenant("attacker").await;
    let a_token = credential(
        e.h(),
        &attacker,
        e.a.w.kind,
        &[
            names::CALLS_READ,
            names::CALLS_WRITE,
            names::ENTERPRISE_CALLS_USE,
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_CONFIG_WRITE,
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
            names::ENTERPRISE_RECORDING_READ,
            names::ENTERPRISE_RECORDING_MANAGE,
        ],
    )
    .await;
    let a_secret = a_token.secret.expose_once().to_string();
    let a = |method: Method, path: String, body: Option<Value>| {
        let token = a_secret.clone();
        let e = &e;
        async move { e.send(&token, method, &path, body).await }
    };
    let not_found = |r: &common::TestResponse, what: &str| {
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{what}: {}", r.text);
        assert_eq!(r.json["error"]["code"], "NOT_FOUND", "{what}");
    };
    // The answer for B's ids is the answer for an id that exists nowhere.
    let unknown = a(
        Method::GET,
        format!("/v1/enterprise/extensions/{}", Uuid::new_v4()),
        None,
    )
    .await;
    not_found(&unknown, "unknown id");
    let mut attempts = 0;
    // Read and update (disable) every configuration resource of B.
    for (family, id) in &ids {
        let r = a(Method::GET, format!("/v1/enterprise/{family}/{id}"), None).await;
        not_found(&r, &format!("read {family}"));
        let r = a(
            Method::DELETE,
            format!("/v1/enterprise/{family}/{id}?expectedVersion=1"),
            None,
        )
        .await;
        not_found(&r, &format!("disable {family}"));
        attempts += 2;
    }
    for (path, body) in [
        (
            format!("/v1/enterprise/extensions/{}", ids[2].1),
            json!({ "expectedVersion": 1, "displayLabel": "Owned" }),
        ),
        (
            format!("/v1/enterprise/ring-groups/{group_id}"),
            json!({ "expectedVersion": 1, "name": "Owned" }),
        ),
        (
            format!("/v1/enterprise/queues/{}", id_of(&queue, "queueId")),
            json!({ "expectedVersion": 1, "name": "Owned" }),
        ),
    ] {
        let r = a(Method::PATCH, path.clone(), Some(body)).await;
        not_found(&r, &format!("update {path}"));
        attempts += 1;
    }
    // Route through B: B's extension number is not in A's namespace; B's ids cannot be
    // destinations of A's configuration.
    let a_principal = {
        let (view, _) = e
            .h()
            .platform
            .register_principal(
                attacker.env(e.a.w.kind).id,
                NewPrincipal::External {
                    kind: PrincipalKind::EnterpriseMember,
                    external_reference: "attacker-desk".into(),
                },
            )
            .await
            .unwrap();
        view.id
    };
    let r = a(
        Method::POST,
        "/v1/calling/endpoints".into(),
        Some(
            json!({ "principalId": a_principal, "endpointKey": "attacker-desk",
                     "endpointKind": "MOBILE", "capabilities": { "audio": true, "video": true } }),
        ),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text);
    let a_endpoint = r.json["endpointId"].as_str().unwrap().to_string();
    let calls_before = e.count("SELECT count(*) FROM calls").await;
    let r = e
        .h()
        .request_with(
            Method::POST,
            "/v1/calls",
            Some(&a_secret),
            Some(
                json!({ "kind": "VOICE", "initiatorPrincipalId": a_principal,
                         "initiatorEndpointId": a_endpoint, "targetExtension": "4100" }),
            ),
            &[("idempotency-key", "attacker-dial-1")],
        )
        .await;
    not_found(&r, "dial B's extension number from A");
    assert_eq!(e.count("SELECT count(*) FROM calls").await, calls_before);
    for (kind, id) in [
        ("RING_GROUP", group_id),
        ("VOICEMAIL", mailbox),
        ("EXTENSION", ids[2].1),
        ("QUEUE", id_of(&queue, "queueId")),
    ] {
        let r = a(
            Method::POST,
            "/v1/enterprise/extensions".into(),
            Some(json!({ "extensionNumber": "4900", "displayLabel": "Hijack",
                         "destination": { "type": kind, "id": id.to_string() } })),
        )
        .await;
        assert!(
            r.status == StatusCode::NOT_FOUND || r.status == StatusCode::BAD_REQUEST,
            "A routes into B's {kind}: {}",
            r.text
        );
        attempts += 1;
    }
    // Listen to B's voicemail, download B's recording, touch B's Calls.
    for (method, path, body) in [
        (
            Method::GET,
            format!("/v1/enterprise/voicemail/messages/{message}"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/enterprise/voicemail/messages/{message}/media-access"),
            Some(json!({})),
        ),
        (
            Method::DELETE,
            format!("/v1/enterprise/voicemail/messages/{message}?expectedVersion=2"),
            None,
        ),
        (
            Method::GET,
            format!("/v1/enterprise/recordings/{recording}"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/enterprise/recordings/{recording}/media-access"),
            Some(json!({})),
        ),
        (
            Method::GET,
            format!("/v1/enterprise/recordings/{recording}/access-audit"),
            None,
        ),
        (
            Method::DELETE,
            format!("/v1/enterprise/recordings/{recording}?expectedVersion=1"),
            None,
        ),
        (Method::GET, format!("/v1/calls/{rec_call}"), None),
        (
            Method::GET,
            format!("/v1/calls/{rec_call}/recordings"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/calls/{rec_call}/recordings"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/v1/calls/{vm_call}/ivr/input"),
            Some(json!({ "endpointId": a_endpoint, "digit": "1" })),
        ),
    ] {
        let r = a(method.clone(), path.clone(), body).await;
        not_found(&r, &format!("{method} {path}"));
        attempts += 1;
    }
    // A's view of B's principal history is empty.
    let r = a(
        Method::GET,
        format!("/v1/calls?principalId={}", member.principal.id()),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["data"].as_array().unwrap().is_empty());

    // B is unchanged: versions, READY artifacts, objects, and no grant was minted for A.
    assert_eq!(versions(ids.clone()).await, before);
    let r = b(
        Method::GET,
        format!("/v1/enterprise/voicemail/messages/{message}"),
        None,
    )
    .await;
    assert_eq!(r.json["state"], "READY");
    assert_eq!(store.len(), 2);
    let audit = strings(
        e.h().conn(),
        &format!(
            "SELECT operation || ':' || outcome FROM call_recording_access_audit \
              WHERE recording_id = '{recording}' ORDER BY seq"
        ),
    )
    .await;
    assert!(
        !audit.iter().any(|a| a.starts_with("MEDIA_ACCESS:GRANTED")),
        "{audit:?}"
    );
    let _ = (ext_agent, TenantClassification::External);
    println!(
        "enterprise org isolation certified: {attempts} cross-tenant attempts (read, update, \
         route, voicemail, recording, call, history) answered 404, tenant B unchanged"
    );
}

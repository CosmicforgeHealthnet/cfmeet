//! Part M §25 — automated cross-conference IDOR fuzz matrix.
//!
//! Two complete, independent worlds (A and B) are built: participant, participant
//! session, member, invitation, breakout, message, poll + response, question + vote,
//! recording / artefact, transcript + transcription job, registration. Then, as A's
//! OWNER (the most privileged legitimate caller of A), every `/api/v2` operation that
//! carries a subordinate id — derived from the OpenAPI contract, not a hand list — is
//! called with A's conference / space and B's subordinate id, plus every operation that
//! carries a subordinate id in its body. Each attempt must fail, and a fingerprint of
//! B's rows, every provider call and every stored object must be unchanged.

mod common;

use axum::http::{Method, StatusCode};
use common::{artifacts as art, collab::*, contract::openapi, rtc::*, *};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    ($ctor:expr) => {
        match $ctor.await {
            Some(a) => a,
            None => return,
        }
    };
}

struct World {
    owner: User,
    sid: Uuid,
    cid: Uuid,
    webinar_sid: Uuid,
    guest: Joined,
    member_id: Uuid,
    invitation_id: Uuid,
    breakout_id: Uuid,
    message_id: Uuid,
    poll_id: Uuid,
    question_id: Uuid,
    recording_id: Uuid,
    transcript_id: Uuid,
    job_id: Uuid,
    registration_id: Uuid,
}

async fn world(app: &TestApp, label: &str) -> World {
    let (owner, sid, cid, guests) = art::meeting_with(
        app,
        json!({ "title": label, "accessPolicy": "PUBLIC", "transcriptionPolicy": "HOST_OPTIONAL" }),
        2,
    )
    .await;
    let guest = guests[0].clone();
    let member = app.user(&format!("{label}-member")).await;
    app.add_member(sid, &owner, &member, backend::domain::Role::Participant)
        .await;
    let (_, members) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/members"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let member_id = members["items"]
        .as_array()
        .or_else(|| members["members"].as_array())
        .unwrap()
        .iter()
        .find(|m| m["userId"] == json!(member.id))
        .map(|m| uuid(&m["id"]))
        .unwrap();
    let (_, inv) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "conferenceId": cid }),
        )
        .await;
    let invitation_id = uuid(&inv["invitation"]["id"]);
    let breakout_id = create_rooms(app, cid, &owner, json!({ "count": 1 })).await[0];
    let (_, msg) = send(
        app,
        cid,
        guests[1].auth(),
        json!({ "body": format!("{label} message") }),
    )
    .await;
    let message_id = uuid(&msg["id"]);
    let poll_id = open_poll(app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    assert!(vote(app, cid, poll_id, guests[1].auth(), "o1")
        .await
        .0
        .is_success());
    let (_, q) = app
        .post(
            format!("{}/questions", base(cid)),
            guests[1].auth(),
            json!({ "body": "Q?" }),
        )
        .await;
    let question_id = uuid(&q["id"]);
    app.post(
        format!("{}/questions/{question_id}/vote", base(cid)),
        guest.auth(),
        json!({}),
    )
    .await;
    let (recording_id, _) = art::recording_ready(app, cid, &owner).await;
    let (job_id, transcript_id) =
        art::transcript_ready(app, cid, &owner, &[("hello", None), ("world", None)]).await;
    let webinar = app
        .create_space(
            &owner,
            json!({ "title": format!("{label} webinar"), "accessPolicy": "PUBLIC", "meetingType": "WEBINAR" }),
        )
        .await;
    let webinar_sid = uuid(&webinar["id"]);
    let (s, reg) = app
        .post(
            format!("/api/v2/meeting-spaces/{webinar_sid}/registrations"),
            Auth::None,
            json!({ "displayName": format!("{label} registrant") }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{reg}");
    let registration_id = uuid(&reg["registrationId"]);
    World {
        owner,
        sid,
        cid,
        webinar_sid,
        guest,
        member_id,
        invitation_id,
        breakout_id,
        message_id,
        poll_id,
        question_id,
        recording_id,
        transcript_id,
        job_id,
        registration_id,
    }
}

/// Digest of every row that belongs to `w` (conference, space and subordinate tables).
async fn fingerprint(app: &TestApp, w: &World) -> String {
    let (cid, sid, wsid) = (w.cid, w.sid, w.webinar_sid);
    let tables = [
        format!("SELECT * FROM conference_sessions WHERE id='{cid}'"),
        format!("SELECT * FROM conference_participants WHERE conference_id='{cid}'"),
        format!("SELECT * FROM participant_sessions WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_members WHERE meeting_space_id='{sid}'"),
        format!("SELECT * FROM meeting_invitations WHERE meeting_space_id='{sid}'"),
        format!("SELECT * FROM breakout_rooms WHERE conference_id='{cid}'"),
        format!("SELECT * FROM breakout_assignments WHERE breakout_room_id IN (SELECT id FROM breakout_rooms WHERE conference_id='{cid}')"),
        format!("SELECT * FROM conference_messages WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_polls WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_poll_responses WHERE poll_id IN (SELECT id FROM meeting_polls WHERE conference_id='{cid}')"),
        format!("SELECT * FROM meeting_questions WHERE conference_id='{cid}'"),
        format!("SELECT * FROM meeting_question_votes WHERE question_id IN (SELECT id FROM meeting_questions WHERE conference_id='{cid}')"),
        format!("SELECT * FROM meeting_artifacts WHERE conference_id='{cid}'"),
        format!("SELECT * FROM transcription_jobs WHERE conference_id='{cid}'"),
        format!("SELECT * FROM webinar_registrations WHERE meeting_space_id='{wsid}'"),
        format!("SELECT * FROM meeting_consents WHERE conference_id='{cid}'"),
    ];
    let mut out = String::new();
    for sql in tables {
        let row = app
            .db
            .conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT coalesce(md5(string_agg(row_to_json(t)::text, '|' ORDER BY row_to_json(t)::text)), '-') FROM ({sql}) t"),
            ))
            .await
            .unwrap()
            .unwrap();
        out.push_str(&row.try_get_by_index::<String>(0).unwrap());
        out.push(':');
    }
    out
}

fn subordinate(name: &str, path: &str, b: &World) -> Option<Uuid> {
    Some(match name {
        "participantId" => b.guest.participant_id,
        "memberId" => b.member_id,
        "invitationId" => b.invitation_id,
        "breakoutId" => b.breakout_id,
        "messageId" => b.message_id,
        "pollId" => b.poll_id,
        "questionId" => b.question_id,
        "recordingId" => b.recording_id,
        "artifactId" if path.contains("/transcripts/") => b.transcript_id,
        "artifactId" => b.recording_id,
        "jobId" => b.job_id,
        "registrationId" => b.registration_id,
        _ => return None,
    })
}

fn body_for(method: &str, path: &str) -> Option<Value> {
    if method == "get" || method == "delete" {
        return None;
    }
    let last = path.rsplit('/').next().unwrap();
    Some(match last {
        "announcement" => json!({ "message": "x" }),
        "assignments" => json!({ "add": [] }),
        "close" if path.contains("/breakouts/") => json!({ "countdownSeconds": 0 }),
        "ban" => json!({ "scope": "CONFERENCE" }),
        "mute" => json!({ "trackKind": "AUDIO" }),
        "role" => json!({ "role": "VIEWER" }),
        "responses" => json!({ "answers": [{ "questionId": "q1", "optionIds": ["o1"] }] }),
        "answer" => json!({ "answer": "x" }),
        "translations" => json!({ "targetLanguage": "fr" }),
        "{memberId}" => json!({ "role": "HOST", "version": 1 }),
        _ => json!({}),
    })
}

#[tokio::test]
async fn idor_cross_conference_matrix() {
    let app = app!(TestApp::artifacts());
    let a = world(&app, "A").await;
    let b = world(&app, "B").await;
    let doc = openapi();
    let mut attempts = 0;
    let mut failures = vec![];
    let check = |status: StatusCode, v: &Value, what: &str, failures: &mut Vec<String>| {
        if status.is_success() || !matches!(status.as_u16(), 403 | 404 | 409 | 410) {
            failures.push(format!("{what} → {status} {v}"));
        }
    };

    let path_params = regex::Regex::new(r"\{(\w+)\}").unwrap();
    // 1. Path-carried subordinate ids (contract-derived).
    for (path, item) in doc["paths"].as_object().unwrap() {
        if !path.starts_with("/api/v2/") || path.contains("{userId}") {
            continue;
        }
        let names: Vec<String> = path_params
            .captures_iter(path)
            .map(|c| c[1].to_string())
            .collect();
        if names.len() < 2 {
            continue;
        }
        for method in item.as_object().unwrap().keys() {
            let mut uri = path.clone();
            let mut ok = true;
            for n in &names {
                let value = if n == "id" {
                    if path.contains("/registrations/") {
                        a.webinar_sid
                    } else if path.starts_with("/api/v2/meeting-spaces/") {
                        a.sid
                    } else {
                        a.cid
                    }
                } else if n == "consentType" {
                    uri = uri.replace("{consentType}", "RECORDING");
                    continue;
                } else {
                    match subordinate(n, path, &b) {
                        Some(v) => v,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                };
                uri = uri.replace(&format!("{{{n}}}"), &value.to_string());
            }
            if !ok || path.contains("{consentType}") {
                continue;
            }
            let before = (
                fingerprint(&app, &b).await,
                lk(&app).calls.lock().unwrap().len(),
                app.storage.keys().len(),
            );
            let m: Method = method.to_uppercase().parse().unwrap();
            let (s, v) = app
                .call(
                    m,
                    &uri,
                    Auth::Bearer(&a.owner.token),
                    body_for(method, path),
                )
                .await;
            attempts += 1;
            check(
                s,
                &v,
                &format!("{} {path}", method.to_uppercase()),
                &mut failures,
            );
            let after = (
                fingerprint(&app, &b).await,
                lk(&app).calls.lock().unwrap().len(),
                app.storage.keys().len(),
            );
            if before != after {
                failures.push(format!(
                    "{} {path}: B state / provider / storage changed",
                    method.to_uppercase()
                ));
            }
        }
    }

    // 2. Body-carried subordinate ids.
    let bp = b.guest.participant_id;
    let body_cases: Vec<(Method, String, Auth<'_>, Value)> = vec![
        (
            Method::POST,
            format!("{}/waiting-room/admit", base(a.cid)),
            Auth::Bearer(&a.owner.token),
            json!({ "participantIds": [bp] }),
        ),
        (
            Method::POST,
            format!("{}/waiting-room/deny", base(a.cid)),
            Auth::Bearer(&a.owner.token),
            json!({ "participantIds": [bp] }),
        ),
        (
            Method::POST,
            format!("{}/spotlights", base(a.cid)),
            Auth::Bearer(&a.owner.token),
            json!({ "participantId": bp }),
        ),
        (
            Method::POST,
            format!("{}/breakouts/move", base(a.cid)),
            Auth::Bearer(&a.owner.token),
            json!({ "participantId": bp, "toBreakoutId": a.breakout_id }),
        ),
        (
            Method::POST,
            format!("{}/breakouts/move", base(a.cid)),
            Auth::Bearer(&a.owner.token),
            json!({ "participantId": a.guest.participant_id, "toBreakoutId": b.breakout_id }),
        ),
        (
            Method::POST,
            format!("{}/breakouts/assign", base(a.cid)),
            Auth::Bearer(&a.owner.token),
            json!({ "mode": "MANUAL", "assignments": [{ "participantId": bp, "breakoutId": a.breakout_id }] }),
        ),
        (
            Method::POST,
            format!("{}/leave", base(a.cid)),
            a.guest.auth(),
            json!({ "participantSessionId": b.guest.session_id }),
        ),
        (
            Method::POST,
            format!("{}/media-credential", base(a.cid)),
            a.guest.auth(),
            json!({ "participantSessionId": b.guest.session_id }),
        ),
        (
            Method::POST,
            format!("{}/intelligence/requests", base(a.cid)),
            Auth::Bearer(&a.owner.token),
            json!({ "sourceArtifactId": b.transcript_id, "kind": "SUMMARY" }),
        ),
        (
            Method::POST,
            format!("/api/v2/meeting-spaces/{}/invitations", a.sid),
            Auth::Bearer(&a.owner.token),
            json!({ "conferenceId": b.cid }),
        ),
        (
            Method::POST,
            format!("{}/messages", base(a.cid)),
            a.guest.auth(),
            json!({ "body": "dm", "recipientParticipantId": bp }),
        ),
        (
            Method::POST,
            format!("{}/messages", base(a.cid)),
            a.guest.auth(),
            json!({ "body": "x", "breakoutId": b.breakout_id }),
        ),
        // B's own guest cannot act on A through its credential either.
        (
            Method::POST,
            format!("{}/join", base(a.cid)),
            b.guest.auth(),
            json!({ "previousSessionId": b.guest.session_id }),
        ),
    ];
    for (m, uri, auth, body) in body_cases {
        let before = (
            fingerprint(&app, &b).await,
            lk(&app).calls.lock().unwrap().len(),
            app.storage.keys().len(),
        );
        let (s, v) = app.call(m.clone(), &uri, auth, Some(body.clone())).await;
        attempts += 1;
        let leave_noop = uri.ends_with("/leave") && s.is_success() && v["closedSessions"] == 0;
        let join_as_new =
            uri.ends_with("/join") && s.is_success() && v["participant"]["id"] != json!(bp);
        if !leave_noop && !join_as_new {
            check(s, &v, &format!("{m} {uri} {body}"), &mut failures);
        }
        let after = (
            fingerprint(&app, &b).await,
            lk(&app).calls.lock().unwrap().len(),
            app.storage.keys().len(),
        );
        if before.0 != after.0 || before.2 != after.2 {
            failures.push(format!("{m} {uri}: B state / storage changed"));
        }
        if !join_as_new && before.1 != after.1 {
            failures.push(format!("{m} {uri}: provider called"));
        }
    }
    // Positive control: the same read routes with A's OWN ids succeed for A's owner,
    // so every rejection above is the scope check, not a malformed request.
    let mut controls = 0;
    for (path, item) in doc["paths"].as_object().unwrap() {
        if !path.starts_with("/api/v2/") || !item.as_object().unwrap().contains_key("get") {
            continue;
        }
        let names: Vec<String> = path_params
            .captures_iter(path)
            .map(|c| c[1].to_string())
            .collect();
        if names.len() < 2 {
            continue;
        }
        let mut uri = path.clone();
        for n in &names {
            let v = if n == "id" {
                if path.contains("/registrations/") {
                    a.webinar_sid
                } else if path.starts_with("/api/v2/meeting-spaces/") {
                    a.sid
                } else {
                    a.cid
                }
            } else {
                subordinate(n, path, &a).unwrap()
            };
            uri = uri.replace(&format!("{{{n}}}"), &v.to_string());
        }
        let (s, v) = app.get(&uri, Auth::Bearer(&a.owner.token)).await;
        assert!(
            s.is_success(),
            "control GET {uri} must succeed for A's owner: {s} {v}"
        );
        controls += 1;
    }
    assert!(controls >= 6, "controls: {controls}");
    println!("IDOR attempts: {attempts}; positive controls: {controls}");
    assert!(attempts >= 60, "matrix too small: {attempts}");
    assert!(
        failures.is_empty(),
        "IDOR failures:\n{}",
        failures.join("\n")
    );
    let _ = (
        a.registration_id,
        a.message_id,
        a.member_id,
        a.invitation_id,
        a.poll_id,
        a.question_id,
        a.recording_id,
        a.job_id,
    );
}

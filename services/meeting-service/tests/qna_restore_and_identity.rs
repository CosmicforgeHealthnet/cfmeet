//! Session 2 — Q&A restore (`POST /questions/{id}/restore`, `qna.question_restored`) and
//! the canonical participant id in LiveKit credentials (`participantId` metadata and
//! attribute), over the production `LiveKitProvider` and the mock SFU.

mod common;

use std::collections::HashSet;

use axum::http::StatusCode;
use common::{collab::*, livekit_mock, rtc::*, *};
use livekit_api::access_token::{Claims, TokenVerifier};
use livekit_protocol as proto;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::collab().await {
            Some(a) => a,
            None => return,
        }
    };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn claims(cred: &str) -> Claims {
    TokenVerifier::with_api_key(livekit_mock::API_KEY, livekit_mock::API_SECRET)
        .verify(cred)
        .unwrap()
}

/// The canonical participant id a client reads from a LiveKit participant: metadata JSON
/// and the `participantId` attribute must agree.
fn token_participant_id(cred: &str) -> Uuid {
    let c = claims(cred);
    let meta: Value = serde_json::from_str(&c.metadata).expect("metadata is JSON");
    let obj = meta.as_object().expect("metadata object");
    assert_eq!(obj.len(), 1, "metadata carries only participantId: {meta}");
    let from_meta: Uuid = meta["participantId"].as_str().unwrap().parse().unwrap();
    assert_eq!(c.attributes.len(), 1, "{:?}", c.attributes);
    let from_attr: Uuid = c.attributes["participantId"].parse().unwrap();
    assert_eq!(from_meta, from_attr);
    // Clients cannot rewrite metadata/attributes (forged identity is impossible).
    assert!(!c.video.can_update_own_metadata);
    from_meta
}

fn credential(v: &Value) -> &str {
    v["rtc"]["credential"].as_str().expect("credential issued")
}

async fn ask(app: &TestApp, cid: Uuid, who: &Joined, body: Value) -> Uuid {
    let (s, q) = app
        .post(&format!("{}/questions", base(cid)), who.auth(), body)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{q}");
    uuid(&q["id"])
}

async fn qop(app: &TestApp, cid: Uuid, qid: Uuid, op: &str, auth: Auth<'_>) -> (StatusCode, Value) {
    let body = if op == "answer" {
        json!({ "answer": "At noon" })
    } else {
        json!({})
    };
    app.post(&format!("{}/questions/{qid}/{op}", base(cid)), auth, body)
        .await
}

async fn restore(app: &TestApp, cid: Uuid, qid: Uuid, auth: Auth<'_>) -> (StatusCode, Value) {
    app.post(
        &format!("{}/questions/{qid}/restore", base(cid)),
        auth,
        json!({}),
    )
    .await
}

async fn listed(app: &TestApp, cid: Uuid, auth: Auth<'_>, qid: Uuid) -> Option<Value> {
    let (s, v) = app.get(&format!("{}/questions", base(cid)), auth).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["questions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["id"] == json!(qid))
        .cloned()
}

async fn votes(app: &TestApp, qid: Uuid) -> i64 {
    app.scalar(&format!(
        "SELECT count(*) FROM meeting_question_votes WHERE question_id='{qid}'"
    ))
    .await
}

// ---------------------------------------------------------------------------
// Q&A restore
// ---------------------------------------------------------------------------

/// Unanswered: OPEN → DISMISSED → OPEN; reappears for attendees; votes, body and version.
#[tokio::test]
async fn qna_restore_unanswered_to_open() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    let qid = ask(&app, cid, &g[0], json!({ "body": "When is lunch?" })).await;
    for p in &g[1..] {
        assert_eq!(
            qop(&app, cid, qid, "vote", p.auth()).await.0,
            StatusCode::OK
        );
    }
    let (s, d) = qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token)).await;
    assert_eq!(
        (s, d["question"]["status"].as_str()),
        (StatusCode::OK, Some("DISMISSED"))
    );
    let dismissed_version = d["question"]["version"].as_i64().unwrap();
    assert!(
        listed(&app, cid, g[2].auth(), qid).await.is_none(),
        "hidden from attendees"
    );

    let (s, r) = restore(&app, cid, qid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["changed"], true);
    let q = &r["question"];
    assert_eq!(q["status"], "OPEN");
    assert_eq!(q["body"], "When is lunch?");
    assert_eq!(q["upvoteCount"], 2);
    assert!(q["answer"].is_null());
    assert_eq!(q["version"].as_i64().unwrap(), dismissed_version + 1);
    assert_eq!(
        q["participantId"],
        json!(g[0].participant_id),
        "named author kept"
    );
    assert_eq!(votes(&app, qid).await, 2, "vote history unchanged");

    // Visible again to attendees, with the voter's own state intact.
    let back = listed(&app, cid, g[1].auth(), qid)
        .await
        .expect("restored question listed");
    assert_eq!(
        (back["status"].as_str(), back["votedByMe"].as_bool()),
        (Some("OPEN"), Some(true))
    );
    // Restored questions accept votes again.
    assert_eq!(
        qop(&app, cid, qid, "vote", g[1].auth()).await.1["changed"],
        false
    );

    let ev = event_payloads(&app, cid, "qna.question_restored").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["questionId"], json!(qid));
    assert_eq!(ev[0]["conferenceId"], json!(cid));
    assert_eq!(ev[0]["status"], "OPEN");
    assert_eq!(ev[0]["upvoteCount"], 2);
    assert_eq!(ev[0]["pinned"], false);
    assert_eq!(ev[0]["anonymous"], false);
    assert_eq!(ev[0]["version"], q["version"]);
    assert_eq!(audits(&app, cid, "QNA_RESTORE").await, 1);
    // Moderation history kept.
    assert_eq!(audits(&app, cid, "QNA_DISMISS").await, 1);
}

/// Answered: ANSWERED → DISMISSED → ANSWERED with the original answer and timestamp.
#[tokio::test]
async fn qna_restore_answered_keeps_answer() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let qid = ask(&app, cid, &g[0], json!({ "body": "Where?" })).await;
    let (_, a) = qop(&app, cid, qid, "answer", Auth::Bearer(&owner.token)).await;
    let answered_at = a["question"]["answeredAt"].clone();
    assert!(answered_at.is_string());
    qop(&app, cid, qid, "pin", Auth::Bearer(&owner.token)).await;
    qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token)).await;
    let (s, r) = restore(&app, cid, qid, Auth::Bearer(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let q = &r["question"];
    assert_eq!(q["status"], "ANSWERED");
    assert_eq!(q["answer"], "At noon");
    assert_eq!(q["answeredAt"], answered_at);
    assert_eq!(q["pinned"], false, "the dismissal's unpin is not undone");
    let ev = event_payloads(&app, cid, "qna.question_restored").await;
    assert_eq!(
        (ev.len(), ev[0]["status"].as_str(), ev[0]["answer"].as_str()),
        (1, Some("ANSWERED"), Some("At noon"))
    );
    // A restored question can be dismissed again (OPEN/ANSWERED → DISMISSED still holds).
    assert_eq!(
        qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token))
            .await
            .1["changed"],
        true
    );
}

/// Anonymous question: restore never reveals the author (response, list, event).
#[tokio::test]
async fn qna_restore_anonymous_stays_anonymous() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let qid = ask(
        &app,
        cid,
        &g[0],
        json!({ "body": "Is this safe?", "anonymous": true }),
    )
    .await;
    qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token)).await;
    let (_, r) = restore(&app, cid, qid, Auth::Bearer(&owner.token)).await;
    let me = g[0].participant_id.to_string();
    let host_view = listed(&app, cid, Auth::Bearer(&owner.token), qid)
        .await
        .unwrap();
    let peer_view = listed(&app, cid, g[1].auth(), qid).await.unwrap();
    for v in [&r, &host_view, &peer_view] {
        let text = v.to_string();
        assert!(!text.contains(&me) && !text.contains("guest-0"), "{text}");
    }
    assert_eq!(r["question"]["anonymous"], true);
    for e in event_payloads(&app, cid, "qna.question_restored").await {
        assert!(!e.to_string().contains(&me), "{e}");
        assert!(e["participantId"].is_null() && e["displayName"].is_null());
    }
    // The submitter still recognises their own question.
    assert_eq!(
        listed(&app, cid, g[0].auth(), qid).await.unwrap()["isMine"],
        true
    );
}

/// Authorization: attendees (even the author) cannot restore; a co-host can.
#[tokio::test]
async fn qna_restore_requires_qna_dismiss() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let qid = ask(&app, cid, &g[0], json!({ "body": "Q?" })).await;
    qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token)).await;
    for who in [&g[0], &g[1]] {
        let (s, v) = restore(&app, cid, qid, who.auth()).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
            "{v}"
        );
    }
    let (s, _) = restore(&app, cid, qid, Auth::None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // An outsider (signed in, not a participant) is refused too.
    let outsider = app.user("outsider").await;
    let (s, _) = restore(&app, cid, qid, Auth::Bearer(&outsider.token)).await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "{s}"
    );
    assert_eq!(events(&app, cid, "qna.question_restored").await, 0);

    let cohost = app.user("cohost").await;
    let cj = join_user(&app, cid, &cohost).await;
    change_role(&app, cid, &owner, cj.participant_id, "CO_HOST").await;
    let (s, v) = restore(&app, cid, qid, Auth::Bearer(&cohost.token)).await;
    assert_eq!(
        (s, v["question"]["status"].as_str()),
        (StatusCode::OK, Some("OPEN")),
        "{v}"
    );
}

/// ARCHIVED is terminal; unknown and cross-conference questions are not found.
#[tokio::test]
async fn qna_restore_invalid_targets() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let qid = ask(&app, cid, &g[0], json!({ "body": "Q?" })).await;
    qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token)).await;
    qop(&app, cid, qid, "archive", Auth::Bearer(&owner.token)).await;
    let (s, v) = restore(&app, cid, qid, Auth::Bearer(&owner.token)).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT"),
        "{v}"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_questions WHERE id='{qid}' AND status='ARCHIVED'"
        ))
        .await,
        1
    );
    // Archive straight from OPEN: still terminal.
    let q2 = ask(&app, cid, &g[0], json!({ "body": "Q2?" })).await;
    qop(&app, cid, q2, "archive", Auth::Bearer(&owner.token)).await;
    assert_eq!(
        restore(&app, cid, q2, Auth::Bearer(&owner.token)).await.0,
        StatusCode::CONFLICT
    );

    let (s, v) = restore(&app, cid, Uuid::new_v4(), Auth::Bearer(&owner.token)).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "QUESTION_NOT_FOUND"));

    // A question of another conference, addressed through this one.
    let (owner2, cid2, g2) = meeting(&app, public(), 1).await;
    let other = ask(&app, cid2, &g2[0], json!({ "body": "Elsewhere" })).await;
    qop(&app, cid2, other, "dismiss", Auth::Bearer(&owner2.token)).await;
    let (s, v) = restore(&app, cid, other, Auth::Bearer(&owner.token)).await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "QUESTION_NOT_FOUND"));
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_questions WHERE id='{other}' AND status='DISMISSED'"
        ))
        .await,
        1
    );
    // Answering a dismissed question is still refused (restore is the only way back).
    let q3 = ask(&app, cid, &g[0], json!({ "body": "Q3?" })).await;
    qop(&app, cid, q3, "dismiss", Auth::Bearer(&owner.token)).await;
    let (s, v) = qop(&app, cid, q3, "answer", Auth::Bearer(&owner.token)).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT")
    );
    assert_eq!(events(&app, cid, "qna.question_restored").await, 0);
}

/// Idempotency: repeat restores and restoring a never-dismissed question are no-ops.
#[tokio::test]
async fn qna_restore_is_idempotent() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let qid = ask(&app, cid, &g[0], json!({ "body": "Q?" })).await;
    let (s, v) = restore(&app, cid, qid, Auth::Bearer(&owner.token)).await;
    assert_eq!(
        (s, v["changed"].as_bool(), v["question"]["version"].as_i64()),
        (StatusCode::OK, Some(false), Some(1))
    );
    qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token)).await;
    let first = restore(&app, cid, qid, Auth::Bearer(&owner.token)).await.1;
    let again = restore(&app, cid, qid, Auth::Bearer(&owner.token)).await.1;
    assert_eq!(
        (first["changed"].as_bool(), again["changed"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(first["question"]["version"], again["question"]["version"]);
    assert_eq!(events(&app, cid, "qna.question_restored").await, 1);
    assert_eq!(audits(&app, cid, "QNA_RESTORE").await, 1);
}

/// Concurrency: two moderators restoring at once → exactly one transition and event;
/// dismiss racing restore always ends in a consistent state with one event per change.
#[tokio::test]
async fn qna_restore_concurrent_moderators() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let cohost = app.user("cohost").await;
    let cj = join_user(&app, cid, &cohost).await;
    change_role(&app, cid, &owner, cj.participant_id, "CO_HOST").await;
    let qid = ask(&app, cid, &g[0], json!({ "body": "Q?" })).await;
    for round in 0..3 {
        qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token)).await;
        let (a, b) = tokio::join!(
            restore(&app, cid, qid, Auth::Bearer(&owner.token)),
            restore(&app, cid, qid, Auth::Bearer(&cohost.token)),
        );
        assert_eq!(
            (a.0, b.0),
            (StatusCode::OK, StatusCode::OK),
            "{} {}",
            a.1,
            b.1
        );
        let changed = [&a.1, &b.1].iter().filter(|v| v["changed"] == true).count();
        assert_eq!(changed, 1, "round {round}: exactly one restore wins");
        assert_eq!(events(&app, cid, "qna.question_restored").await, round + 1);
    }
    // Dismiss vs restore race.
    let (d, r) = tokio::join!(
        qop(&app, cid, qid, "dismiss", Auth::Bearer(&owner.token)),
        restore(&app, cid, qid, Auth::Bearer(&cohost.token)),
    );
    assert_eq!(
        (d.0, r.0),
        (StatusCode::OK, StatusCode::OK),
        "{} {}",
        d.1,
        r.1
    );
    let status = listed(&app, cid, Auth::Bearer(&owner.token), qid)
        .await
        .unwrap()["status"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(status == "OPEN" || status == "DISMISSED", "{status}");
    let version = app
        .scalar(&format!(
            "SELECT version::bigint FROM meeting_questions WHERE id='{qid}'"
        ))
        .await;
    // Every committed change bumped the version exactly once.
    let changes = app
        .scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND aggregate_id='{qid}' AND event_type <> 'qna.question_created'"
        ))
        .await;
    assert_eq!(version, 1 + changes);
}

// ---------------------------------------------------------------------------
// LiveKit participant identity
// ---------------------------------------------------------------------------

/// Two guests named "Alex" plus a signed-in user: distinct, correct participant ids; the
/// media identity and display name are unchanged; no account data leaks into RTC claims.
#[tokio::test]
async fn identity_same_display_name_is_distinguishable() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, public()).await;
    let a = join_guest(&app, cid, "Alex").await;
    let b = join_guest(&app, cid, "Alex").await;
    let host = join_user(&app, cid, &owner).await;
    let mut seen = HashSet::new();
    for j in [&a, &b, &host] {
        connect(&app, cid, j).await;
        let pid = token_participant_id(&j.credential);
        assert_eq!(
            pid, j.participant_id,
            "token maps to the join response participant"
        );
        assert!(seen.insert(pid), "unique per participant");
        let c = claims(&j.credential);
        assert_eq!(c.sub, j.identity, "media identity unchanged (ps_<session>)");
        assert_ne!(c.sub, pid.to_string());
        assert_eq!(c.video.room, room(cid));
        assert!(!c.video.room_admin && !c.video.room_record);
    }
    assert_eq!(claims(&a.credential).name, "Alex");
    assert_eq!(claims(&b.credential).name, "Alex");
    let host_claims = claims(&host.credential);
    assert!(
        !host_claims.metadata.contains(&owner.id.to_string()),
        "no account id"
    );
    assert!(!host_claims.metadata.contains('@'), "no email");
    assert!(!format!("{:?}", host_claims.attributes).contains(&owner.id.to_string()));
    // The participant list resolves each token id to exactly one record.
    let (s, list) = app
        .get(
            &format!("{}/participants", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let text = list.to_string();
    for j in [&a, &b] {
        assert!(text.contains(&j.participant_id.to_string()), "{text}");
    }
}

/// Refresh, reconnect (new session) and leave/rejoin keep the same participant id; a new
/// session never inherits another participant's id.
#[tokio::test]
async fn identity_stable_across_refresh_reconnect_and_rejoin() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, public()).await;
    let g = join_guest(&app, cid, "Sam").await;
    connect(&app, cid, &g).await;
    let other = join_guest(&app, cid, "Sam").await;

    // Token refresh: same session, same id.
    let (s, r) = refresh(&app, cid, &g, g.auth()).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(token_participant_id(credential(&r)), g.participant_id);
    assert_eq!(claims(credential(&r)).sub, g.identity);

    // Reconnect on a new device session: new media identity, same participant id.
    lk(&app).disconnect(&room(cid), &g.identity);
    let (s, re) = app
        .join(cid, g.auth(), json!({ "previousSessionId": g.session_id }))
        .await;
    assert_eq!(s, StatusCode::OK, "{re}");
    let new_session = uuid(&re["participantSession"]["id"]);
    assert_ne!(new_session, g.session_id);
    assert_eq!(uuid(&re["participant"]["id"]), g.participant_id);
    let cred = credential(&re);
    assert_eq!(token_participant_id(cred), g.participant_id);
    assert_eq!(claims(cred).sub, format!("ps_{}", new_session.simple()));
    assert_eq!(
        lk(&app).connect(cred).unwrap(),
        format!("ps_{}", new_session.simple())
    );

    // Leave and return: the participant record (and id) is reused.
    let user = app.user("returning").await;
    let first = join_user(&app, cid, &user).await;
    let (s, _) = app
        .post(
            &format!("{}/leave", base(cid)),
            Auth::Bearer(&user.token),
            json!({}),
        )
        .await;
    assert!(s.is_success());
    let back = join_user(&app, cid, &user).await;
    assert_eq!(back.participant_id, first.participant_id);
    assert_ne!(back.session_id, first.session_id);
    assert_eq!(token_participant_id(&back.credential), first.participant_id);

    // Nobody inherits another participant's id.
    assert_ne!(token_participant_id(&other.credential), g.participant_id);
    assert_ne!(other.participant_id, g.participant_id);
}

/// Hands, spotlight, mute and removal target the participant the token names — even
/// when display names collide.
#[tokio::test]
async fn identity_drives_hand_spotlight_mute_and_removal() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, public()).await;
    let a = join_guest(&app, cid, "Alex").await;
    let b = join_guest(&app, cid, "Alex").await;
    for j in [&a, &b] {
        connect(&app, cid, j).await;
    }
    // Media tile → participant id, the way a client maps it.
    let tile_b = token_participant_id(&b.credential);

    // Raised hand by B is attributed to B's token id.
    let (s, _) = app
        .post(&format!("{}/hand-raise", base(cid)), b.auth(), json!({}))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (_, hands) = app
        .get(
            &format!("{}/hand-raise", base(cid)),
            Auth::Bearer(&owner.token),
        )
        .await;
    let raised: Vec<Uuid> = hands["hands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| uuid(&h["participantId"]))
        .collect();
    assert_eq!(raised, vec![tile_b]);
    let ev = event_payloads(&app, cid, "hand.raised").await;
    assert_eq!(ev[0]["participantId"], json!(tile_b));

    // Spotlight the tile → the shared state names that participant.
    let (s, v) = app
        .post(
            &format!("{}/spotlights", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "participantId": tile_b }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, st) = app
        .get(&format!("{}/collaboration", base(cid)), a.auth())
        .await;
    assert_eq!(st["spotlights"][0]["participantId"], json!(tile_b));

    // Mute the tile's audio → only B's media identity is muted at the provider.
    for j in [&a, &b] {
        lk(&app)
            .publish(&room(cid), &j.identity, proto::TrackSource::Microphone)
            .unwrap();
    }
    let (s, v) = app
        .post(
            &format!("{}/participants/{tile_b}/mute", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "trackKind": "AUDIO" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(lk(&app)
        .tracks_of(&room(cid), &b.identity, proto::TrackSource::Microphone)
        .iter()
        .all(|(_, m)| *m));
    assert!(lk(&app)
        .tracks_of(&room(cid), &a.identity, proto::TrackSource::Microphone)
        .iter()
        .all(|(_, m)| !*m));

    // Remove the tile's participant → B's session ends, A's stays.
    let (s, v) = app
        .post(
            &format!("{}/participants/{tile_b}/remove", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_ne!(session_state(&app, b.session_id).await, "CONNECTED");
    assert_eq!(session_state(&app, a.session_id).await, "CONNECTED");
}

/// Recorder / service identities carry no participant id and are never mapped to one; a
/// client cannot forge metadata (no update grant; a foreign-signed token is refused).
#[tokio::test]
async fn identity_service_participants_and_forgery() {
    let app = app!();
    let owner = app.user("owner").await;
    let (_, cid) = app.live_conference(&owner, public()).await;
    let a = join_guest(&app, cid, "Alex").await;
    connect(&app, cid, &a).await;
    lk(&app).insert_recorder(&room(cid), "EG_recorder");
    let recorder = lk(&app).participant(&room(cid), "EG_recorder").unwrap();
    assert_eq!(recorder.kind, proto::participant_info::Kind::Egress as i32);
    // Server-side mapping only knows human sessions.
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM participant_sessions WHERE conference_id='{cid}' AND media_identity='EG_recorder'"
        ))
        .await,
        0
    );
    // A token minted with someone else's id but not signed by the server is refused.
    let forged = livekit_api::access_token::AccessToken::with_api_key(
        "mock-key",
        "not-the-secret-not-the-secret-0000",
    )
    .with_identity("ps_forged")
    .with_metadata(&json!({ "participantId": a.participant_id }).to_string())
    .with_grants(livekit_api::access_token::VideoGrants {
        room_join: true,
        room: room(cid),
        ..Default::default()
    })
    .to_jwt()
    .unwrap();
    assert!(lk(&app).connect(&forged).is_err());
    assert!(!claims(&a.credential).video.can_update_own_metadata);
}

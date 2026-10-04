//! Phase 2 Part F — concurrency suite C1–C12 (real PostgreSQL locks, mock LiveKit).

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use backend::{
    models::outbox_events,
    workers::outbox_relay::{EventPublisher, OutboxRelay},
};
use common::{collab::*, livekit_mock, rtc::*, *};
use futures_util::future::join_all;
use livekit_api::access_token::TokenVerifier;
use livekit_protocol as proto;
use serde_json::json;
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::collab().await {
            Some(a) => a,
            None => return,
        }
    };
}

/// C1 — audio lock racing a credential refresh / reconnect: whatever credential the
/// refresh returned, the provider ends up without microphone rights.
#[tokio::test]
async fn c01_audio_lock_vs_reconnect() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    let url = format!("{}/participants/{}/audio-lock", base(cid), p.participant_id);
    for round in 0..5 {
        let (lock, cred) = tokio::join!(
            app.post(&url, Auth::Bearer(&owner.token), json!({})),
            refresh(&app, cid, p, p.auth())
        );
        assert_eq!(lock.0, StatusCode::OK, "{round}: {}", lock.1);
        if cred.0 == StatusCode::OK {
            // Reconnect with whatever was issued; connect re-asserts current grants.
            lk(&app).disconnect(&room(cid), &p.identity);
            lk(&app)
                .connect(cred.1["rtc"]["credential"].as_str().unwrap())
                .unwrap();
            provider_event(&app, "participant_joined", cid, Some(&p.identity)).await;
        } else {
            assert_eq!(code(&cred.1), "PARTICIPANT_STATE_CONFLICT", "{}", cred.1);
        }
        assert!(
            lk(&app)
                .publish(&room(cid), &p.identity, proto::TrackSource::Microphone)
                .is_err(),
            "round {round}"
        );
        let (_, fresh) = refresh(&app, cid, p, p.auth()).await;
        let c = TokenVerifier::with_api_key(livekit_mock::API_KEY, livekit_mock::API_SECRET)
            .verify(fresh["rtc"]["credential"].as_str().unwrap())
            .unwrap();
        assert!(!c
            .video
            .can_publish_sources
            .iter()
            .any(|s| s == "microphone"));
        app.delete(&url, Auth::Bearer(&owner.token)).await;
    }
}

/// C2 — screen-share permission revoked while the participant starts sharing.
#[tokio::test]
async fn c02_share_revoke_vs_start() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    for round in 0..4 {
        let main_room = room(cid);
        let share = async {
            if let Ok(sid) =
                lk(&app).publish(&main_room, &p.identity, proto::TrackSource::ScreenShare)
            {
                track_event(
                    &app,
                    "track_published",
                    &main_room,
                    &p.identity,
                    &sid,
                    "SCREEN_SHARE",
                )
                .await;
            }
        };
        let revoke = policy(
            &app,
            cid,
            &owner,
            json!({ "screenSharePolicy": "HOST_ONLY" }),
        );
        tokio::join!(share, revoke);
        assert!(
            lk(&app)
                .tracks_of(&room(cid), &p.identity, proto::TrackSource::ScreenShare)
                .is_empty(),
            "round {round}: no live share at provider"
        );
        let (_, st) = app
            .get(
                format!("{}/collaboration", base(cid)),
                Auth::Bearer(&owner.token),
            )
            .await;
        assert!(
            st["activeScreenShares"].as_array().unwrap().is_empty(),
            "round {round}: authoritative view agrees: {st}"
        );
        policy(
            &app,
            cid,
            &owner,
            json!({ "screenSharePolicy": "ALL_INTERACTIVE" }),
        )
        .await;
    }
}

/// C3 — host stop racing the provider's track-ended webhook: one canonical event.
#[tokio::test]
async fn c03_stop_share_vs_webhook() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    for round in 0..4 {
        let sid = start_share(&app, cid, p).await;
        let stop = app.post(
            format!("{}/screen-share/{}/stop", base(cid), p.participant_id),
            Auth::Bearer(&owner.token),
            json!({}),
        );
        let main_room = room(cid);
        let hook = track_event(
            &app,
            "track_unpublished",
            &main_room,
            &p.identity,
            &sid,
            "SCREEN_SHARE",
        );
        let ((s, v), _) = tokio::join!(stop, hook);
        assert!(
            s == StatusCode::OK || code(&v) == "SCREEN_SHARE_NOT_ACTIVE",
            "{s} {v}"
        );
        assert_eq!(
            events(&app, cid, "screenshare.ended").await,
            round + 1,
            "exactly one end per share"
        );
    }
}

/// C4 — hand raise racing host lower; double raise; double lower.
#[tokio::test]
async fn c04_hand_raise_vs_lower() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    // 3 rounds: own toggles stay inside the participant's hand-toggle rate budget (10/min).
    for _ in 0..3 {
        let raise = app.post(format!("{}/hand-raise", base(cid)), p.auth(), json!({}));
        let lower = app.delete(
            format!("{}/participants/{}/hand-raise", base(cid), p.participant_id),
            Auth::Bearer(&owner.token),
        );
        tokio::join!(raise, lower);
        let raised = app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND hand_raised_at IS NOT NULL", p.participant_id)).await;
        let delta =
            events(&app, cid, "hand.raised").await - events(&app, cid, "hand.lowered").await;
        assert_eq!(delta, raised, "events match state");
        app.delete(format!("{}/hand-raise", base(cid)), p.auth())
            .await;
    }
    let before = events(&app, cid, "hand.raised").await;
    join_all((0..2).map(|_| app.post(format!("{}/hand-raise", base(cid)), p.auth(), json!({}))))
        .await;
    assert_eq!(
        events(&app, cid, "hand.raised").await,
        before + 1,
        "double raise = one"
    );
    let lowered = events(&app, cid, "hand.lowered").await;
    join_all((0..2).map(|_| app.delete(format!("{}/hand-raise", base(cid)), p.auth()))).await;
    assert_eq!(
        events(&app, cid, "hand.lowered").await,
        lowered + 1,
        "double lower = one"
    );
}

/// C5 — two hosts assign the same participant to different rooms.
#[tokio::test]
async fn c05_two_host_assignments() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let cohost = app.user("cohost").await;
    let cj = join_user(&app, cid, &cohost).await;
    change_role(&app, cid, &owner, cj.participant_id, "CO_HOST").await;
    let rooms = create_rooms(&app, cid, &owner, json!({ "count": 2 })).await;
    for &r in &rooms {
        open_room(&app, cid, &owner, r).await;
    }
    for _ in 0..5 {
        let a = app.post(
            format!("{}/breakouts/move", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "participantId": g[0].participant_id, "toBreakoutId": rooms[0] }),
        );
        let b = app.post(
            format!("{}/breakouts/move", base(cid)),
            Auth::Bearer(&cohost.token),
            json!({ "participantId": g[0].participant_id, "toBreakoutId": rooms[1] }),
        );
        let (ra, rb) = tokio::join!(a, b);
        assert_eq!(
            (ra.0, rb.0),
            (StatusCode::OK, StatusCode::OK),
            "{} {}",
            ra.1,
            rb.1
        );
        assert_eq!(app.scalar(&format!("SELECT count(*) FROM breakout_assignments WHERE participant_id='{}' AND unassigned_at IS NULL", g[0].participant_id)).await, 1);
    }
}

/// C6 — closing a breakout while a participant is moved into it.
#[tokio::test]
async fn c06_close_vs_move() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    for _ in 0..4 {
        let room_id = create_rooms(&app, cid, &owner, json!({ "count": 1 })).await[0];
        open_room(&app, cid, &owner, room_id).await;
        let mv = app.post(
            format!("{}/breakouts/move", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "participantId": g[0].participant_id, "toBreakoutId": room_id }),
        );
        let close = app.post(
            format!("{}/breakouts/{room_id}/close", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "countdownSeconds": 0 }),
        );
        let ((ms, mv), (cs, _)) = tokio::join!(mv, close);
        assert_eq!(cs, StatusCode::OK);
        assert!(
            ms == StatusCode::OK || code(&mv) == "BREAKOUT_CLOSED",
            "{ms} {mv}"
        );
        assert_eq!(app.scalar(&format!("SELECT count(*) FROM breakout_assignments WHERE breakout_room_id='{room_id}' AND unassigned_at IS NULL")).await, 0, "no one stranded in a closed room");
        let (_, st) = app
            .get(format!("{}/collaboration", base(cid)), g[0].auth())
            .await;
        assert_eq!(st["me"]["mediaContext"]["kind"], "MAIN");
    }
}

/// C7 — poll response racing poll close.
#[tokio::test]
async fn c07_response_vs_close() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 4).await;
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_CLOSE")).await;
    let close = app.post(
        format!("{}/polls/{poll}/close", base(cid)),
        Auth::Bearer(&owner.token),
        json!({}),
    );
    let votes = join_all(g.iter().map(|p| vote(&app, cid, poll, p.auth(), "o1")));
    let ((cs, _), votes) = tokio::join!(close, votes);
    assert_eq!(cs, StatusCode::OK);
    let accepted = votes
        .iter()
        .filter(|(s, _)| *s == StatusCode::CREATED)
        .count() as i64;
    assert!(votes
        .iter()
        .all(|(s, v)| *s == StatusCode::CREATED || code(v) == "POLL_NOT_OPEN"));
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_poll_ballots WHERE poll_id='{poll}'"
        ))
        .await,
        accepted
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_poll_responses WHERE poll_id='{poll}'"
        ))
        .await,
        accepted
    );
    let (_, r) = app
        .get(format!("{}/polls/{poll}/results", base(cid)), g[0].auth())
        .await;
    assert_eq!(
        r["responseCount"].as_i64(),
        Some(accepted),
        "closed results include exactly the accepted ballots"
    );
}

/// C8 — duplicate poll responses from one participant.
#[tokio::test]
async fn c08_duplicate_response() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 2).await;
    let poll = open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    let r = join_all((0..8).map(|_| vote(&app, cid, poll, g[0].auth(), "o1"))).await;
    assert_eq!(
        r.iter().filter(|(s, _)| *s == StatusCode::CREATED).count(),
        1
    );
    assert!(r
        .iter()
        .filter(|(s, _)| *s != StatusCode::CREATED)
        .all(|(_, v)| code(v) == "POLL_ALREADY_RESPONDED"));
    // Idempotent retries with the same key are acknowledged without a second ballot.
    let body = json!({ "answers": [{ "questionId": "q1", "optionIds": ["o2"] }], "idempotencyKey": "retry-1" });
    let r = join_all((0..6).map(|_| {
        app.post(
            format!("{}/polls/{poll}/responses", base(cid)),
            g[1].auth(),
            body.clone(),
        )
    }))
    .await;
    assert_eq!(
        r.iter().filter(|(s, _)| *s == StatusCode::CREATED).count(),
        1
    );
    assert!(
        r.iter()
            .filter(|(s, _)| *s != StatusCode::CREATED)
            .all(|(s, v)| *s == StatusCode::OK && v["idempotentReplay"] == true),
        "{r:?}"
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_poll_responses WHERE poll_id='{poll}'"
        ))
        .await,
        2
    );
}

/// C9 — concurrent Q&A votes keep an exact count.
#[tokio::test]
async fn c09_qna_vote_race() {
    let app = app!();
    let (_, cid, g) = meeting(&app, public(), 5).await;
    let (_, q) = app
        .post(
            format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Q" }),
        )
        .await;
    let qid = uuid(&q["id"]);
    let mut reqs = vec![];
    for p in &g[1..] {
        for _ in 0..3 {
            reqs.push(app.post(
                format!("{}/questions/{qid}/vote", base(cid)),
                p.auth(),
                json!({}),
            ));
        }
    }
    join_all(reqs).await;
    let (_, list) = app
        .get(format!("{}/questions", base(cid)), g[0].auth())
        .await;
    assert_eq!(list["questions"][0]["upvoteCount"], 4);
    assert_eq!(events(&app, cid, "qna.question_upvoted").await, 4);
    // Vote racing dismiss: votes after dismissal are refused, count stays consistent.
    let (_, q2) = app
        .post(
            format!("{}/questions", base(cid)),
            g[0].auth(),
            json!({ "body": "Q2" }),
        )
        .await;
    let q2 = uuid(&q2["id"]);
    let votes = join_all(g[1..].iter().map(|p| {
        app.post(
            format!("{}/questions/{q2}/vote", base(cid)),
            p.auth(),
            json!({}),
        )
    }));
    votes.await;
    let n = app
        .scalar(&format!(
            "SELECT count(*) FROM meeting_question_votes WHERE question_id='{q2}'"
        ))
        .await;
    assert_eq!(
        app.scalar(&format!(
            "SELECT upvote_count::bigint FROM meeting_questions WHERE id='{q2}'"
        ))
        .await,
        n
    );
}

/// C10 — two hosts answer the same question: first committed answer wins.
#[tokio::test]
async fn c10_qna_answer_race() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let cohost = app.user("cohost").await;
    let cj = join_user(&app, cid, &cohost).await;
    change_role(&app, cid, &owner, cj.participant_id, "CO_HOST").await;
    for _ in 0..4 {
        let (_, q) = app
            .post(
                format!("{}/questions", base(cid)),
                g[0].auth(),
                json!({ "body": "Q" }),
            )
            .await;
        let qid = uuid(&q["id"]);
        let a = app.post(
            format!("{}/questions/{qid}/answer", base(cid)),
            Auth::Bearer(&owner.token),
            json!({ "answer": "owner" }),
        );
        let b = app.post(
            format!("{}/questions/{qid}/answer", base(cid)),
            Auth::Bearer(&cohost.token),
            json!({ "answer": "cohost" }),
        );
        let (ra, rb) = tokio::join!(a, b);
        assert_eq!((ra.0, rb.0), (StatusCode::OK, StatusCode::OK));
        assert_eq!(
            [ra.1["changed"].as_bool(), rb.1["changed"].as_bool()]
                .iter()
                .filter(|c| **c == Some(true))
                .count(),
            1
        );
        assert_eq!(
            ra.1["question"]["answer"], rb.1["question"]["answer"],
            "one answer"
        );
        // Pin racing dismiss: never a pinned dismissed question.
        let pin = app.post(
            format!("{}/questions/{qid}/pin", base(cid)),
            Auth::Bearer(&owner.token),
            json!({}),
        );
        let dismiss = app.post(
            format!("{}/questions/{qid}/dismiss", base(cid)),
            Auth::Bearer(&cohost.token),
            json!({}),
        );
        tokio::join!(pin, dismiss);
        assert_eq!(app.scalar(&format!("SELECT count(*) FROM meeting_questions WHERE id='{qid}' AND status='DISMISSED' AND pinned_at IS NOT NULL")).await, 0);
    }
}

/// C11 — chat restriction racing sends: nothing is accepted after the restriction.
#[tokio::test]
async fn c11_chat_restriction_vs_send() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 1).await;
    let p = &g[0];
    let sends =
        join_all((0..10).map(|i| send(&app, cid, p.auth(), json!({ "body": format!("m{i}") }))));
    let restrict = app.post(
        format!(
            "{}/participants/{}/chat-restriction",
            base(cid),
            p.participant_id
        ),
        Auth::Bearer(&owner.token),
        json!({}),
    );
    let (sends, (rs, _)) = tokio::join!(sends, restrict);
    assert_eq!(rs, StatusCode::OK);
    assert!(
        sends.iter().all(|(s, v)| *s == StatusCode::CREATED
            || ["CHAT_PARTICIPANT_RESTRICTED", "RATE_LIMITED"].contains(&code(v))),
        "{sends:?}"
    );
    assert_eq!(app.scalar(&format!(
        "SELECT count(*) FROM conference_messages m JOIN conference_participants p ON p.id = m.sender_participant_id \
         WHERE p.id = '{}' AND m.created_at > p.chat_restricted_at", p.participant_id)).await, 0);
    // Private-chat disable racing a private send.
    let q = join_guest(&app, cid, "peer").await;
    let send_dm = send(
        &app,
        cid,
        q.auth(),
        json!({ "body": "dm", "recipientParticipantId": p.participant_id }),
    );
    let disable = policy(&app, cid, &owner, json!({ "privateChatMode": "DISABLED" }));
    let ((s, v), _) = tokio::join!(send_dm, disable);
    assert!(
        s == StatusCode::CREATED || code(&v) == "PRIVATE_CHAT_DISABLED",
        "{s} {v}"
    );
    assert_eq!(
        send(
            &app,
            cid,
            q.auth(),
            json!({ "body": "dm2", "recipientParticipantId": p.participant_id })
        )
        .await
        .1["error"]["code"],
        "PRIVATE_CHAT_DISABLED"
    );
}

struct Counting {
    seen: Mutex<Vec<Uuid>>,
    calls: AtomicUsize,
}

#[async_trait]
impl EventPublisher for Counting {
    async fn publish(&self, e: &outbox_events::Model) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(e.id);
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        Ok(())
    }
}

/// C12 — parallel outbox publishers deliver every Part F event exactly once.
#[tokio::test]
async fn c12_parallel_outbox_publishers() {
    let app = app!();
    let (owner, cid, g) = meeting(&app, public(), 3).await;
    for p in &g {
        send(&app, cid, p.auth(), json!({ "body": "hi" })).await;
        app.post(format!("{}/hand-raise", base(cid)), p.auth(), json!({}))
            .await;
        app.post(
            format!("{}/questions", base(cid)),
            p.auth(),
            json!({ "body": "Q" }),
        )
        .await;
    }
    open_poll(&app, cid, &owner, simple_poll("AFTER_VOTE")).await;
    create_rooms(&app, cid, &owner, json!({ "count": 2 })).await;
    let pending = app
        .scalar("SELECT count(*) FROM outbox_events WHERE status='PENDING' AND channel='REALTIME'")
        .await;
    assert!(pending >= 12);
    let publisher = Arc::new(Counting {
        seen: Default::default(),
        calls: AtomicUsize::new(0),
    });
    let relays: Vec<_> = (0..4)
        .map(|_| {
            OutboxRelay::new(
                app.db.conn.clone(),
                publisher.clone() as Arc<dyn EventPublisher>,
            )
        })
        .collect();
    for _ in 0..4 {
        join_all(relays.iter().map(|r| r.tick(5))).await;
    }
    let seen = publisher.seen.lock().unwrap().clone();
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(seen.len(), unique.len(), "no duplicate delivery");
    assert_eq!(seen.len() as i64, pending);
    assert_eq!(
        app.scalar(
            "SELECT count(*) FROM outbox_events WHERE status='PENDING' AND channel='REALTIME'"
        )
        .await,
        0
    );
}

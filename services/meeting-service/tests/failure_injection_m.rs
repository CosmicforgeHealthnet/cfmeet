//! Part M §31 / §35 — failure-injection cases with no prior dedicated regression:
//! - LiveKit unavailable during participant removal (business state commits,
//!   enforcement stays honest, the durable command converges after recovery);
//! - a slow WebSocket consumer (backpressure is per socket: other subscribers are
//!   unaffected, the slow one later receives every event in order, gap-free);
//! - a cursor that is too old / points into purged history → `resync_required`
//!   and canonical refetch.

mod common;

use std::{sync::atomic::Ordering, sync::Arc, time::Duration};

use axum::http::StatusCode;
use common::{collab::*, rtc::*, ws::*, *};
use serde_json::json;

macro_rules! app {
    ($ctor:expr) => {
        match $ctor.await {
            Some(a) => a,
            None => return,
        }
    };
}

fn relay(app: &TestApp) -> backend::workers::outbox_relay::OutboxRelay {
    backend::workers::outbox_relay::OutboxRelay::with_control(
        app.db.conn.clone(),
        Some(Arc::new(backend::workers::outbox_relay::LogPublisher)
            as Arc<dyn backend::workers::outbox_relay::EventPublisher>),
        Some(Arc::new(backend::services::rtc_control::RtcControl::new(
            app.state.meeting.core.clone(),
        ))),
    )
}

/// §31 — provider unavailable during remove: REMOVED commits (business state wins),
/// no fake provider success is reported, the removed participant gets no credential,
/// and the durable provider command evicts them once the provider recovers.
#[tokio::test]
async fn m_f_livekit_unavailable_during_remove() {
    let app = app!(TestApp::collab());
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "remove", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let victim = join_guest(&app, cid, "victim").await;
    connect(&app, cid, &victim).await;
    let other = join_guest(&app, cid, "other").await;
    connect(&app, cid, &other).await;

    lk(&app).faults.fail_remove.store(true, Ordering::SeqCst);
    let (s, v) = app
        .post(
            format!(
                "/api/v2/conferences/{cid}/participants/{}/remove",
                victim.participant_id
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert!(s.is_success(), "{s} {v}");
    assert_ne!(
        v["providerEnforcement"], "ENFORCED",
        "no fake provider success: {v}"
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_participants WHERE id='{}' AND admission_state='REMOVED'", victim.participant_id)).await,
        1
    );
    // Relay attempts the command while the provider is down: it stays retryable.
    exec(
        &app,
        "UPDATE outbox_events SET available_at = now() WHERE status='PENDING'",
    )
    .await;
    relay(&app).tick(100).await.unwrap();
    assert!(
        lk(&app).participant(&room(cid), &victim.identity).is_some(),
        "provider still holds the identity"
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM outbox_events WHERE status='FAILED'")
            .await,
        0
    );
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND status='PENDING' AND event_type='rtc.participant_remove_requested'"
        ))
        .await
            >= 1,
        "remove command retained for retry"
    );
    // Business state already denies authority.
    let (s, v) = refresh(&app, cid, &victim, victim.auth()).await;
    assert!(!s.is_success() && v.get("credential").is_none(), "{s} {v}");
    assert_ne!(code(&v), "FEATURE_DISABLED", "{v}");
    // The rest of the conference keeps working.
    let (s, _) = refresh(&app, cid, &other, other.auth()).await;
    assert_eq!(s, StatusCode::OK);

    // Recovery → the retained command converges.
    lk(&app).faults.fail_remove.store(false, Ordering::SeqCst);
    for _ in 0..3 {
        exec(
            &app,
            "UPDATE outbox_events SET available_at = now() WHERE status='PENDING'",
        )
        .await;
        relay(&app).tick(100).await.unwrap();
    }
    assert!(
        lk(&app).participant(&room(cid), &victim.identity).is_none(),
        "evicted after recovery"
    );
    assert!(lk(&app).participant(&room(cid), &other.identity).is_some());
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM outbox_events WHERE conference_id='{cid}' AND status IN ('PENDING','FAILED') AND channel='PROVIDER_COMMAND'")).await,
        0
    );
}

/// §35 — a slow consumer never blocks other subscribers and, once it reads, gets
/// every event in order with no gap (PostgreSQL-tailed, bounded batches).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m_f_ws_slow_consumer_isolated_and_gap_free() {
    let app = app!(TestApp::livekit());
    let addr = app.serve().await;
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "slow", "accessPolicy": "PUBLIC" }))
        .await;
    let fast_g = join_guest(&app, cid, "fast").await;
    let slow_g = join_guest(&app, cid, "slow").await;
    let mut fast = subscribe(&app, addr, cid, fast_g.auth(), None).await;
    let mut slow = subscribe(&app, addr, cid, slow_g.auth(), None).await;
    let start = frame(&mut slow, Duration::from_secs(5)).await.unwrap()["cursor"]
        .as_i64()
        .unwrap();
    assert_eq!(
        frame(&mut fast, Duration::from_secs(5)).await.unwrap()["type"],
        "subscribed"
    );

    const N: usize = 300;
    for i in 0..N {
        let action = if i % 2 == 0 { "lock" } else { "unlock" };
        let (s, _) = app
            .post(
                format!("/api/v2/conferences/{cid}/{action}"),
                Auth::Bearer(&owner.token),
                json!({}),
            )
            .await;
        assert!(s.is_success());
    }
    // The fast consumer receives everything while the slow one reads nothing.
    let mut fast_seq = vec![];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while fast_seq.len() < N && tokio::time::Instant::now() < deadline {
        if let Some(f) = frame(&mut fast, Duration::from_secs(10)).await {
            if f["type"] == "event"
                && f["event"]["eventType"]
                    .as_str()
                    .map(|t| t.starts_with("conference."))
                    .unwrap_or(false)
            {
                fast_seq.push(f["event"]["sequence"].as_i64().unwrap());
            }
        }
    }
    assert_eq!(
        fast_seq.len(),
        N,
        "fast consumer not blocked by the slow one"
    );
    // Now the slow consumer drains: same events, strictly increasing, no gap.
    let mut slow_seq = vec![];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while slow_seq.len() < N && tokio::time::Instant::now() < deadline {
        match frame(&mut slow, Duration::from_secs(10)).await {
            Some(f)
                if f["type"] == "event"
                    && f["event"]["eventType"]
                        .as_str()
                        .map(|t| t.starts_with("conference."))
                        .unwrap_or(false) =>
            {
                slow_seq.push(f["event"]["sequence"].as_i64().unwrap())
            }
            Some(f) => assert_ne!(
                f["type"], "resync_required",
                "no resync needed for a live slow reader"
            ),
            None => break,
        }
    }
    assert_eq!(
        slow_seq, fast_seq,
        "slow consumer receives the identical ordered stream"
    );
    assert!(slow_seq.windows(2).all(|w| w[1] > w[0]));
    assert!(slow_seq[0] > start);
    let (s, c) = app
        .get(format!("/api/v2/conferences/{cid}"), slow_g.auth())
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        c["isLocked"], false,
        "canonical state agrees with the last event"
    );
}

/// §35 — cursor too old (beyond the replay window, or into purged history) →
/// `resync_required` on both the socket and the HTTP replay, then canonical refetch.
#[tokio::test]
async fn m_f_ws_cursor_too_old_requires_resync() {
    let app = app!(TestApp::livekit());
    let addr = app.serve().await;
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "old", "accessPolicy": "PUBLIC" }))
        .await;
    let g = join_guest(&app, cid, "g").await;
    let window = backend::services::realtime::MAX_REPLAY as usize;
    for i in 0..(window + 10) {
        let action = if i % 2 == 0 { "lock" } else { "unlock" };
        app.post(
            format!("/api/v2/conferences/{cid}/{action}"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    }
    // Beyond the replay window.
    let mut ws = subscribe(&app, addr, cid, g.auth(), Some(1)).await;
    let f = frame(&mut ws, Duration::from_secs(5)).await.unwrap();
    assert_eq!(f["type"], "resync_required", "{f}");
    let (s, v) = app
        .get(
            format!("/api/v2/conferences/{cid}/events?after=1"),
            g.auth(),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["resyncRequired"], true, "{v}");
    drop(ws);
    // Within the window but into purged history (retention removed old rows).
    let current = app
        .scalar(&format!(
            "SELECT max(conference_seq) FROM outbox_events WHERE conference_id='{cid}'"
        ))
        .await;
    let cursor = current - 20;
    exec(&app, &format!("DELETE FROM outbox_events WHERE conference_id='{cid}' AND conference_seq BETWEEN {} AND {}", cursor + 1, cursor + 5)).await;
    let mut ws = subscribe(&app, addr, cid, g.auth(), Some(cursor)).await;
    let f = frame(&mut ws, Duration::from_secs(5)).await.unwrap();
    assert_eq!(f["type"], "resync_required", "gap → resync: {f}");
    let (s, c) = app.get(f["canonical"].as_str().unwrap(), g.auth()).await;
    assert_eq!(s, StatusCode::OK);
    let expected_locked = (window + 10) % 2 == 1;
    assert_eq!(
        c["isLocked"], expected_locked,
        "canonical refetch recovers state"
    );
    // A valid recent cursor still replays normally.
    let mut ws = subscribe(&app, addr, cid, g.auth(), Some(current - 2)).await;
    let f = frame(&mut ws, Duration::from_secs(5)).await.unwrap();
    assert_eq!(f["type"], "subscribed", "{f}");
}

/// §34 / §41 regression (defect found during the Part M scheduler load run):
/// conferences that overrun their scheduled end but must NOT be auto-ended (auto-end
/// disabled, or still inside the grace period) must never starve due conferences out
/// of the bounded scheduler batch (50). The previous candidate query had no ordering
/// and filtered eligibility after `LIMIT`; PostgreSQL walked the live-per-space index,
/// so the due conference in the highest-id space was skipped on every tick.
#[tokio::test]
async fn m_scheduler_auto_end_not_starved_by_ineligible_overruns() {
    let app = app!(TestApp::livekit());
    let owner = app.user("owner").await;
    let mut confs = vec![];
    for i in 0..60 {
        let (sid, cid) = app
            .live_conference(
                &owner,
                json!({ "title": format!("long {i}"), "accessPolicy": "PUBLIC" }),
            )
            .await;
        confs.push((sid, cid));
    }
    // The conference of the highest space id is the only due one; it also has the
    // latest scheduled end, so neither physical nor index order can favour it.
    confs.sort();
    let (due_space, due) = confs.pop().unwrap();
    for (i, (sid, cid)) in confs.iter().enumerate() {
        let policy = if i % 2 == 0 {
            r#"{"autoEnd": false}"#
        } else {
            r#"{"autoEndGraceMinutes": 1440}"#
        };
        exec(
            &app,
            &format!("UPDATE meeting_spaces SET schedule_policy = '{policy}' WHERE id='{sid}'"),
        )
        .await;
        exec(&app, &format!(
            "UPDATE conference_sessions SET scheduled_start_at = now() - interval '5 hours', scheduled_end_at = now() - interval '4 hours' WHERE id='{cid}'"
        ))
        .await;
    }
    exec(&app, &format!(
        "UPDATE conference_sessions SET scheduled_start_at = now() - interval '2 hours', scheduled_end_at = now() - interval '1 hour' WHERE id='{due}'"
    ))
    .await;
    let _ = due_space;
    let scheduler = backend::workers::ConferenceScheduler::new(app.state.meeting.clone());
    let r = scheduler.tick(chrono::Utc::now()).await.unwrap();
    assert_eq!(r.end_requested, 1, "{r:?}");
    assert!(matches!(
        conference_status(&app, due).await.as_str(),
        "ENDING" | "ENDED"
    ));
    for (_, cid) in confs {
        assert_eq!(
            conference_status(&app, cid).await,
            "LIVE",
            "ineligible conference left running"
        );
    }
    // Idempotent: nothing further is due.
    assert_eq!(
        scheduler
            .tick(chrono::Utc::now())
            .await
            .unwrap()
            .end_requested,
        0
    );
}

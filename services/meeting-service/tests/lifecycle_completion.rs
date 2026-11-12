//! Final production completion: empty instant conferences end after 10 minutes, archiving
//! a meeting space ends its LIVE conferences, and the archived-space invariant is repaired.
//!
//! Mock LiveKit provider (real `LiveKitProvider` against the in-process RoomService), so
//! provider presence, room termination and ENDED confirmation are exercised end to end.

mod common;

use axum::http::StatusCode;
use backend::workers::{empty_conference::DEFAULT_EMPTY_TIMEOUT_SECS, EmptyConferenceSweeper};
use chrono::{Duration, Utc};
use common::{rtc::*, *};
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::livekit().await {
            Some(a) => a,
            None => return,
        }
    };
}

fn sweeper(app: &TestApp) -> EmptyConferenceSweeper {
    EmptyConferenceSweeper::new(
        app.state.meeting.clone(),
        Duration::seconds(DEFAULT_EMPTY_TIMEOUT_SECS),
    )
}

async fn instant(app: &TestApp, owner: &User) -> (Uuid, Uuid, Value) {
    let (s, v) = app
        .post(
            "/api/v2/instant-meetings",
            Auth::Bearer(&owner.token),
            json!({ "title": "Quick sync" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["conference"]["status"], "LIVE");
    (
        uuid(&v["meetingSpace"]["id"]),
        uuid(&v["conference"]["id"]),
        v,
    )
}

async fn termination_reason(app: &TestApp, cid: Uuid) -> String {
    use sea_orm::ConnectionTrait;
    app.db
        .conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT COALESCE(termination_reason, '') FROM conference_sessions WHERE id='{cid}'"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<String>(0)
        .unwrap()
}

/// An instant conference nobody ever joined ends once it is 10 minutes old, through the
/// canonical end path (ENDING → provider termination → ENDED, `conference.ended`).
#[tokio::test]
async fn never_joined_instant_conference_ends_after_ten_minutes() {
    let app = app!();
    let owner = app.user("host").await;
    let (_, cid, _) = instant(&app, &owner).await;
    let sweeper = sweeper(&app);

    // 9 minutes: still LIVE (the interval started at creation).
    let r = sweeper.tick(Utc::now() + Duration::minutes(9)).await.unwrap();
    assert_eq!((r.empty, r.ended), (1, 0), "{r:?}");
    assert_eq!(conference_status(&app, cid).await, "LIVE");

    // 11 minutes: ended with EMPTY_TIMEOUT.
    let r = sweeper.tick(Utc::now() + Duration::minutes(11)).await.unwrap();
    assert_eq!(r.ended, 1, "{r:?}");
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(termination_reason(&app, cid).await, "EMPTY_TIMEOUT");
    assert_eq!(events(&app, cid, "conference.ending").await, 1);
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
    assert!(!lk(&app).has_room(&room(cid)), "provider room terminated");

    // Idempotent: further sweeps change nothing and emit nothing.
    let r = sweeper.tick(Utc::now() + Duration::minutes(30)).await.unwrap();
    assert_eq!((r.checked, r.ended), (0, 0));
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
}

/// Someone in the room keeps the conference LIVE however old it is (no duration limit);
/// when the last person leaves a fresh 10-minute interval starts; a rejoin clears it.
#[tokio::test]
async fn occupied_conference_is_never_ended_and_leaving_starts_a_fresh_interval() {
    let app = app!();
    let owner = app.user("host").await;
    let (_, cid, _) = instant(&app, &owner).await;
    let sweeper = sweeper(&app);
    let host = join_user(&app, cid, &owner).await;
    connect(&app, cid, &host).await;

    // Hours later, still occupied: never ended for its age.
    let r = sweeper.tick(Utc::now() + Duration::hours(5)).await.unwrap();
    assert_eq!((r.occupied, r.ended), (1, 0), "{r:?}");
    assert_eq!(conference_status(&app, cid).await, "LIVE");

    // The host closes the browser (no leave call): the provider room is empty.
    lk(&app).disconnect(&room(cid), &host.identity);
    let left_at = Utc::now();
    let r = sweeper.tick(left_at).await.unwrap();
    assert_eq!(r.empty, 1, "{r:?}");
    // 9 minutes after leaving: still LIVE.
    let r = sweeper.tick(left_at + Duration::minutes(9)).await.unwrap();
    assert_eq!(r.ended, 0, "{r:?}");
    assert_eq!(conference_status(&app, cid).await, "LIVE");

    // A rejoin clears the interval…
    let again = join_user(&app, cid, &owner).await;
    lk(&app).connect(&again.credential).unwrap();
    let r = sweeper.tick(left_at + Duration::minutes(9)).await.unwrap();
    assert_eq!(r.occupied, 1, "{r:?}");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_presence WHERE conference_id='{cid}' AND empty_since IS NULL"
        ))
        .await,
        1
    );
    // …and the next departure starts a fresh interval (not the earlier one).
    lk(&app).disconnect(&room(cid), &again.identity);
    let second_leave = Utc::now() + Duration::minutes(10);
    sweeper.tick(second_leave).await.unwrap();
    let r = sweeper.tick(second_leave + Duration::minutes(5)).await.unwrap();
    assert_eq!(r.ended, 0, "{r:?}");
    let r = sweeper.tick(second_leave + Duration::minutes(11)).await.unwrap();
    assert_eq!(r.ended, 1, "{r:?}");
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(termination_reason(&app, cid).await, "EMPTY_TIMEOUT");
}

/// A join that races the sweeper wins: the re-check under the conference lock sees the new
/// session and resets the interval instead of ending the conference.
#[tokio::test]
async fn a_join_racing_the_sweeper_resets_the_interval() {
    let app = app!();
    let owner = app.user("host").await;
    let guest_user = app.user("guest").await;
    let (_, cid, _) = instant(&app, &owner).await;
    let sweeper = sweeper(&app);
    sweeper.tick(Utc::now()).await.unwrap();
    let empty_since: chrono::DateTime<Utc> = {
        use sea_orm::ConnectionTrait;
        app.db
            .conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT empty_since FROM conference_presence WHERE conference_id='{cid}'"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap()
    };
    // Someone joins after the sweeper observed emptiness …
    let _late = join_user(&app, cid, &guest_user).await;
    // … the end attempt is refused and the interval cleared.
    let ended = app
        .state
        .meeting
        .conferences
        .end_if_still_empty(cid, empty_since)
        .await
        .unwrap();
    assert!(!ended);
    assert_eq!(conference_status(&app, cid).await, "LIVE");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_presence WHERE conference_id='{cid}' AND empty_since IS NULL AND last_occupied_at IS NOT NULL"
        ))
        .await,
        1
    );
}

/// Scheduled conferences keep their own lifecycle (scheduled end + grace): the empty
/// instant-conference rule does not apply to them.
#[tokio::test]
async fn scheduled_conferences_are_not_subject_to_the_empty_rule() {
    let app = app!();
    let owner = app.user("host").await;
    let (_, cid) = app
        .live_conference(&owner, json!({ "title": "Board", "accessPolicy": "PUBLIC" }))
        .await;
    let r = sweeper(&app)
        .tick(Utc::now() + Duration::minutes(45))
        .await
        .unwrap();
    assert_eq!((r.checked, r.ended), (0, 0), "{r:?}");
    assert_eq!(conference_status(&app, cid).await, "LIVE");
}

/// Archiving ends the LIVE conference immediately, even with someone connected: the
/// provider room is terminated (participants disconnected), the conference reaches ENDED,
/// history is kept, and the archived space cannot start a conference again.
#[tokio::test]
async fn archiving_a_space_ends_its_live_conference_with_participants_connected() {
    let app = app!();
    let owner = app.user("host").await;
    let (space_id, cid, _) = instant(&app, &owner).await;
    let host = join_user(&app, cid, &owner).await;
    connect(&app, cid, &host).await;

    let (s, v) = app
        .post(
            &format!("/api/v2/meeting-spaces/{space_id}/archive"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "ARCHIVED");
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(termination_reason(&app, cid).await, "HOST_ENDED");
    assert!(!lk(&app).has_room(&room(cid)), "participants disconnected");
    assert_eq!(events(&app, cid, "conference.ending").await, 1);
    assert_eq!(events(&app, cid, "conference.ended").await, 1);
    // Invariant: an archived space has zero LIVE conferences.
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{space_id}' AND status='LIVE'"
        ))
        .await,
        0
    );
    // History kept: participants, sessions and audit stay.
    assert!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_participants WHERE conference_id='{cid}'"
        ))
        .await
            >= 1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_audit_events WHERE conference_id='{cid}' AND action='CONFERENCE_END_REQUEST' AND metadata->>'cause'='SPACE_ARCHIVED'"
        ))
        .await,
        1
    );

    // Repeated archive: idempotent (no second end, no error).
    let (s, _) = app
        .post(
            &format!("/api/v2/meeting-spaces/{space_id}/archive"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(events(&app, cid, "conference.ending").await, 1);

    // No new LIVE conference in an archived space.
    let (s, v) = app
        .post(
            &format!("/api/v2/meeting-spaces/{space_id}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert!(s.is_client_error(), "{s} {v}");
}

/// Only an authorized user may archive (and thereby end the conference).
#[tokio::test]
async fn unauthorized_archive_changes_nothing() {
    let app = app!();
    let owner = app.user("host").await;
    let stranger = app.user("stranger").await;
    let (space_id, cid, _) = instant(&app, &owner).await;
    let (s, _) = app
        .post(
            &format!("/api/v2/meeting-spaces/{space_id}/archive"),
            Auth::Bearer(&stranger.token),
            json!({}),
        )
        .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "{s}"
    );
    assert_eq!(conference_status(&app, cid).await, "LIVE");
}

/// Concurrent archive requests end the conference exactly once.
#[tokio::test]
async fn concurrent_archives_end_once() {
    let app = app!();
    let owner = app.user("host").await;
    let (space_id, cid, _) = instant(&app, &owner).await;
    let uri = format!("/api/v2/meeting-spaces/{space_id}/archive");
    let (a, b) = tokio::join!(
        app.post(&uri, Auth::Bearer(&owner.token), json!({})),
        app.post(&uri, Auth::Bearer(&owner.token), json!({}))
    );
    assert_eq!((a.0, b.0), (StatusCode::OK, StatusCode::OK));
    assert_eq!(events(&app, cid, "conference.ending").await, 1);
    assert_eq!(conference_status(&app, cid).await, "ENDED");
}

/// Spaces archived before the rule (LIVE conference left behind) are repaired by the
/// sweeper: the conference ends at once (ADMIN, cause SPACE_ARCHIVED).
#[tokio::test]
async fn sweeper_repairs_live_conferences_of_archived_spaces() {
    let app = app!();
    let owner = app.user("host").await;
    let (space_id, cid, _) = instant(&app, &owner).await;
    exec(
        &app,
        &format!("UPDATE meeting_spaces SET status='ARCHIVED', archived_at=now() WHERE id='{space_id}'"),
    )
    .await;
    let r = sweeper(&app).tick(Utc::now()).await.unwrap();
    assert_eq!(r.archived_repaired, 1, "{r:?}");
    assert_eq!(conference_status(&app, cid).await, "ENDED");
    assert_eq!(termination_reason(&app, cid).await, "ADMIN");
}

//! Recurrence exceptions independent of the 30-day materialization window (A–D closure 5).

mod common;

use axum::http::{Method, StatusCode};
use backend::{
    domain::Role,
    services::recurrence::{build_rule_set, occurrences_between},
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc};
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

macro_rules! app {
    () => {
        match TestApp::new().await {
            Some(a) => a,
            None => return,
        }
    };
}

async fn put_exception(app: &TestApp, user: &User, sid: Uuid, body: Value) -> (StatusCode, Value) {
    app.call(
        Method::PUT,
        &format!("/api/v2/meeting-spaces/{sid}/recurrence/exceptions"),
        Auth::Bearer(&user.token),
        Some(body),
    )
    .await
}

async fn set_rule(
    app: &TestApp,
    owner: &User,
    sid: Uuid,
    rrule: &str,
    start_local: chrono::NaiveDateTime,
) {
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/meeting-spaces/{sid}/recurrence"),
            Auth::Bearer(&owner.token),
            Some(json!({ "rrule": rrule, "startLocal": start_local, "durationMinutes": 60 })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

async fn rows_for(
    app: &TestApp,
    sid: Uuid,
    original: DateTime<Utc>,
) -> Vec<(String, bool, String)> {
    let rows = app
        .db
        .conn
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT status, is_exception, to_char(scheduled_start_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI') \
                 FROM conference_sessions WHERE meeting_space_id='{sid}' AND original_start_at = '{}'",
                original.to_rfc3339()
            ),
        ))
        .await
        .unwrap();
    rows.iter()
        .map(|r| {
            (
                r.try_get_by_index(0).unwrap(),
                r.try_get_by_index(1).unwrap(),
                r.try_get_by_index(2).unwrap(),
            )
        })
        .collect()
}

fn next_monday() -> NaiveDate {
    let mut d = (Utc::now() + Duration::days(1)).date_naive();
    while d.weekday() != chrono::Weekday::Mon {
        d += Duration::days(1);
    }
    d
}

/// Occurrences far beyond the horizon can be cancelled/rescheduled; the scheduler
/// later generates them with the exception applied, across DST changes, once.
#[tokio::test]
async fn rx01_far_future_exceptions_applied_by_scheduler_across_dst() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "weekly", "timezone": "Europe/London" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let start_local = next_monday().and_hms_opt(9, 0, 0).unwrap();
    set_rule(&app, &owner, sid, "FREQ=WEEKLY;BYDAY=MO", start_local).await;

    let set = build_rule_set("FREQ=WEEKLY;BYDAY=MO", "Europe/London", start_local).unwrap();
    let all = occurrences_between(
        set,
        chrono_tz::Europe::London,
        Utc::now(),
        Utc::now() + Duration::days(400),
        100,
    );
    let london = |d: &DateTime<Utc>| d.with_timezone(&chrono_tz::Europe::London);
    // One occurrence in GMT (winter, 09:00Z) and one in BST (summer, 08:00Z), both > 60 days out.
    let winter = *all
        .iter()
        .find(|d| **d > Utc::now() + Duration::days(60) && d.hour() == 9)
        .unwrap();
    let summer = *all
        .iter()
        .find(|d| **d > winter + Duration::days(30) && d.hour() == 8)
        .unwrap();
    assert_eq!(london(&winter).hour(), 9);
    assert_eq!(
        london(&summer).hour(),
        9,
        "wall-clock time is stable across DST"
    );
    assert!(
        rows_for(&app, sid, winter).await.is_empty(),
        "not materialized yet (bounded horizon)"
    );
    let horizon_rows = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await;
    assert!(horizon_rows <= 5);

    let (s, cancel) = put_exception(
        &app,
        &owner,
        sid,
        json!({ "originalStartAt": winter, "action": "CANCEL" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cancel}");
    assert_eq!(cancel["kind"], "CANCELLED");
    assert!(cancel.get("appliedToConferenceId").is_none());
    assert!(cancel["originalStartLocal"]
        .as_str()
        .unwrap()
        .ends_with("09:00:00"));
    // Idempotent: same request → same exception, no version bump.
    let (_, again) = put_exception(
        &app,
        &owner,
        sid,
        json!({ "originalStartAt": winter, "action": "CANCEL" }),
    )
    .await;
    assert_eq!(again["id"], cancel["id"]);
    assert_eq!(again["version"], 1);
    let moved_to = summer + Duration::hours(3);
    let (s, resched) = put_exception(
        &app,
        &owner,
        sid,
        json!({ "originalStartAt": summer, "action": "RESCHEDULE", "scheduledStartAt": moved_to }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{resched}");
    // Update an exception in place (idempotent key = original wall-clock start).
    let (_, flipped) = put_exception(&app, &owner, sid, json!({ "originalStartAt": winter, "action": "RESCHEDULE", "scheduledStartAt": winter + Duration::hours(1) })).await;
    assert_eq!(flipped["id"], cancel["id"]);
    assert_eq!(flipped["version"], 2);
    let (_, back) = put_exception(
        &app,
        &owner,
        sid,
        json!({ "originalStartAt": winter, "action": "CANCEL" }),
    )
    .await;
    assert_eq!(back["version"], 3);

    // Invalid targets.
    let (s, _) = put_exception(
        &app,
        &owner,
        sid,
        json!({ "originalStartAt": winter + Duration::minutes(1), "action": "CANCEL" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "not an occurrence");
    let (s, _) = put_exception(
        &app,
        &owner,
        sid,
        json!({ "originalStartAt": Utc::now() - Duration::days(7), "action": "CANCEL" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "past");
    let (s, _) = put_exception(
        &app,
        &owner,
        sid,
        json!({ "originalStartAt": summer, "action": "RESCHEDULE" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "reschedule needs a time");
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}'"
        ))
        .await,
        horizon_rows,
        "no rows generated beyond the horizon"
    );

    // Scheduler reaches the winter occurrence later (simulated clock), twice, concurrently.
    let mut handles = vec![];
    for _ in 0..4 {
        let m = app.state.meeting.clone();
        handles.push(tokio::spawn(async move {
            m.recurrence
                .materialize_due(winter - Duration::days(3), 10)
                .await
                .unwrap()
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    app.db
        .conn
        .execute_unprepared("UPDATE meeting_recurrence_rules SET materialized_until = NULL")
        .await
        .unwrap();
    app.state
        .meeting
        .recurrence
        .materialize_due(summer - Duration::days(3), 10)
        .await
        .unwrap();
    app.db
        .conn
        .execute_unprepared("UPDATE meeting_recurrence_rules SET materialized_until = NULL")
        .await
        .unwrap();
    app.state
        .meeting
        .recurrence
        .materialize_due(summer - Duration::days(3), 10)
        .await
        .unwrap();

    let w = rows_for(&app, sid, winter).await;
    assert_eq!(w.len(), 1, "exactly one row");
    assert_eq!((w[0].0.as_str(), w[0].1), ("CANCELLED", true));
    let su = rows_for(&app, sid, summer).await;
    assert_eq!(su.len(), 1);
    assert_eq!((su[0].0.as_str(), su[0].1), ("CREATED", true));
    assert_eq!(su[0].2, moved_to.format("%Y-%m-%dT%H:%M").to_string());
    let dup = app
        .scalar(&format!("SELECT count(*) - count(DISTINCT occurrence_key) FROM conference_sessions WHERE meeting_space_id='{sid}'"))
        .await;
    assert_eq!(dup, 0);
    let (_, list) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/recurrence/exceptions"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(list.as_array().unwrap().len(), 2);
}

/// Exceptions on materialized occurrences apply immediately; conference-level
/// cancel/reschedule write the same authoritative exception; timezone re-anchoring
/// keeps exceptions aligned without duplicating occurrences.
#[tokio::test]
async fn rx02_materialized_occurrences_and_timezone_reanchor() {
    let app = app!();
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let space = app
        .create_space(&owner, json!({ "title": "daily", "timezone": "UTC" }))
        .await;
    let sid = uuid(&space["id"]);
    app.add_member(sid, &owner, &member, Role::Participant)
        .await;
    let start_local = (Utc::now() + Duration::days(2))
        .date_naive()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    set_rule(&app, &owner, sid, "FREQ=DAILY;COUNT=5", start_local).await;
    let (_, list) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
        )
        .await;
    let items = list["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 5);
    let original = |i: usize| -> DateTime<Utc> {
        items[i]["originalStartAt"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    };

    // Members without scheduling rights cannot create exceptions.
    let (s, _) = put_exception(
        &app,
        &member,
        sid,
        json!({ "originalStartAt": original(1), "action": "CANCEL" }),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Conference-level cancel records the authoritative exception.
    let c1 = items[1]["id"].as_str().unwrap();
    app.post(
        &format!("/api/v2/conferences/{c1}/cancel"),
        Auth::Bearer(&owner.token),
        json!({}),
    )
    .await;
    // Series-level reschedule of a materialized occurrence applies immediately.
    let moved = original(2) + Duration::hours(2);
    let (s, r) = put_exception(&app, &owner, sid, json!({ "originalStartAt": original(2), "action": "RESCHEDULE", "scheduledStartAt": moved })).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["appliedToConferenceId"], items[2]["id"]);
    let (_, ex) = app
        .get(
            &format!("/api/v2/meeting-spaces/{sid}/recurrence/exceptions"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(ex.as_array().unwrap().len(), 2);

    // A started occurrence can no longer take an exception.
    let c0 = items[0]["id"].as_str().unwrap();
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE conference_sessions SET scheduled_start_at = now() WHERE id='{c0}'"
        ))
        .await
        .unwrap();
    let (s, _) = app
        .post(
            &format!("/api/v2/conferences/{c0}/start"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = put_exception(
        &app,
        &owner,
        sid,
        json!({ "originalStartAt": original(0), "action": "CANCEL" }),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT")
    );

    // Re-anchor the series to New York: same wall-clock occurrences, exceptions still applied.
    let (s, v) = app
        .patch(
            &format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
            json!({ "version": 1, "timezone": "America/New_York" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ny = chrono_tz::America::New_York;
    let day1_ny = ny
        .from_local_datetime(&original(1).date_naive().and_hms_opt(10, 0, 0).unwrap())
        .single()
        .unwrap()
        .with_timezone(&Utc);
    let rows = rows_for(&app, sid, day1_ny).await;
    assert_eq!(rows.len(), 1, "regenerated once");
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1),
        ("CANCELLED", true),
        "cancellation follows the wall-clock occurrence"
    );
    let created_dups = app
        .scalar(&format!(
            "SELECT count(*) FROM (SELECT scheduled_start_at FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='CREATED' GROUP BY scheduled_start_at HAVING count(*) > 1) d"
        ))
        .await;
    assert_eq!(created_dups, 0, "no duplicate scheduled occurrences");
    let moved_rows = app
        .scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='CREATED' AND scheduled_start_at = '{}'",
            moved.to_rfc3339()
        ))
        .await;
    assert_eq!(
        moved_rows, 1,
        "rescheduled occurrence survives re-anchoring exactly once"
    );
}

/// Concurrent identical exception requests create exactly one exception.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn rx03_concurrent_exception_upserts_are_idempotent() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app
        .create_space(&owner, json!({ "title": "w", "timezone": "Africa/Lagos" }))
        .await;
    let sid = uuid(&space["id"]);
    let start_local = (Utc::now() + Duration::days(1))
        .date_naive()
        .and_hms_opt(8, 0, 0)
        .unwrap();
    set_rule(&app, &owner, sid, "FREQ=DAILY", start_local).await;
    let set = build_rule_set("FREQ=DAILY", "Africa/Lagos", start_local).unwrap();
    let far = occurrences_between(
        set,
        chrono_tz::Africa::Lagos,
        Utc::now() + Duration::days(200),
        Utc::now() + Duration::days(202),
        1,
    )[0];
    let mut handles = vec![];
    for _ in 0..8 {
        let router = app.router.clone();
        let token = owner.token.clone();
        handles.push(tokio::spawn(async move {
            use tower::ServiceExt;
            let req = axum::http::Request::put(format!(
                "/api/v2/meeting-spaces/{sid}/recurrence/exceptions"
            ))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({ "originalStartAt": far, "action": "CANCEL" }).to_string(),
            ))
            .unwrap();
            router.oneshot(req).await.unwrap().status()
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM meeting_recurrence_exceptions WHERE meeting_space_id='{sid}'"
        ))
        .await,
        1
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_audit_events WHERE meeting_space_id='{sid}' AND action='RECURRENCE_EXCEPTION_SET'")).await,
        1
    );
}

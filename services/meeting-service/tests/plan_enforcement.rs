//! ADR 0068 A6 — CF Meet user plan limits enforced by the Meeting Service
//! (`services::plan_limits`): participants per meeting (every seat), recording, and the
//! meeting length (on extension). A holder without a plan answer, and every failure of the
//! plan read, keeps today's limits (fail open).

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::http::StatusCode;
use backend::services::plan_limits::{
    Holder, PlanEnforcement, PlanLimits, PlanSource, StaticPlans,
};
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

fn limits(v: Value) -> PlanLimits {
    PlanLimits::from_json(&v).unwrap()
}

async fn app_with(plans: Arc<dyn PlanSource>) -> Option<TestApp> {
    TestApp::with(TestOptions {
        rtc: RtcMode::LiveKitMock,
        flags: TestApp::ghi_flags(),
        plan_limits: Some(Arc::new(PlanEnforcement::new(
            plans,
            Duration::from_secs(60),
        ))),
        ..TestOptions::default()
    })
    .await
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

async fn guest(app: &TestApp, cid: Uuid, name: &str) -> (StatusCode, Value) {
    app.join(cid, Auth::None, json!({ "displayName": name }))
        .await
}

#[tokio::test]
async fn plan_limits_bound_seats_recording_and_meeting_length() {
    let plans = Arc::new(StaticPlans(Mutex::new(HashMap::new())));
    let Some(app) = app_with(plans.clone()).await else {
        return;
    };
    let free = app.user("free-owner").await;
    let unlimited = app.user("no-plan-owner").await;
    plans.0.lock().unwrap().insert(
        Holder::User(free.id),
        limits(json!({
            "planCode": "FREE",
            "limits": { "participants": 2, "recording": false, "meetingMinutes": 30, "webinars": null }
        })),
    );
    let room = json!({ "title": "plan", "accessPolicy": "PUBLIC", "interactiveCapacity": 50 });

    // participants: the plan's 2 seats, although the room has 50
    let (_, cid) = app.live_conference(&free, room.clone()).await;
    for name in ["a", "b"] {
        let (s, v) = guest(&app, cid, name).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let (s, v) = guest(&app, cid, "c").await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "MEETING_CAPACITY_REACHED"),
        "{v}"
    );
    // no plan answer: today's limits only
    let (_, open) = app.live_conference(&unlimited, room.clone()).await;
    for name in ["a", "b", "c"] {
        let (s, v) = guest(&app, open, name).await;
        assert_eq!(s, StatusCode::OK, "{name}: {v}");
    }

    // recording: not included in the plan
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/recordings"),
            Auth::Bearer(&free.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::FORBIDDEN, "RECORDING_NOT_ALLOWED"),
        "{v}"
    );
    let (_, v) = app
        .post(
            format!("/api/v2/conferences/{open}/recordings"),
            Auth::Bearer(&unlimited.token),
            json!({}),
        )
        .await;
    assert_ne!(code(&v), "RECORDING_NOT_ALLOWED", "{v}");

    // meeting length: an extension past the plan's 30 minutes is refused
    for c in [cid, open] {
        app.db
            .conn
            .execute_unprepared(&format!(
                "UPDATE conference_sessions SET started_at = now() - interval '20 minutes', \
                        scheduled_end_at = now() + interval '5 minutes' WHERE id = '{c}'"
            ))
            .await
            .unwrap();
    }
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/extend"),
            Auth::Bearer(&free.token),
            json!({ "minutes": 15 }),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/extend"),
            Auth::Bearer(&free.token),
            json!({ "minutes": 2 }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "within the plan: {v}");
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{open}/extend"),
            Auth::Bearer(&unlimited.token),
            json!({ "minutes": 15 }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

struct Unreachable;

#[async_trait]
impl PlanSource for Unreachable {
    async fn limits(&self, _: Holder) -> Result<Option<PlanLimits>, String> {
        Err("connection refused".into())
    }
}

#[tokio::test]
async fn an_unreachable_plan_read_keeps_todays_limits() {
    let Some(app) = app_with(Arc::new(Unreachable)).await else {
        return;
    };
    let owner = app.user("owner").await;
    let (_, cid) = app
        .live_conference(
            &owner,
            json!({ "title": "open", "accessPolicy": "PUBLIC", "interactiveCapacity": 2 }),
        )
        .await;
    for name in ["a", "b"] {
        let (s, v) = guest(&app, cid, name).await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    // the room's own capacity still applies
    let (_, v) = guest(&app, cid, "c").await;
    assert_eq!(code(&v), "MEETING_CAPACITY_REACHED", "{v}");
}

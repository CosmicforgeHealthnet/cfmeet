//! Part F §58 business hours and forwarding on ONE canonical Call: IANA time-zone
//! evaluation (DST, cross-midnight, exception dates), open / closed routes, typed
//! forwarding (ALWAYS / BUSY / NO_ANSWER / UNAVAILABLE), loop protection (visited set +
//! hop budget) and public forwarding that cannot bypass any Part E gate (simulated
//! Telephony and Privacy Relay — never a real carrier).

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{
    calls::CallState,
    routing::{
        engine::Evidence,
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::{events::TelephonyEvent, spend::SpendPolicy},
};
use chrono::{DateTime, Utc};
use common::{
    app::{AppWorld, Device},
    enterprise::{conforms, default_rules, id_of, strings, EnterpriseWorld},
    privacy::FakePrivacyRelay,
    telephony::{leg_event, FakePublicMedia, FakeTelephony},
};
use serde_json::{json, Value};
use uuid::Uuid;

type Principal = calling::platform::principals::ResolvedPrincipal;

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn tag() -> String {
    Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .map(|c| {
            if c.is_ascii_digit() {
                (b'g' + (c as u8 - b'0')) as char
            } else {
                c
            }
        })
        .collect()
}

fn dest(kind: &str, id: impl std::fmt::Display) -> Value {
    json!({ "type": kind, "id": id.to_string() })
}

fn weekdays(start: &str, end: &str) -> Value {
    json!(["MONDAY", "TUESDAY", "WEDNESDAY", "THURSDAY", "FRIDAY"]
        .iter()
        .map(|d| json!({ "dayOfWeek": d, "start": start, "end": end }))
        .collect::<Vec<_>>())
}

async fn hours(e: &EnterpriseWorld, body: Value) -> (StatusCode, Value) {
    let r = e
        .admin(Method::POST, "/v1/enterprise/business-hours", Some(body))
        .await;
    (r.status, r.json)
}

async fn evaluate(e: &EnterpriseWorld, id: Uuid, instant: &str) -> Value {
    let r = e
        .admin(
            Method::POST,
            &format!("/v1/enterprise/business-hours/{id}/evaluation"),
            Some(json!({ "at": instant })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    conforms("BusinessHoursEvaluation", &r.json);
    r.json
}

struct Desk {
    principal: Principal,
    phone: Device,
}

async fn desk(e: &EnterpriseWorld, name: &str) -> Desk {
    let principal = e.member(&format!("{name}-{}", tag())).await;
    let phone = e.a.device(&principal, &format!("{name}-desk")).await;
    Desk { principal, phone }
}

async fn dial(e: &EnterpriseWorld, from: &Desk, number: &str) -> Uuid {
    let r = e
        .call_extension(
            "VOICE",
            &from.principal,
            &from.phone,
            number,
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

async fn steps(e: &EnterpriseWorld, call: Uuid) -> Vec<String> {
    strings(
        e.h().conn(),
        &format!(
            "SELECT decision || ':' || reason FROM enterprise_routing_steps WHERE call_id = '{call}' \
              ORDER BY step_number"
        ),
    )
    .await
}

async fn rings(e: &EnterpriseWorld, call: Uuid, d: &Desk) -> bool {
    e.a.invitation_for(call, d.phone.endpoint).await.is_some()
}

async fn tick(e: &EnterpriseWorld) {
    e.h().state.app.expire_due(100).await.unwrap();
}

async fn set_policy(e: &EnterpriseWorld, rules: Value) {
    let existing = e
        .admin(Method::GET, "/v1/enterprise/policies?status=ACTIVE", None)
        .await;
    let default = existing.json["policies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["isDefault"] == true)
        .cloned();
    match default {
        Some(p) => {
            let r = e
                .admin(
                    Method::PATCH,
                    &format!(
                        "/v1/enterprise/policies/{}",
                        p["policyId"].as_str().unwrap()
                    ),
                    Some(json!({ "expectedVersion": p["version"], "rules": rules })),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        }
        None => {
            e.create(
                "/v1/enterprise/policies",
                json!({ "name": format!("Default {}", tag()), "isDefault": true, "rules": rules }),
            )
            .await;
        }
    }
}

async fn forward(
    e: &EnterpriseWorld,
    from: &Principal,
    condition: &str,
    to: Value,
) -> (StatusCode, Value) {
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/forwarding-rules",
            Some(json!({ "principalId": from.id(), "condition": condition, "destination": to })),
        )
        .await;
    (r.status, r.json)
}

#[tokio::test]
async fn business_hours_administration_and_deterministic_evaluation() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let open = desk(&e, "open").await;
    let closed = desk(&e, "closed").await;
    let open_d = dest("ENTERPRISE_MEMBER", open.principal.id());
    let closed_d = dest("ENTERPRISE_MEMBER", closed.principal.id());
    let base = |tz: &str, windows: Value| {
        json!({ "name": format!("Office {}", tag()), "timezone": tz, "openDestination": open_d,
                "closedDestination": closed_d, "weeklyWindows": windows })
    };

    // Validation: only IANA zones, HH:MM windows, no overlaps, typed destinations of the
    // scope, no unknown fields, no public route without a permitting policy.
    for (body, status) in [
        (
            base("+01:00", weekdays("09:00", "17:00")),
            StatusCode::BAD_REQUEST,
        ),
        (
            base("Mars/Olympus", weekdays("09:00", "17:00")),
            StatusCode::BAD_REQUEST,
        ),
        (
            base("Africa/Lagos", weekdays("9:00", "17:00")),
            StatusCode::BAD_REQUEST,
        ),
        (
            base(
                "Africa/Lagos",
                json!([{ "dayOfWeek": "FRIDAY", "start": "22:00", "end": "02:00" },
                       { "dayOfWeek": "SATURDAY", "start": "01:00", "end": "05:00" }]),
            ),
            StatusCode::BAD_REQUEST,
        ),
        (
            {
                let mut b = base("Africa/Lagos", weekdays("09:00", "17:00"));
                b["dialplan"] = json!("GotoIfTime(...)");
                b
            },
            StatusCode::BAD_REQUEST,
        ),
        (
            {
                let mut b = base("Africa/Lagos", weekdays("09:00", "17:00"));
                b["closedDestination"] = dest("QUEUE", Uuid::new_v4());
                b
            },
            StatusCode::NOT_FOUND,
        ),
        (
            {
                let mut b = base("Africa/Lagos", weekdays("09:00", "17:00"));
                b["closedDestination"] = json!({ "type": "PHONE", "number": "+2348012345678" });
                b
            },
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (s, j) = hours(&e, body.clone()).await;
        assert_eq!(s, status, "{body}: {j}");
    }

    let mut body = base("Africa/Lagos", weekdays("09:00", "17:00"));
    body["exceptions"] = json!([
        { "date": "2031-03-03", "kind": "CLOSED" },
        { "date": "2031-03-04", "kind": "CUSTOM_HOURS", "windows": [{ "start": "10:00", "end": "12:00" }] }
    ]);
    let (s, created) = hours(&e, body.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    conforms("BusinessHours", &created);
    let id = id_of(&created, "businessHoursId");
    assert_eq!(created["version"], 1);
    assert_eq!(created["weeklyWindows"].as_array().unwrap().len(), 5);

    // Monday 2031-03-03 is a CLOSED exception; Tuesday has custom hours; Wednesday the
    // weekly window (Lagos = UTC+1).
    let ev = evaluate(&e, id, "2031-03-03T09:00:00Z").await;
    assert_eq!(
        (ev["status"].as_str(), ev["reason"].as_str()),
        (Some("CLOSED"), Some("EXCEPTION_CLOSED"))
    );
    assert_eq!(ev["destination"], closed_d);
    let ev = evaluate(&e, id, "2031-03-04T08:30:00Z").await;
    assert_eq!(ev["reason"], "OUTSIDE_EXCEPTION_HOURS");
    let ev = evaluate(&e, id, "2031-03-04T09:30:00Z").await;
    assert_eq!(
        (ev["status"].as_str(), ev["reason"].as_str()),
        (Some("OPEN"), Some("EXCEPTION_HOURS"))
    );
    let ev = evaluate(&e, id, "2031-03-05T08:00:00Z").await;
    assert_eq!(ev["status"], "OPEN");
    assert_eq!(ev["localTime"], "09:00");
    assert_eq!(ev["utcOffset"], "+01:00");
    assert_eq!(ev["destination"], open_d);
    // Deterministic.
    assert_eq!(evaluate(&e, id, "2031-03-05T08:00:00Z").await, ev);

    // Replace: stale version refused; a zone change moves the boundaries.
    let mut replace = base("America/New_York", weekdays("09:00", "17:00"));
    replace["expectedVersion"] = json!(7);
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/business-hours/{id}"),
            Some(replace.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    replace["expectedVersion"] = json!(1);
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/business-hours/{id}"),
            Some(replace),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["version"], 2);
    assert_eq!(r.json["exceptions"], json!([]));
    let ev = evaluate(&e, id, "2031-03-05T08:00:00Z").await;
    assert_eq!(ev["status"], "CLOSED");
    assert_eq!(ev["utcOffset"], "-05:00");

    // Read-only administrators read; nobody else's tenant sees it.
    let r = e
        .send(
            e.reader.secret.expose_once(),
            Method::GET,
            &format!("/v1/enterprise/business-hours/{id}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = e
        .send(
            e.reader.secret.expose_once(),
            Method::POST,
            "/v1/enterprise/business-hours",
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let (_, other) = e.other_tenant("hours").await;
    let r = e
        .send(
            other.secret.expose_once(),
            Method::GET,
            &format!("/v1/enterprise/business-hours/{id}"),
            None,
        )
        .await;
    assert!(
        r.status == StatusCode::NOT_FOUND || r.status == StatusCode::FORBIDDEN,
        "{}",
        r.text
    );
    let list = e
        .admin(Method::GET, "/v1/enterprise/business-hours", None)
        .await;
    conforms("BusinessHoursList", &list.json);
    assert_eq!(list.json["businessHours"].as_array().unwrap().len(), 1);

    // Disable; every change audited.
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/business-hours/{id}?expectedVersion=2"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "DISABLED");
    assert_eq!(
        strings(
            e.h().conn(),
            &format!(
                "SELECT operation FROM enterprise_admin_audit WHERE resource_id = '{id}' ORDER BY seq"
            )
        )
        .await,
        ["CREATE", "REPLACE_SCHEDULE", "DISABLE"]
    );
}

/// An extension routed through a schedule: open hours ring the open route, closed hours
/// the closed route, an exception date closes, a DST boundary is honoured, and every
/// decision is evidence on the SAME Call.
#[tokio::test]
async fn schedules_route_open_and_closed_hours_on_one_call() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let day = desk(&e, "day").await;
    let night = desk(&e, "night").await;
    let (s, h) = hours(
        &e,
        json!({ "name": "Lagos", "timezone": "Africa/Lagos",
                "openDestination": dest("ENTERPRISE_MEMBER", day.principal.id()),
                "closedDestination": dest("ENTERPRISE_MEMBER", night.principal.id()),
                "weeklyWindows": weekdays("09:00", "17:00"),
                "exceptions": [{ "date": "2031-03-06", "kind": "CLOSED" }] }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{h}");
    let h = id_of(&h, "businessHoursId");
    e.extension("6200", "SCHEDULE", h).await;

    // Wednesday 10:00 Lagos: open.
    e.h().clock.set(at("2031-03-05T09:00:00Z"));
    let call = dial(&e, &caller, "6200").await;
    assert!(rings(&e, call, &day).await);
    assert!(!rings(&e, call, &night).await);
    assert!(steps(&e, call).await.contains(&"SCHEDULE:OPEN".to_string()));
    // Wednesday 17:00 Lagos: closed (end is exclusive).
    e.h().clock.set(at("2031-03-05T16:00:00Z"));
    let call = dial(&e, &caller, "6200").await;
    assert!(rings(&e, call, &night).await);
    assert!(steps(&e, call)
        .await
        .contains(&"SCHEDULE:CLOSED".to_string()));
    assert_eq!(
        e.count(&format!("SELECT count(*) FROM calls WHERE id = '{call}'"))
            .await,
        1
    );
    // Thursday 2031-03-06 is a CLOSED exception date.
    e.h().clock.set(at("2031-03-06T10:00:00Z"));
    let call = dial(&e, &caller, "6200").await;
    assert!(rings(&e, call, &night).await);

    // DST: Europe/London springs forward on Sunday 2031-03-30 (01:00 GMT -> 02:00 BST).
    let (s, london) = hours(
        &e,
        json!({ "name": "London", "timezone": "Europe/London",
                "openDestination": dest("ENTERPRISE_MEMBER", day.principal.id()),
                "closedDestination": dest("ENTERPRISE_MEMBER", night.principal.id()),
                "weeklyWindows": [{ "dayOfWeek": "SUNDAY", "start": "02:00", "end": "03:00" }] }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{london}");
    let london = id_of(&london, "businessHoursId");
    e.extension("6201", "SCHEDULE", london).await;
    // 00:59 UTC = 00:59 GMT: closed. 01:00 UTC = 02:00 BST: open. 02:00 UTC = 03:00 BST.
    for (instant, open) in [
        ("2031-03-30T00:59:00Z", false),
        ("2031-03-30T01:00:00Z", true),
        ("2031-03-30T01:59:00Z", true),
        ("2031-03-30T02:00:00Z", false),
    ] {
        e.h().clock.set(at(instant));
        let call = dial(&e, &caller, "6201").await;
        assert_eq!(rings(&e, call, &day).await, open, "{instant}");
        assert_eq!(rings(&e, call, &night).await, !open, "{instant}");
    }

    // A disabled schedule never routes: NO_ROUTE / DESTINATION_DISABLED, the Call fails.
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/business-hours/{h}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let call = dial(&e, &caller, "6200").await;
    assert_eq!(e.a.state(call).await.0, CallState::Failed);
    assert!(steps(&e, call)
        .await
        .contains(&"NO_ROUTE:DESTINATION_DISABLED".to_string()));
}

/// Typed forwarding of a principal's calls: ALWAYS before ringing, BUSY after every offer
/// was declined, NO_ANSWER after they expired, UNAVAILABLE without an endpoint — always
/// the SAME Call. The policy gates it.
#[tokio::test]
async fn forwarding_rules_take_the_same_call() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let a = desk(&e, "a").await;
    let b = desk(&e, "b").await;
    let c = desk(&e, "c").await;
    // D has no device: unavailable.
    let d = e.member(&format!("d-{}", tag())).await;
    e.extension("6301", "ENTERPRISE_MEMBER", a.principal.id())
        .await;
    e.extension("6304", "ENTERPRISE_MEMBER", d.id()).await;

    // Validation: self-forwarding, foreign / unknown targets, raw numbers, duplicates.
    let (s, _) = forward(
        &e,
        &a.principal,
        "ALWAYS",
        dest("ENTERPRISE_MEMBER", a.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = forward(&e, &a.principal, "ALWAYS", dest("QUEUE", Uuid::new_v4())).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = forward(
        &e,
        &a.principal,
        "ALWAYS",
        json!({ "type": "PHONE", "number": "+2348012345678" }),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = forward(
        &e,
        &a.principal,
        "SOMETIMES",
        dest("ENTERPRISE_MEMBER", b.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // ALWAYS: B rings instead of A.
    let (s, always) = forward(
        &e,
        &a.principal,
        "ALWAYS",
        dest("ENTERPRISE_MEMBER", b.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{always}");
    conforms("ForwardingRule", &always);
    let (s, _) = forward(
        &e,
        &a.principal,
        "ALWAYS",
        dest("ENTERPRISE_MEMBER", c.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // A configured cycle of ALWAYS rules is refused (B -> A while A -> B).
    let (s, j) = forward(
        &e,
        &b.principal,
        "ALWAYS",
        dest("ENTERPRISE_MEMBER", a.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{j}");
    let call = dial(&e, &caller, "6301").await;
    assert!(rings(&e, call, &b).await);
    assert!(!rings(&e, call, &a).await);
    assert!(steps(&e, call)
        .await
        .contains(&"FORWARD:FORWARD_ALWAYS".to_string()));

    // The policy gates forwarding: switched off, A rings again (and no new rule).
    let mut rules = default_rules();
    rules["forwardingAllowed"] = json!(false);
    set_policy(&e, rules).await;
    let call = dial(&e, &caller, "6301").await;
    assert!(rings(&e, call, &a).await);
    let (s, _) = forward(
        &e,
        &c.principal,
        "ALWAYS",
        dest("ENTERPRISE_MEMBER", b.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    set_policy(&e, default_rules()).await;

    // BUSY: A declines -> C rings on the same Call.
    let id = id_of(&always, "forwardingRuleId");
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/forwarding-rules/{id}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (s, _) = forward(
        &e,
        &a.principal,
        "BUSY",
        dest("ENTERPRISE_MEMBER", c.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = forward(
        &e,
        &a.principal,
        "NO_ANSWER",
        dest("ENTERPRISE_MEMBER", b.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let call = dial(&e, &caller, "6301").await;
    let inv = e.a.invitation_for(call, a.phone.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.decline(call, inv, &a.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(rings(&e, call, &c).await);
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    assert!(steps(&e, call)
        .await
        .contains(&"FORWARD:FORWARD_BUSY".to_string()));
    // C answers: the SAME Call is answered by C.
    let inv = e.a.invitation_for(call, c.phone.endpoint).await.unwrap();
    let (_, v) = e.a.state(call).await;
    let r = e.a.accept(call, inv, &c.phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        e.count(&format!("SELECT count(*) FROM calls WHERE id = '{call}'"))
            .await,
        1
    );

    // NO_ANSWER: A's offers expire -> B rings.
    let call = dial(&e, &caller, "6301").await;
    e.h().clock.advance(chrono::Duration::seconds(46));
    tick(&e).await;
    assert!(rings(&e, call, &b).await, "{:?}", steps(&e, call).await);
    assert!(steps(&e, call)
        .await
        .contains(&"FORWARD:FORWARD_NO_ANSWER".to_string()));

    // UNAVAILABLE: D has no endpoint -> B rings.
    let (s, _) = forward(
        &e,
        &d,
        "UNAVAILABLE",
        dest("ENTERPRISE_MEMBER", b.principal.id()),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let call = dial(&e, &caller, "6304").await;
    assert!(rings(&e, call, &b).await, "{:?}", steps(&e, call).await);
    assert!(steps(&e, call)
        .await
        .contains(&"FORWARD:FORWARD_UNAVAILABLE".to_string()));

    let list = e
        .admin(
            Method::GET,
            &format!(
                "/v1/enterprise/forwarding-rules?principalId={}",
                a.principal.id()
            ),
            None,
        )
        .await;
    conforms("ForwardingRuleList", &list.json);
    assert_eq!(list.json["forwardingRules"].as_array().unwrap().len(), 3);
}

/// A -> B -> A (and deeper) can never route forever: through extensions the configured
/// cycle is only stopped at routing time (visited set), conditional forwarding back to a
/// principal already rung ends with the ordinary outcome, and a long chain exhausts the
/// policy's hop budget.
#[tokio::test]
async fn forwarding_loops_are_impossible() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let a = desk(&e, "a").await;
    let b = desk(&e, "b").await;
    let c = desk(&e, "c").await;
    let ea = e
        .extension("6401", "ENTERPRISE_MEMBER", a.principal.id())
        .await;
    let eb = e
        .extension("6402", "ENTERPRISE_MEMBER", b.principal.id())
        .await;
    let ec = e
        .extension("6403", "ENTERPRISE_MEMBER", c.principal.id())
        .await;
    // A -> ext B, B -> ext C, C -> ext A (ALWAYS, through extensions).
    for (from, to) in [(&a, &eb), (&b, &ec), (&c, &ea)] {
        let (s, j) = forward(
            &e,
            &from.principal,
            "ALWAYS",
            dest("EXTENSION", id_of(to, "extensionId")),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{j}");
    }
    let call = dial(&e, &caller, "6401").await;
    assert_eq!(e.a.state(call).await.0, CallState::Failed);
    let s = steps(&e, call).await;
    assert!(s.contains(&"NO_ROUTE:LOOP_BLOCKED".to_string()), "{s:?}");
    assert_eq!(
        s.iter().filter(|x| *x == "FORWARD:FORWARD_ALWAYS").count(),
        3,
        "{s:?}"
    );
    assert!(!rings(&e, call, &a).await && !rings(&e, call, &b).await && !rings(&e, call, &c).await);
    let visited = e
        .count(&format!(
            "SELECT cardinality(visited)::bigint FROM enterprise_call_contexts WHERE call_id = '{call}'"
        ))
        .await;
    assert!(visited <= 32);

    // A tighter hop budget stops the same chain earlier.
    let mut rules = default_rules();
    rules["maxRoutingHops"] = json!(2);
    set_policy(&e, rules).await;
    let call = dial(&e, &caller, "6402").await;
    assert_eq!(e.a.state(call).await.0, CallState::Failed);
    assert!(steps(&e, call)
        .await
        .contains(&"NO_ROUTE:HOP_BUDGET_EXHAUSTED".to_string()));
    set_policy(&e, default_rules()).await;

    // NO_ANSWER ping-pong between two principals: B rings after A, then the forward back
    // to A is blocked and the Call ends NO_ANSWER (never rings A twice).
    let p = desk(&e, "p").await;
    let q = desk(&e, "q").await;
    e.extension("6410", "ENTERPRISE_MEMBER", p.principal.id())
        .await;
    forward(
        &e,
        &p.principal,
        "NO_ANSWER",
        dest("ENTERPRISE_MEMBER", q.principal.id()),
    )
    .await;
    forward(
        &e,
        &q.principal,
        "NO_ANSWER",
        dest("ENTERPRISE_MEMBER", p.principal.id()),
    )
    .await;
    let call = dial(&e, &caller, "6410").await;
    assert!(rings(&e, call, &p).await);
    e.h().clock.advance(chrono::Duration::seconds(46));
    tick(&e).await;
    assert!(rings(&e, call, &q).await);
    assert!(!rings(&e, call, &p).await);
    e.h().clock.advance(chrono::Duration::seconds(46));
    tick(&e).await;
    assert_eq!(e.a.state(call).await.0, CallState::NoAnswer);
    let s = steps(&e, call).await;
    assert!(s.contains(&"NO_ROUTE:LOOP_BLOCKED".to_string()), "{s:?}");
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}' AND principal_id = '{}'",
            p.principal.id()
        ))
        .await,
        1
    );
}

// ---------------------------------------------------------------------------
// Public forwarding: every Part E gate, simulated Telephony / Privacy Relay
// ---------------------------------------------------------------------------

struct PublicWorld {
    e: EnterpriseWorld,
    privacy: Arc<FakePrivacyRelay>,
    telephony: Arc<FakeTelephony>,
    media: Arc<FakePublicMedia>,
}

fn routing_policy() -> RoutingPolicy {
    RoutingPolicy {
        allowed_route_classes: vec![
            RoutingClass::AppRtc,
            RoutingClass::AppToPstn,
            RoutingClass::EnterpriseInternal,
        ],
        public_calling_enabled: true,
        destinations: DestinationPolicy {
            allow: vec!["NG".into(), "GH".into()],
            deny: vec![],
            default: CountryDecision::Deny,
        },
        ..RoutingPolicy::default()
    }
}

fn spend(enabled: bool, daily: i64) -> SpendPolicy {
    SpendPolicy {
        currency: "NGN".into(),
        leg_allowance_minor: 5_000,
        max_leg_seconds: 3600,
        daily_limit_minor: daily,
        monthly_limit_minor: 500_000,
        max_concurrent_legs: 5,
        max_originations_per_minute: 20,
        max_per_party_per_hour: 10,
        max_per_principal_per_hour: None,
        max_per_service_account_per_hour: None,
        max_per_context_per_hour: None,
        enabled,
    }
}

async fn public_world() -> Option<PublicWorld> {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let media = Arc::new(FakePublicMedia::default());
    let a = AppWorld::with_public_network(privacy.clone(), telephony.clone(), Some(media.clone()))
        .await?;
    let e = EnterpriseWorld::around(a).await;
    let scope = e.a.w.scope();
    calling::routing::store::store_policy(
        e.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        scope.environment_id().as_uuid(),
        &routing_policy(),
        Utc::now(),
    )
    .await
    .unwrap();
    set_spend(&e, spend(true, 50_000)).await;
    // The operator lets this organization's administrators configure public forwarding.
    e.h()
        .state
        .enterprise
        .operator_set_capabilities(
            scope,
            vec![
                calling::enterprise::policy::AdminCapability::ManageRouting,
                calling::enterprise::policy::AdminCapability::ManagePublicForwarding,
            ],
        )
        .await
        .unwrap();
    Some(PublicWorld {
        e,
        privacy,
        telephony,
        media,
    })
}

async fn set_spend(e: &EnterpriseWorld, p: SpendPolicy) {
    let scope = e.a.w.scope();
    calling::telephony::spend::store_policy(
        e.h().conn(),
        calling::telephony::spend::SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: scope.environment_id().as_uuid(),
        },
        &p,
        Utc::now(),
    )
    .await
    .unwrap();
}

fn public_rules() -> Value {
    let mut rules = default_rules();
    rules["externalPublicCallingAllowed"] = json!(true);
    rules["publicForwardingAllowed"] = json!(true);
    rules["publicDestinationCountries"] = json!(["NG"]);
    rules["adminCapabilities"] = json!(["MANAGE_ROUTING", "MANAGE_PUBLIC_FORWARDING"]);
    rules
}

async fn public_contact(x: &PublicWorld, country: &str) -> Uuid {
    let reference = x.privacy.add_contact(country);
    let c =
        x.e.create(
            "/v1/enterprise/public-contacts",
            json!({ "label": format!("On call {}", tag()), "contactReference": reference,
                    "privacyRelaySessionId": Uuid::new_v4() }),
        )
        .await;
    conforms("PublicContact", &c);
    id_of(&c, "publicContactId")
}

async fn event(x: &PublicWorld, value: Value) -> &'static str {
    let event: TelephonyEvent = serde_json::from_value(value).unwrap();
    x.e.h()
        .state
        .app
        .apply_telephony_event(&event)
        .await
        .unwrap()
        .as_str()
}

async fn public_leg(x: &PublicWorld, call: Uuid) -> Option<(Uuid, Uuid)> {
    let rows = strings(
        x.e.h().conn(),
        &format!(
            "SELECT l.id::text || ',' || a.id::text FROM call_legs l JOIN call_attempts a ON a.leg_id = l.id \
              WHERE l.call_id = '{call}' AND l.leg_kind = 'PUBLIC_NETWORK'"
        ),
    )
    .await;
    rows.first().map(|r| {
        let (l, a) = r.split_once(',').unwrap();
        (l.parse().unwrap(), a.parse().unwrap())
    })
}

/// The happy path: a closed-hours route to a protected public contact passes every gate,
/// places ONE public leg on the SAME Call (never a number in Calling), and the answered
/// public party is bridged to the caller's media.
#[tokio::test]
async fn public_forwarding_passes_every_part_e_gate() {
    let Some(x) = public_world().await else {
        return;
    };
    let e = &x.e;
    let caller = desk(e, "caller").await;
    x.privacy.allow(caller.principal.id().as_uuid());
    let day = desk(e, "day").await;

    // Registering a public destination requires a permitting policy.
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/public-contacts",
            Some(
                json!({ "label": "On call", "contactReference": Uuid::new_v4(),
                         "privacyRelaySessionId": Uuid::new_v4() }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    set_policy(e, public_rules()).await;
    let contact = public_contact(&x, "NG").await;

    // Always closed (no windows) -> the public contact.
    let (s, h) = hours(
        e,
        json!({ "name": "After hours", "timezone": "Africa/Lagos",
                "openDestination": dest("ENTERPRISE_MEMBER", day.principal.id()),
                "closedDestination": dest("PUBLIC_CONTACT", contact),
                "weeklyWindows": [] }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{h}");
    e.extension("6500", "SCHEDULE", id_of(&h, "businessHoursId"))
        .await;

    let call = dial(e, &caller, "6500").await;
    let s = steps(e, call).await;
    assert!(s.contains(&"SCHEDULE:CLOSED".to_string()), "{s:?}");
    assert!(
        s.contains(&"FORWARD_PUBLIC:ORIGINATING".to_string()),
        "{s:?}"
    );
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 1);
    let cmd = &originates[0].1;
    assert_eq!(cmd["call"]["callId"], json!(call));
    assert_eq!(cmd["call"]["routeClass"], "APP_TO_PSTN");
    assert_eq!(cmd["destination"]["country"], "NG");
    assert!(cmd.get("callerId").is_none() && cmd.get("fromNumber").is_none());
    let (leg, attempt) = public_leg(&x, call).await.expect("one public leg");
    let t = x
        .telephony
        .telephony_leg_of(leg)
        .expect("telephony accepted the originate");
    assert_eq!(
        event(&x, leg_event("LEG_RINGING", call, leg, t, Some(attempt), 1)).await,
        "APPLIED"
    );
    assert_eq!(e.a.state(call).await.0, CallState::Ringing);
    assert_eq!(
        event(
            &x,
            leg_event("LEG_ANSWERED", call, leg, t, Some(attempt), 2)
        )
        .await,
        "APPLIED"
    );
    assert_eq!(e.a.state(call).await.0, CallState::Answered);
    assert_eq!(x.telephony.bridges().len(), 1);
    assert_eq!(x.media.attached.lock().unwrap().len(), 1);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' AND outcome = 'FORWARDED_PUBLIC'"
        ))
        .await,
        1
    );
    // The bridge attaches, the leg connects, the caller's media is up: CONNECTED.
    let identity = x.media.attached.lock().unwrap()[0].1.clone();
    let p = calling::media::provider::ProviderParticipant {
        identity: identity.clone(),
        generation: None,
        permissions: calling::media::MediaPermissions::NONE,
        active: true,
        published_audio: true,
        published_video: false,
        joined_at: Some(Utc::now()),
    };
    e.a.media().inject(&AppWorld::room(call), p.clone());
    let (auth, body) =
        e.a.media()
            .signed_event("participant_joined", &AppWorld::room(call), Some(&p));
    let r = e.a.callback(&auth, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        event(
            &x,
            leg_event("LEG_CONNECTED", call, leg, t, Some(attempt), 3)
        )
        .await,
        "APPLIED"
    );
    let bridge: Uuid = strings(
        e.h().conn(),
        &format!(
            "SELECT telephony_bridge_id::text FROM call_public_bridges WHERE call_id = '{call}'"
        ),
    )
    .await[0]
        .parse()
        .unwrap();
    let mut est = leg_event("BRIDGE_ESTABLISHED", call, leg, t, None, 1);
    est.as_object_mut().unwrap().remove("callLegId");
    est["bridgeId"] = json!(bridge);
    assert_eq!(event(&x, est).await, "APPLIED");
    let session =
        e.a.session_on(call, caller.phone.endpoint)
            .await
            .expect("caller session");
    e.a.connect(call, session).await;
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    // The forwarded Call is a protected communication: Part D enforcement reaches it.
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM calls WHERE id = '{call}' AND privacy_relay_ref IS NOT NULL \
               AND communication_context_ref IS NOT NULL"
        ))
        .await,
        1
    );
    // One canonical Call; the only contact data is the opaque reference.
    assert_eq!(
        e.count(&format!("SELECT count(*) FROM calls WHERE id = '{call}'"))
            .await,
        1
    );
    let dump = common::dump_database(e.h().conn()).await;
    assert!(!dump.contains("+234"), "a number reached Calling");
    // A forwarded call is real-carrier-pending: nothing here marks PSTN certified.
}

/// Every refused gate fails the Call safely with bounded evidence and NO public leg.
#[tokio::test]
async fn public_forwarding_cannot_bypass_part_e() {
    let Some(x) = public_world().await else {
        return;
    };
    let e = &x.e;
    let caller = desk(e, "caller").await;
    x.privacy.allow(caller.principal.id().as_uuid());
    let stranger = desk(e, "stranger").await;
    set_policy(e, public_rules()).await;
    let ng = public_contact(&x, "NG").await;
    let gh = public_contact(&x, "GH").await;
    let a = desk(e, "a").await;
    let g = desk(e, "g").await;
    e.extension("6601", "ENTERPRISE_MEMBER", a.principal.id())
        .await;
    e.extension("6602", "ENTERPRISE_MEMBER", g.principal.id())
        .await;
    forward(e, &a.principal, "ALWAYS", dest("PUBLIC_CONTACT", ng)).await;
    forward(e, &g.principal, "ALWAYS", dest("PUBLIC_CONTACT", gh)).await;

    async fn refused(x: &PublicWorld, from: &Desk, number: &str, reason: &str) {
        let before = x.telephony.originates().len();
        let call = dial(&x.e, from, number).await;
        assert_eq!(x.e.a.state(call).await.0, CallState::Failed, "{reason}");
        let s = steps(&x.e, call).await;
        assert!(s.contains(&format!("NO_ROUTE:{reason}")), "{reason}: {s:?}");
        assert_eq!(x.telephony.originates().len(), before, "{reason}");
        assert!(public_leg(x, call).await.is_none(), "{reason}");
    }

    // Privacy Relay's current authority does not cover this caller.
    refused(&x, &stranger, "6601", "PRIVACY_AUTHORITY_DENIED").await;
    // The contact's country is not one the enterprise allows.
    refused(&x, &caller, "6602", "DESTINATION_COUNTRY_NOT_PERMITTED").await;
    // No healthy Telephony route.
    x.telephony.set_routes(Evidence::Known(Vec::new()));
    refused(&x, &caller, "6601", "ROUTE_UNAVAILABLE").await;
    x.telephony.set_routes(common::telephony::healthy_routes());
    // Spend is switched off for the environment (no headroom).
    set_spend(e, spend(false, 50_000)).await;
    let call = dial(e, &caller, "6601").await;
    assert_eq!(e.a.state(call).await.0, CallState::Failed);
    let s = steps(e, call).await;
    assert!(s.iter().any(|x| x.starts_with("NO_ROUTE:SPEND")), "{s:?}");
    set_spend(e, spend(true, 50_000)).await;
    // The environment may not produce live telecom effects.
    e.h()
        .platform
        .set_live_telecom_allowed(e.a.w.consumer.env(e.a.w.kind).id, false)
        .await
        .unwrap();
    let call = dial(e, &caller, "6601").await;
    assert_eq!(e.a.state(call).await.0, CallState::Failed);
    assert!(steps(e, call)
        .await
        .iter()
        .any(|x| x.starts_with("NO_ROUTE:LIVE_TELECOM")));
    e.h()
        .platform
        .set_live_telecom_allowed(e.a.w.consumer.env(e.a.w.kind).id, true)
        .await
        .unwrap();
    // The enterprise policy withdraws public forwarding: refused before any gate.
    let mut rules = public_rules();
    rules["publicForwardingAllowed"] = json!(false);
    set_policy(e, rules).await;
    refused(&x, &caller, "6601", "PUBLIC_FORWARDING_NOT_PERMITTED").await;
    set_policy(e, public_rules()).await;
    // A disabled public contact never routes.
    let r = e
        .admin(
            Method::DELETE,
            &format!("/v1/enterprise/public-contacts/{ng}?expectedVersion=1"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    refused(&x, &caller, "6601", "DESTINATION_DISABLED").await;
    // Nothing was ever originated in this test.
    assert_eq!(x.telephony.originates().len(), 0);
}

/// §73: an IVR branch routes through business hours (ROUTE_SCHEDULE), and a closed route
/// may be another IVR flow — the schedule logic lives in Calling, never in the edge.
#[tokio::test]
async fn ivr_branches_through_business_hours() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = desk(&e, "caller").await;
    let day = desk(&e, "day").await;
    // The after-hours flow: a straight goodbye.
    let night_flow = e
        .create(
            "/v1/enterprise/ivr-flows",
            json!({ "name": format!("Night {}", tag()) }),
        )
        .await;
    let night_flow = id_of(&night_flow, "ivrFlowId");
    let r = e
        .admin(
            Method::POST,
            &format!("/v1/enterprise/ivr-flows/{night_flow}/versions"),
            Some(json!({ "graph": { "startNode": "closed", "nodes": [
                { "id": "closed", "kind": "HANGUP", "prompt": { "builtin": "CLOSED" } }] } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ivr-flows/{night_flow}/activation"),
            Some(json!({ "expectedVersion": 2, "versionNumber": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (s, h) = hours(
        &e,
        json!({ "name": "Desk hours", "timezone": "Africa/Lagos",
                "openDestination": dest("ENTERPRISE_MEMBER", day.principal.id()),
                "closedDestination": dest("IVR", night_flow),
                "weeklyWindows": weekdays("09:00", "17:00") }),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{h}");
    let h = id_of(&h, "businessHoursId");
    let main = e
        .create(
            "/v1/enterprise/ivr-flows",
            json!({ "name": format!("Main {}", tag()) }),
        )
        .await;
    let main = id_of(&main, "ivrFlowId");
    let r = e
        .admin(
            Method::POST,
            &format!("/v1/enterprise/ivr-flows/{main}/versions"),
            Some(json!({ "graph": { "startNode": "menu", "nodes": [
                { "id": "menu", "kind": "MENU", "prompt": { "builtin": "MENU" }, "inputTimeoutSeconds": 5,
                  "maxRetries": 1, "options": [{ "digit": "1", "next": "desk" }], "onInvalid": "bye",
                  "onTimeout": "bye" },
                { "id": "desk", "kind": "ROUTE_SCHEDULE", "scheduleId": h },
                { "id": "bye", "kind": "HANGUP" }] } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/ivr-flows/{main}/activation"),
            Some(json!({ "expectedVersion": 2, "versionNumber": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("6700", "IVR", main).await;
    let press = |call: Uuid| {
        let e = &e;
        let phone = caller.phone.endpoint;
        async move {
            let r = e
                .send(
                    e.caller_token(),
                    Method::POST,
                    &format!("/v1/calls/{call}/ivr/input"),
                    Some(json!({ "endpointId": phone, "digit": "1" })),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
            assert_eq!(r.json["accepted"], true);
        }
    };

    // Open: key 1 -> the schedule -> the day desk rings (same Call).
    e.h().clock.set(at("2031-03-05T09:00:00Z"));
    let call = dial(&e, &caller, "6700").await;
    press(call).await;
    assert!(rings(&e, call, &day).await);
    assert!(steps(&e, call).await.contains(&"SCHEDULE:OPEN".to_string()));
    // Closed: key 1 -> the schedule -> the night IVR (CLOSED prompt, then hangup).
    e.h().clock.set(at("2031-03-08T09:00:00Z"));
    let call = dial(&e, &caller, "6700").await;
    press(call).await;
    assert!(!rings(&e, call, &day).await);
    let s = steps(&e, call).await;
    assert!(s.contains(&"SCHEDULE:CLOSED".to_string()), "{s:?}");
    e.h().clock.advance(chrono::Duration::seconds(3));
    tick(&e).await;
    assert_eq!(e.a.state(call).await.0, CallState::Declined);
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM enterprise_call_contexts WHERE call_id = '{call}' AND outcome = 'COMPLETED_IN_IVR'"
        ))
        .await,
        1
    );
}

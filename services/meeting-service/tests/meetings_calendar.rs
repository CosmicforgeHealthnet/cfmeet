//! Blueprint v1.0 Section 10 — Meeting Spaces, scheduling, the first-party calendar and
//! organization meetings (MTG-005/007/008/009/010/011/012/014/018, ORG-011, SEC-014).

mod common;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use common::{contract::*, *};
use futures_util::future::join_all;
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

fn ts(v: &Value) -> DateTime<Utc> {
    v.as_str()
        .unwrap_or_else(|| panic!("timestamp expected: {v}"))
        .parse()
        .unwrap()
}

fn rfc(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(h, min, 0)
        .unwrap()
}

async fn agenda(app: &TestApp, user: &User, query: &str) -> (StatusCode, Value) {
    app.get(
        format!("/api/v2/me/conferences{query}"),
        Auth::Bearer(&user.token),
    )
    .await
}

async fn agenda_ok(app: &TestApp, user: &User, query: &str) -> Value {
    let (s, v) = agenda(app, user, query).await;
    assert_eq!(s, StatusCode::OK, "{query}: {v}");
    assert_conforms(&openapi(), "AgendaResponse", &v);
    v
}

fn items(v: &Value) -> Vec<Value> {
    v["items"].as_array().unwrap().clone()
}

async fn schedule(
    app: &TestApp,
    owner: &User,
    sid: Uuid,
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
) -> Value {
    let mut body = json!({ "scheduledStartAt": start });
    if let Some(e) = end {
        body["scheduledEndAt"] = json!(e);
    }
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            body,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    c
}

async fn set_rule(
    app: &TestApp,
    owner: &User,
    sid: Uuid,
    rrule: &str,
    start_local: NaiveDateTime,
    minutes: i32,
) -> Value {
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/meeting-spaces/{sid}/recurrence"),
            Auth::Bearer(&owner.token),
            Some(json!({ "rrule": rrule, "startLocal": start_local, "durationMinutes": minutes })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

async fn list_conferences(app: &TestApp, owner: &User, sid: Uuid) -> Vec<Value> {
    let (s, v) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    items(&v)
}

async fn ics(
    app: &TestApp,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, String, Value) {
    let bearer = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = bearer
        .as_deref()
        .map(|b| vec![("authorization", b)])
        .unwrap_or_default();
    let (s, h, v) = raw(app, Method::GET, uri, &headers, None).await;
    let body = v["raw"].as_str().unwrap_or_default().to_string();
    (s, h, body, v)
}

/// Parse-level RFC 5545 checks; returns the unfolded content lines.
fn assert_valid_ics(body: &str) -> Vec<String> {
    assert!(body.ends_with("\r\n"), "CRLF terminated");
    for line in body.split("\r\n") {
        assert!(line.len() <= 75, "unfolded line over 75 octets: {line}");
    }
    let lines: Vec<String> = body
        .replace("\r\n ", "")
        .split("\r\n")
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(lines.first().map(String::as_str), Some("BEGIN:VCALENDAR"));
    assert_eq!(lines.last().map(String::as_str), Some("END:VCALENDAR"));
    assert!(lines.iter().any(|l| l == "VERSION:2.0"));
    assert!(lines.iter().any(|l| l.starts_with("PRODID:")));
    let begins = lines.iter().filter(|l| *l == "BEGIN:VEVENT").count();
    assert_eq!(begins, lines.iter().filter(|l| *l == "END:VEVENT").count());
    assert!(begins >= 1);
    // Every VEVENT carries UID, DTSTAMP, DTSTART, DTEND, SEQUENCE and STATUS.
    let mut in_event = false;
    let mut seen: Vec<&str> = vec![];
    for l in &lines {
        match l.as_str() {
            "BEGIN:VEVENT" => {
                in_event = true;
                seen.clear();
            }
            "END:VEVENT" => {
                for p in ["UID", "DTSTAMP", "DTSTART", "DTEND", "SEQUENCE", "STATUS"] {
                    assert!(seen.contains(&p), "VEVENT without {p}: {body}");
                }
                in_event = false;
            }
            l if in_event => {
                let name = l.split([':', ';']).next().unwrap();
                seen.push(match name {
                    "UID" => "UID",
                    "DTSTAMP" => "DTSTAMP",
                    "DTSTART" => "DTSTART",
                    "DTEND" => "DTEND",
                    "SEQUENCE" => "SEQUENCE",
                    "STATUS" => "STATUS",
                    _ => "other",
                });
            }
            _ => {}
        }
    }
    // Every TZID used is defined by a VTIMEZONE.
    for l in &lines {
        if let Some(i) = l.find(";TZID=") {
            let zone = l[i + 6..].split(':').next().unwrap();
            assert!(
                lines.iter().any(|x| x == &format!("TZID:{zone}")),
                "TZID {zone} without VTIMEZONE"
            );
        }
    }
    lines
}

fn prop<'a>(lines: &'a [String], name: &str) -> Vec<&'a str> {
    lines
        .iter()
        .filter(|l| l.starts_with(name))
        .map(String::as_str)
        .collect()
}

// ---------------------------------------------------------------------------
// Agenda (MTG-005)
// ---------------------------------------------------------------------------

/// The agenda spans every relation: owner, member, invitee, registrant, participant —
/// one call, role-filtered, never another user's unrelated meetings.
#[tokio::test]
async fn cal01_agenda_across_spaces_and_relations() {
    let app = app!();
    let alice = app.user("alice").await;
    let bob = app.user("bob").await;
    let carol = app.user("carol").await;
    let soon = Utc::now() + Duration::days(2);

    // Bob owns S1.
    let s1 = uuid(
        &app.create_space(&bob, json!({ "title": "Bob's own" }))
            .await["id"],
    );
    let c1 = schedule(&app, &bob, s1, soon, Some(soon + Duration::hours(1))).await;
    // Bob is a member of Alice's S2.
    let s2 = uuid(
        &app.create_space(&alice, json!({ "title": "Team sync" }))
            .await["id"],
    );
    app.add_member(s2, &alice, &bob, backend::domain::Role::Participant)
        .await;
    let c2 = schedule(&app, &alice, s2, soon + Duration::hours(2), None).await;
    // Bob holds an email-bound invitation to Alice's S3 (conference-scoped).
    let s3 = uuid(
        &app.create_space(
            &alice,
            json!({ "title": "Interview", "accessPolicy": "INVITE_ONLY" }),
        )
        .await["id"],
    );
    let c3 = schedule(&app, &alice, s3, soon + Duration::hours(3), None).await;
    let (s, v) = app
        .post(
            format!("/api/v2/meeting-spaces/{s3}/invitations"),
            Auth::Bearer(&alice.token),
            json!({ "inviteeEmail": bob.email, "conferenceId": c3["id"] }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // Bob registered for Alice's webinar S4.
    let s4 = uuid(&app.create_space(&alice, json!({ "title": "Launch webinar", "accessPolicy": "PUBLIC", "meetingType": "WEBINAR" })).await["id"]);
    let c4 = schedule(&app, &alice, s4, soon + Duration::hours(4), None).await;
    let (s, v) = app
        .post(
            format!("/api/v2/meeting-spaces/{s4}/registrations"),
            Auth::Bearer(&bob.token),
            json!({ "displayName": "Bob" }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    // Bob took part in Alice's live public conference S5 (scheduled, started early).
    let s5 = uuid(
        &app.create_space(
            &alice,
            json!({ "title": "Open house", "accessPolicy": "PUBLIC" }),
        )
        .await["id"],
    );
    let c5 = schedule(&app, &alice, s5, Utc::now() + Duration::minutes(5), None).await;
    let c5 = uuid(&c5["id"]);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{c5}/start"),
            Auth::Bearer(&alice.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app.join(c5, Auth::Bearer(&bob.token), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    // Carol's meeting is unrelated to Bob.
    let s6 = uuid(
        &app.create_space(&carol, json!({ "title": "Private" }))
            .await["id"],
    );
    let c6 = schedule(&app, &carol, s6, soon, None).await;

    let from = rfc(Utc::now() - Duration::hours(1));
    let v = agenda_ok(&app, &bob, &format!("?from={from}")).await;
    let list = items(&v);
    let by_conf = |c: &Value| list.iter().find(|i| i["conferenceId"] == c["id"]).cloned();
    let rel = |c: &Value| -> Vec<String> {
        by_conf(c)
            .map(|i| {
                i["relations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r.as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(rel(&c1), ["OWNER"]);
    assert_eq!(by_conf(&c1).unwrap()["view"], "FULL");
    assert_eq!(by_conf(&c1).unwrap()["viewerRole"], "OWNER");
    assert_eq!(rel(&c2), ["MEMBER"]);
    assert_eq!(by_conf(&c2).unwrap()["view"], "SUMMARY");
    assert_eq!(by_conf(&c2).unwrap()["meetingSpace"]["title"], "Team sync");
    assert!(by_conf(&c2).unwrap().get("startedAt").is_none());
    assert_eq!(rel(&c3), ["INVITEE"]);
    assert_eq!(rel(&c4), ["REGISTRANT"]);
    assert_eq!(rel(&json!({ "id": c5 })), ["PARTICIPANT"]);
    assert!(by_conf(&c6).is_none(), "unrelated meeting never listed");
    // Ordered by start.
    let starts: Vec<DateTime<Utc>> = list.iter().map(|i| ts(&i["scheduledStartAt"])).collect();
    assert!(starts.windows(2).all(|w| w[0] <= w[1]));
    // Never a secret.
    assert!(!v.to_string().contains("passcode") && !v.to_string().contains("token"));

    // Role filter.
    let v = agenda_ok(&app, &bob, &format!("?from={from}&role=INVITEE,REGISTRANT")).await;
    let ids: Vec<Value> = items(&v)
        .iter()
        .map(|i| i["conferenceId"].clone())
        .collect();
    assert_eq!(ids, [c3["id"].clone(), c4["id"].clone()]);
    // Status filter.
    let v = agenda_ok(&app, &bob, &format!("?from={from}&status=LIVE")).await;
    assert_eq!(items(&v).len(), 1);
    assert_eq!(items(&v)[0]["conferenceId"], json!(c5));
    // Carol sees only her own meeting.
    let v = agenda_ok(&app, &carol, &format!("?from={from}")).await;
    assert_eq!(items(&v).len(), 1);
    assert_eq!(items(&v)[0]["conferenceId"], c6["id"]);
}

/// Validation and authentication errors use the standard envelope.
#[tokio::test]
async fn cal02_agenda_rejects_bad_queries() {
    let app = app!();
    let u = app.user("u").await;
    let (s, v) = app.get("/api/v2/me/conferences", Auth::None).await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_REQUIRED")
    );
    let now = Utc::now();
    for q in [
        "?limit=0".to_string(),
        "?limit=201".to_string(),
        "?status=NOPE".to_string(),
        "?role=BOSS".to_string(),
        "?cursor=not-a-cursor".to_string(),
        format!("?from={}&to={}", rfc(now), rfc(now - Duration::hours(1))),
        format!("?from={}&to={}", rfc(now), rfc(now + Duration::days(367))),
        "?unknown=1".to_string(),
    ] {
        let (s, v) = agenda(&app, &u, &q).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{q}: {v}"
        );
    }
}

async fn create_org(app: &TestApp, admin: &User, slug: &str, owner: &User) -> Uuid {
    let (s, v) = app
        .post(
            "/api/v2/admin/organizations",
            Auth::Bearer(&admin.token),
            json!({ "name": slug, "slug": slug, "ownerUserId": owner.id }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    uuid(&v["id"])
}

/// ORG-011 / MTG-016: the organization calendar is an agenda filter authorized per request
/// from verified membership; unknown and unauthorized organizations are indistinguishable.
#[tokio::test]
async fn cal03_organization_filter_member_vs_outsider() {
    let app = app!();
    let admin = app.admin("platform").await;
    let owner = app.user("org-owner").await;
    let insider = app.user("insider").await;
    let outsider = app.user("outsider").await;
    let acme = create_org(&app, &admin, "acme-cal", &owner).await;
    let other = create_org(&app, &admin, "globex-cal", &outsider).await;
    let (s, v) = app
        .call(
            Method::PUT,
            &format!("/api/v2/organizations/{acme}/members/{}", insider.id),
            Auth::Bearer(&owner.token),
            Some(json!({ "role": "MEMBER", "status": "ACTIVE" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let soon = Utc::now() + Duration::days(1);
    let all_hands = uuid(&app.create_space(&owner, json!({ "title": "All hands", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": acme })).await["id"]);
    let c_all = schedule(&app, &owner, all_hands, soon, None).await;
    let secret = uuid(
        &app.create_space(
            &owner,
            json!({ "title": "Board", "accessPolicy": "INVITE_ONLY", "organizationId": acme }),
        )
        .await["id"],
    );
    let c_secret = schedule(&app, &owner, secret, soon, None).await;
    let clinic = uuid(&app.create_space(&owner, json!({ "title": "Clinic", "accessPolicy": "ORGANIZATION_ONLY", "organizationId": acme, "securityProfile": "HEALTHCARE" })).await["id"]);
    let c_clinic = schedule(&app, &owner, clinic, soon, None).await;

    let v = agenda_ok(&app, &insider, &format!("?organizationId={acme}")).await;
    let list = items(&v);
    assert_eq!(list.len(), 1, "{v}");
    assert_eq!(list[0]["conferenceId"], c_all["id"]);
    assert_eq!(list[0]["relations"], json!(["ORGANIZATION"]));
    assert_eq!(list[0]["view"], "SUMMARY");
    assert_eq!(list[0]["meetingSpace"]["title"], "All hands");
    assert!(list[0]["meetingSpace"].get("organizationId").is_none());
    let ids: Vec<&Value> = list.iter().map(|i| &i["conferenceId"]).collect();
    assert!(!ids.contains(&&c_secret["id"]) && !ids.contains(&&c_clinic["id"]));
    // Without the filter the personal agenda does not list organization-only visibility.
    let v = agenda_ok(&app, &insider, "").await;
    assert!(items(&v).is_empty(), "{v}");
    // The owner's organization agenda lists all three (OWNER relation), FULL view.
    let v = agenda_ok(&app, &owner, &format!("?organizationId={acme}")).await;
    assert_eq!(items(&v).len(), 3);
    assert!(items(&v).iter().all(|i| i["view"] == "FULL"));

    // Outsider, unknown organization and a foreign organization: one identical denial.
    let (s1, d1) = agenda(&app, &outsider, &format!("?organizationId={acme}")).await;
    let (s2, d2) = agenda(
        &app,
        &outsider,
        &format!("?organizationId={}", Uuid::new_v4()),
    )
    .await;
    let (s3, d3) = agenda(&app, &insider, &format!("?organizationId={other}")).await;
    for (s, d) in [(s1, &d1), (s2, &d2), (s3, &d3)] {
        assert_eq!(
            (s, code(d)),
            (StatusCode::FORBIDDEN, "PERMISSION_DENIED"),
            "{d}"
        );
    }
    let msg = |d: &Value| d["error"]["message"].clone();
    assert_eq!(msg(&d1), msg(&d2));
    assert_eq!(msg(&d1), msg(&d3));
}

/// Cursor pagination is stable: no duplicates, no gaps, new earlier rows never shift
/// later pages.
#[tokio::test]
async fn cal04_agenda_pagination_is_stable() {
    let app = app!();
    let owner = app.user("owner").await;
    let a = uuid(&app.create_space(&owner, json!({ "title": "A" })).await["id"]);
    let b = uuid(&app.create_space(&owner, json!({ "title": "B" })).await["id"]);
    let base = (Utc::now() + Duration::days(3))
        .date_naive()
        .and_hms_opt(9, 0, 0)
        .unwrap()
        .and_utc();
    for h in 0..4 {
        schedule(&app, &owner, a, base + Duration::hours(h), None).await;
        schedule(&app, &owner, b, base + Duration::hours(h), None).await;
    }
    let full = agenda_ok(&app, &owner, "?limit=200").await;
    let expected: Vec<Value> = items(&full)
        .iter()
        .map(|i| i["conferenceId"].clone())
        .collect();
    assert_eq!(expected.len(), 8);
    assert!(full["nextCursor"].is_null());

    let mut seen = vec![];
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let q = match &cursor {
            Some(c) => format!("?limit=3&cursor={c}"),
            None => "?limit=3".into(),
        };
        let page = agenda_ok(&app, &owner, &q).await;
        seen.extend(items(&page).iter().map(|i| i["conferenceId"].clone()));
        pages += 1;
        if pages == 1 {
            // Rows created before the cursor position never shift later pages.
            schedule(&app, &owner, a, Utc::now() + Duration::hours(1), None).await;
        }
        match page["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, expected, "same order, no duplicates, no gaps");
}

// ---------------------------------------------------------------------------
// Recurrence beyond the horizon (MTG-008) and series edits (MTG-014)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cal05_recurrence_beyond_the_horizon_has_stable_identity() {
    let app = app!();
    let owner = app.user("owner").await;
    let sid = uuid(
        &app.create_space(
            &owner,
            json!({ "title": "Weekly", "timezone": "Europe/Rome" }),
        )
        .await["id"],
    );
    let first = (Utc::now() + Duration::days(1))
        .date_naive()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    set_rule(&app, &owner, sid, "FREQ=WEEKLY;COUNT=16", first, 45).await;
    let to = rfc(Utc::now() + Duration::days(120));
    let v = agenda_ok(&app, &owner, &format!("?to={to}&limit=200")).await;
    let list = items(&v);
    assert_eq!(list.len(), 16, "all 16 weekly occurrences within 120 days");
    let materialized = list.iter().filter(|i| i["materialized"] == true).count();
    assert!(
        (4..=5).contains(&materialized),
        "30-day horizon: {materialized}"
    );
    for i in list.iter().filter(|i| i["materialized"] == false) {
        assert!(i["conferenceId"].is_null());
        assert_eq!(i["kind"], "RECURRING");
        assert_eq!(i["status"], "CREATED");
        let start = ts(&i["scheduledStartAt"]);
        assert_eq!(ts(&i["scheduledEndAt"]) - start, Duration::minutes(45));
        // 10:00 Rome wall-clock on every date, across the DST change.
        assert_eq!(
            start
                .with_timezone(&chrono_tz::Europe::Rome)
                .format("%H:%M")
                .to_string(),
            "10:00"
        );
    }
    let keys: Vec<Value> = list.iter().map(|i| i["occurrenceKey"].clone()).collect();

    // A far-future occurrence can take exceptions before materialization.
    let far = list
        .iter()
        .filter(|i| i["materialized"] == false)
        .collect::<Vec<_>>();
    let cancelled_start = ts(&far[2]["scheduledStartAt"]);
    let moved_original = ts(&far[3]["scheduledStartAt"]);
    let moved_to = moved_original + Duration::hours(3);
    for body in [
        json!({ "originalStartAt": cancelled_start, "action": "CANCEL" }),
        json!({ "originalStartAt": moved_original, "action": "RESCHEDULE", "scheduledStartAt": moved_to }),
    ] {
        let (s, v) = app
            .call(
                Method::PUT,
                &format!("/api/v2/meeting-spaces/{sid}/recurrence/exceptions"),
                Auth::Bearer(&owner.token),
                Some(body),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }
    let v = agenda_ok(&app, &owner, &format!("?to={to}&limit=200")).await;
    let list = items(&v);
    let cancelled = list
        .iter()
        .find(|i| ts(&i["scheduledStartAt"]) == cancelled_start)
        .unwrap();
    assert_eq!(cancelled["status"], "CANCELLED");
    let moved = list
        .iter()
        .find(|i| ts(&i["scheduledStartAt"]) == moved_to)
        .unwrap();
    assert_eq!(moved["isException"], true);
    assert_eq!(ts(&moved["originalStartAt"]), moved_original);
    let v = agenda_ok(&app, &owner, &format!("?to={to}&limit=200&status=CREATED")).await;
    assert_eq!(items(&v).len(), 15);

    // Materializing further keeps every occurrence key: no duplicates, now materialized.
    app.state
        .meeting
        .recurrence
        .materialize_due(Utc::now() + Duration::days(30), 10)
        .await
        .unwrap();
    let v = agenda_ok(&app, &owner, &format!("?to={to}&limit=200")).await;
    let after: Vec<Value> = items(&v)
        .iter()
        .map(|i| i["occurrenceKey"].clone())
        .collect();
    let mut sorted_before = keys.clone();
    sorted_before.sort_by_key(|k| k.to_string());
    let mut sorted_after = after.clone();
    sorted_after.sort_by_key(|k| k.to_string());
    assert_eq!(
        sorted_before, sorted_after,
        "identity survives materialization"
    );
    assert!(
        items(&v)
            .iter()
            .filter(|i| i["materialized"] == true)
            .count()
            > materialized
    );
    let moved = items(&v)
        .into_iter()
        .find(|i| ts(&i["scheduledStartAt"]) == moved_to)
        .unwrap();
    assert_eq!(
        moved["materialized"], true,
        "exception applied on materialization"
    );
}

/// MTG-014: a series edit keeps the rule, its exceptions and unchanged occurrences;
/// ending the series retires rescheduled occurrences too.
#[tokio::test]
async fn cal06_series_edit_keeps_exceptions_and_end_retires_rescheduled() {
    let app = app!();
    let owner = app.user("owner").await;
    let sid = uuid(
        &app.create_space(&owner, json!({ "title": "Daily", "timezone": "UTC" }))
            .await["id"],
    );
    let first = (Utc::now() + Duration::days(1))
        .date_naive()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    let rule = set_rule(&app, &owner, sid, "FREQ=DAILY;COUNT=6", first, 30).await;
    let occ = list_conferences(&app, &owner, sid).await;
    assert_eq!(occ.len(), 6);
    let day1 = uuid(&occ[1]["id"]);
    let day2 = uuid(&occ[2]["id"]);
    let moved_to = ts(&occ[2]["scheduledStartAt"]) + Duration::hours(2);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{day1}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{day2}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": moved_to }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        ts(&v["conference"]["scheduledEndAt"]) - moved_to,
        Duration::minutes(30),
        "duration kept"
    );
    let (_, _, ics_before, _) = ics(
        &app,
        &format!("/api/v2/meeting-spaces/{sid}/calendar.ics"),
        Some(&owner.token),
    )
    .await;
    let seq_before: i32 = prop(&assert_valid_ics(&ics_before), "SEQUENCE:")[0][9..]
        .parse()
        .unwrap();

    // Edit: same days, new duration.
    let edited = set_rule(&app, &owner, sid, "FREQ=DAILY;COUNT=6", first, 45).await;
    assert_eq!(edited["id"], rule["id"], "edited in place, not replaced");
    assert!(edited["version"].as_i64() > rule["version"].as_i64());
    let occ = list_conferences(&app, &owner, sid).await;
    assert_eq!(occ.len(), 6, "no duplicates after the edit");
    for c in &occ {
        let id = uuid(&c["id"]);
        if id == day1 {
            assert_eq!(c["status"], "CANCELLED");
        } else if id == day2 {
            assert_eq!(
                ts(&c["scheduledStartAt"]),
                moved_to,
                "rescheduled occurrence kept"
            );
        } else {
            assert_eq!(
                ts(&c["scheduledEndAt"]) - ts(&c["scheduledStartAt"]),
                Duration::minutes(45)
            );
        }
    }
    let (_, ex) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/recurrence/exceptions"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(ex.as_array().unwrap().len(), 2, "exceptions kept: {ex}");
    let (_, _, ics_after, _) = ics(
        &app,
        &format!("/api/v2/meeting-spaces/{sid}/calendar.ics"),
        Some(&owner.token),
    )
    .await;
    let seq_after: i32 = prop(&assert_valid_ics(&ics_after), "SEQUENCE:")[0][9..]
        .parse()
        .unwrap();
    assert!(seq_after > seq_before, "SEQUENCE increases on change");

    // Edit: every other day. Day 1 no longer exists (its exception is dropped), day 2 stays.
    set_rule(
        &app,
        &owner,
        sid,
        "FREQ=DAILY;INTERVAL=2;COUNT=3",
        first,
        45,
    )
    .await;
    let (_, ex) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}/recurrence/exceptions"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(ex.as_array().unwrap().len(), 1, "{ex}");
    let occ = list_conferences(&app, &owner, sid).await;
    let created: Vec<&Value> = occ.iter().filter(|c| c["status"] == "CREATED").collect();
    assert_eq!(created.len(), 3, "days 0, 2 (moved) and 4: {occ:?}");
    let keys: std::collections::HashSet<_> =
        occ.iter().map(|c| c["occurrenceKey"].clone()).collect();
    assert_eq!(keys.len(), occ.len(), "unique occurrence keys");

    // End the series: the rescheduled occurrence does not survive.
    let (s, _) = app
        .delete(
            format!("/api/v2/meeting-spaces/{sid}/recurrence"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let left = app
        .scalar(&format!("SELECT count(*) FROM conference_sessions WHERE meeting_space_id='{sid}' AND status='CREATED'"))
        .await;
    assert_eq!(left, 0, "no unstarted occurrence outlives the series");
}

// ---------------------------------------------------------------------------
// Temporal matrix (MTG-012)
// ---------------------------------------------------------------------------

/// America/New_York spring-forward (nonexistent 02:30), Europe/Rome fall-back (repeated
/// 02:30), Africa/Lagos midnight boundaries, a 24-hour conference.
#[tokio::test]
async fn cal07_temporal_matrix_across_zones() {
    let app = app!();
    let owner = app.user("owner").await;
    let ny = uuid(
        &app.create_space(
            &owner,
            json!({ "title": "NY", "timezone": "America/New_York" }),
        )
        .await["id"],
    );
    let rome = uuid(
        &app.create_space(
            &owner,
            json!({ "title": "Rome", "timezone": "Europe/Rome" }),
        )
        .await["id"],
    );
    let lagos = uuid(
        &app.create_space(
            &owner,
            json!({ "title": "Lagos", "timezone": "Africa/Lagos" }),
        )
        .await["id"],
    );
    let starts = |v: &Value, sid: Uuid| -> Vec<DateTime<Utc>> {
        items(v)
            .iter()
            .filter(|i| uuid(&i["meetingSpaceId"]) == sid)
            .map(|i| ts(&i["scheduledStartAt"]))
            .collect()
    };

    // A series cannot start at a nonexistent or ambiguous local time.
    for (sid, at) in [
        (ny, local(2027, 3, 14, 2, 30)),
        (rome, local(2027, 10, 31, 2, 30)),
    ] {
        let (s, v) = app
            .call(
                Method::PUT,
                &format!("/api/v2/meeting-spaces/{sid}/recurrence"),
                Auth::Bearer(&owner.token),
                Some(json!({ "rrule": "FREQ=DAILY", "startLocal": at, "durationMinutes": 30 })),
            )
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{v}"
        );
    }
    set_rule(
        &app,
        &owner,
        ny,
        "FREQ=DAILY;COUNT=3",
        local(2027, 3, 13, 2, 30),
        30,
    )
    .await;
    set_rule(
        &app,
        &owner,
        rome,
        "FREQ=DAILY;COUNT=3",
        local(2027, 10, 30, 2, 30),
        30,
    )
    .await;
    set_rule(
        &app,
        &owner,
        lagos,
        "FREQ=DAILY;COUNT=2",
        local(2027, 1, 1, 0, 0),
        30,
    )
    .await;

    let v = agenda_ok(
        &app,
        &owner,
        "?from=2027-03-12T00:00:00Z&to=2027-03-17T00:00:00Z",
    )
    .await;
    assert_eq!(
        starts(&v, ny),
        [
            Utc.with_ymd_and_hms(2027, 3, 13, 7, 30, 0).unwrap(), // 02:30 EST
            Utc.with_ymd_and_hms(2027, 3, 14, 7, 30, 0).unwrap(), // 02:30 does not exist: 03:30 EDT
            Utc.with_ymd_and_hms(2027, 3, 15, 6, 30, 0).unwrap(), // 02:30 EDT
        ]
    );
    let v = agenda_ok(
        &app,
        &owner,
        "?from=2027-10-29T00:00:00Z&to=2027-11-03T00:00:00Z",
    )
    .await;
    assert_eq!(
        starts(&v, rome),
        [
            Utc.with_ymd_and_hms(2027, 10, 30, 0, 30, 0).unwrap(), // 02:30 CEST
            Utc.with_ymd_and_hms(2027, 10, 31, 0, 30, 0).unwrap(), // repeated 02:30: the first (CEST)
            Utc.with_ymd_and_hms(2027, 11, 1, 1, 30, 0).unwrap(),  // 02:30 CET
        ]
    );
    for i in items(&v) {
        assert_eq!(i["meetingSpace"]["timezone"], "Europe/Rome");
    }
    // Midnight in Lagos is 23:00Z the previous day; `from` is inclusive, `to` exclusive.
    let v = agenda_ok(
        &app,
        &owner,
        "?from=2027-01-01T00:00:00Z&to=2027-01-02T00:00:00Z",
    )
    .await;
    assert_eq!(
        starts(&v, lagos),
        [Utc.with_ymd_and_hms(2027, 1, 1, 23, 0, 0).unwrap()]
    );
    let v = agenda_ok(
        &app,
        &owner,
        "?from=2026-12-31T23:00:00Z&to=2027-01-01T23:00:00Z",
    )
    .await;
    assert_eq!(
        starts(&v, lagos),
        [Utc.with_ymd_and_hms(2026, 12, 31, 23, 0, 0).unwrap()]
    );

    // Long conference: exactly 24 hours is allowed, longer is not.
    let start = Utc::now() + Duration::days(5);
    let c = schedule(
        &app,
        &owner,
        lagos,
        start,
        Some(start + Duration::hours(24)),
    )
    .await;
    assert_eq!(
        ts(&c["scheduledEndAt"]) - ts(&c["scheduledStartAt"]),
        Duration::hours(24)
    );
    let (s, v) = app
        .post(format!("/api/v2/meeting-spaces/{lagos}/conferences"), Auth::Bearer(&owner.token), json!({ "scheduledStartAt": start, "scheduledEndAt": start + Duration::hours(24) + Duration::minutes(1) }))
        .await;
    assert_eq!((s, code(&v)), (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"));
}

/// Cancellation after an invitation: the invitee's agenda and calendar file follow it.
#[tokio::test]
async fn cal08_cancellation_after_invite_reaches_agenda_and_ics() {
    let app = app!();
    let host = app.user("host").await;
    let guest = app.user("guest").await;
    let sid = uuid(&app.create_space(&host, json!({ "title": "Consult", "timezone": "America/New_York", "accessPolicy": "INVITE_ONLY" })).await["id"]);
    let start = Utc::now() + Duration::days(4);
    let c = schedule(&app, &host, sid, start, Some(start + Duration::minutes(30))).await;
    let cid = uuid(&c["id"]);
    let (s, inv) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&host.token),
            json!({ "inviteeEmail": guest.email }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{inv}");
    let token = inv["token"].as_str().unwrap().to_string();
    let uri = format!("/api/v2/conferences/{cid}/calendar.ics");
    let (s, h, body, _) = ics(&app, &uri, Some(&guest.token)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(h["cache-control"], "no-store");
    assert!(h["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/calendar"));
    let lines = assert_valid_ics(&body);
    assert_eq!(prop(&lines, "STATUS:"), ["STATUS:CONFIRMED"]);
    assert_eq!(prop(&lines, "SUMMARY:"), ["SUMMARY:Consult"]);
    assert!(!body.contains(&token), "no invitation token in the file");
    let seq0: i32 = prop(&lines, "SEQUENCE:")[0][9..].parse().unwrap();
    let uid0 = prop(&lines, "UID:")[0].to_string();

    let (s, _) = app
        .post(
            format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&host.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let v = agenda_ok(&app, &guest, "").await;
    let it = items(&v)
        .into_iter()
        .find(|i| uuid(&i["conferenceId"]) == cid)
        .unwrap();
    assert_eq!(it["status"], "CANCELLED");
    assert_eq!(it["relations"], json!(["INVITEE"]));
    let v = agenda_ok(&app, &guest, "?status=CREATED").await;
    assert!(items(&v).is_empty());
    let (_, _, body, _) = ics(&app, &uri, Some(&guest.token)).await;
    let lines = assert_valid_ics(&body);
    assert_eq!(prop(&lines, "STATUS:"), ["STATUS:CANCELLED"]);
    assert_eq!(prop(&lines, "UID:")[0], uid0, "UID stable");
    let seq1: i32 = prop(&lines, "SEQUENCE:")[0][9..].parse().unwrap();
    assert!(seq1 > seq0);
}

/// Concurrent reschedule vs cancel: one serial order wins, the result is consistent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cal09_concurrent_reschedule_and_cancel() {
    let app = app!();
    let owner = app.user("owner").await;
    let sid = uuid(&app.create_space(&owner, json!({ "title": "Race" })).await["id"]);
    for _ in 0..3 {
        let start = Utc::now() + Duration::days(2);
        let c = schedule(&app, &owner, sid, start, Some(start + Duration::hours(1))).await;
        let cid = uuid(&c["id"]);
        let reschedule = app.post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": start + Duration::days(1) }),
        );
        let cancel = app.post(
            format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        );
        let ((rs, rv), (cs, cv)) = tokio::join!(reschedule, cancel);
        assert_eq!(cs, StatusCode::OK, "{cv}");
        assert!(
            rs == StatusCode::OK
                || (rs == StatusCode::CONFLICT && code(&rv) == "CONFERENCE_STATE_CONFLICT"),
            "{rs} {rv}"
        );
        let (_, now) = app
            .get(
                format!("/api/v2/conferences/{cid}"),
                Auth::Bearer(&owner.token),
            )
            .await;
        assert_eq!(now["status"], "CANCELLED");
        let expected_version = if rs == StatusCode::OK { 3 } else { 2 };
        assert_eq!(
            now["version"], expected_version,
            "every committed change is counted once"
        );
    }
}

// ---------------------------------------------------------------------------
// Reschedule / create rules (MTG-009, MTG-010)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cal10_reschedule_keeps_end_and_rejects_past_and_stale_versions() {
    let app = app!();
    let owner = app.user("owner").await;
    let sid = uuid(&app.create_space(&owner, json!({ "title": "R" })).await["id"]);
    let now = Utc::now();
    // Past starts are rejected on create (5-minute skew tolerance).
    let (s, v) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": now - Duration::hours(1) }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
        "{v}"
    );
    schedule(&app, &owner, sid, now - Duration::minutes(2), None).await;

    let start = now + Duration::days(1);
    let c = schedule(
        &app,
        &owner,
        sid,
        start,
        Some(start + Duration::minutes(90)),
    )
    .await;
    let cid = uuid(&c["id"]);
    let new_start = start + Duration::days(2);
    let uri = format!("/api/v2/conferences/{cid}/reschedule");
    let (s, v) = app
        .post(
            &uri,
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": new_start, "version": 1 }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(ts(&v["conference"]["scheduledStartAt"]), new_start);
    assert_eq!(
        ts(&v["conference"]["scheduledEndAt"]),
        new_start + Duration::minutes(90),
        "end kept"
    );
    // Stale version.
    let (s, v) = app
        .post(
            &uri,
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": new_start, "version": 1 }),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "MEETING_VERSION_CONFLICT"),
        "{v}"
    );
    // Same validations as create.
    for body in [
        json!({ "scheduledStartAt": now - Duration::hours(2) }),
        json!({ "scheduledStartAt": new_start, "scheduledEndAt": new_start - Duration::minutes(1) }),
        json!({ "scheduledStartAt": new_start, "scheduledEndAt": new_start + Duration::hours(25) }),
    ] {
        let (s, v) = app.post(&uri, Auth::Bearer(&owner.token), body).await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{v}"
        );
    }
    // A start-only occurrence stays start-only.
    let c = schedule(&app, &owner, sid, start, None).await;
    let (s, v) = app
        .post(
            format!(
                "/api/v2/conferences/{}/reschedule",
                c["id"].as_str().unwrap()
            ),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": new_start }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["conference"].get("scheduledEndAt").is_none());
}

/// MTG-010: stale occurrences never win the canonical selection, cannot be started and
/// are expired by the scheduler.
#[tokio::test]
async fn cal11_stale_occurrences_expire_and_never_are_canonical() {
    let app = app!();
    let owner = app.user("owner").await;
    let space = app
        .create_space(
            &owner,
            json!({ "title": "Clinic hours", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let code_ = space["meetingCode"].as_str().unwrap().to_string();
    let next = schedule(&app, &owner, sid, Utc::now() + Duration::days(2), None).await;
    let stale = schedule(&app, &owner, sid, Utc::now() + Duration::days(1), None).await;
    let stale_id = uuid(&stale["id"]);
    app.db
        .conn
        .execute_unprepared(&format!(
            "UPDATE conference_sessions SET scheduled_start_at = now() - interval '3 hours', scheduled_end_at = NULL WHERE id='{stale_id}'"
        ))
        .await
        .unwrap();
    let (_, r) = app
        .get(format!("/api/v2/resolve/{code_}"), Auth::None)
        .await;
    assert_eq!(r["conference"]["id"], next["id"], "{r}");
    let (_, sp) = app
        .get(
            format!("/api/v2/meeting-spaces/{sid}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(sp["latestConference"]["id"], next["id"]);
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{stale_id}/start"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT"),
        "{v}"
    );
    let scheduler = backend::workers::ConferenceScheduler::new(app.state.meeting.clone());
    let tick = scheduler.tick(Utc::now()).await.unwrap();
    assert_eq!(tick.expired, 1, "{tick:?}");
    assert_eq!(
        scheduler.tick(Utc::now()).await.unwrap().expired,
        0,
        "idempotent"
    );
    assert_eq!(
        app.scalar(&format!("SELECT count(*) FROM conference_sessions WHERE id='{stale_id}' AND status='CANCELLED' AND cancelled_by='system:expired'")).await,
        1
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE id='{}' AND status='CREATED'",
            next["id"].as_str().unwrap()
        ))
        .await,
        1
    );
}

// ---------------------------------------------------------------------------
// Summary privacy (MTG-011)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cal12_summary_views_agree_and_protect_healthcare_schedules() {
    let app = app!();
    let owner = app.user("owner").await;
    let start = Utc::now() + Duration::days(3);
    for (profile, schedule_visible) in [("STANDARD", true), ("HEALTHCARE", false)] {
        let space = app
            .create_space(&owner, json!({ "title": format!("{profile} visit"), "accessPolicy": "PUBLIC", "securityProfile": profile }))
            .await;
        let sid = uuid(&space["id"]);
        let code_ = space["meetingCode"].as_str().unwrap().to_string();
        let c = schedule(
            &app,
            &owner,
            sid,
            start,
            Some(start + Duration::minutes(20)),
        )
        .await;
        let cid = c["id"].as_str().unwrap().to_string();
        let (_, conf) = app
            .get(format!("/api/v2/conferences/{cid}"), Auth::None)
            .await;
        assert_eq!(conf["view"], "SUMMARY");
        let (_, sp) = app
            .get(format!("/api/v2/meeting-spaces/{code_}"), Auth::None)
            .await;
        let (_, res) = app
            .get(format!("/api/v2/resolve/{code_}"), Auth::None)
            .await;
        for (name, v) in [
            ("conference", &conf),
            ("latestConference", &sp["latestConference"]),
            ("resolve", &res["conference"]),
        ] {
            assert_eq!(
                v.get("scheduledStartAt").is_some(),
                schedule_visible,
                "{profile} {name}: {v}"
            );
            assert_eq!(
                v.get("scheduledEndAt").is_some(),
                schedule_visible,
                "{profile} {name}: {v}"
            );
            assert!(v.get("startedAt").is_none());
            assert!(v.get("id").is_some() && v.get("status").is_some());
        }
        assert_conforms(&openapi(), "ConferenceResponse", &conf);
        assert_conforms(&openapi(), "MeetingSpaceResponse", &sp);
        // The owner always sees everything.
        let (_, full) = app
            .get(
                format!("/api/v2/meeting-spaces/{sid}"),
                Auth::Bearer(&owner.token),
            )
            .await;
        assert!(full["latestConference"].get("scheduledStartAt").is_some());
    }
}

// ---------------------------------------------------------------------------
// ICS (MTG-007)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cal13_ics_exports_are_valid_private_and_authorized() {
    let app = app!();
    let owner = app.user("owner").await;
    let invitee = app.user("invitee").await;
    let stranger = app.user("stranger").await;
    let passcode = "s3cret-Passc0de";
    let space = app
        .create_space(&owner, json!({ "title": "Weekly review; Q4, ops", "timezone": "America/New_York", "accessPolicy": "PUBLIC_PASSCODE", "passcode": passcode }))
        .await;
    let sid = uuid(&space["id"]);
    let first = (Utc::now() + Duration::days(1))
        .with_timezone(&chrono_tz::America::New_York)
        .date_naive()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    set_rule(&app, &owner, sid, "FREQ=WEEKLY;COUNT=20", first, 60).await;
    let occ = list_conferences(&app, &owner, sid).await;
    let (s, _) = app
        .post(
            format!(
                "/api/v2/conferences/{}/cancel",
                occ[1]["id"].as_str().unwrap()
            ),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let moved = ts(&occ[2]["scheduledStartAt"]) + Duration::hours(1);
    let (s, _) = app
        .post(
            format!(
                "/api/v2/conferences/{}/reschedule",
                occ[2]["id"].as_str().unwrap()
            ),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": moved }),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let start = Utc::now() + Duration::days(9);
    schedule(&app, &owner, sid, start, None).await;

    let space_uri = format!("/api/v2/meeting-spaces/{sid}/calendar.ics");
    let (s, h, body, _) = ics(&app, &space_uri, Some(&owner.token)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(h["cache-control"], "no-store");
    assert!(h["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment"));
    let lines = assert_valid_ics(&body);
    assert!(lines.iter().any(|l| l == "TZID:America/New_York"));
    assert!(lines.iter().any(|l| l == "BEGIN:VTIMEZONE"));
    let rrule = prop(&lines, "RRULE:");
    assert_eq!(rrule.len(), 1);
    assert_eq!(rrule[0], "RRULE:FREQ=WEEKLY;COUNT=20");
    assert_eq!(prop(&lines, "EXDATE;TZID=America/New_York:").len(), 1);
    assert_eq!(
        prop(&lines, "RECURRENCE-ID;TZID=America/New_York:").len(),
        1
    );
    assert!(
        prop(&lines, "DTSTART;TZID=America/New_York:").len() >= 3,
        "master, override, one-off"
    );
    assert!(lines
        .iter()
        .any(|l| l == "SUMMARY:Weekly review\\; Q4\\, ops"));
    let uids: std::collections::HashSet<_> = prop(&lines, "UID:").into_iter().collect();
    assert_eq!(
        uids.len(),
        2,
        "series UID (master + override) and one one-off UID"
    );
    assert!(!body.contains(passcode), "no passcode in the file");
    for c in &occ {
        let room = format!("cf-{}", uuid(&c["id"]).simple());
        assert!(!body.contains(&room), "no provider room names");
    }

    // Conference export: stable per-occurrence UID, also for the owner.
    let conf_uri = format!(
        "/api/v2/conferences/{}/calendar.ics",
        occ[0]["id"].as_str().unwrap()
    );
    let (s, _, body, _) = ics(&app, &conf_uri, Some(&owner.token)).await;
    assert_eq!(s, StatusCode::OK);
    let lines = assert_valid_ics(&body);
    assert!(prop(&lines, "UID:occurrence-")[0].ends_with("@cfmeet.com"));

    // Authorization: a space invitation bound to the invitee's email grants the export.
    let (s, h, _, v) = ics(&app, &space_uri, Some(&stranger.token)).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    assert_error_contract(&openapi(), s, &h, &v);
    let (s, _, _, v) = ics(&app, &conf_uri, Some(&stranger.token)).await;
    assert_eq!((s, code(&v)), (StatusCode::FORBIDDEN, "PERMISSION_DENIED"));
    let (s, _, _, _) = ics(&app, &conf_uri, None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            json!({ "inviteeEmail": invitee.email }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _, body, _) = ics(&app, &space_uri, Some(&invitee.token)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(!body.contains(passcode));
    // Unknown ids and an unscheduled conference.
    let (s, _, _, v) = ics(
        &app,
        &format!("/api/v2/conferences/{}/calendar.ics", Uuid::new_v4()),
        Some(&owner.token),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::NOT_FOUND, "CONFERENCE_NOT_FOUND")
    );
    let (s, _, _, v) = ics(
        &app,
        &format!("/api/v2/meeting-spaces/{}/calendar.ics", Uuid::new_v4()),
        Some(&owner.token),
    )
    .await;
    assert_eq!((s, code(&v)), (StatusCode::NOT_FOUND, "MEETING_NOT_FOUND"));
    let (_, unscheduled) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    let (s, _, _, v) = ics(
        &app,
        &format!(
            "/api/v2/conferences/{}/calendar.ics",
            unscheduled["id"].as_str().unwrap()
        ),
        Some(&owner.token),
    )
    .await;
    assert_eq!(
        (s, code(&v)),
        (StatusCode::CONFLICT, "CONFERENCE_STATE_CONFLICT")
    );
}

/// Healthcare titles stay hidden from non-managers in calendar files and the agenda.
#[tokio::test]
async fn cal14_healthcare_titles_hidden_from_non_managers() {
    let app = app!();
    let clinician = app.user("clinician").await;
    let patient = app.user("patient").await;
    let space = app
        .create_space(&clinician, json!({ "title": "Oncology follow-up", "securityProfile": "HEALTHCARE", "accessPolicy": "INVITE_ONLY" }))
        .await;
    let sid = uuid(&space["id"]);
    let start = Utc::now() + Duration::days(2);
    let c = schedule(
        &app,
        &clinician,
        sid,
        start,
        Some(start + Duration::minutes(30)),
    )
    .await;
    let (s, _) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&clinician.token),
            json!({ "inviteeEmail": patient.email }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let v = agenda_ok(&app, &patient, "").await;
    let it = &items(&v)[0];
    assert!(it["meetingSpace"].get("title").is_none(), "{it}");
    assert_eq!(
        ts(&it["scheduledStartAt"]),
        start,
        "the patient still sees their own appointment time"
    );
    let (s, _, body, _) = ics(
        &app,
        &format!(
            "/api/v2/conferences/{}/calendar.ics",
            c["id"].as_str().unwrap()
        ),
        Some(&patient.token),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(!body.contains("Oncology"), "{body}");
    assert!(body.contains("SUMMARY:CF Meet meeting"));
    let (_, _, body, _) = ics(
        &app,
        &format!(
            "/api/v2/conferences/{}/calendar.ics",
            c["id"].as_str().unwrap()
        ),
        Some(&clinician.token),
    )
    .await;
    assert!(body.contains("SUMMARY:Oncology follow-up"));
}

// ---------------------------------------------------------------------------
// Idempotent creation (SEC-014) and the paginated space list (MTG-018)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cal15_idempotent_space_and_conference_creation() {
    let app = app!();
    let owner = app.user("owner").await;
    let other = app.user("other").await;
    let bearer = format!("Bearer {}", owner.token);
    let post = |uri: String, key: &'static str, body: &'static str, auth: String| {
        let app = &app;
        async move {
            raw(
                app,
                Method::POST,
                &uri,
                &[("authorization", auth.as_str()), ("idempotency-key", key)],
                Some(body),
            )
            .await
        }
    };
    let (s1, h1, v1) = post(
        "/api/v2/meeting-spaces".into(),
        "space-key-1",
        r#"{"title":"Idem","timezone":"UTC"}"#,
        bearer.clone(),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED, "{v1}");
    assert!(h1.get("idempotent-replayed").is_none());
    // Same key, same body (different key order and whitespace) → the same space, replayed.
    let (s2, h2, v2) = post(
        "/api/v2/meeting-spaces".into(),
        "space-key-1",
        r#"{ "timezone": "UTC", "title": "Idem" }"#,
        bearer.clone(),
    )
    .await;
    assert_eq!(s2, StatusCode::CREATED, "{v2}");
    assert_eq!(v2["id"], v1["id"]);
    assert_eq!(h2["idempotent-replayed"], "true");
    // Same key, different body → conflict.
    let (s3, h3, v3) = post(
        "/api/v2/meeting-spaces".into(),
        "space-key-1",
        r#"{"title":"Other"}"#,
        bearer.clone(),
    )
    .await;
    assert_eq!(
        (s3, code(&v3)),
        (StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT")
    );
    assert_error_contract(&openapi(), s3, &h3, &v3);
    // Keys are per caller.
    let (s4, _, v4) = post(
        "/api/v2/meeting-spaces".into(),
        "space-key-1",
        r#"{"title":"Idem","timezone":"UTC"}"#,
        format!("Bearer {}", other.token),
    )
    .await;
    assert_eq!(s4, StatusCode::CREATED);
    assert_ne!(v4["id"], v1["id"]);
    // Invalid key.
    let (s5, _, v5) = raw(
        &app,
        Method::POST,
        "/api/v2/meeting-spaces",
        &[("authorization", &bearer), ("idempotency-key", "bad key")],
        Some(r#"{"title":"x"}"#),
    )
    .await;
    assert_eq!(
        (s5, code(&v5)),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR")
    );
    assert_eq!(
        app.scalar("SELECT count(*) FROM meeting_spaces WHERE title = 'Idem'")
            .await,
        2
    );

    // Concurrent first requests with one key create exactly one space.
    let results = join_all((0..6).map(|_| {
        post(
            "/api/v2/meeting-spaces".into(),
            "space-key-race",
            r#"{"title":"Race"}"#,
            bearer.clone(),
        )
    }))
    .await;
    let ids: std::collections::HashSet<_> = results
        .iter()
        .map(|(s, _, v)| {
            assert_eq!(*s, StatusCode::CREATED, "{v}");
            v["id"].clone()
        })
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(
        app.scalar("SELECT count(*) FROM meeting_spaces WHERE title = 'Race'")
            .await,
        1
    );

    // Conference creation (empty body allowed) — keyed per space.
    let sid = v1["id"].as_str().unwrap().to_string();
    let uri = format!("/api/v2/meeting-spaces/{sid}/conferences");
    let (c1s, _, c1) = post(uri.clone(), "conf-key", "", bearer.clone()).await;
    assert_eq!(c1s, StatusCode::CREATED, "{c1}");
    let (c2s, c2h, c2) = post(uri.clone(), "conf-key", "{}", bearer.clone()).await;
    assert_eq!(c2s, StatusCode::CREATED);
    assert_eq!(c2["id"], c1["id"], "an empty body equals {{}}");
    assert_eq!(c2h["idempotent-replayed"], "true");
    let (c3s, _, c3) = post(
        uri.clone(),
        "conf-key",
        r#"{"scheduledStartAt":"2099-01-01T10:00:00Z"}"#,
        bearer.clone(),
    )
    .await;
    assert_eq!(
        (c3s, code(&c3)),
        (StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT")
    );
    assert_eq!(
        app.scalar(&format!(
            "SELECT count(*) FROM conference_sessions WHERE meeting_space_id = '{sid}'"
        ))
        .await,
        1
    );
    // Expired records are purged by the scheduler.
    app.db.conn.execute_unprepared("UPDATE meeting_idempotency_keys SET created_at = now() - interval '2 days', expires_at = now() - interval '1 day'").await.unwrap();
    backend::workers::ConferenceScheduler::new(app.state.meeting.clone())
        .tick(Utc::now())
        .await
        .unwrap();
    assert_eq!(
        app.scalar("SELECT count(*) FROM meeting_idempotency_keys")
            .await,
        0
    );
}

#[tokio::test]
async fn cal16_space_list_paginates_without_changing_the_legacy_list() {
    let app = app!();
    let owner = app.user("owner").await;
    let member_of = app.user("host").await;
    for i in 0..4 {
        app.create_space(&owner, json!({ "title": format!("Own {i}") }))
            .await;
    }
    let shared = uuid(
        &app.create_space(&member_of, json!({ "title": "Shared" }))
            .await["id"],
    );
    app.add_member(
        shared,
        &member_of,
        &owner,
        backend::domain::Role::Participant,
    )
    .await;
    schedule(
        &app,
        &member_of,
        shared,
        Utc::now() + Duration::days(1),
        None,
    )
    .await;

    let (s, legacy) = app
        .get("/api/v2/meeting-spaces", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(items(&legacy).len(), 5);
    assert!(legacy.get("nextCursor").is_none(), "legacy shape unchanged");
    assert_conforms(&openapi(), "MeetingSpaceListResponse", &legacy);
    let shared_item = items(&legacy)
        .into_iter()
        .find(|i| uuid(&i["id"]) == shared)
        .unwrap();
    assert_eq!(shared_item["viewerRole"], "PARTICIPANT");
    assert_eq!(shared_item["view"], "SUMMARY");
    assert!(shared_item["latestConference"]["scheduledStartAt"].is_string());

    let mut seen = vec![];
    let mut cursor = None::<String>;
    loop {
        let q = match &cursor {
            Some(c) => format!("?limit=2&cursor={c}"),
            None => "?limit=2".into(),
        };
        let (s, page) = app
            .get(
                format!("/api/v2/meeting-spaces{q}"),
                Auth::Bearer(&owner.token),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{page}");
        assert!(items(&page).len() <= 2);
        seen.extend(items(&page).iter().map(|i| i["id"].clone()));
        match page.get("nextCursor").and_then(Value::as_str) {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    let legacy_ids: Vec<Value> = items(&legacy).iter().map(|i| i["id"].clone()).collect();
    assert_eq!(seen, legacy_ids, "same newest-first order, no duplicates");
    for q in ["?limit=0", "?limit=500", "?cursor=@@@", "?page=2"] {
        let (s, v) = app
            .get(
                format!("/api/v2/meeting-spaces{q}"),
                Auth::Bearer(&owner.token),
            )
            .await;
        assert_eq!(
            (s, code(&v)),
            (StatusCode::BAD_REQUEST, "VALIDATION_ERROR"),
            "{q}: {v}"
        );
    }
}

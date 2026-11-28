//! ADR 0071 — Events: the one gate (organizer, people on a personal event, ACTIVE
//! conversation members decided live, Messaging blocks), invitations limited to mutual
//! contacts / members, reserved scopes, RSVP and counts, edits with versions, cancellation
//! and its notices, deletion, server-side recurrence across daylight saving, the dashboard
//! views with keyset pages and search, reminders that fire once and move on, ICS export,
//! idempotent creation and cover media.
//!
//! Communications (contacts and personal blocks) is a scripted Hub answering
//! `contacts/eligible` like the real one: mutual, unblocked contacts only.

mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{http::Method, routing::post, Json, Router};
use cf_object_storage::MemoryObjectStore;
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner, SharedClock,
    SystemClock, TokenTtl,
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use common::{flows::ALL_SCOPES, *};
use serde_json::{json, Value};
use uuid::Uuid;

type Pairs = HashMap<(Uuid, Uuid), DateTime<Utc>>;

#[derive(Clone, Default)]
struct Hub {
    pairs: Arc<Mutex<Pairs>>,
}

impl Hub {
    fn connect(&self, a: Uuid, b: Uuid, since: DateTime<Utc>) {
        let k = if a < b { (a, b) } else { (b, a) };
        self.pairs.lock().unwrap().insert(k, since);
    }
}

async fn eligible(
    axum::extract::State(hub): axum::extract::State<Hub>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let actor: Uuid = serde_json::from_value(body["actorPrincipalId"].clone()).unwrap();
    let among: Option<Vec<Uuid>> = body
        .get("principalIds")
        .map(|v| serde_json::from_value(v.clone()).unwrap());
    let contacts: Vec<Value> = hub
        .pairs
        .lock()
        .unwrap()
        .iter()
        .filter_map(|((a, b), since)| {
            let other = if *a == actor {
                *b
            } else if *b == actor {
                *a
            } else {
                return None;
            };
            if among.as_ref().is_some_and(|l| !l.contains(&other)) {
                return None;
            }
            Some(json!({ "principalId": other, "eligibleSince": since.to_rfc3339() }))
        })
        .collect();
    Json(json!({ "schemaVersion": 1, "contacts": contacts }))
}

struct World {
    h: Harness,
    c: Customer,
    hub: Hub,
}

async fn world() -> Option<World> {
    let hub = Hub::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route(
            "/internal/v1/communications/contacts/eligible",
            post(eligible),
        )
        .with_state(hub.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = Arc::new(MemoryObjectStore::new());
    let mut h = Harness::build(move |state| {
        state.storage = store;
        state.event_notices = true;
    })
    .await?;
    let c = h.customer(ALL_SCOPES);
    let clock: SharedClock = Arc::new(SystemClock);
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::messaging(),
        RuntimeEnvironment::Test,
        ServiceSigningKey::generate(KeyId::parse("events-test").unwrap()),
        TokenTtl::default(),
        clock,
    ));
    let mut targets = InternalTargets::new(RuntimeEnvironment::Test);
    targets.insert(ServiceId::communications(), &url).unwrap();
    let client = Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(5)).unwrap(),
    );
    h.state.communications = Some(cf_communications_policy::PolicyClient::new(
        client,
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    ));
    Some(World { h, c, hub })
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

impl World {
    fn person(&self) -> Uuid {
        self.h.principal_of_kind(&self.c, "NATIVE_CF_USER")
    }
    async fn call(&self, method: Method, who: Uuid, path: &str, body: Option<Value>) -> Response {
        self.h
            .send(method, Some(&self.c.token), Some(who), path, body, &[])
            .await
    }
    async fn get(&self, who: Uuid, path: &str) -> Response {
        self.call(Method::GET, who, path, None).await
    }
    async fn post(&self, who: Uuid, path: &str, body: Value) -> Response {
        self.call(Method::POST, who, path, Some(body)).await
    }
    async fn put(&self, who: Uuid, path: &str, body: Value) -> Response {
        self.call(Method::PUT, who, path, Some(body)).await
    }
    async fn patch(&self, who: Uuid, path: &str, body: Value) -> Response {
        self.call(Method::PATCH, who, path, Some(body)).await
    }
    async fn delete(&self, who: Uuid, path: &str) -> Response {
        self.call(Method::DELETE, who, path, None).await
    }
    /// A one-hour PHYSICAL event starting `in_hours` from now, with `extra` fields.
    fn body(&self, in_hours: i64, extra: Value) -> Value {
        let start = self.h.now() + Duration::hours(in_hours);
        let mut b = json!({
            "scope": "PERSONAL",
            "title": "Dinner, at Ada's; bring cake",
            "type": "PHYSICAL",
            "location": "Ada's place",
            "startsAt": ts(start),
            "endsAt": ts(start + Duration::hours(1)),
        });
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    }
    async fn create(&self, who: Uuid, body: Value) -> Value {
        let r = self.post(who, "/v1/messaging/events", body).await;
        assert_eq!(r.status.as_u16(), 201, "{}", r.text);
        r.json
    }
    async fn sees(&self, who: Uuid, event: &str) -> bool {
        let r = self
            .get(who, &format!("/v1/messaging/events/{event}"))
            .await;
        match r.status.as_u16() {
            200 => true,
            404 => {
                assert_eq!(r.code(), "NOT_FOUND", "{}", r.text);
                false
            }
            other => panic!("unexpected {other}: {}", r.text),
        }
    }
    async fn listed(&self, who: Uuid, query: &str) -> Vec<Value> {
        let r = self
            .get(who, &format!("/v1/messaging/events?{query}"))
            .await;
        assert_eq!(r.status.as_u16(), 200, "{}", r.text);
        r.json["items"].as_array().unwrap().clone()
    }
    async fn lists(&self, who: Uuid, query: &str, event: &str) -> bool {
        self.listed(who, query)
            .await
            .iter()
            .any(|i| i["event"]["eventId"] == event)
    }
}

fn id(v: &Value) -> String {
    v["eventId"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn personal_events_are_seen_only_by_the_organizer_and_the_people_on_them() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob, carol, dave) = (w.person(), w.person(), w.person(), w.person());
    let now = w.h.now();
    w.hub.connect(alice, bob, now);
    w.hub.connect(alice, dave, now);

    // Only mutual contacts can be invited.
    let r = w
        .post(
            alice,
            "/v1/messaging/events",
            w.body(24, json!({ "inviteePrincipalIds": [bob, carol] })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
    assert_eq!(r.reason(), "INVITEE_NOT_ELIGIBLE");
    assert_eq!(w.h.scalar("SELECT count(*) FROM messaging_events").await, 0);

    let e = w
        .create(alice, w.body(24, json!({ "inviteePrincipalIds": [bob] })))
        .await;
    let ev = id(&e);
    assert_eq!(e["myRole"], "ORGANIZER");
    assert_eq!(e["myResponse"], "GOING");
    assert_eq!(e["counts"]["going"], 1);
    assert_eq!(e["counts"]["awaiting"], 1);
    assert_eq!(e["canEdit"], true);
    assert_eq!(e["canDelete"], true);

    assert!(w.sees(bob, &ev).await);
    assert!(!w.sees(carol, &ev).await, "not invited");
    assert!(!w.sees(dave, &ev).await, "a contact who was not invited");
    assert!(w.lists(bob, "view=INVITATIONS", &ev).await);
    assert!(!w.lists(carol, "view=UPCOMING", &ev).await);
    for path in ["attendees", "ics"] {
        let r = w
            .get(carol, &format!("/v1/messaging/events/{ev}/{path}"))
            .await;
        assert_eq!(r.status.as_u16(), 404, "{path}: {}", r.text);
    }
    let r = w
        .put(
            carol,
            &format!("/v1/messaging/events/{ev}/rsvp"),
            json!({ "response": "GOING" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 404, "{}", r.text);
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_event_people")
            .await,
        2,
        "a refused answer changes nothing"
    );

    // Bob answers; the counts and views follow.
    let r = w
        .put(
            bob,
            &format!("/v1/messaging/events/{ev}/rsvp"),
            json!({ "response": "GOING" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["myResponse"], "GOING");
    assert_eq!(r.json["myRole"], "INVITEE");
    assert_eq!(r.json["counts"]["going"], 2);
    assert_eq!(r.json["counts"]["awaiting"], 0);
    assert_eq!(r.json["canEdit"], false);
    assert!(!w.lists(bob, "view=INVITATIONS", &ev).await);
    assert!(w.lists(bob, "view=ACCEPTED", &ev).await);
    assert!(w.lists(alice, "view=MINE", &ev).await);
    assert!(!w.lists(bob, "view=MINE", &ev).await);
    let r = w
        .put(
            bob,
            &format!("/v1/messaging/events/{ev}/rsvp"),
            json!({ "response": "SOMETIMES" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);

    // An invitee cannot edit, invite, cancel or delete.
    let r = w
        .patch(
            bob,
            &format!("/v1/messaging/events/{ev}"),
            json!({ "title": "Mine now" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    assert_eq!(r.reason(), "NOT_EVENT_ORGANIZER");
    let r = w
        .post(bob, &format!("/v1/messaging/events/{ev}/cancel"), json!({}))
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    let r = w.delete(bob, &format!("/v1/messaging/events/{ev}")).await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    let r = w
        .post(
            bob,
            &format!("/v1/messaging/events/{ev}/invitations"),
            json!({ "principalIds": [dave] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);

    // The organizer invites another contact; an invitation notice is written.
    let r = w
        .post(
            alice,
            &format!("/v1/messaging/events/{ev}/invitations"),
            json!({ "principalIds": [dave] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["attendees"].as_array().unwrap().len(), 3);
    assert!(w.sees(dave, &ev).await);
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_event_notices WHERE kind = 'INVITED'")
            .await,
        2
    );
    let r = w
        .delete(
            alice,
            &format!("/v1/messaging/events/{ev}/invitations/{dave}"),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert!(!w.sees(dave, &ev).await, "removed invitee");

    // A Messaging block between an invitee and the organizer hides the event.
    let b = w
        .post(bob, "/v1/messaging/blocks", json!({ "principalId": alice }))
        .await;
    assert_eq!(b.status.as_u16(), 201, "{}", b.text);
    assert!(!w.sees(bob, &ev).await);
    assert!(!w.lists(bob, "view=ACCEPTED", &ev).await);
    let r = w
        .post(
            alice,
            &format!("/v1/messaging/events/{ev}/invitations"),
            json!({ "principalIds": [bob] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
}

#[tokio::test]
async fn conversation_events_follow_live_membership() {
    let Some(w) = world().await else {
        return;
    };
    let (conv, people) = w.h.managed_group(&w.c, 3).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let (owner, member, leaver) = (people[0], people[1], people[2]);
    let outsider = w.person();

    // Outsiders cannot create events in the conversation.
    let r = w
        .post(
            outsider,
            "/v1/messaging/events",
            w.body(5, json!({ "scope": "CONVERSATION", "conversationId": cid })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 404, "{}", r.text);
    // Invitees must be members.
    let r = w
        .post(
            owner,
            "/v1/messaging/events",
            w.body(
                5,
                json!({ "scope": "CONVERSATION", "conversationId": cid, "inviteePrincipalIds": [outsider] }),
            ),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
    assert_eq!(r.reason(), "INVITEE_NOT_ELIGIBLE");

    let e = w
        .create(
            owner,
            w.body(5, json!({ "scope": "CONVERSATION", "conversationId": cid })),
        )
        .await;
    let ev = id(&e);
    assert_eq!(e["counts"]["awaiting"], 2, "two members have not answered");
    assert!(w.sees(member, &ev).await);
    assert!(w.sees(leaver, &ev).await);
    assert!(!w.sees(outsider, &ev).await);
    assert!(w.lists(member, "view=INVITATIONS", &ev).await);
    assert!(w.lists(member, &format!("conversationId={cid}"), &ev).await);
    assert!(
        !w.lists(outsider, &format!("conversationId={cid}"), &ev)
            .await
    );

    let r = w
        .put(
            member,
            &format!("/v1/messaging/events/{ev}/rsvp"),
            json!({ "response": "MAYBE" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["myRole"], "MEMBER");
    assert_eq!(r.json["counts"]["maybe"], 1);
    assert_eq!(r.json["counts"]["awaiting"], 1);
    assert!(!w.lists(member, "view=INVITATIONS", &ev).await);

    // A member who leaves loses the event at once.
    let r =
        w.h.delete(
            &w.c,
            Some(leaver),
            &format!("/v1/conversations/{cid}/members/{leaver}"),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    assert!(!w.sees(leaver, &ev).await);
    let a = w
        .get(owner, &format!("/v1/messaging/events/{ev}/attendees"))
        .await;
    assert_eq!(a.status.as_u16(), 200, "{}", a.text);
    assert_eq!(a.json["counts"]["awaiting"], 0);
    assert_eq!(a.json["attendees"].as_array().unwrap().len(), 2);

    // The organizer makes the member a co-organizer, who may then edit but not delete.
    let r = w
        .patch(
            owner,
            &format!("/v1/messaging/events/{ev}"),
            json!({ "coOrganizerPrincipalIds": [member], "version": e["version"] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["coOrganizerPrincipalIds"], json!([member]));
    let r = w
        .patch(
            member,
            &format!("/v1/messaging/events/{ev}"),
            json!({ "title": "Stand-up" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["title"], "Stand-up");
    assert_eq!(r.json["myRole"], "CO_ORGANIZER");
    assert!(w.lists(member, "view=MINE", &ev).await);
    let r = w
        .patch(
            member,
            &format!("/v1/messaging/events/{ev}"),
            json!({ "coOrganizerPrincipalIds": [] }),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        403,
        "only the organizer changes co-organizers"
    );
    let r = w
        .delete(member, &format!("/v1/messaging/events/{ev}"))
        .await;
    assert_eq!(r.status.as_u16(), 403, "{}", r.text);
    let r = w.delete(owner, &format!("/v1/messaging/events/{ev}")).await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["deleted"], true);
    assert!(!w.sees(owner, &ev).await);
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_event_people")
            .await,
        0
    );
}

#[tokio::test]
async fn organization_and_community_scopes_are_reserved_and_bodies_are_checked() {
    let Some(w) = world().await else {
        return;
    };
    let alice = w.person();
    for scope in ["ORGANIZATION", "COMMUNITY"] {
        let r = w
            .post(
                alice,
                "/v1/messaging/events",
                w.body(3, json!({ "scope": scope })),
            )
            .await;
        assert_eq!(r.status.as_u16(), 422, "{}", r.text);
        assert_eq!(r.code(), "CONTEXT_NOT_ALLOWED");
        assert_eq!(r.reason(), "EVENT_SCOPE_NOT_AVAILABLE");
    }
    let bad = [
        json!({ "type": "ONLINE" }),
        json!({ "type": "ONLINE", "meetingUrl": "http://cfmeet.com/abc" }),
        json!({ "title": "  " }),
        json!({ "timeZone": "Mars/Base" }),
        json!({ "endsAt": ts(w.h.now()) }),
        json!({ "startsAt": ts(w.h.now() - Duration::hours(3)), "endsAt": ts(w.h.now() - Duration::hours(2)) }),
        json!({ "allDay": true }),
        json!({ "recurrence": { "frequency": "DAILY", "count": 3, "until": "2099-01-01" } }),
        json!({ "recurrence": { "frequency": "YEARLY" } }),
        json!({ "conversationId": Uuid::new_v4() }),
        json!({ "remindersMinutesBefore": [1, 2, 3, 4, 5, 6] }),
    ];
    for extra in bad {
        let r = w
            .post(alice, "/v1/messaging/events", w.body(3, extra.clone()))
            .await;
        assert!(
            matches!(r.status.as_u16(), 400 | 422),
            "{extra}: {} {}",
            r.status,
            r.text
        );
    }
    assert_eq!(w.h.scalar("SELECT count(*) FROM messaging_events").await, 0);
    // An online event links to a CF Meet meeting.
    let e = w
        .create(
            alice,
            w.body(
                3,
                json!({ "type": "ONLINE", "meetingId": "abc-defg-hij", "meetingUrl": "https://cfmeet.com/abc-defg-hij" }),
            ),
        )
        .await;
    assert_eq!(e["meetingUrl"], "https://cfmeet.com/abc-defg-hij");
    assert_eq!(e["meetingId"], "abc-defg-hij");
}

/// The Monday a week before the last Sunday of March of next year (London changes to BST
/// on that Sunday).
fn monday_before_bst() -> NaiveDate {
    let year = Utc::now().year() + 1;
    let mut d = NaiveDate::from_ymd_opt(year, 3, 31).unwrap();
    while d.weekday() != Weekday::Sun {
        d = d.pred_opt().unwrap();
    }
    d - Duration::days(6)
}

#[tokio::test]
async fn recurring_events_expand_in_their_time_zone_across_daylight_saving() {
    let Some(w) = world().await else {
        return;
    };
    let alice = w.person();
    let monday = monday_before_bst();
    let start = Utc.from_utc_datetime(&monday.and_hms_opt(10, 0, 0).unwrap()); // 10:00 GMT
    let e = w
        .create(
            alice,
            json!({
                "scope": "PERSONAL",
                "title": "Weekly sync",
                "type": "PHYSICAL",
                "startsAt": ts(start),
                "endsAt": ts(start + Duration::minutes(30)),
                "timeZone": "Europe/London",
                "recurrence": { "frequency": "WEEKLY", "count": 3, "byDay": ["MO"] },
            }),
        )
        .await;
    let ev = id(&e);
    assert_eq!(e["rrule"], "FREQ=WEEKLY;COUNT=3;BYDAY=MO");
    let second = start + Duration::days(7) - Duration::hours(1); // 10:00 BST
    let third = second + Duration::days(7);
    assert_eq!(
        utc(e["seriesEndsAt"].as_str().unwrap()),
        third + Duration::minutes(30)
    );
    let r = w.get(alice, &format!("/v1/messaging/events/{ev}")).await;
    let upcoming: Vec<DateTime<Utc>> = r.json["upcomingOccurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| utc(o["startsAt"].as_str().unwrap()))
        .collect();
    assert_eq!(upcoming, vec![start, second, third]);

    // The month calendar returns every occurrence in its range, each with its event.
    let from = start - Duration::days(1);
    let to = third + Duration::days(1);
    let items = w
        .listed(
            alice,
            &format!(
                "from={}&to={}",
                urlencoding(&ts(from)),
                urlencoding(&ts(to))
            ),
        )
        .await;
    let starts: Vec<DateTime<Utc>> = items
        .iter()
        .map(|i| utc(i["startsAt"].as_str().unwrap()))
        .collect();
    assert_eq!(starts, vec![start, second, third]);
    assert!(items.iter().all(|i| i["event"]["eventId"] == ev.as_str()));
    // A range longer than 92 days is refused.
    let r = w
        .get(
            alice,
            &format!(
                "/v1/messaging/events?from={}&to={}",
                urlencoding(&ts(from)),
                urlencoding(&ts(from + Duration::days(93)))
            ),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);

    // BYDAY must include the start's day.
    let r = w
        .post(
            alice,
            "/v1/messaging/events",
            json!({
                "scope": "PERSONAL", "title": "x", "type": "CALL",
                "startsAt": ts(start), "endsAt": ts(start + Duration::minutes(30)),
                "timeZone": "Europe/London",
                "recurrence": { "frequency": "WEEKLY", "byDay": ["TU"] },
            }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
}

fn urlencoding(s: &str) -> String {
    s.replace('+', "%2B").replace(':', "%3A")
}

#[tokio::test]
async fn views_pages_and_search() {
    let Some(w) = world().await else {
        return;
    };
    let alice = w.person();
    let mut ids = Vec::new();
    for (i, title) in ["Book club", "Bowling night", "Team retro"]
        .iter()
        .enumerate()
    {
        let e = w
            .create(alice, w.body(2 + i as i64, json!({ "title": title })))
            .await;
        ids.push(id(&e));
    }
    let r = w.get(alice, "/v1/messaging/events?limit=2").await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    let first: Vec<String> = r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["event"]["eventId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(first, ids[..2].to_vec());
    let cursor = r.json["nextCursor"].as_str().unwrap().to_string();
    let r = w
        .get(
            alice,
            &format!("/v1/messaging/events?limit=2&cursor={cursor}"),
        )
        .await;
    let rest: Vec<String> = r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["event"]["eventId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(rest, ids[2..].to_vec());
    assert!(r.json["nextCursor"].is_null());

    let hits = w.listed(alice, "q=bo").await;
    assert_eq!(hits.len(), 2, "prefix search: Book, Bowling");
    let hits = w.listed(alice, "q=retro&type=PHYSICAL").await;
    assert_eq!(hits.len(), 1);
    assert!(w.listed(alice, "q=retro&type=ONLINE").await.is_empty());
    assert!(w.listed(alice, "view=PAST").await.is_empty());

    // Today in a zone, past after time passes.
    let today = w.listed(alice, "view=TODAY&tz=UTC").await;
    assert!(today.len() <= 3);
    w.h.clock.advance(Duration::hours(10));
    let past = w.listed(alice, "view=PAST").await;
    assert_eq!(past.len(), 3);
    assert_eq!(past[0]["event"]["eventId"], ids[2].as_str(), "newest first");
    assert!(w.listed(alice, "view=UPCOMING").await.is_empty());
    for bad in [
        "view=SOON",
        "limit=0",
        "limit=101",
        "cursor=nope",
        "from=2026-01-01T00:00:00Z",
        "unknown=1",
    ] {
        let r = w.get(alice, &format!("/v1/messaging/events?{bad}")).await;
        assert_eq!(r.status.as_u16(), 400, "{bad}: {}", r.text);
    }
}

#[tokio::test]
async fn edits_are_versioned_and_cancellation_notifies_and_closes_answers() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob, carol) = (w.person(), w.person(), w.person());
    let now = w.h.now();
    w.hub.connect(alice, bob, now);
    w.hub.connect(alice, carol, now);
    let e = w
        .create(
            alice,
            w.body(48, json!({ "inviteePrincipalIds": [bob, carol] })),
        )
        .await;
    let ev = id(&e);
    let v1 = e["version"].as_i64().unwrap();
    let r = w
        .patch(
            alice,
            &format!("/v1/messaging/events/{ev}"),
            json!({ "location": null, "description": "Bring cake", "version": v1 }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert!(r.json["location"].is_null());
    assert_eq!(r.json["version"], v1 + 1);
    let r = w
        .patch(
            alice,
            &format!("/v1/messaging/events/{ev}"),
            json!({ "title": "Stale", "version": v1 }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "{}", r.text);
    assert_eq!(r.code(), "VERSION_CONFLICT");
    let r = w
        .patch(
            alice,
            &format!("/v1/messaging/events/{ev}"),
            json!({ "scope": "CONVERSATION" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);

    let r = w
        .put(
            carol,
            &format!("/v1/messaging/events/{ev}/rsvp"),
            json!({ "response": "NOT_GOING" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);

    let r = w
        .post(
            alice,
            &format!("/v1/messaging/events/{ev}/cancel"),
            json!({ "reason": "Rain" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["state"], "CANCELLED");
    assert_eq!(r.json["cancelReason"], "Rain");
    assert_eq!(r.json["canEdit"], false);
    // Bob (no answer) is told; Carol (not going) and Alice are not.
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_event_notices WHERE kind = 'CANCELLED'")
            .await,
        1
    );
    let again = w
        .post(
            alice,
            &format!("/v1/messaging/events/{ev}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(again.status.as_u16(), 200, "{}", again.text);
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_event_notices WHERE kind = 'CANCELLED'")
            .await,
        1,
        "cancelling again changes nothing"
    );
    let r = w
        .put(
            bob,
            &format!("/v1/messaging/events/{ev}/rsvp"),
            json!({ "response": "GOING" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "{}", r.text);
    assert_eq!(r.reason(), "EVENT_CANCELLED");
    let r = w
        .patch(
            alice,
            &format!("/v1/messaging/events/{ev}"),
            json!({ "title": "Back on" }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "{}", r.text);
    assert!(!w.lists(bob, "view=INVITATIONS", &ev).await);
    assert!(
        w.lists(bob, "view=UPCOMING", &ev).await,
        "still shown, as cancelled"
    );
}

#[tokio::test]
async fn reminders_fire_once_per_occurrence_and_move_on() {
    let Some(w) = world().await else {
        return;
    };
    let alice = w.person();
    let start = w.h.now() + Duration::hours(2);
    let e = w
        .create(
            alice,
            json!({
                "scope": "PERSONAL", "title": "Daily walk", "type": "PHYSICAL",
                "startsAt": ts(start), "endsAt": ts(start + Duration::minutes(30)),
                "recurrence": { "frequency": "DAILY", "count": 2 },
                "remindersMinutesBefore": [60],
            }),
        )
        .await;
    let ev = id(&e);
    assert_eq!(e["myReminders"], json!([60]));
    let r = w
        .put(
            alice,
            &format!("/v1/messaging/events/{ev}/reminders"),
            json!({ "minutesBefore": [60, 10, 60] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["minutesBefore"], json!([10, 60]));

    // Nothing is due yet.
    assert_eq!(
        messaging::calendar::notices::fire_due(&w.h.state, 100)
            .await
            .unwrap(),
        0
    );
    // One hour before the first occurrence: the 60-minute reminder fires, once.
    w.h.clock.advance(Duration::hours(1) + Duration::seconds(1));
    assert_eq!(
        messaging::calendar::notices::fire_due(&w.h.state, 100)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        messaging::calendar::notices::fire_due(&w.h.state, 100)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_event_notices WHERE kind = 'REMINDER'")
            .await,
        1
    );
    // It moved on to the next day's occurrence.
    assert_eq!(
        w.h.scalar(
            "SELECT count(*) FROM messaging_event_reminders WHERE minutes_before = 60 \
               AND next_fire_at > now() + interval '20 hours'"
        )
        .await,
        1
    );
    // Moving the event reschedules; cancelling stops every reminder.
    let r = w
        .post(
            alice,
            &format!("/v1/messaging/events/{ev}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_event_reminders WHERE next_fire_at IS NOT NULL")
            .await,
        0
    );
    let r = w
        .put(
            alice,
            &format!("/v1/messaging/events/{ev}/reminders"),
            json!({ "minutesBefore": [40321] }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
}

#[tokio::test]
async fn ics_export_of_one_event_and_the_callers_calendar() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob, carol) = (w.person(), w.person(), w.person());
    w.hub.connect(alice, bob, w.h.now());
    let e = w
        .create(
            alice,
            w.body(
                24,
                json!({ "inviteePrincipalIds": [bob], "recurrence": { "frequency": "WEEKLY", "interval": 2, "count": 4 } }),
            ),
        )
        .await;
    let ev = id(&e);
    let other = w
        .create(carol, w.body(30, json!({ "title": "Carol only" })))
        .await;
    let r = w.get(bob, &format!("/v1/messaging/events/{ev}/ics")).await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(r.json["contentType"], "text/calendar; charset=utf-8");
    assert_eq!(r.json["filename"], "Dinner-at-Ada-s-bring-cake.ics");
    let ics = r.json["content"].as_str().unwrap();
    assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(ics.contains(&format!("UID:{ev}@events.cfmeet.com\r\n")));
    assert!(ics.contains("RRULE:FREQ=WEEKLY;INTERVAL=2;COUNT=4\r\n"));
    assert!(ics.contains("SUMMARY:Dinner\\, at Ada's\\; bring cake\r\n"));
    assert!(!ics.contains(&alice.to_string()), "no principal ids");
    let r = w
        .get(carol, &format!("/v1/messaging/events/{ev}/ics"))
        .await;
    assert_eq!(r.status.as_u16(), 404, "{}", r.text);

    let r = w.get(bob, "/v1/messaging/events/calendar").await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    let feed = r.json["content"].as_str().unwrap();
    assert!(feed.contains(&format!("UID:{ev}@")));
    assert!(!feed.contains(&format!("UID:{}@", id(&other))));
}

#[tokio::test]
async fn creation_is_idempotent_per_key_and_principal() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob) = (w.person(), w.person());
    let body = w.body(6, json!({}));
    let key = "events-key-0001";
    let send = |who: Uuid, b: Value| {
        let w = &w;
        async move {
            w.h.send(
                Method::POST,
                Some(&w.c.token),
                Some(who),
                "/v1/messaging/events",
                Some(b),
                &[("idempotency-key", key)],
            )
            .await
        }
    };
    let a = send(alice, body.clone()).await;
    assert_eq!(a.status.as_u16(), 201, "{}", a.text);
    let b = send(alice, body.clone()).await;
    assert_eq!(b.status.as_u16(), 200, "{}", b.text);
    assert_eq!(a.json["eventId"], b.json["eventId"]);
    let mut changed = body.clone();
    changed["title"] = json!("Another");
    let c = send(alice, changed).await;
    assert_eq!(c.status.as_u16(), 422, "{}", c.text);
    assert_eq!(c.code(), "IDEMPOTENCY_KEY_REUSED");
    let d = send(bob, body).await;
    assert_eq!(
        d.status.as_u16(),
        422,
        "another principal's reuse: {}",
        d.text
    );
    assert_eq!(w.h.scalar("SELECT count(*) FROM messaging_events").await, 1);
}

#[tokio::test]
async fn cover_photos_use_the_protected_media_store_and_follow_the_event_gate() {
    let Some(w) = world().await else {
        return;
    };
    let (alice, bob, carol) = (w.person(), w.person(), w.person());
    w.hub.connect(alice, bob, w.h.now());
    // Only photos.
    let r = w
        .post(
            alice,
            "/v1/messaging/events/media",
            json!({ "kind": "VIDEO", "contentType": "video/mp4", "sizeBytes": 10, "durationMs": 1000 }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 400, "{}", r.text);
    let r = w
        .post(
            alice,
            "/v1/messaging/events/media",
            json!({ "kind": "IMAGE", "contentType": "image/png", "sizeBytes": 64 }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 201, "{}", r.text);
    let media = r.json["mediaId"].as_str().unwrap().to_string();
    // An unfinished upload cannot be a cover.
    let r = w
        .post(
            alice,
            "/v1/messaging/events",
            w.body(5, json!({ "coverMediaId": media })),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "{}", r.text);
    // Somebody else's media is not found.
    let r = w
        .get(bob, &format!("/v1/messaging/events/media/{media}"))
        .await;
    assert_eq!(r.status.as_u16(), 404, "{}", r.text);
    let r = w
        .post(
            carol,
            &format!("/v1/messaging/events/media/{media}/download-grants"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 404, "{}", r.text);
    let r = w
        .post(
            alice,
            &format!("/v1/messaging/events/media/{media}/parts/0"),
            json!({ "sha256": "0".repeat(64) }),
        )
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    let r = w
        .post(
            alice,
            &format!("/v1/messaging/events/media/{media}/complete"),
            json!({}),
        )
        .await;
    assert_eq!(r.status.as_u16(), 409, "parts missing: {}", r.text);
    let r = w
        .delete(alice, &format!("/v1/messaging/events/media/{media}"))
        .await;
    assert_eq!(r.status.as_u16(), 200, "{}", r.text);
    assert_eq!(
        w.h.scalar("SELECT count(*) FROM messaging_social_media WHERE purpose = 'EVENT'")
            .await,
        0
    );
}

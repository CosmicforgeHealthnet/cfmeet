//! Roadmap N10 (Document 5 §6, Phase 4.2; Document 7 M-09 / F-21): the copy of the Hub's
//! notification preferences, the catch-up of later Hub changes, the verification counts,
//! and the Hub facade's access to U6 / U7.
//!
//! The Hub owner API (`exportNotificationPreferences` / `listNotificationPreferenceChanges`,
//! contract `communications/notification-owner/v1`) is an in-memory fake with the same
//! paging, feed-position and current-value semantics as `services/communications`
//! (`preference_authority.rs`, tested there against the real Hub database).

mod common;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::http::Method;
use cf_notification_client::Producer;
use common::*;
use notifications::preference_copy::{
    self, ChangePage, ExportPage, HubError, HubPreferenceSource, Tick,
};
use serde_json::{json, Value};
use uuid::Uuid;

const SWITCHES: [&str; 7] = [
    "directMessages",
    "groupMessages",
    "incomingCalls",
    "missedCalls",
    "contactRequests",
    "groupInvitations",
    "meetingInvitations",
];

#[derive(Default)]
struct HubData {
    /// key (sort order of the Hub cursor) → exported item
    accounts: BTreeMap<String, Value>,
    conversations: BTreeMap<String, Value>,
    contacts: BTreeMap<String, Value>,
    /// (seq, entry without its current value, settled)
    feed: Vec<(i64, Value, bool)>,
    seq: i64,
    down: bool,
    moved: bool,
    /// Fail every export after this many successful export calls (interruption).
    fail_after: Option<usize>,
    exports: usize,
}

#[derive(Default, Clone)]
struct FakeHub(Arc<Mutex<HubData>>);

fn account(r: &str, version: i64, off: &[&str]) -> Value {
    let mut v = json!({ "accountRef": r, "version": version });
    for k in SWITCHES {
        v[k] = json!(!off.contains(&k));
    }
    v
}

impl FakeHub {
    fn with<R>(&self, f: impl FnOnce(&mut HubData) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
    fn log(d: &mut HubData, entry: Value) {
        d.seq += 1;
        let seq = d.seq;
        d.feed.push((seq, entry, false));
    }
    fn set_account(&self, r: &str, version: i64, off: &[&str]) {
        self.with(|d| {
            d.accounts.insert(r.to_owned(), account(r, version, off));
            Self::log(d, json!({ "kind": "ACCOUNT", "accountRef": r }));
        })
    }
    fn set_conversation(&self, r: &str, c: Uuid, level: &str, muted: Option<&str>, version: i64) {
        self.with(|d| {
            d.conversations.insert(
                format!("{r}:{c}"),
                json!({ "accountRef": r, "conversationId": c, "notifications": level, "mutedUntil": muted, "version": version }),
            );
            Self::log(d, json!({ "kind": "CONVERSATION", "accountRef": r, "conversationId": c }));
        })
    }
    fn remove_conversation(&self, r: &str, c: Uuid) {
        self.with(|d| {
            d.conversations.remove(&format!("{r}:{c}"));
            Self::log(
                d,
                json!({ "kind": "CONVERSATION", "accountRef": r, "conversationId": c }),
            );
        })
    }
    fn set_contact(&self, r: &str, contact: &str, level: &str, version: i64) {
        self.with(|d| {
            d.contacts.insert(
                format!("{r}:{contact}"),
                json!({ "accountRef": r, "contactRef": contact, "notifications": level, "version": version }),
            );
            Self::log(d, json!({ "kind": "CONTACT", "accountRef": r, "contactRef": contact }));
        })
    }
    fn settle_all(&self) {
        self.with(|d| d.feed.iter_mut().for_each(|e| e.2 = true))
    }
    fn positions(d: &HubData) -> (i64, i64, i64) {
        let latest = d.feed.last().map(|e| e.0).unwrap_or(0);
        let settled = d
            .feed
            .iter()
            .take_while(|e| e.2)
            .last()
            .map(|e| e.0)
            .unwrap_or(0);
        let oldest = d.feed.first().map(|e| e.0).unwrap_or(0);
        (latest, settled, oldest)
    }
}

#[async_trait]
impl HubPreferenceSource for FakeHub {
    async fn export(
        &self,
        kind: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<ExportPage, HubError> {
        self.with(|d| {
            if d.moved {
                return Err(HubError::AuthorityMoved);
            }
            if d.down || d.fail_after.is_some_and(|n| d.exports >= n) {
                return Err(HubError::Unavailable);
            }
            d.exports += 1;
            let (latest, settled, oldest) = Self::positions(d);
            let rows = match kind {
                "ACCOUNT" => &d.accounts,
                "CONVERSATION" => &d.conversations,
                _ => &d.contacts,
            };
            let mut page: Vec<(&String, &Value)> = rows
                .iter()
                .filter(|(k, _)| after.is_none_or(|a| k.as_str() > a))
                .take(limit as usize + 1)
                .collect();
            let more = page.len() > limit as usize;
            page.truncate(limit as usize);
            Ok(ExportPage {
                next_cursor: if more {
                    page.last().map(|(k, _)| (*k).clone())
                } else {
                    None
                },
                items: page.into_iter().map(|(_, v)| v.clone()).collect(),
                feed_position: latest,
                settled_position: settled,
                oldest_position: oldest,
            })
        })
    }

    async fn changes(&self, after: i64, limit: u32) -> Result<ChangePage, HubError> {
        self.with(|d| {
            if d.moved {
                return Err(HubError::AuthorityMoved);
            }
            if d.down {
                return Err(HubError::Unavailable);
            }
            let (latest, settled, oldest) = Self::positions(d);
            let mut entries: Vec<(i64, Value)> = d
                .feed
                .iter()
                .filter(|e| e.0 > after)
                .take(limit as usize + 1)
                .map(|e| (e.0, e.1.clone()))
                .collect();
            let more = entries.len() > limit as usize;
            entries.truncate(limit as usize);
            let next_after = entries.last().map(|e| e.0).unwrap_or(after);
            // Each entry carries the CURRENT value, as the Hub resolves it.
            let items = entries
                .into_iter()
                .map(|(seq, mut e)| {
                    let r = e["accountRef"].as_str().unwrap().to_owned();
                    let current = match e["kind"].as_str().unwrap() {
                        "ACCOUNT" => d.accounts.get(&r).cloned(),
                        "CONVERSATION" => d
                            .conversations
                            .get(&format!("{r}:{}", e["conversationId"].as_str().unwrap()))
                            .cloned(),
                        _ => d
                            .contacts
                            .get(&format!("{r}:{}", e["contactRef"].as_str().unwrap()))
                            .cloned(),
                    };
                    e["seq"] = json!(seq);
                    e["preference"] = current.unwrap_or(Value::Null);
                    e
                })
                .collect();
            Ok(ChangePage {
                items,
                next_after,
                more,
                feed_position: latest,
                settled_position: settled,
                oldest_position: oldest,
            })
        })
    }
}

fn user(n: u32) -> String {
    format!("cfmeet-user:00000000-0000-4000-8000-{n:012}")
}

async fn run_mode(h: &Harness) {
    h.exec("UPDATE notif_preference_copy SET mode = 'RUN', version = version + 1, changed_by_run = 'test'")
        .await
        .unwrap();
}

/// Ticks until the snapshot completes; returns the number of snapshot ticks.
async fn snapshot(h: &Harness, hub: &FakeHub) -> usize {
    for n in 1..=200 {
        match preference_copy::tick(&h.state.db, hub).await.unwrap() {
            Tick::Snapshot { done: true, .. } => return n,
            Tick::Snapshot { .. } => {}
            other => panic!("unexpected tick {other:?}"),
        }
    }
    panic!("snapshot did not finish");
}

async fn verify(h: &Harness, hub: &FakeHub) -> Value {
    h.exec("UPDATE notif_preference_copy SET verify_requested_at = clock_timestamp()")
        .await
        .unwrap();
    assert!(matches!(
        preference_copy::tick(&h.state.db, hub).await.unwrap(),
        Tick::Verified { .. }
    ));
    preference_copy::state(&h.state.db)
        .await
        .unwrap()
        .verification
        .unwrap()
}

async fn row(h: &Harness, sql: &str) -> String {
    h.text(sql).await.unwrap_or_default()
}

#[tokio::test]
async fn the_copy_is_lossless_versioned_and_never_resets_a_preference() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let hub = FakeHub::default();
    // 1 201 accounts (three pages), one with every switch off, one with the defaults.
    for n in 1..=1201u32 {
        let off: &[&str] = match n {
            1 => &SWITCHES,
            2 => &[],
            _ if n % 3 == 0 => &["contactRequests", "meetingInvitations"],
            _ => &["directMessages"],
        };
        // User 3 already has a service row (version 1): its Hub version is lower.
        let version = if n == 3 { 1 } else { 1 + (n as i64 % 4) };
        hub.set_account(&user(n), version, off);
    }
    let conversation = Uuid::new_v4();
    hub.set_conversation(
        &user(1),
        conversation,
        "MUTED",
        Some("2027-01-01T08:00:00+00:00"),
        3,
    );
    hub.set_conversation(&user(1), Uuid::new_v4(), "MENTIONS", None, 1);
    hub.set_conversation(&user(2), Uuid::new_v4(), "ALL", None, 2);
    hub.set_contact(&user(1), &user(2), "MUTED", 4);
    hub.set_contact(&user(2), &user(1), "ALL", 1);
    // A service-only setting made before the copy (quiet hours) must survive it.
    h.user(
        &user(3),
        "PUT",
        "/preferences",
        None,
        Some(
            json!({"expectedVersion": 0, "categories": {"voicemail": false},
        "quietHours": {"enabled": true, "start": "22:00", "end": "07:00", "days": []}}),
        ),
    )
    .await
    .expect(200);

    // OFF by default: nothing is read or written.
    assert_eq!(
        preference_copy::tick(&h.state.db, &hub).await.unwrap(),
        Tick::Off
    );
    assert_eq!(hub.with(|d| d.exports), 0);
    run_mode(&h).await;
    let ticks = snapshot(&h, &hub).await;
    assert_eq!(
        ticks,
        3 + 1 + 1,
        "three account pages, one page of each other kind"
    );

    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_preferences WHERE source_version IS NOT NULL")
            .await,
        1201
    );
    // Values and versions: the service version equals the Hub version for copied rows.
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_preferences WHERE version = source_version")
            .await,
        1200
    );
    assert_eq!(
        row(&h, &format!("SELECT concat_ws(',', direct_messages, group_messages, incoming_calls, missed_calls, contact_requests, group_invitations, meeting_invitations, version, source_version) FROM notif_preferences WHERE account_ref = '{}'", user(1))).await,
        "f,f,f,f,f,f,f,2,2"
    );
    assert_eq!(
        row(&h, &format!("SELECT concat_ws(',', direct_messages, contact_requests, meeting_invitations, version) FROM notif_preferences WHERE account_ref = '{}'", user(3))).await,
        "t,f,f,2",
        "the pre-existing service row moves forward (version 1 -> 2) and keeps its own fields"
    );
    // U6 shows the copied switches; the service-only settings were not reset.
    let doc = h.user(&user(3), "GET", "/preferences", None, None).await;
    let doc = doc.expect(200);
    assert_eq!(doc["categories"]["contactRequests"], false);
    assert_eq!(doc["categories"]["voicemail"], false);
    assert_eq!(doc["quietHours"]["start"], "22:00");
    assert_eq!(doc["securityEvents"], true);
    let doc = h.user(&user(1), "GET", "/preferences", None, None).await;
    let doc = doc.expect(200);
    assert_eq!(doc["version"], 2);
    assert_eq!(
        doc["conversations"][conversation.to_string()]["notifications"],
        "MUTED"
    );
    assert_eq!(
        doc["conversations"][conversation.to_string()]["mutedUntil"],
        "2027-01-01T08:00:00Z"
    );
    assert_eq!(doc["contacts"][user(2)]["notifications"], "MUTED");
    // No preference outside the Hub rows is invented (absent stays absent = enabled).
    assert_eq!(
        h.count(&format!(
            "SELECT count(*)::bigint FROM notif_preferences WHERE account_ref = '{}'",
            user(5000)
        ))
        .await,
        0
    );
    // Copies are not service-side writes: the rollback change log holds only U7's write.
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_preference_change_log")
            .await,
        1
    );
    let s = preference_copy::state(&h.state.db).await.unwrap();
    assert_eq!(
        (s.phase.as_str(), s.rows_written, s.rows_skipped),
        ("CATCH_UP", 1206, 0)
    );

    // The policy engine reads the copied value: user 1 turned contact requests off at the
    // Hub, so the contact request becomes a silent in-app item and nothing is pushed.
    h.set_category("CONTACT_REQUESTS", "ACTIVE").await;
    h.owners.contacts.lock().unwrap().insert(
        user(1),
        notifications::authorities::AccountContact {
            account_ref: user(1),
            status: "ACTIVE".into(),
            email: None,
            display_name: None,
            locale: None,
            time_zone: None,
            devices: vec![],
        },
    );
    let peer = h.peers.communications.clone();
    h.ingest(
        &peer,
        vec![envelope(
            Producer::Communications,
            "communications.connection.requested",
            &[user(1)],
            &[
                ("actorRef", json!(user(2))),
                ("connectionId", json!(Uuid::new_v4())),
            ],
            None,
        )],
    )
    .await
    .expect(200);
    h.process_all().await;
    assert_eq!(
        h.count(&format!(
            "SELECT count(*)::bigint FROM notif_inbox_items WHERE recipient_ref = '{}' AND silent",
            user(1)
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn the_copy_is_idempotent_and_never_moves_a_value_backwards() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let hub = FakeHub::default();
    hub.set_account(&user(1), 5, &["groupMessages"]);
    hub.set_contact(&user(1), &user(2), "MUTED", 2);
    run_mode(&h).await;
    snapshot(&h, &hub).await;
    // A second full snapshot (the operator re-runs the copy) writes nothing.
    h.exec("UPDATE notif_preference_copy SET phase = 'SNAPSHOT', snapshot_kind = 'ACCOUNT', snapshot_cursor = NULL")
        .await
        .unwrap();
    snapshot(&h, &hub).await;
    let s = preference_copy::state(&h.state.db).await.unwrap();
    assert_eq!(
        (s.rows_written, s.rows_unchanged, s.snapshots_completed),
        (2, 2, 2)
    );
    assert_eq!(
        row(&h, &format!("SELECT concat_ws(',', group_messages, version, source_version) FROM notif_preferences WHERE account_ref = '{}'", user(1))).await,
        "f,5,5"
    );
    // A replayed OLDER Hub row (an overlapping page, a late entry) changes nothing.
    let older: notifications::preferences::HubPreferences =
        serde_json::from_value(account(&user(1), 4, &[])).unwrap();
    assert!(
        !notifications::preferences::copy_account(&h.state.db, &older)
            .await
            .unwrap()
    );
    assert_eq!(
        row(&h, &format!("SELECT concat_ws(',', group_messages, version) FROM notif_preferences WHERE account_ref = '{}'", user(1))).await,
        "f,5"
    );
}

#[tokio::test]
async fn an_interrupted_copy_resumes_where_it_stopped() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let hub = FakeHub::default();
    for n in 1..=1100u32 {
        hub.set_account(&user(n), 1, &["missedCalls"]);
    }
    run_mode(&h).await;
    hub.with(|d| d.fail_after = Some(1));
    assert!(matches!(
        preference_copy::tick(&h.state.db, &hub).await.unwrap(),
        Tick::Snapshot {
            written: 500,
            done: false,
            ..
        }
    ));
    // The Hub goes away mid-snapshot (or this replica stops): the error is recorded and
    // the committed cursor stays.
    assert_eq!(
        preference_copy::tick(&h.state.db, &hub).await.unwrap(),
        Tick::HubError(HubError::Unavailable)
    );
    let s = preference_copy::state(&h.state.db).await.unwrap();
    assert_eq!(s.last_error.as_deref(), Some("HUB_UNAVAILABLE"));
    assert_eq!(s.snapshot_cursor.as_deref(), Some(user(500).as_str()));
    // Another replica holding the row makes this one wait (single runner).
    {
        use sea_orm::TransactionTrait;
        let other = h.state.db.begin().await.unwrap();
        sea_orm::ConnectionTrait::execute_unprepared(
            &other,
            "SELECT 1 FROM notif_preference_copy FOR UPDATE",
        )
        .await
        .unwrap();
        assert_eq!(
            preference_copy::tick(&h.state.db, &hub).await.unwrap(),
            Tick::Busy
        );
        other.rollback().await.unwrap();
    }
    hub.with(|d| d.fail_after = None);
    snapshot(&h, &hub).await;
    let s = preference_copy::state(&h.state.db).await.unwrap();
    // Every row exactly once: nothing re-copied, nothing missed.
    assert_eq!(
        (s.rows_written, s.rows_unchanged, s.last_error),
        (1100, 0, None)
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_preferences WHERE NOT missed_calls AND source_version = 1").await,
        1100
    );
}

#[tokio::test]
async fn the_catch_up_applies_every_hub_change_made_after_the_copy_began() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let hub = FakeHub::default();
    for n in 1..=600u32 {
        hub.set_account(&user(n), 1, &[]);
    }
    let c1 = Uuid::new_v4();
    hub.set_conversation(&user(1), c1, "MUTED", None, 1);
    hub.settle_all();
    run_mode(&h).await;
    // First page copied (users 1..500); then the user changes an already-copied row.
    assert!(matches!(
        preference_copy::tick(&h.state.db, &hub).await.unwrap(),
        Tick::Snapshot { written: 500, .. }
    ));
    hub.set_account(&user(1), 2, &["directMessages"]);
    snapshot(&h, &hub).await;
    assert_eq!(
        row(
            &h,
            &format!(
                "SELECT direct_messages::text FROM notif_preferences WHERE account_ref = '{}'",
                user(1)
            )
        )
        .await,
        "true",
        "the snapshot page already passed user 1"
    );
    // Later changes of every kind, including a conversation row the Hub removed.
    hub.set_account(&user(600), 2, &["incomingCalls"]);
    hub.set_conversation(&user(2), Uuid::new_v4(), "MENTIONS", None, 1);
    hub.set_contact(&user(3), &user(4), "MUTED", 3);
    hub.remove_conversation(&user(1), c1);
    assert_eq!(
        preference_copy::tick(&h.state.db, &hub).await.unwrap(),
        Tick::CatchUp { applied: 5 }
    );
    assert_eq!(
        row(&h, &format!("SELECT concat_ws(',', direct_messages, version, source_version) FROM notif_preferences WHERE account_ref = '{}'", user(1))).await,
        "f,2,2"
    );
    assert_eq!(
        row(
            &h,
            &format!(
                "SELECT incoming_calls::text FROM notif_preferences WHERE account_ref = '{}'",
                user(600)
            )
        )
        .await,
        "false"
    );
    assert_eq!(
        row(&h, &format!("SELECT notifications FROM notif_contact_preferences WHERE account_ref = '{}' AND contact_ref = '{}'", user(3), user(4))).await,
        "MUTED"
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*)::bigint FROM notif_conversation_preferences WHERE account_ref = '{}'",
            user(1)
        ))
        .await,
        0
    );
    // The cursor never passes an unsettled entry: they are read again (harmlessly)...
    let settled_before = preference_copy::state(&h.state.db)
        .await
        .unwrap()
        .feed_cursor;
    assert_eq!(settled_before, Some(601));
    assert_eq!(
        preference_copy::tick(&h.state.db, &hub).await.unwrap(),
        Tick::CatchUp { applied: 0 }
    );
    // ...and once settled the cursor moves to the head.
    hub.settle_all();
    preference_copy::tick(&h.state.db, &hub).await.unwrap();
    let head = hub.with(|d| d.seq);
    assert_eq!(
        preference_copy::state(&h.state.db)
            .await
            .unwrap()
            .feed_cursor,
        Some(head)
    );
    // A pruned feed window (cursor older than the oldest retained entry) re-runs the
    // snapshot instead of silently skipping changes.
    hub.with(|d| {
        d.feed.retain(|e| e.0 > head);
        d.seq += 10;
    });
    hub.set_account(&user(7), 2, &["voicemail"]);
    preference_copy::tick(&h.state.db, &hub).await.unwrap();
    let s = preference_copy::state(&h.state.db).await.unwrap();
    assert_eq!(
        (s.phase.as_str(), s.last_error.as_deref()),
        ("SNAPSHOT", Some("FEED_PRUNED_RESYNC"))
    );
}

#[tokio::test]
async fn verification_counts_match_only_when_every_hub_row_is_held() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let hub = FakeHub::default();
    for n in 1..=3u32 {
        hub.set_account(&user(n), 1, &["groupInvitations"]);
    }
    hub.set_conversation(&user(1), Uuid::new_v4(), "MUTED", None, 1);
    hub.set_contact(&user(1), &user(2), "MENTIONS", 1);
    run_mode(&h).await;
    snapshot(&h, &hub).await;
    let report = verify(&h, &hub).await;
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(
        report["account"],
        json!({"hubRows":3,"serviceRows":3,"equal":3,"different":0,"skipped":0,"writtenByThisPass":0,"complete":true})
    );
    assert_eq!(report["conversation"]["equal"], 1);
    assert_eq!(report["contact"]["equal"], 1);

    // Drift (a row changed in the service without a newer Hub version) and a Hub row the
    // service cannot hold are both reported; the counts no longer match.
    h.exec(&format!(
        "UPDATE notif_preferences SET group_invitations = true WHERE account_ref = '{}'",
        user(2)
    ))
    .await
    .unwrap();
    hub.with(|d| {
        d.accounts
            .insert("zz".into(), account("someone@example.com", 1, &[]));
    });
    let report = verify(&h, &hub).await;
    assert_eq!(report["complete"], false, "{report}");
    assert_eq!(report["account"]["different"], 1);
    assert_eq!(report["account"]["skipped"], 1);
    assert_eq!(report["account"]["hubRows"], 4);

    // A newer Hub row fixes the drift in the final pass; the invalid row is removed at the
    // Hub. The final pass inside the freeze is exact, even for changes the catch-up missed.
    hub.with(|d| {
        d.accounts.remove("zz");
    });
    hub.set_account(&user(2), 2, &["groupInvitations"]);
    hub.set_contact(&user(3), &user(1), "MUTED", 1);
    let report = verify(&h, &hub).await;
    assert_eq!(report["complete"], true, "{report}");
    assert_eq!(report["account"]["writtenByThisPass"], 1);
    assert_eq!(report["contact"]["writtenByThisPass"], 1);
    assert_eq!(report["contact"]["hubRows"], 2);
    // Verification is not repeated until requested again.
    assert!(matches!(
        preference_copy::tick(&h.state.db, &hub).await.unwrap(),
        Tick::CatchUp { .. }
    ));
}

#[tokio::test]
async fn the_copy_stops_once_the_hub_facade_points_here() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let hub = FakeHub::default();
    hub.set_account(&user(1), 3, &["directMessages"]);
    hub.with(|d| d.moved = true);
    run_mode(&h).await;
    assert_eq!(
        preference_copy::tick(&h.state.db, &hub).await.unwrap(),
        Tick::HubError(HubError::AuthorityMoved)
    );
    assert_eq!(
        preference_copy::state(&h.state.db)
            .await
            .unwrap()
            .last_error
            .as_deref(),
        Some("PREFERENCE_AUTHORITY_MOVED")
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_preferences")
            .await,
        0
    );
}

/// I2 as the Hub facade signs it.
async fn facade(h: &Harness, method: &str, path: &str, body: Option<Value>) -> Response {
    let mut d = json!({ "method": method, "path": path, "account": user(1), "idempotencyKey": Uuid::new_v4().to_string() });
    if let Some(b) = body {
        d["body"] = b;
    }
    let peer = h.peers.communications.clone();
    h.signed(
        &peer,
        Method::POST,
        "/internal/v1/notifications/dispatch",
        Some(d),
        &[],
    )
    .await
}

#[tokio::test]
async fn the_hub_facade_reaches_only_u6_and_u7_and_security_stays_fixed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let doc = facade(&h, "GET", "/preferences", None).await;
    assert_eq!(doc.expect(200)["securityEvents"], true);
    // Inbox and push operations are refused to the Hub.
    for (m, p) in [
        ("GET", ""),
        ("GET", "/unread-count"),
        ("POST", "/read"),
        ("PUT", "/devices/00000000-0000-4000-8000-000000000001/push"),
    ] {
        facade(&h, m, p, Some(json!({})))
            .await
            .expect_error(403, "FORBIDDEN");
    }
    // Security is not a preference: it cannot be stored, through any caller.
    facade(
        &h,
        "PUT",
        "/preferences",
        Some(json!({"expectedVersion": 0, "categories": {"security": false}})),
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    facade(
        &h,
        "PUT",
        "/preferences",
        Some(json!({"expectedVersion": 0, "securityEvents": false})),
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    let put = facade(
        &h,
        "PUT",
        "/preferences",
        Some(json!({"expectedVersion": 0, "categories": {"directMessages": false}})),
    )
    .await;
    assert_eq!(put.expect(200)["version"], 1);
    assert_eq!(put.json["securityEvents"], true);
    h.user(
        &user(1),
        "PUT",
        "/preferences",
        None,
        Some(json!({"expectedVersion": 1, "categories": {"voicemail": false}})),
    )
    .await
    .expect(200);
    // The change log tells facade writes (mirrored in the Hub) from direct U7 writes
    // (written back on a rollback).
    assert_eq!(
        row(
            &h,
            "SELECT string_agg(origin, ',' ORDER BY version) FROM notif_preference_change_log"
        )
        .await,
        "HUB_FACADE,RELAY"
    );
}

//! Part V I §§119–122 — query-plan evidence. Every hot path of the new APIs and workers
//! is served by an index of Messaging's own schema: with sequential scans priced out
//! (`enable_seqscan = off`, so a missing index shows up as a Seq Scan on the table), the
//! plan of each query must not scan the table it reads. Each plan's index is printed as
//! `PLAN {json}` for the evidence record.

mod common;

use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

fn walk<'a>(plan: &'a Value, out: &mut Vec<(&'a str, Option<&'a str>, Option<&'a str>)>) {
    out.push((
        plan["Node Type"].as_str().unwrap_or(""),
        plan["Relation Name"].as_str(),
        plan["Index Name"].as_str(),
    ));
    if let Some(children) = plan["Plans"].as_array() {
        for c in children {
            walk(c, out);
        }
    }
}

#[tokio::test]
async fn hot_queries_are_index_served() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.first_party(false);
    let (conv, people) = h.managed_group(&c, 3).await;
    let cid = conv["conversationId"].as_str().unwrap().to_string();
    let path = format!("/v1/conversations/{cid}/messages");
    let root = h.send_managed(&c, &conv, people[0], "root").await;
    let root_id = root.json["messageId"].as_str().unwrap().to_string();
    for i in 0..40 {
        let body = if i % 3 == 0 {
            json!({ "clientMessageId": Uuid::new_v4(), "text": "reply", "replyToMessageId": root_id })
        } else {
            json!({ "clientMessageId": Uuid::new_v4(), "text": "message" })
        };
        h.post(&c, Some(people[i % 3]), &path, body).await;
    }
    h.put(
        &c,
        Some(people[1]),
        &format!("{path}/{root_id}/reactions/:tada:"),
        json!({}),
    )
    .await;
    h.put(
        &c,
        Some(people[0]),
        &format!("/v1/conversations/{cid}/pins/{root_id}"),
        json!({}),
    )
    .await;
    let env = c.context.scope.environment_id;
    let (p1, p2) = (people[1], people[2]);
    h.sql("ANALYZE").await.unwrap();

    let queries: Vec<(&str, &str, String)> = vec![
        ("message history page", "messaging_messages",
         format!("SELECT id FROM messaging_messages WHERE conversation_id = '{cid}' AND sequence > 5 ORDER BY sequence LIMIT 50")),
        ("history before (newest first)", "messaging_messages",
         format!("SELECT id FROM messaging_messages WHERE conversation_id = '{cid}' AND sequence < 30 ORDER BY sequence DESC LIMIT 50")),
        ("thread", "messaging_messages",
         format!("SELECT id FROM messaging_messages WHERE conversation_id = '{cid}' AND thread_root_id = '{root_id}' ORDER BY sequence")),
        ("idempotent send lookup", "messaging_messages",
         format!("SELECT id FROM messaging_messages WHERE conversation_id = '{cid}' AND sender_principal_id = '{p1}' AND client_message_id = '{}'", Uuid::new_v4())),
        ("unread count", "messaging_messages",
         format!("SELECT count(*) FROM messaging_messages WHERE conversation_id = '{cid}' AND sequence > 10 AND sender_principal_id <> '{p2}'")),
        ("reactions of a message", "messaging_message_reactions",
         format!("SELECT reaction, count(*) FROM messaging_message_reactions WHERE message_id = '{root_id}' GROUP BY reaction")),
        ("pins of a conversation", "messaging_message_pins",
         format!("SELECT message_id FROM messaging_message_pins WHERE conversation_id = '{cid}'")),
        ("membership check", "messaging_conversation_members",
         format!("SELECT role FROM messaging_conversation_members WHERE conversation_id = '{cid}' AND principal_id = '{p1}'")),
        ("principal's conversations", "messaging_conversation_members",
         format!("SELECT conversation_id FROM messaging_conversation_members WHERE environment_id = '{env}' AND principal_id = '{p1}'")),
        ("read cursor", "messaging_read_cursors",
         format!("SELECT read_sequence FROM messaging_read_cursors WHERE conversation_id = '{cid}' AND principal_id = '{p1}'")),
        ("usage summary window", "messaging_usage_records",
         format!("SELECT usage_type, sum(quantity) FROM messaging_usage_records WHERE environment_id = '{env}' AND occurred_at >= now() - interval '1 day' AND occurred_at < now() GROUP BY usage_type")),
        ("outbox due", "messaging_outbox_events",
         "SELECT id FROM messaging_outbox_events WHERE status = 'PENDING' AND next_attempt_at <= now() ORDER BY next_attempt_at, sequence LIMIT 500".into()),
        ("conversation event replay", "messaging_outbox_events",
         format!("SELECT id FROM messaging_outbox_events WHERE conversation_id = '{cid}' AND conversation_event_sequence > 3 ORDER BY conversation_event_sequence LIMIT 100")),
        ("webhook relay due", "messaging_webhook_relay",
         "SELECT event_id FROM messaging_webhook_relay WHERE status = 'PENDING' AND next_attempt_at <= now() ORDER BY next_attempt_at LIMIT 1".into()),
        ("SMS transports due", "messaging_sms_transports",
         "SELECT message_id FROM messaging_sms_transports WHERE next_attempt_at IS NOT NULL AND next_attempt_at <= now() ORDER BY next_attempt_at LIMIT 1".into()),
    ];
    let db = h.conn();
    let mut failures = Vec::new();
    for (name, table, sql) in &queries {
        let txn = db.begin().await.unwrap();
        txn.execute_unprepared("SET LOCAL enable_seqscan = off")
            .await
            .unwrap();
        let row = txn
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!("EXPLAIN (FORMAT JSON) {sql}"),
            ))
            .await
            .unwrap()
            .unwrap();
        txn.rollback().await.unwrap();
        let plan: Value = row.try_get_by_index(0).unwrap();
        let mut nodes = Vec::new();
        walk(&plan[0]["Plan"], &mut nodes);
        let seq = nodes
            .iter()
            .any(|(n, r, _)| *n == "Seq Scan" && *r == Some(*table));
        // Each query reads one table; bitmap index scans carry no relation name.
        let indexes: Vec<&str> = nodes.iter().filter_map(|(_, _, i)| *i).collect();
        println!(
            "PLAN {}",
            json!({ "query": name, "table": table, "indexes": indexes, "seq_scan": seq })
        );
        if seq || indexes.is_empty() {
            failures.push(format!("{name}: {plan}"));
        }
    }
    assert!(
        failures.is_empty(),
        "not index-served:\n{}",
        failures.join("\n")
    );
}

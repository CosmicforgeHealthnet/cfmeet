//! DATA-017 — Messaging's report-only reference reconciliation: invalid foreign
//! references are REPORTED (a member recorded in another environment than its
//! conversation, an environment recorded under two tenants), nothing is repaired or
//! deleted.

mod common;

use common::{flows::*, *};
use messaging::reference_reconciliation::{report, ReferenceCheck, ReferenceReport};
use uuid::Uuid;

fn check<'a>(r: &'a ReferenceReport, name: &str) -> &'a ReferenceCheck {
    r.checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{name}"))
}

const TABLES: &[&str] = &[
    "messaging_conversations",
    "messaging_conversation_members",
    "messaging_principal_restrictions",
];

async fn counts(h: &Harness) -> Vec<i64> {
    let mut out = Vec::new();
    for t in TABLES {
        out.push(h.scalar(&format!("SELECT count(*) FROM {t}")).await);
    }
    out
}

#[tokio::test]
async fn invalid_foreign_references_are_reported_and_nothing_is_deleted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let (conv, people) = h.managed_group(&c, 3).await;
    let cid = conv["conversationId"].as_str().unwrap();

    let clean = report(h.conn()).await.unwrap();
    assert_eq!(clean.anomalies, 0, "{clean:#?}");
    assert!(clean.checks.iter().all(|k| k.status == "OK"));
    assert_eq!(clean.service, "messaging");
    assert!(clean.remote_verification.starts_with("NOT_PERFORMED"));
    let members = check(&clean, "conversation_member_principals");
    assert_eq!((members.rows, members.distinct_foreign_ids), (3, 3));
    assert_eq!(check(&clean, "conversation_owner_principals").rows, 1);

    // A member recorded in another environment than its conversation.
    h.sql(&format!(
        "INSERT INTO messaging_conversation_members (conversation_id, principal_id, environment_id, role, joined_at, updated_at) \
         VALUES ('{cid}', '{}', '{}', 'MEMBER', now(), now())",
        Uuid::new_v4(),
        Uuid::new_v4()
    ))
    .await
    .unwrap();
    // The conversation's environment recorded under another tenant / application.
    let env = c.context.scope.environment_id;
    h.sql(&format!(
        "INSERT INTO messaging_principal_restrictions (id, tenant_id, application_id, environment_id, principal_id, \
           reason, source, starts_at, ends_at) \
         VALUES ('{}', '{}', '{}', '{env}', '{}', 'RECONCILIATION_TEST', 'MODERATOR', now(), now() + interval '1 day')",
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        people[1]
    ))
    .await
    .unwrap();
    let before = counts(&h).await;
    let r = report(h.conn()).await.unwrap();
    assert_eq!(
        check(&r, "conversation_member_principals").anomalies,
        1,
        "{r:#?}"
    );
    assert_eq!(
        check(&r, "conversation_member_principals").status,
        "ATTENTION"
    );
    assert_eq!(check(&r, "environment_single_owner").anomalies, 1);
    for name in [
        "client_session_devices",
        "sms_bridge_numbers",
        "controlled_context_authorities",
    ] {
        assert_eq!(check(&r, name).anomalies, 0, "{name}");
    }
    assert_eq!(r.anomalies, 2);
    // Report only: nothing changed; ids and counters only.
    assert_eq!(counts(&h).await, before);
    let text = serde_json::to_string(&r).unwrap();
    assert!(!text.contains(cid), "no row ids in the report");
}

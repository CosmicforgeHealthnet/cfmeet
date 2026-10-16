//! §29: final expiry — multi-instance safe workers.

mod common;

use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use privacy_relay::service::RelayService;
use serde_json::json;

#[tokio::test]
async fn concurrent_workers_expire_each_context_and_session_exactly_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    for round in 0..4 {
        h.clock.set(at(round * 200));
        let mut contexts = Vec::new();
        for i in 0..5 {
            let mut req = context_request(&[a, b], "BIDIRECTIONAL", &[]);
            req["startsAt"] = json!(iso(at(round * 200)));
            req["expiresAt"] = json!(iso(at(round * 200 + 60)));
            if i % 2 == 0 {
                req["graceExpiresAt"] = json!(iso(at(round * 200 + 90)));
            }
            let ctx = h.create_context(&c, req).await;
            let id = ctx["contextId"].as_str().unwrap().to_string();
            h.app_session(&c, &id).await;
            if i == 1 {
                h.lifecycle(&c, &id, "complete", 1).await;
            }
            if i == 3 {
                h.lifecycle(&c, &id, "revoke", 1).await;
            }
            contexts.push(id);
        }
        // A session that ends before its context.
        let ctx = h.bidirectional_at(&c, &[a, b], round * 200).await;
        let sid = h
            .create_session(
                &c,
                ctx["contextId"].as_str().unwrap(),
                json!({ "relayStrategy": "APP_ONLY", "expiresAt": iso(at(round * 200 + 30)) }),
            )
            .await["relaySessionId"]
            .as_str()
            .unwrap()
            .to_string();

        h.clock.set(at(round * 200 + 120));
        let workers: Vec<RelayService> = (0..4).map(|_| h.sibling()).collect();
        let reports = futures::future::join_all(workers.iter().map(|w| w.run_sweep())).await;
        let contexts_expired: usize = reports
            .iter()
            .map(|r| r.as_ref().unwrap().contexts_expired)
            .sum();
        // 4 live contexts of this round (one revoked stays REVOKED) + the session's context.
        assert_eq!(contexts_expired, 5, "round {round}");
        for id in &contexts {
            assert_eq!(
                h.count(&format!("SELECT count(*) FROM relay_audit_events WHERE context_id = '{id}' AND event_type = 'CONTEXT_EXPIRED'")).await
                    + h.count(&format!("SELECT count(*) FROM communication_contexts WHERE id = '{id}' AND status = 'REVOKED'")).await,
                1,
                "exactly one terminal fact per context"
            );
            assert!(
                h.count(&format!("SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE context_id = '{id}' AND reason = 'CONTEXT_EXPIRED'")).await <= 1,
                "no duplicate enforcement job"
            );
        }
        let _ = sid;
    }
    // Revoked contexts stayed REVOKED; everything else is EXPIRED with its sessions.
    assert_eq!(
        h.count("SELECT count(*) FROM communication_contexts WHERE status = 'REVOKED'")
            .await,
        4
    );
    assert_eq!(h.count("SELECT count(*) FROM communication_contexts WHERE status <> 'REVOKED' AND status <> 'EXPIRED'").await, 0);
    assert_eq!(h.count("SELECT count(*) FROM privacy_relay_sessions WHERE status NOT IN ('EXPIRED', 'REVOKED')").await, 0);
    assert_eq!(
        h.count("SELECT count(*) FROM (SELECT context_id FROM privacy_relay_enforcement_jobs WHERE reason = 'CONTEXT_EXPIRED' GROUP BY context_id HAVING count(*) > 1) d").await,
        0
    );
    // Every expiry job was delivered exactly once.
    let mut delivered = h.calling.delivered_event_ids();
    let total = delivered.len();
    delivered.sort();
    delivered.dedup();
    assert_eq!(delivered.len(), total);
    // A second sweep finds nothing.
    let again = h.relay.run_sweep().await.unwrap();
    assert_eq!((again.contexts_expired, again.sessions_expired), (0, 0));
}

#[tokio::test]
async fn a_session_expires_on_its_own_window() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let short = h
        .create_session(
            &c,
            &id,
            json!({ "relayStrategy": "APP_ONLY", "expiresAt": iso(at(10)) }),
        )
        .await;
    let long = h.app_session(&c, &id).await;
    h.clock.set(at(11));
    let workers: Vec<RelayService> = (0..3).map(|_| h.sibling()).collect();
    let reports = futures::future::join_all(workers.iter().map(|w| w.expire_sessions(10))).await;
    assert_eq!(reports.into_iter().map(Result::unwrap).sum::<usize>(), 1);
    let s = h
        .get(
            &format!(
                "/internal/v1/sessions/{}",
                short["relaySessionId"].as_str().unwrap()
            ),
            &c,
        )
        .await
        .json;
    assert_eq!(s["status"], "EXPIRED");
    assert_eq!(
        h.count(
            "SELECT count(*) FROM privacy_relay_enforcement_jobs WHERE reason = 'SESSION_EXPIRED'"
        )
        .await,
        1
    );
    assert_eq!(
        h.reason(&c, long["relaySessionId"].as_str().unwrap(), a, &[b])
            .await,
        "AUTHORIZED"
    );
    assert_eq!(
        h.version_of(&c, &id).await,
        1,
        "a session expiry is not a context change"
    );
}

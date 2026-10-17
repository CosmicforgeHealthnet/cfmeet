//! `telephony-admin` operator actions against a real Telephony database (Part I §86):
//! provider / route disable and incident resolution each write EXACTLY ONE operator
//! audit row in the same transaction as their effect; a refused action (no operator,
//! nothing to resolve) writes none and changes nothing. Incidents move OPEN → RESOLVED
//! by the operator; the same condition later opens a NEW incident.

mod common;

use std::process::Command;

use common::Harness;
use serde_json::Value;
use telephony::incidents::{self, Incident, Severity};
use uuid::Uuid;

/// Runs the real `telephony-admin` binary on the test database.
fn admin(h: &Harness, operator: Option<&str>, args: &[&str]) -> (bool, Value, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_telephony-admin"));
    cmd.current_dir(std::env::temp_dir())
        .env("DATABASE_URL", &h.db.url)
        .env_remove("CF_OPERATOR_ID");
    if let Some(op) = operator {
        cmd.arg("--operator").arg(op);
    }
    let out = cmd.args(args).output().expect("telephony-admin runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (
        out.status.success(),
        serde_json::from_str(&stdout).unwrap_or(Value::Null),
        format!("{stdout}{stderr}"),
    )
}

async fn audit_rows(h: &Harness, action: &str, target: &str) -> i64 {
    h.scalar(&format!(
        "SELECT count(*) FROM telephony_operator_audit WHERE action = '{action}' AND target_id = '{target}' \
         AND actor = 'ops.test'"
    ))
    .await
}

async fn raise(h: &Harness, key: &str) -> Uuid {
    incidents::raise(
        h.svc.db(),
        &Incident::new("SIP_EDGE_UNAVAILABLE", Severity::High, key.to_string())
            .evidence(serde_json::json!({ "health": "UNAVAILABLE" })),
        h.now(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn provider_and_route_disable_each_write_exactly_one_audit_row() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (ok, out, text) = admin(&h, Some("ops.test"), &["provider", "disable", "dp-b"]);
    assert!(ok, "{text}");
    assert_eq!(out["status"], "DISABLED");
    assert_eq!(
        h.text("SELECT status FROM telecom_providers WHERE id = 'dp-b'")
            .await
            .unwrap(),
        "DISABLED"
    );
    assert_eq!(audit_rows(&h, "PROVIDER_DISABLE", "dp-b").await, 1);

    let (ok, _, text) = admin(&h, Some("ops.test"), &["route", "disable", "ng-a"]);
    assert!(ok, "{text}");
    assert_eq!(
        h.text("SELECT status FROM telecom_routes WHERE id = 'ng-a'")
            .await
            .unwrap(),
        "DISABLED"
    );
    assert_eq!(audit_rows(&h, "ROUTE_DISABLE", "ng-a").await, 1);
    // Exactly these two rows: one per action, nothing else.
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_operator_audit")
            .await,
        2
    );

    // Without an operator identity: refused, no effect, no audit row.
    let (ok, _, text) = admin(&h, None, &["route", "disable", "ng-b"]);
    assert!(!ok, "{text}");
    assert_eq!(
        h.text("SELECT status FROM telecom_routes WHERE id = 'ng-b'")
            .await
            .unwrap(),
        "ENABLED"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_operator_audit")
            .await,
        2
    );
}

#[tokio::test]
async fn an_operator_resolves_an_open_incident_with_exactly_one_audit_row() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let incident = raise(&h, "sip-edge-health").await;
    let other = raise(&h, "sip-edge-health-other").await;
    // The list (default and explicit) shows open incidents.
    for args in [&["incidents"][..], &["incidents", "list"][..]] {
        let (ok, out, text) = admin(&h, None, args);
        assert!(ok, "{text}");
        assert_eq!(out.as_array().unwrap().len(), 2, "{text}");
    }
    let id = incident.to_string();

    // No operator: refused, nothing changes.
    let (ok, _, text) = admin(&h, None, &["incidents", "resolve", &id]);
    assert!(!ok, "{text}");
    assert_eq!(
        h.text(&format!(
            "SELECT status FROM telephony_incidents WHERE id = '{id}'"
        ))
        .await
        .unwrap(),
        "OPEN"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_operator_audit")
            .await,
        0
    );

    let (ok, out, text) = admin(
        &h,
        Some("ops.test"),
        &[
            "incidents",
            "resolve",
            &id,
            "--note",
            "edge restarted by the on-call",
        ],
    );
    assert!(ok, "{text}");
    assert_eq!(out["status"], "RESOLVED");
    assert_eq!(out["category"], "SIP_EDGE_UNAVAILABLE");
    assert_eq!(
        h.text(&format!(
            "SELECT status FROM telephony_incidents WHERE id = '{id}'"
        ))
        .await
        .unwrap(),
        "RESOLVED"
    );
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_incidents WHERE id = '{id}' AND resolved_at IS NOT NULL"
        ))
        .await,
        1
    );
    assert_eq!(audit_rows(&h, "INCIDENT_RESOLVE", &id).await, 1);
    assert_eq!(
        h.text(&format!(
            "SELECT target_kind FROM telephony_operator_audit WHERE target_id = '{id}'"
        ))
        .await
        .unwrap(),
        "INCIDENT"
    );
    // The other incident is untouched.
    assert_eq!(
        h.text(&format!(
            "SELECT status FROM telephony_incidents WHERE id = '{other}'"
        ))
        .await
        .unwrap(),
        "OPEN"
    );

    // Resolving again (no longer OPEN) or an unknown id: refused, no further audit row.
    let (ok, _, text) = admin(&h, Some("ops.test"), &["incidents", "resolve", &id]);
    assert!(!ok, "{text}");
    let unknown = Uuid::new_v4().to_string();
    let (ok, _, text) = admin(&h, Some("ops.test"), &["incidents", "resolve", &unknown]);
    assert!(!ok, "{text}");
    // A note carrying a number is refused before anything is written.
    let other_id = other.to_string();
    let (ok, _, text) = admin(
        &h,
        Some("ops.test"),
        &[
            "incidents",
            "resolve",
            &other_id,
            "--note",
            "caller +2348031234567",
        ],
    );
    assert!(!ok, "{text}");
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_operator_audit")
            .await,
        1
    );
    let audit = h
        .text("SELECT detail::text FROM telephony_operator_audit")
        .await
        .unwrap();
    assert!(!audit.contains("+234"), "{audit}");

    // The same condition again opens a NEW incident (dedupe holds only while OPEN).
    let again = raise(&h, "sip-edge-health").await;
    assert_ne!(again, incident);
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE dedupe_key = 'sip-edge-health' AND status = 'OPEN'"
        )
        .await,
        1
    );
}

/// `telephony-admin` with `CF_RUNTIME_ENVIRONMENT` (None: unset).
fn admin_in(h: &Harness, runtime: Option<&str>, args: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_telephony-admin"));
    cmd.current_dir(std::env::temp_dir())
        .env("DATABASE_URL", &h.db.url)
        .env_remove("CF_RUNTIME_ENVIRONMENT")
        .arg("--operator")
        .arg("ops.test");
    if let Some(r) = runtime {
        cmd.env("CF_RUNTIME_ENVIRONMENT", r);
    }
    let out = cmd.args(args).output().expect("telephony-admin runs");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Part IV L closure: a SYNTHETIC_TEST route is admitted only as the loopback SIP edge
/// with a simulated carrier — a local / test runtime, a SIP_TRUNK provider, a loopback-only
/// SIP peer and zero cost — its class never changes, and a staging / production Telephony
/// refuses to start while one exists.
#[tokio::test]
async fn synthetic_test_routes_are_admitted_only_as_a_loopback_zero_cost_sip_trunk() {
    let Some(h) = Harness::new().await else {
        return;
    };
    for args in [
        &[
            "provider",
            "add",
            "--id",
            "sip-syn",
            "--adapter",
            "SIP_TRUNK",
            "--capabilities",
            "OUTBOUND_VOICE,INBOUND_VOICE",
        ][..],
        &[
            "sip-peer",
            "add",
            "--id",
            "peer-loop",
            "--provider",
            "sip-syn",
            "--secret-ref",
            "trunk/loop",
            "--allowed-cidrs",
            "127.0.0.2/32",
        ],
        &[
            "sip-peer",
            "add",
            "--id",
            "peer-public",
            "--provider",
            "sip-syn",
            "--secret-ref",
            "trunk/public",
            "--allowed-cidrs",
            "192.0.2.0/24",
        ],
        &[
            "sip-peer",
            "add",
            "--id",
            "peer-dp",
            "--provider",
            "dp-a",
            "--secret-ref",
            "trunk/dp",
            "--allowed-cidrs",
            "127.0.0.2/32",
        ],
    ] {
        let (ok, text) = admin_in(&h, Some("test"), args);
        assert!(ok, "{args:?}: {text}");
    }
    let route =
        |id: &'static str, provider: &'static str, peer: &'static str, cost: &'static str| {
            vec![
                "route",
                "add",
                "--id",
                id,
                "--provider",
                provider,
                "--countries",
                "NG",
                "--capabilities",
                "VOICE_OUTBOUND,VOICE_INBOUND",
                "--sip-peer",
                peer,
                "--estimated-leg-cost-minor",
                cost,
                "--cost-currency",
                "USD",
                "--interconnect-class",
                "SYNTHETIC_TEST",
            ]
        };
    for (runtime, args, why) in [
        (
            None,
            route("syn-1", "sip-syn", "peer-loop", "0"),
            "local / test runtimes only",
        ),
        (
            Some("production"),
            route("syn-1", "sip-syn", "peer-loop", "0"),
            "local / test runtimes only",
        ),
        (
            Some("staging"),
            route("syn-1", "sip-syn", "peer-loop", "0"),
            "local / test runtimes only",
        ),
        (
            Some("test"),
            route("syn-1", "sip-syn", "peer-loop", "100"),
            "costs nothing",
        ),
        (
            Some("test"),
            route("syn-1", "dp-a", "peer-dp", "0"),
            "SIP_TRUNK provider",
        ),
        (
            Some("test"),
            route("syn-1", "sip-syn", "peer-public", "0"),
            "loopback sources only",
        ),
    ] {
        let (ok, text) = admin_in(&h, runtime, &args);
        assert!(!ok && text.contains(why), "{runtime:?} {args:?}: {text}");
    }
    assert_eq!(
        h.scalar("SELECT count(*) FROM telecom_routes WHERE interconnect_class <> 'PUBLIC'")
            .await,
        0,
        "nothing was created"
    );
    let (ok, text) = admin_in(
        &h,
        Some("test"),
        &route("syn-1", "sip-syn", "peer-loop", "0"),
    );
    assert!(ok, "{text}");
    assert_eq!(
        h.text("SELECT interconnect_class FROM telecom_routes WHERE id = 'syn-1'")
            .await
            .as_deref(),
        Some("SYNTHETIC_TEST")
    );
    assert_eq!(audit_rows(&h, "ROUTE_ADD", "syn-1").await, 1);
    // The class is immutable; a synthetic route never costs anything.
    for sql in [
        "UPDATE telecom_routes SET interconnect_class = 'PUBLIC' WHERE id = 'syn-1'",
        "UPDATE telecom_routes SET interconnect_class = 'SYNTHETIC_TEST' WHERE id = 'ng-a'",
        "UPDATE telecom_routes SET estimated_leg_cost_minor = 5 WHERE id = 'syn-1'",
    ] {
        assert!(h.try_sql(sql).await.is_err(), "{sql}");
    }
    // A protected runtime refuses to start while a synthetic route exists.
    for runtime in [
        cf_service_auth::RuntimeEnvironment::Staging,
        cf_service_auth::RuntimeEnvironment::Production,
    ] {
        let Err(err) = telephony::providers::registry_from_database(
            h.svc.db(),
            runtime,
            telephony::sip::NoEdge::shared(),
        )
        .await
        else {
            panic!("{runtime}: a synthetic route must refuse startup");
        };
        assert!(
            matches!(
                err,
                telephony::providers::RegistryError::SyntheticRouteRefused(..)
            ),
            "{err}"
        );
    }
}

//! Abuse reports (Part H §77.20): only a participant of the Call may report it (a Call of
//! another tenant / application — or one the reporter took no part in — is 404), the
//! category and comment code are bounded vocabularies (never free text), a report is
//! idempotent per (Call, reporter), a burst against one principal raises exactly one
//! `ABUSE_REPORTS_BURST` incident whose occurrences grow, and a stored report holds no
//! number or provider payload.

mod common;

use axum::http::{Method, StatusCode};
use calling::{domain::EnvironmentKind, platform::scopes::names};
use common::{app::AppWorld, enterprise::strings, public::number_like_runs, TestResponse};
use serde_json::{json, Value};
use uuid::Uuid;

async fn report(w: &AppWorld, token: &str, call: Uuid, body: Value) -> TestResponse {
    w.h()
        .request(
            Method::POST,
            &format!("/v1/calls/{call}/abuse-reports"),
            Some(token),
            Some(body),
        )
        .await
}

async fn rows(w: &AppWorld) -> i64 {
    w.w.count("SELECT count(*) FROM calling_abuse_reports")
        .await
}

#[tokio::test]
async fn only_a_participant_of_the_callers_own_call_may_report_it() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (call, _, _) = w.connected_call("VOICE").await;
    let stranger = w.principal("stranger-1").await;
    // A principal of the same scope who was not on the Call: indistinguishable from an
    // unknown Call.
    let missing = report(
        &w,
        w.secret(),
        Uuid::new_v4(),
        json!({ "principalId": w.w.initiator.id(), "category": "SPAM" }),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.text);
    let r = report(
        &w,
        w.secret(),
        call,
        json!({ "principalId": stranger.id(), "category": "SPAM" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_eq!(r.normalized_error(), missing.normalized_error());
    assert_eq!(rows(&w).await, 0);

    // Another tenant — with its own principal or naming this tenant's participant — and
    // another application of the same tenant: 404, no row.
    let h = w.h();
    let globex = h
        .consumer("globex", calling::domain::TenantClassification::External)
        .await;
    let sibling = h.application_in(&w.w.consumer.tenant, "sibling").await;
    for consumer in [&globex, &sibling] {
        let token = h
            .issue(
                consumer.sa(EnvironmentKind::Production),
                &[
                    names::CALLS_READ,
                    names::CALLS_WRITE,
                    names::PRINCIPALS_READ,
                ],
            )
            .await;
        let ctx = h.ctx(token.secret.expose_once()).await;
        let own =
            common::calls::resolve(h, consumer, EnvironmentKind::Production, &ctx, "x-1").await;
        for principal in [own.id().as_uuid(), w.w.initiator.id().as_uuid()] {
            let r = report(
                &w,
                token.secret.expose_once(),
                call,
                json!({ "principalId": principal, "category": "HARASSMENT" }),
            )
            .await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
        }
    }
    assert_eq!(rows(&w).await, 0);
    // A participant may.
    let r = report(
        &w,
        w.secret(),
        call,
        json!({ "principalId": w.w.recipient.id(), "category": "HARASSMENT" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        r.json["reportedPrincipalId"],
        w.w.initiator.id().to_string()
    );
    assert_eq!(rows(&w).await, 1);
}

#[tokio::test]
async fn categories_and_comment_codes_are_bounded_and_reports_are_idempotent() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (call, _, _) = w.connected_call("VOICE").await;
    let reporter = w.w.initiator.id();
    for body in [
        json!({ "principalId": reporter, "category": "RUDE" }),
        json!({ "principalId": reporter, "category": "spam" }),
        json!({ "principalId": reporter, "category": "SPAM", "commentCode": "he called me at night" }),
        json!({ "principalId": reporter, "category": "SPAM", "commentCode": "+2348012345678" }),
        json!({ "principalId": reporter, "category": "SPAM", "commentCode": "lower_case" }),
        json!({ "principalId": reporter, "category": "SPAM", "commentCode": "A".repeat(65) }),
        json!({ "principalId": reporter, "category": "SPAM", "commentCode": "" }),
        json!({ "principalId": reporter, "category": "SPAM", "comment": "free text" }),
        json!({ "principalId": reporter, "category": "SPAM", "reportedPrincipalId": w.w.recipient.id() }),
    ] {
        let r = report(&w, w.secret(), call, body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text);
        assert!(number_like_runs(&r.text).is_empty());
    }
    assert_eq!(rows(&w).await, 0);

    let body = json!({ "principalId": reporter, "category": "FRAUD_SUSPECTED",
                       "commentCode": "REPEATED_CALLS_2" });
    let first = report(&w, w.secret(), call, body.clone()).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    let second = report(
        &w,
        w.secret(),
        call,
        json!({ "principalId": reporter, "category": "SPAM" }),
    )
    .await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text);
    assert_eq!(second.json["abuseReportId"], first.json["abuseReportId"]);
    assert_eq!(second.json["category"], "FRAUD_SUSPECTED");
    assert_eq!(rows(&w).await, 1);
    let listed = w
        .get(&format!("/v1/abuse-reports?principalId={reporter}"))
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.json["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_burst_against_one_principal_is_one_incident_whose_occurrences_grow() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let target = &w.w.initiator;
    let phone = w.device(target, "target-main").await;
    let threshold = calling::calls::app::AbuseControls::default().abuse_reports_incident_threshold;
    let incident = |w: &AppWorld| {
        let conn = w.h().conn().clone();
        async move {
            strings(
                &conn,
                "SELECT json_build_object('status', status, 'occurrences', occurrences, \
                   'resourceId', resource_id, 'evidence', restricted_evidence)::text \
                 FROM calling_incidents WHERE category = 'ABUSE_REPORTS_BURST'",
            )
            .await
            .iter()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
            .collect::<Vec<_>>()
        }
    };
    let mut reporters = Vec::new();
    for i in 0..threshold + 1 {
        let reporter = w.principal(&format!("reporter-{i}")).await;
        w.device(&reporter, &format!("reporter-{i}-main")).await;
        let r = w
            .create_call("VOICE", target, &phone, &[&reporter], &format!("burst-{i}"))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let r = report(
            &w,
            w.secret(),
            call,
            json!({ "principalId": reporter.id(), "category": "HARASSMENT" }),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        reporters.push(reporter);
        let open = incident(&w).await;
        if (i + 1) < threshold {
            assert!(open.is_empty(), "below the threshold: {open:?}");
        } else {
            assert_eq!(open.len(), 1, "{open:?}");
            assert_eq!(open[0]["status"], "OPEN");
            assert_eq!(open[0]["resourceId"], target.id().to_string());
            assert_eq!(open[0]["occurrences"], i + 2 - threshold);
        }
    }
    let open = incident(&w).await;
    assert_eq!(open[0]["occurrences"], 2);
    assert_eq!(open[0]["evidence"]["reports"], threshold + 1);
    let evidence = open[0]["evidence"].to_string();
    for r in &reporters {
        assert!(
            !evidence.contains(&r.id().to_string()),
            "a reporter reached the evidence"
        );
    }
}

#[tokio::test]
async fn a_stored_report_holds_no_number_free_text_or_provider_payload() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (call, _, _) = w.connected_call("VOICE").await;
    let r = report(
        &w,
        w.secret(),
        call,
        json!({ "principalId": w.w.recipient.id(), "category": "OTHER", "commentCode": "SEE_CASE_12" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // The table has no column that could hold one.
    let columns = strings(
        w.h().conn(),
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_name = 'calling_abuse_reports' ORDER BY column_name",
    )
    .await;
    for column in &columns {
        for forbidden in [
            "number", "phone", "msisdn", "payload", "provider", "text", "body", "media",
        ] {
            assert!(!column.contains(forbidden), "column {column}");
        }
    }
    let stored = strings(w.h().conn(), "SELECT t::text FROM calling_abuse_reports t").await;
    assert_eq!(stored.len(), 1);
    assert!(number_like_runs(&stored[0]).is_empty(), "{}", stored[0]);
    assert!(!stored[0].contains("+234"));
    assert!(stored[0].contains("SEE_CASE_12"));
}

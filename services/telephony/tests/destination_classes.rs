//! Pre-toll destination classification (Part H §66, §75): the executor classifies the
//! resolved destination by the LONGEST enabled prefix rule of the destination country
//! before any provider action. Blocked classes (`TELEPHONY_BLOCKED_DESTINATION_CLASSES`,
//! default `PREMIUM,SHARED_COST,SATELLITE,HIGH_RISK`) and — under
//! `TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY=DENY` — unclassified destinations are
//! refused definitely with zero provider originates; the refusal metric's only label is
//! the class.

mod common;

use axum::http::{Method, StatusCode};
use common::Harness;
use telephony::{
    destinations::{DestinationClass, UnclassifiedPolicy},
    metrics,
};
use uuid::Uuid;

/// The NG destination every attempt dials (national-significant digits `8031234567`).
const DESTINATION: &str = "+2348031234567";

async fn rule(h: &Harness, country: &str, prefix: &str, class: &str) {
    let now = h.clock_now();
    h.sql(&format!(
        "INSERT INTO telecom_destination_classes (id, country, prefix, class, source, created_at, updated_at) \
         VALUES ('{}', '{country}', '{prefix}', '{class}', 'test-load', '{now}', '{now}')",
        Uuid::new_v4()
    ))
    .await;
}

async fn clear_rules(h: &Harness) {
    h.sql("DELETE FROM telecom_destination_classes").await;
}

/// One origination to `DESTINATION`: (leg status, the command's result code, provider
/// originates this attempt caused, interconnect bindings prepared for the leg).
async fn attempt(h: &Harness) -> (String, Option<String>, usize, i64) {
    let sent_before = h.provider_a.requests().len() + h.provider_b.requests().len();
    let contact = h.privacy.add_contact(DESTINATION, "NG");
    let body = h.originate_body(contact, Uuid::new_v4(), 1);
    let r = h
        .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    let leg: Uuid = r.json["leg"]["telephonyLegId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    h.run().await;
    let code = h
        .text(&format!(
            "SELECT result_code FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
        ))
        .await;
    let sent = h.provider_a.requests().len() + h.provider_b.requests().len() - sent_before;
    let bindings = h
        .scalar(&format!(
            "SELECT count(*) FROM provider_call_bindings WHERE telephony_leg_id = '{leg}'"
        ))
        .await;
    (h.leg_status(leg).await, code, sent, bindings)
}

fn refusals(class: &'static str) -> u64 {
    metrics::counter(
        "telephony_destination_class_refusals_total",
        &[("class", class)],
    )
}

async fn assert_refused(h: &Harness, class: &'static str, code: &str) {
    let before = refusals(class);
    let (status, result, sent, bindings) = attempt(h).await;
    assert_eq!(status, "FAILED", "{class}");
    assert_eq!(result.as_deref(), Some(code), "{class}");
    assert_eq!(sent, 0, "{class}: zero provider originates");
    assert_eq!(
        bindings, 0,
        "{class}: refused pre-toll, before any route choice or capacity reservation"
    );
    assert!(refusals(class) > before, "{class}: refusal counted");
}

async fn assert_proceeds(h: &Harness, what: &str) {
    let (status, _, sent, _) = attempt(h).await;
    assert_eq!(status, "ORIGINATING", "{what}");
    assert_eq!(sent, 1, "{what}: one provider originate");
}

#[tokio::test]
async fn standard_proceeds_and_premium_and_high_risk_are_refused_before_any_provider_action() {
    let Some(h) = Harness::new().await else {
        return;
    };
    rule(&h, "NG", "803", "STANDARD").await;
    assert_proceeds(&h, "STANDARD").await;
    for class in ["PREMIUM", "HIGH_RISK"] {
        clear_rules(&h).await;
        rule(&h, "NG", "803", class).await;
        let label: &'static str = DestinationClass::parse(class).unwrap().as_str();
        assert_refused(&h, label, "DESTINATION_CLASS_BLOCKED").await;
    }
    // A disabled rule classifies nothing (unclassified under ALLOW proceeds).
    h.sql("UPDATE telecom_destination_classes SET enabled = FALSE")
        .await;
    assert_proceeds(&h, "disabled rule").await;
    // The refusal is an abuse signal of the application (never a number).
    let evidence = h
        .text(
            "SELECT string_agg(restricted_evidence::text || coalesce(dedupe_key, ''), ' ') FROM telephony_incidents",
        )
        .await
        .unwrap_or_default();
    assert!(evidence.contains("DESTINATION_CLASS_REFUSED"), "{evidence}");
    for forbidden in ["+234", "8031234567"] {
        assert!(!evidence.contains(forbidden), "{forbidden} in {evidence}");
    }
}

#[tokio::test]
async fn satellite_and_shared_cost_follow_the_configured_blocked_list() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    for class in ["SATELLITE", "SHARED_COST"] {
        clear_rules(&h).await;
        rule(&h, "NG", "803", class).await;
        let label: &'static str = DestinationClass::parse(class).unwrap().as_str();
        // Blocked by the default list.
        h.svc.settings.blocked_destination_classes = DestinationClass::DEFAULT_BLOCKED.to_vec();
        assert_refused(&h, label, "DESTINATION_CLASS_BLOCKED").await;
        // Allowed once the operator removes it from the list.
        h.svc.settings.blocked_destination_classes =
            vec![DestinationClass::Premium, DestinationClass::HighRisk];
        assert_proceeds(&h, class).await;
    }
    // Even STANDARD can be blocked by configuration: the list is the only authority.
    clear_rules(&h).await;
    rule(&h, "NG", "803", "STANDARD").await;
    h.svc.settings.blocked_destination_classes = vec![DestinationClass::Standard];
    assert_refused(&h, "STANDARD", "DESTINATION_CLASS_BLOCKED").await;
}

#[tokio::test]
async fn unclassified_destinations_follow_the_policy() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    // No rule at all: ALLOW proceeds.
    h.svc.settings.unclassified_destination_policy = UnclassifiedPolicy::Allow;
    assert_proceeds(&h, "unclassified under ALLOW").await;
    // DENY refuses.
    h.svc.settings.unclassified_destination_policy = UnclassifiedPolicy::Deny;
    assert_refused(&h, "UNCLASSIFIED", "DESTINATION_UNCLASSIFIED").await;
    // A rule for a range the destination is not in leaves it unclassified.
    rule(&h, "NG", "809", "STANDARD").await;
    assert_refused(&h, "UNCLASSIFIED", "DESTINATION_UNCLASSIFIED").await;
    // Once classified STANDARD it proceeds under DENY too.
    rule(&h, "NG", "80", "STANDARD").await;
    assert_proceeds(&h, "classified under DENY").await;
}

#[tokio::test]
async fn the_longest_prefix_wins() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // A blocked short range with an allowed longer range inside it: allowed.
    rule(&h, "NG", "80", "PREMIUM").await;
    rule(&h, "NG", "803", "STANDARD").await;
    assert_proceeds(&h, "longer STANDARD inside PREMIUM").await;
    // An even longer blocked range inside the allowed one: blocked.
    rule(&h, "NG", "8031", "HIGH_RISK").await;
    assert_refused(&h, "HIGH_RISK", "DESTINATION_CLASS_BLOCKED").await;
    // Only the matching longest prefix counts (a longer, non-matching one does not).
    clear_rules(&h).await;
    rule(&h, "NG", "803", "PREMIUM").await;
    rule(&h, "NG", "8039", "STANDARD").await;
    assert_refused(&h, "PREMIUM", "DESTINATION_CLASS_BLOCKED").await;
}

#[tokio::test]
async fn the_same_prefix_in_another_country_does_not_apply() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    // GH 803 is premium; the NG destination 803… is not GH's range.
    rule(&h, "GH", "803", "PREMIUM").await;
    assert_proceeds(&h, "GH rule against an NG destination").await;
    // Under DENY the GH rule does not classify the NG destination either.
    h.svc.settings.unclassified_destination_policy = UnclassifiedPolicy::Deny;
    assert_refused(&h, "UNCLASSIFIED", "DESTINATION_UNCLASSIFIED").await;
    rule(&h, "NG", "803", "STANDARD").await;
    assert_proceeds(&h, "NG rule").await;
}

#[tokio::test]
async fn the_refusal_metric_carries_the_class_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    rule(&h, "NG", "803", "PREMIUM").await;
    assert_refused(&h, "PREMIUM", "DESTINATION_CLASS_BLOCKED").await;
    let text = h.svc.metrics_text().await.unwrap();
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("telephony_destination_class_refusals_total{"))
        .collect();
    assert!(!lines.is_empty(), "{text}");
    let vocabulary = [
        "STANDARD",
        "PREMIUM",
        "SHARED_COST",
        "SATELLITE",
        "HIGH_RISK",
        "UNCLASSIFIED",
    ];
    for line in lines {
        let labels = line
            .split_once('{')
            .and_then(|(_, rest)| rest.split_once('}'))
            .map(|(labels, _)| labels)
            .unwrap();
        let value = labels
            .strip_prefix("class=\"")
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or_else(|| panic!("only the class label: {line}"));
        assert!(vocabulary.contains(&value), "{line}");
        // Never a number, prefix, route or provider.
        assert!(!labels.bytes().any(|b| b.is_ascii_digit()), "{line}");
    }
}

//! Compliance eligibility of paid public routes (Part H §78.23–24), through the public
//! app-to-phone path with scripted Privacy Relay and Telephony: a destination country
//! that requires eligibility (platform overlay `CALLING_COMPLIANCE_REQUIRED_COUNTRIES`
//! or the customer routing policy) is reached only with an ELIGIBLE, unexpired record of
//! THIS environment covering that country — otherwise the route is denied before any
//! spend reservation or Telephony command. A customer policy can add required
//! countries but never remove a platform-required one.

mod common;

use axum::http::StatusCode;
use calling::routing::compliance::{
    store_eligibility, ComplianceOverlay, EligibilityStatus, SetEligibility,
};
use chrono::{DateTime, Duration, Utc};
use common::public::{public_policy, PublicWorld};
use uuid::Uuid;

/// A world whose platform overlay requires eligibility for `overlay` (e.g. "NG").
async fn world(overlay: &str) -> Option<PublicWorld> {
    let overlay = ComplianceOverlay::parse(overlay).unwrap();
    PublicWorld::configured(move |runtime| runtime.compliance_overlay = overlay).await
}

fn now(x: &PublicWorld) -> DateTime<Utc> {
    use calling::domain::Clock;
    x.w.h().clock.now()
}

/// Records `status` for (`tenant`, `application`, `environment`) as decided at `at`.
#[allow(clippy::too_many_arguments)]
async fn record(
    x: &PublicWorld,
    tenant: Uuid,
    application: Uuid,
    environment: Uuid,
    status: EligibilityStatus,
    countries: &[&str],
    expires_at: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
) {
    store_eligibility(
        x.w.h().conn(),
        tenant,
        application,
        environment,
        &SetEligibility {
            status,
            countries: countries.iter().map(|c| c.to_string()).collect(),
            evidence_ref: Some("case-7".into()),
            decided_by: "ops.test".into(),
            expires_at,
        },
        at,
    )
    .await
    .unwrap()
    .unwrap();
}

/// Records `status` for the world's own environment.
async fn own(
    x: &PublicWorld,
    status: EligibilityStatus,
    countries: &[&str],
    expires_at: Option<DateTime<Utc>>,
) {
    let s = x.w.w.scope();
    record(
        x,
        s.tenant_id().as_uuid(),
        s.application_id().as_uuid(),
        s.environment_id().as_uuid(),
        status,
        countries,
        expires_at,
        now(x),
    )
    .await;
}

fn denials() -> u64 {
    calling::metrics::counter(
        "calling_public_route_denials_total",
        &[("reason", "COMPLIANCE_ELIGIBILITY_REQUIRED")],
    )
}

/// The call is refused as a route denial and nothing paid happened.
async fn assert_denied(x: &PublicWorld, key: &str) {
    let before = denials();
    let originates = x.telephony.originates().len();
    let effects = x.effects().await;
    let grants = x.privacy.grants().len();
    let r = x.call_to(x.contact, key).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{key}: {}", r.text);
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_DENIED", "{key}");
    assert!(denials() > before, "{key}: not a compliance denial");
    assert_eq!(x.telephony.originates().len(), originates, "{key}");
    assert_eq!(
        x.effects().await,
        effects,
        "{key}: a paid side effect exists"
    );
    assert_eq!(
        x.privacy.grants().len(),
        grants,
        "{key}: a grant was issued"
    );
}

async fn assert_allowed(x: &PublicWorld, key: &str) {
    let originates = x.telephony.originates().len();
    let r = x.call_to(x.contact, key).await;
    assert_eq!(r.status, StatusCode::CREATED, "{key}: {}", r.text);
    assert_eq!(x.telephony.originates().len(), originates + 1, "{key}");
}

#[tokio::test]
async fn a_country_without_a_requirement_routes_ordinarily() {
    // The overlay requires GB only; NG needs no record at all.
    let Some(x) = world("GB").await else {
        return;
    };
    assert_eq!(
        x.count("SELECT count(*) FROM platform_compliance_eligibility")
            .await,
        0
    );
    assert_allowed(&x, "no-requirement").await;
}

#[tokio::test]
async fn a_required_country_without_a_record_is_denied_before_any_paid_action() {
    let Some(x) = world("NG").await else {
        return;
    };
    assert_denied(&x, "missing-record").await;
    assert_eq!(x.effects().await, (0, 0, 0));
    // A NOT_ASSESSED record is no better.
    own(&x, EligibilityStatus::NotAssessed, &[], None).await;
    assert_denied(&x, "not-assessed").await;
}

#[tokio::test]
async fn ineligible_expired_and_other_country_records_are_denied() {
    let Some(x) = world("NG").await else {
        return;
    };
    own(&x, EligibilityStatus::Ineligible, &["NG"], None).await;
    assert_denied(&x, "ineligible").await;
    // ELIGIBLE but expired (decided two hours ago, expired one hour ago).
    let s = x.w.w.scope();
    record(
        &x,
        s.tenant_id().as_uuid(),
        s.application_id().as_uuid(),
        s.environment_id().as_uuid(),
        EligibilityStatus::Eligible,
        &["NG"],
        Some(now(&x) - Duration::hours(1)),
        now(&x) - Duration::hours(2),
    )
    .await;
    assert_denied(&x, "expired").await;
    // ELIGIBLE, unexpired, but for another country.
    own(
        &x,
        EligibilityStatus::Eligible,
        &["GB"],
        Some(now(&x) + Duration::days(30)),
    )
    .await;
    assert_denied(&x, "other-country").await;
    assert_eq!(x.effects().await, (0, 0, 0));
}

#[tokio::test]
async fn a_valid_eligible_record_allows_the_route_and_revocation_denies_it_again() {
    let Some(x) = world("NG").await else {
        return;
    };
    own(
        &x,
        EligibilityStatus::Eligible,
        &["NG"],
        Some(now(&x) + Duration::days(30)),
    )
    .await;
    assert_allowed(&x, "eligible").await;
    // An empty country list covers every country.
    own(&x, EligibilityStatus::Eligible, &[], None).await;
    assert_allowed(&x, "eligible-everywhere").await;
    own(&x, EligibilityStatus::Ineligible, &[], None).await;
    assert_denied(&x, "revoked").await;
}

#[tokio::test]
async fn a_customer_policy_adds_requirements_but_never_removes_the_platform_overlay() {
    // No overlay: the customer policy alone requires NG.
    let Some(x) = world("").await else {
        return;
    };
    assert_allowed(&x, "no-policy-requirement").await;
    let mut policy = public_policy();
    policy.compliance_required_countries = vec!["NG".into()];
    x.set_policy(&policy).await;
    assert_denied(&x, "customer-requires").await;

    // The overlay requires NG; the customer's policy lists nothing (or another
    // country): NG is still required.
    let Some(y) = world("NG").await else {
        return;
    };
    for (n, required) in [vec![], vec!["GB".to_string()]].into_iter().enumerate() {
        let mut policy = public_policy();
        policy.compliance_required_countries = required;
        y.set_policy(&policy).await;
        assert_denied(&y, &format!("overlay-kept-{n}")).await;
    }
    // `*` in the overlay requires every country.
    let Some(z) = world("*").await else {
        return;
    };
    assert_denied(&z, "overlay-star").await;
}

#[tokio::test]
async fn another_tenants_or_environments_eligibility_never_satisfies_this_one() {
    let Some(x) = world("NG").await else {
        return;
    };
    // Tenant B is ELIGIBLE for NG and may call.
    let other = x.other_tenant("globex").await;
    let b = other
        .consumer
        .env(calling::domain::EnvironmentKind::Production);
    record(
        &x,
        other.consumer.tenant.id.as_uuid(),
        other.consumer.application.id.as_uuid(),
        b.id.as_uuid(),
        EligibilityStatus::Eligible,
        &["NG"],
        None,
        now(&x),
    )
    .await;
    let r = other.call_to(&x, x.contact, "tenant-b").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    // This tenant's own SANDBOX environment is ELIGIBLE too — still another environment.
    let sandbox =
        x.w.w
            .consumer
            .env(calling::domain::EnvironmentKind::Sandbox);
    record(
        &x,
        x.tenant(),
        x.w.w.scope().application_id().as_uuid(),
        sandbox.id.as_uuid(),
        EligibilityStatus::Eligible,
        &["NG"],
        None,
        now(&x),
    )
    .await;
    assert_denied(&x, "foreign-eligibility").await;
    assert_eq!(x.effects().await, (0, 0, 0));
}

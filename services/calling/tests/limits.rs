//! Limit policy foundation: server-authoritative application / credential limits, the
//! stricter-wins resolver, and request-rate enforcement.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, TenantClassification},
    platform::{
        limits::{EffectiveLimit, LimitDefaults, LimitSource, LimitValues},
        tenants::NewTenant,
        PlatformError,
    },
};
use chrono::{Duration, DurationRound};
use common::Harness;
use serde_json::json;

#[tokio::test]
async fn effective_policy_is_resolved_from_persisted_levels() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let token = issued.secret.expose_once();
    let defaults = LimitDefaults::default();

    // Nothing configured: platform defaults; telecom spend zero.
    let ctx = h.ctx(token).await;
    let eff = h.state.limits.effective_for(&ctx).await.unwrap();
    assert_eq!(
        eff.requests_per_minute,
        EffectiveLimit {
            value: defaults.requests_per_minute,
            source: LimitSource::PlatformDefault
        }
    );
    assert_eq!(eff.telecom_spend_daily_limit_minor.value, 0);
    assert_eq!(eff.telecom_spend_currency, "USD");

    h.platform
        .set_application_limits(
            c.application.id,
            LimitValues {
                requests_per_minute: Some(100),
                max_concurrent_communications: Some(5),
                telecom_spend_daily_limit_minor: Some(10_000),
                telecom_spend_monthly_limit_minor: Some(200_000),
            },
        )
        .await
        .unwrap();
    h.platform
        .set_credential_limits(
            issued.credential.id,
            LimitValues {
                requests_per_minute: Some(40),
                max_concurrent_communications: Some(9),
                telecom_spend_daily_limit_minor: Some(2_500),
                telecom_spend_monthly_limit_minor: None,
            },
        )
        .await
        .unwrap();

    // Production with live telecom disabled: spend forced to zero by environment policy.
    let eff = h.state.limits.effective_for(&ctx).await.unwrap();
    assert_eq!(
        eff.requests_per_minute,
        EffectiveLimit {
            value: 40,
            source: LimitSource::Credential
        }
    );
    assert_eq!(
        eff.max_concurrent_communications,
        EffectiveLimit {
            value: 5,
            source: LimitSource::Application
        }
    );
    assert_eq!(
        eff.telecom_spend_daily_limit_minor,
        EffectiveLimit {
            value: 0,
            source: LimitSource::EnvironmentPolicy
        }
    );

    // Live telecom enabled: the stricter configured spend ceiling wins.
    h.platform
        .set_live_telecom_allowed(c.env(EnvironmentKind::Production).id, true)
        .await
        .unwrap();
    let ctx = h.ctx(token).await;
    let eff = h.state.limits.effective_for(&ctx).await.unwrap();
    assert_eq!(
        eff.telecom_spend_daily_limit_minor,
        EffectiveLimit {
            value: 2_500,
            source: LimitSource::Credential
        }
    );
    assert_eq!(
        eff.telecom_spend_monthly_limit_minor,
        EffectiveLimit {
            value: 200_000,
            source: LimitSource::Application
        }
    );
    // The admin view resolves identically.
    assert_eq!(
        h.platform
            .effective_limits(issued.credential.id)
            .await
            .unwrap(),
        eff
    );

    // Sandbox never gets telecom spend, whatever is configured.
    let sandbox = h.issue(c.sa(EnvironmentKind::Sandbox), &[]).await;
    let sctx = h.ctx(sandbox.secret.expose_once()).await;
    let seff = h.state.limits.effective_for(&sctx).await.unwrap();
    assert_eq!(
        seff.telecom_spend_daily_limit_minor.source,
        LimitSource::EnvironmentPolicy
    );
    assert_eq!(seff.telecom_spend_monthly_limit_minor.value, 0);
}

#[tokio::test]
async fn limits_cannot_be_supplied_by_the_client() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    h.platform
        .set_credential_limits(
            issued.credential.id,
            LimitValues {
                requests_per_minute: Some(30),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let response = h
        .request_with(
            Method::GET,
            "/test/limits?requests_per_minute=100000",
            Some(issued.secret.expose_once()),
            None,
            &[("x-rate-limit", "100000")],
        )
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json["requests_per_minute"]["value"], 30);
}

#[tokio::test]
async fn invalid_limit_values_are_rejected() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    for bad in [
        LimitValues {
            requests_per_minute: Some(0),
            ..Default::default()
        },
        LimitValues {
            max_concurrent_communications: Some(-1),
            ..Default::default()
        },
        LimitValues {
            telecom_spend_monthly_limit_minor: Some(-1),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            h.platform
                .set_application_limits(c.application.id, bad)
                .await
                .unwrap_err(),
            PlatformError::Validation(_)
        ));
    }
}

#[tokio::test]
async fn tenant_billing_currency_is_validated_and_immutable() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let t = h
        .platform
        .create_tenant(NewTenant {
            slug: "naira".into(),
            display_name: "Naira tenant".into(),
            classification: TenantClassification::External,
            billing_currency: Some("NGN".into()),
        })
        .await
        .unwrap();
    assert_eq!(t.billing_currency, "NGN");
    assert!(common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_tenants SET billing_currency = 'USD' WHERE id = '{}'",
            t.id
        )
    )
    .await
    .is_err());
}

#[tokio::test]
async fn request_rate_limit_is_enforced_per_credential() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let limited = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let other = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    h.platform
        .set_credential_limits(
            limited.credential.id,
            LimitValues {
                requests_per_minute: Some(3),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let token = limited.secret.expose_once();
    for _ in 0..3 {
        assert_eq!(h.whoami(token).await.status, StatusCode::OK);
    }
    let response = h.whoami(token).await;
    assert_eq!(response.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.json["error"]["code"], "RATE_LIMITED");
    // The clock is 30 s into the minute: 30 s until the next window.
    assert_eq!(response.headers.get("retry-after").unwrap(), "30");

    // Other credentials of the same application are unaffected.
    assert_eq!(
        h.whoami(other.secret.expose_once()).await.status,
        StatusCode::OK
    );

    // A new window admits requests again.
    h.clock.advance(Duration::seconds(31));
    assert_eq!(h.whoami(token).await.status, StatusCode::OK);
}

#[tokio::test]
async fn application_limit_is_an_aggregate_ceiling() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    h.platform
        .set_application_limits(
            c.application.id,
            LimitValues {
                requests_per_minute: Some(4),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let a = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let b = h.issue(c.sa(EnvironmentKind::Sandbox), &[]).await;
    for token in [&a, &b, &a, &b] {
        assert_eq!(
            h.whoami(token.secret.expose_once()).await.status,
            StatusCode::OK
        );
    }
    assert_eq!(
        h.whoami(a.secret.expose_once()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        h.whoami(b.secret.expose_once()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn rejected_authentication_is_not_counted_and_old_windows_are_pruned() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    assert_eq!(
        h.whoami(issued.secret.expose_once()).await.status,
        StatusCode::OK
    );
    let bad = h
        .request(
            Method::POST,
            "/test/context",
            Some("cfmk_live_nope"),
            Some(json!({})),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        common::scalar_i64(
            h.conn(),
            "SELECT COALESCE(sum(request_count), 0)::bigint FROM platform_request_rate_windows WHERE subject_kind = 'CREDENTIAL'"
        )
        .await,
        1
    );

    h.clock.advance(Duration::minutes(10));
    let cutoff = calling::domain::Clock::now(&h.clock)
        .duration_trunc(Duration::minutes(1))
        .unwrap()
        - Duration::minutes(5);
    let pruned = h.state.rate_limiter.prune(cutoff).await.unwrap();
    // One window per subject: credential, application and tenant (Part G §70).
    assert_eq!(pruned, 3);
    assert_eq!(
        common::scalar_i64(
            h.conn(),
            "SELECT count(*) FROM platform_request_rate_windows"
        )
        .await,
        0
    );
}

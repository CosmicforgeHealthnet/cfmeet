//! H. Credential rotation with overlap, and revocation during rotation — performed by
//! Platform Access on its (wall) clock and enforced through it. These tests run Calling's
//! clock at the wall time too, so Calling's projected view (effective state) agrees with
//! the authority; the end of an overlap is driven by Platform Access's own tests
//! (manual clock), not waited for here.

mod common;

use common::{IssueCredential, RotateCredential};

use axum::http::StatusCode;
use calling::{
    domain::{EnvironmentKind, TenantClassification},
    platform::{
        authentication::{AuthFailureReason, AuthenticationError},
        credentials::CredentialEffectiveState,
        limits::LimitValues,
        scopes::names,
        PlatformError,
    },
};
use chrono::{Duration, Timelike, Utc};
use common::Harness;

/// A harness whose Calling clock is the wall clock (Platform Access's time base).
async fn wall_clock_harness() -> Option<Harness> {
    let db = common::TestDb::new().await?;
    Some(Harness::on(
        std::sync::Arc::new(db),
        calling::domain::ManualClock::new(Utc::now()),
    ))
}

fn overlap(minutes: i64) -> RotateCredential {
    RotateCredential {
        overlap: Some(Duration::minutes(minutes)),
        ..Default::default()
    }
}

#[tokio::test]
async fn old_and_new_are_valid_during_overlap_then_only_new() {
    let Some(h) = wall_clock_harness().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let old = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let old_token = old.secret.expose_once().to_string();

    // Before rotation.
    assert_eq!(h.whoami(&old_token).await.status, StatusCode::OK);

    let before = Utc::now();
    let new = h
        .platform
        .rotate_credential(old.credential.id, overlap(60))
        .await
        .unwrap();
    let new_token = new.secret.expose_once().to_string();
    assert_ne!(new_token, old_token);
    assert_ne!(new.credential.public_id, old.credential.public_id);

    // Same authority, same scopes, linked both ways.
    assert_eq!(
        new.credential.service_account_id,
        old.credential.service_account_id
    );
    assert_eq!(new.credential.environment_id, old.credential.environment_id);
    assert_eq!(new.credential.application_id, old.credential.application_id);
    assert_eq!(new.credential.tenant_id, old.credential.tenant_id);
    assert_eq!(new.credential.scopes, old.credential.scopes);
    assert_eq!(
        new.credential.rotated_from_credential_id,
        Some(old.credential.id)
    );
    h.clock.set(Utc::now());
    let old_view = h.platform.credential(old.credential.id).await.unwrap();
    assert_eq!(
        old_view.superseded_by_credential_id,
        Some(new.credential.id)
    );
    let rotated_at = old_view.superseded_at.expect("superseded");
    assert!(rotated_at >= before && rotated_at <= Utc::now());
    assert_eq!(
        old_view.supersession_valid_until,
        Some(rotated_at + Duration::minutes(60))
    );
    assert_eq!(
        old_view.expires_at, old.credential.expires_at,
        "original expiry untouched"
    );
    assert_eq!(
        old_view.effective_state,
        CredentialEffectiveState::RotationOverlap
    );

    // During overlap: both valid.
    assert_eq!(h.whoami(&old_token).await.status, StatusCode::OK);
    assert_eq!(h.whoami(&new_token).await.status, StatusCode::OK);

    // Once the overlap is over Calling's projected view says SUPERSEDED (the authority's
    // own rejection after the overlap is `credential_superseded`, proven by Platform
    // Access's authentication tests and by `zero_overlap_...` below).
    h.clock.advance(Duration::minutes(61));
    assert_eq!(
        h.platform
            .credential(old.credential.id)
            .await
            .unwrap()
            .effective_state,
        CredentialEffectiveState::Superseded
    );
    assert_eq!(h.whoami(&new_token).await.status, StatusCode::OK);
}

#[tokio::test]
async fn explicit_revocation_of_the_old_credential_is_immediate_during_overlap() {
    let Some(h) = wall_clock_harness().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let old = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let new = h
        .platform
        .rotate_credential(old.credential.id, overlap(120))
        .await
        .unwrap();
    assert_eq!(
        h.whoami(old.secret.expose_once()).await.status,
        StatusCode::OK
    );

    h.platform
        .revoke_credential(old.credential.id)
        .await
        .unwrap();
    assert_eq!(
        h.whoami(old.secret.expose_once()).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.whoami(new.secret.expose_once()).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn overlap_never_extends_the_original_expiry() {
    let Some(h) = wall_clock_harness().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let expires_at = Utc::now()
        .with_nanosecond(0)
        .map(|t| t + Duration::minutes(10))
        .unwrap();
    let old = h
        .platform
        .issue_credential(
            c.sa(EnvironmentKind::Production),
            IssueCredential {
                expires_at: Some(expires_at),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let new = h
        .platform
        .rotate_credential(old.credential.id, overlap(24 * 60))
        .await
        .unwrap();
    let old_view = h.platform.credential(old.credential.id).await.unwrap();
    assert_eq!(old_view.supersession_valid_until, Some(expires_at));
    assert_eq!(old_view.expires_at, expires_at);
    assert_eq!(
        h.whoami(new.secret.expose_once()).await.status,
        StatusCode::OK
    );

    // The database refuses an overlap beyond the expiry as well.
    let raw = common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_api_credentials SET supersession_valid_until = expires_at + interval '1 day' \
             WHERE id = '{}'",
            new.credential.id
        ),
    )
    .await;
    assert!(raw.is_err());
}

#[tokio::test]
async fn zero_overlap_invalidates_the_old_credential_at_once() {
    let Some(h) = wall_clock_harness().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let old = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let new = h
        .platform
        .rotate_credential(old.credential.id, overlap(0))
        .await
        .unwrap();
    assert_eq!(
        h.whoami(old.secret.expose_once()).await.status,
        StatusCode::UNAUTHORIZED
    );
    match h.authenticate(old.secret.expose_once()).await {
        Err(AuthenticationError::Rejected { reason, .. }) => {
            assert_eq!(reason, AuthFailureReason::CredentialSuperseded)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        h.whoami(new.secret.expose_once()).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn only_an_active_head_credential_can_be_rotated() {
    let Some(h) = wall_clock_harness().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let p = &h.platform;

    // Rotating twice: only the successor can be rotated.
    let first = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let second = p
        .rotate_credential(first.credential.id, overlap(10))
        .await
        .unwrap();
    let err = p
        .rotate_credential(first.credential.id, overlap(10))
        .await
        .unwrap_err();
    assert!(matches!(err, PlatformError::InvalidState(_)), "{err}");
    p.rotate_credential(second.credential.id, overlap(10))
        .await
        .unwrap();

    // Revoked credentials cannot be rotated.
    let revoked = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    p.revoke_credential(revoked.credential.id).await.unwrap();
    assert!(matches!(
        p.rotate_credential(revoked.credential.id, overlap(10))
            .await
            .unwrap_err(),
        PlatformError::InvalidState(_)
    ));

    // Expired credentials cannot be rotated.
    let short = p
        .issue_credential(
            c.sa(EnvironmentKind::Production),
            IssueCredential {
                expires_at: Some(Utc::now() + Duration::seconds(2)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2_300)).await;
    assert!(matches!(
        p.rotate_credential(short.credential.id, overlap(10))
            .await
            .unwrap_err(),
        PlatformError::InvalidState(_)
    ));

    // Overlap beyond the configured maximum is refused.
    let fresh = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    assert!(matches!(
        p.rotate_credential(fresh.credential.id, overlap(31 * 24 * 60))
            .await
            .unwrap_err(),
        PlatformError::PolicyViolation(_)
    ));
    assert!(matches!(
        p.rotate_credential(fresh.credential.id, overlap(-1))
            .await
            .unwrap_err(),
        PlatformError::PolicyViolation(_)
    ));
}

#[tokio::test]
async fn concurrent_rotations_produce_exactly_one_successor() {
    let Some(h) = wall_clock_harness().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let old = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let (a, b, d) = tokio::join!(
        h.platform.rotate_credential(old.credential.id, overlap(10)),
        h.platform.rotate_credential(old.credential.id, overlap(10)),
        h.platform.rotate_credential(old.credential.id, overlap(10)),
    );
    let successes = [a.is_ok(), b.is_ok(), d.is_ok()]
        .iter()
        .filter(|ok| **ok)
        .count();
    assert_eq!(successes, 1);
    let successors = common::scalar_i64(
        h.conn(),
        &format!(
            "SELECT count(*) FROM platform_api_credentials WHERE rotated_from_credential_id = '{}'",
            old.credential.id
        ),
    )
    .await;
    assert_eq!(successors, 1);
}

#[tokio::test]
async fn rotation_preserves_credential_limits_and_label() {
    let Some(h) = wall_clock_harness().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let old = h
        .platform
        .issue_credential(
            c.sa(EnvironmentKind::Production),
            IssueCredential {
                label: Some("primary".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let limits = LimitValues {
        requests_per_minute: Some(42),
        max_concurrent_communications: Some(3),
        ..Default::default()
    };
    h.platform
        .set_credential_limits(old.credential.id, limits)
        .await
        .unwrap();
    let new = h
        .platform
        .rotate_credential(old.credential.id, overlap(5))
        .await
        .unwrap();
    assert_eq!(new.credential.label.as_deref(), Some("primary"));
    let effective = h
        .platform
        .effective_limits(new.credential.id)
        .await
        .unwrap();
    assert_eq!(effective.credential, limits);
}

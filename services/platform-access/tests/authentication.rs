//! Credential authentication through the internal contract (Part IV §B3 / §B6.2–3):
//! the authority chain, every rejection reason, rotation overlap, expiry, revocation,
//! suspension (safety-only authority), failed-authentication throttling and pepper
//! rotation.

mod common;

use axum::http::{Method, StatusCode};
use chrono::Duration;
use common::*;
use platform_access::identity::verifier::{CredentialPepper, VerifierKeyring};
use serde_json::json;

const SCOPES: &[&str] = &["calls.read", "calls.write", "numbers.read"];

#[tokio::test]
async fn a_valid_credential_returns_its_full_trusted_authority_chain() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", SCOPES).await;
    let (view, secret) = h.issue(c.sa("SANDBOX"), &["calls.read", "numbers.read"]).await;
    let r = h.authenticate(&secret).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["outcome"], "AUTHENTICATED");
    let a = &r.body["authority"];
    assert_eq!(a["authorityMode"], "FULL");
    assert!(a["suspendedLink"].is_null());
    assert_eq!(uuid(&a["tenant"]["id"]), c.tenant_id);
    assert_eq!(a["tenant"]["classification"], "EXTERNAL");
    assert_eq!(uuid(&a["application"]["id"]), c.application_id);
    assert_eq!(uuid(&a["environment"]["id"]), c.env("SANDBOX"));
    assert_eq!(a["environment"]["kind"], "SANDBOX");
    assert_eq!(uuid(&a["serviceAccount"]["id"]), c.sa("SANDBOX"));
    assert_eq!(a["credential"]["id"], view["id"]);
    assert_eq!(a["effectiveState"], "ACTIVE");
    assert_eq!(a["scopes"], json!(["calls.read", "numbers.read"]));
    assert!(a["principalRestriction"].is_null());
    // No verifier material anywhere in the response.
    let text = r.body.to_string();
    for forbidden in ["verifier", "pepper", &secret[secret.len() - 64..]] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
}

#[tokio::test]
async fn malformed_unknown_wrong_secret_and_wrong_tag_are_rejected() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", SCOPES).await;
    let (_, secret) = h.issue(c.sa("PRODUCTION"), &["calls.read"]).await;
    let cases = [
        ("not-a-credential".to_string(), "malformed_credential"),
        (
            format!("cfmk_live_{}_{}", "0".repeat(24), "1".repeat(64)),
            "unknown_credential",
        ),
        (
            format!("{}{}", &secret[..secret.len() - 1], if secret.ends_with('0') { "1" } else { "0" }),
            "secret_mismatch",
        ),
        // The tag is bound into the verifier: an edited tag never verifies.
        (secret.replacen("cfmk_live_", "cfmk_sbx_", 1), "secret_mismatch"),
    ];
    for (presented, reason) in cases {
        let r = h.authenticate(&presented).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.body["outcome"], "REJECTED", "{presented}");
        assert_eq!(r.body["reason"], reason, "{presented}");
        assert!(r.body.get("authority").is_none());
    }
}

#[tokio::test]
async fn rotation_overlap_expiry_and_revocation_follow_the_lifecycle() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", SCOPES).await;
    let (old, old_secret) = h.issue(c.sa("TEST"), &["calls.read"]).await;
    let old_id = uuid(&old["id"]);
    let rotated = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/credentials/{old_id}/rotate"),
            Some(json!({ "overlapMinutes": 30 })),
        )
        .await;
    let new_secret = rotated["secret"].as_str().unwrap().to_string();
    assert_eq!(rotated["credential"]["rotatedFromCredentialId"], old["id"]);
    // Both work during the overlap; the predecessor reports ROTATION_OVERLAP.
    let r = h.authenticate(&old_secret).await;
    assert_eq!(r.body["authority"]["effectiveState"], "ROTATION_OVERLAP");
    assert_eq!(h.authenticate(&new_secret).await.body["outcome"], "AUTHENTICATED");
    // After the overlap the predecessor is superseded.
    h.clock.advance(Duration::minutes(31));
    let r = h.authenticate(&old_secret).await;
    assert_eq!(r.body["reason"], "credential_superseded");
    assert_eq!(h.authenticate(&new_secret).await.body["outcome"], "AUTHENTICATED");
    // Revocation is immediate (no cache).
    let new_id = uuid(&rotated["credential"]["id"]);
    h.admin_ok(Method::POST, &format!("/internal/v1/admin/credentials/{new_id}/revoke"), None)
        .await;
    assert_eq!(h.authenticate(&new_secret).await.body["reason"], "credential_revoked");
    // Expiry.
    let (_, short) = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/service-accounts/{}/credentials", c.sa("TEST")),
            Some(json!({ "scopes": ["calls.read"], "expiresAt": (h.clock_now() + Duration::days(1)).to_rfc3339() })),
        )
        .await
        .pipe(|b| (b["credential"].clone(), b["secret"].as_str().unwrap().to_string()));
    assert_eq!(h.authenticate(&short).await.body["outcome"], "AUTHENTICATED");
    h.clock.advance(Duration::days(2));
    assert_eq!(h.authenticate(&short).await.body["reason"], "credential_expired");
}

#[tokio::test]
async fn not_yet_valid_credentials_do_not_authenticate() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", SCOPES).await;
    let body = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/service-accounts/{}/credentials", c.sa("SANDBOX")),
            Some(json!({ "scopes": [], "notBefore": (h.clock_now() + Duration::hours(1)).to_rfc3339() })),
        )
        .await;
    let secret = body["secret"].as_str().unwrap();
    assert_eq!(h.authenticate(secret).await.body["reason"], "credential_not_yet_valid");
    h.clock.advance(Duration::hours(2));
    assert_eq!(h.authenticate(secret).await.body["outcome"], "AUTHENTICATED");
}

#[tokio::test]
async fn suspended_scopes_yield_safety_only_authority_and_inactive_accounts_fail() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", SCOPES).await;
    let (_, secret) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    let env = c.env("SANDBOX");
    for (path, link) in [
        (format!("/internal/v1/admin/environments/{env}/status"), "ENVIRONMENT"),
        (format!("/internal/v1/admin/applications/{}/status", c.application_id), "APPLICATION"),
        (format!("/internal/v1/admin/tenants/{}/status", c.tenant_id), "TENANT"),
    ] {
        h.admin_ok(Method::POST, &path, Some(json!({ "status": "SUSPENDED" }))).await;
        let r = h.authenticate(&secret).await;
        assert_eq!(r.body["outcome"], "AUTHENTICATED");
        assert_eq!(r.body["authority"]["authorityMode"], "SAFETY_ONLY");
        // The OUTERMOST suspended link is reported.
        assert_eq!(r.body["authority"]["suspendedLink"], link);
    }
    for path in [
        format!("/internal/v1/admin/tenants/{}/status", c.tenant_id),
        format!("/internal/v1/admin/applications/{}/status", c.application_id),
        format!("/internal/v1/admin/environments/{env}/status"),
    ] {
        h.admin_ok(Method::POST, &path, Some(json!({ "status": "ACTIVE" }))).await;
    }
    assert_eq!(h.authenticate(&secret).await.body["authority"]["authorityMode"], "FULL");
    let sa = c.sa("SANDBOX");
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/service-accounts/{sa}/status"),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(h.authenticate(&secret).await.body["reason"], "service_account_inactive");
}

#[tokio::test]
async fn repeated_failures_are_throttled_without_locking_out_the_correct_secret() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", SCOPES).await;
    let (_, secret) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    let wrong = format!("{}{}", &secret[..secret.len() - 1], if secret.ends_with('0') { "1" } else { "0" });
    let mut crossed = 0;
    for i in 1..=25 {
        let r = h.authenticate(&wrong).await;
        assert_eq!(r.body["reason"], "secret_mismatch");
        let throttle = &r.body["throttle"];
        if i <= 20 {
            assert!(throttle.is_null(), "attempt {i}");
        } else {
            assert!(throttle["retryAfterSeconds"].as_u64().unwrap() >= 1);
            if throttle["thresholdCrossed"] == true {
                crossed += 1;
            }
        }
    }
    assert_eq!(crossed, 1, "the guessing signal is raised exactly once per window");
    // The correct secret is never refused by the window.
    assert_eq!(h.authenticate(&secret).await.body["outcome"], "AUTHENTICATED");
}

#[tokio::test]
async fn previous_pepper_keeps_verifying_during_a_pepper_rotation() {
    let Some(h) = Harness::new().await else { return };
    let c = h.consumer("acme", "EXTERNAL", SCOPES).await;
    let (_, minted_v1) = h.issue(c.sa("SANDBOX"), &["calls.read"]).await;
    // Deploy pepper v2 with v1 as the previous pepper: old credentials still verify.
    let rotated = VerifierKeyring::new(
        2,
        CredentialPepper::parse("P", "rotated-pepper-v2-0123456789abcdefghijklmnopqrstuv").unwrap(),
        Some((1, CredentialPepper::parse("P", TEST_PEPPER).unwrap())),
    )
    .unwrap();
    let h2 = Harness::on(h.db.clone(), rotated);
    assert_eq!(h2.authenticate(&minted_v1).await.body["outcome"], "AUTHENTICATED");
    // Without the previous pepper the v1 verifier is unsupported (fail closed).
    let only_v2 = VerifierKeyring::new(
        2,
        CredentialPepper::parse("P", "rotated-pepper-v2-0123456789abcdefghijklmnopqrstuv").unwrap(),
        None,
    )
    .unwrap();
    let h3 = Harness::on(h.db.clone(), only_v2);
    assert_eq!(h3.authenticate(&minted_v1).await.body["reason"], "unsupported_verifier");
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

trait ClockNow {
    fn clock_now(&self) -> chrono::DateTime<chrono::Utc>;
}
impl ClockNow for Harness {
    fn clock_now(&self) -> chrono::DateTime<chrono::Utc> {
        use platform_access::domain::Clock;
        self.clock.now()
    }
}

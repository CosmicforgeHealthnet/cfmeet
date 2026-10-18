//! Part IV §B2 / §B6.1 end to end, across the REAL binaries: credentials minted by the
//! Part III authority (Calling) keep authenticating — same identifiers, same lifecycle
//! state, same secret — after the controlled hand-off to Platform Access:
//!
//! `platform_admin identity export` → `platform-access-admin identity import` →
//! `platform_admin identity verify-handoff` → `platform_admin identity complete-handoff`,
//! and the rollback path `platform-access-admin identity export` →
//! `platform_admin identity restore-verifiers`.

mod common;

use std::{path::Path, process::Command, sync::Arc};

use calling::{
    domain::{ManualClock, RequestContext},
    platform::authentication::{AuthFailureReason, AuthenticationError},
};
use chrono::{Duration, Utc};
use common::{identity::PlatformAccess, Harness, TestDb, TEST_PEPPER};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use uuid::Uuid;

const OPERATOR: &str = "ops.handoff";

/// The Part III verifier construction, computed independently of both services.
fn part_iii_verifier(tag: &str, public_id: &str, secret: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(TEST_PEPPER.as_bytes()).unwrap();
    for part in [
        b"cfmeet.calling.api-credential.v1".as_slice(),
        tag.as_bytes(),
        public_id.as_bytes(),
    ] {
        mac.update(part);
        mac.update(&[0]);
    }
    mac.update(secret);
    mac.finalize().into_bytes().to_vec()
}

struct Minted {
    id: Uuid,
    token: String,
    verifier_hex: String,
}

fn mint() -> (String, Vec<u8>) {
    let public = hex::encode(&Uuid::new_v4().as_bytes()[..12]);
    let mut secret = Uuid::new_v4().as_bytes().to_vec();
    secret.extend_from_slice(Uuid::new_v4().as_bytes());
    (public, secret)
}

/// Writes a Part III identity estate exactly as the Part III Calling stored it (keyed
/// verifiers, no Platform Access revision): one sandbox scope, an active and a revoked
/// credential, a principal.
async fn part_iii_estate(h: &Harness) -> (Uuid, Minted, Minted, Uuid) {
    let (t, a, e, sa, p) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let created = Utc::now() - Duration::days(30);
    let expires = Utc::now() + Duration::days(300);
    let mut sql = format!(
        "INSERT INTO platform_tenants (id, slug, display_name, classification, status, billing_currency, created_at, updated_at) \
           VALUES ('{t}', 'legacy', 'Legacy', 'EXTERNAL', 'ACTIVE', 'NGN', '{created}', '{created}'); \
         INSERT INTO platform_applications (id, tenant_id, slug, display_name, status, created_at, updated_at) \
           VALUES ('{a}', '{t}', 'app', 'App', 'ACTIVE', '{created}', '{created}'); \
         INSERT INTO platform_application_scopes (application_id, scope, tenant_id, granted_at) \
           VALUES ('{a}', 'calls.read', '{t}', '{created}'); \
         INSERT INTO platform_environments (id, tenant_id, application_id, kind, status, live_telecom_allowed, created_at, updated_at) \
           VALUES ('{e}', '{t}', '{a}', 'SANDBOX', 'ACTIVE', false, '{created}', '{created}'); \
         INSERT INTO platform_service_accounts (id, tenant_id, application_id, environment_id, name, status, created_at, updated_at) \
           VALUES ('{sa}', '{t}', '{a}', '{e}', 'backend', 'ACTIVE', '{created}', '{created}'); \
         INSERT INTO platform_principals (id, tenant_id, application_id, environment_id, kind, external_reference, status, created_at, updated_at) \
           VALUES ('{p}', '{t}', '{a}', '{e}', 'NATIVE_CF_USER', 'user-1', 'ACTIVE', '{created}', '{created}'); "
    );
    let mut minted = Vec::new();
    for revoked in [false, true] {
        let id = Uuid::new_v4();
        let (public_id, secret) = mint();
        let verifier = part_iii_verifier("sbx", &public_id, &secret);
        let secret_hex = hex::encode(&secret);
        let (status, revoked_at) = if revoked {
            ("REVOKED", format!("'{}'", Utc::now() - Duration::days(1)))
        } else {
            ("ACTIVE", "NULL".to_string())
        };
        sql.push_str(&format!(
            "INSERT INTO platform_api_credentials (id, public_id, tenant_id, application_id, environment_id, \
               service_account_id, label, verifier, verifier_algorithm, verifier_key_version, last_four, status, \
               not_before, expires_at, revoked_at, created_at, updated_at) \
             VALUES ('{id}', '{public_id}', '{t}', '{a}', '{e}', '{sa}', 'part-iii', '\\x{}', 'HMAC_SHA256', 1, '{}', \
               '{status}', '{created}', '{expires}', {revoked_at}, '{created}', '{created}'); \
             INSERT INTO platform_api_credential_scopes (credential_id, scope, tenant_id, application_id, granted_at) \
             VALUES ('{id}', 'calls.read', '{t}', '{a}', '{created}'); ",
            hex::encode(&verifier),
            &secret_hex[60..],
        ));
        minted.push(Minted {
            id,
            token: format!("cfmk_sbx_{public_id}_{secret_hex}"),
            verifier_hex: hex::encode(&verifier),
        });
    }
    common::exec(h.conn(), &sql).await.unwrap();
    let revoked = minted.pop().unwrap();
    let active = minted.pop().unwrap();
    (t, active, revoked, p)
}

fn run(bin: &Path, args: &[&str], env: &[(String, String)]) -> Value {
    let mut all = vec!["--operator", OPERATOR];
    all.extend_from_slice(args);
    let out = Command::new(bin)
        .args(&all)
        .current_dir(std::env::temp_dir())
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .expect("operator CLI runs");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(out.status.success(), "{bin:?} {args:?}: {stderr}");
    assert!(
        !stderr.contains("cfmk_"),
        "a credential reached the CLI logs"
    );
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"))
}

async fn reason(h: &Harness, token: &str) -> AuthFailureReason {
    match h
        .state
        .authenticator
        .authenticate(token, RequestContext::generate())
        .await
    {
        Err(AuthenticationError::Rejected { reason, .. }) => reason,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn part_iii_credentials_survive_the_handoff_and_the_rollback_path() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let h = Harness::on(Arc::new(db), ManualClock::new(Utc::now()));
    let (tenant, active, revoked, principal) = part_iii_estate(&h).await;
    let pa = h.db.pa();
    let calling_env = {
        let mut env = pa.calling_process_env();
        env.push(("DATABASE_URL".into(), h.db.url.clone()));
        env
    };
    let pa_env = vec![
        ("DATABASE_URL".to_string(), pa.database_url()),
        ("API_CREDENTIAL_PEPPER".to_string(), TEST_PEPPER.to_string()),
    ];
    let calling_admin = Path::new(env!("CARGO_BIN_EXE_platform_admin"));
    let pa_admin = PlatformAccess::admin_bin();
    let dir = std::env::temp_dir().join(format!("cf-handoff-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();

    // Before the hand-off Platform Access does not know the Part III credential.
    assert_eq!(
        reason(&h, &active.token).await,
        AuthFailureReason::UnknownCredential
    );

    // 1. Export from Calling (keyed verifiers, never secrets).
    let bundle = dir.join("bundle.json");
    let exported = run(
        calling_admin,
        &["identity", "export", "--out", bundle.to_str().unwrap()],
        &calling_env,
    );
    let digest = exported["bundleDigest"].as_str().unwrap().to_string();
    let text = std::fs::read_to_string(&bundle).unwrap();
    assert!(
        text.contains(&active.verifier_hex),
        "the export carries the keyed verifier"
    );
    for m in [&active, &revoked] {
        assert!(
            !text.contains(secret_hex(&m.token)),
            "the export carries a secret"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&bundle).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Verification before the import fails closed (no receipt yet).
    let mut early = Command::new(calling_admin);
    early
        .args([
            "--operator",
            OPERATOR,
            "identity",
            "verify-handoff",
            "--digest",
            &digest,
        ])
        .env_clear()
        .envs(calling_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    assert!(!early.output().unwrap().status.success());

    // 2. Import into Platform Access (same ids, same verifiers).
    let receipt = run(
        &pa_admin,
        &["identity", "import", "--bundle", bundle.to_str().unwrap()],
        &pa_env,
    );
    assert_eq!(receipt["bundleDigest"], digest.as_str(), "{receipt}");
    // Idempotent per digest.
    let again = run(
        &pa_admin,
        &["identity", "import", "--bundle", bundle.to_str().unwrap()],
        &pa_env,
    );
    assert_eq!(again["bundleDigest"], digest.as_str());
    assert_eq!(again["alreadyImported"], true);

    // 3. Calling verifies the receipt against its export.
    let verified = run(
        calling_admin,
        &["identity", "verify-handoff", "--digest", &digest],
        &calling_env,
    );
    assert!(verified["verifiedAt"].is_string(), "{verified}");

    // The SAME credential now authenticates through Platform Access, with the same
    // identifiers; the revoked one stays revoked.
    h.platform.sync().await;
    let ctx = h.ctx(&active.token).await;
    assert_eq!(ctx.tenant_id().as_uuid(), tenant);
    assert_eq!(ctx.credential_id().as_uuid(), active.id);
    assert!(ctx.has_scope("calls.read"));
    assert_eq!(
        reason(&h, &revoked.token).await,
        AuthFailureReason::CredentialRevoked
    );
    let resolved = h
        .state
        .principals
        .resolve(
            &ctx,
            &calling::platform::principals::PrincipalReference::Id(
                calling::domain::PrincipalId::from_uuid(principal),
            ),
        )
        .await
        .unwrap();
    assert_eq!(resolved.id().as_uuid(), principal);

    // 4. Complete: Calling's verifier material is scrubbed (idempotent).
    let completed = run(
        calling_admin,
        &["identity", "complete-handoff", "--digest", &digest],
        &calling_env,
    );
    assert_eq!(completed["verifiersScrubbed"], 2);
    let again = run(
        calling_admin,
        &["identity", "complete-handoff", "--digest", &digest],
        &calling_env,
    );
    assert_eq!(again["verifiersScrubbed"], 0);
    assert_eq!(
        common::scalar_i64(
            h.conn(),
            "SELECT count(*) FROM platform_api_credentials WHERE verifier IS NOT NULL"
        )
        .await,
        0
    );
    let dump = common::dump_database(h.conn()).await;
    assert!(!dump.contains(&active.verifier_hex));
    h.ctx(&active.token).await;

    // Rollback path: Platform Access's recovery export restores Calling's verifiers
    // (only into credentials without one; byte-identical).
    let recovery = dir.join("recovery.json");
    run(
        &pa_admin,
        &["identity", "export", "--out", recovery.to_str().unwrap()],
        &pa_env,
    );
    let restored = run(
        calling_admin,
        &[
            "identity",
            "restore-verifiers",
            "--bundle",
            recovery.to_str().unwrap(),
        ],
        &calling_env,
    );
    assert_eq!(restored["restored"], 2, "{restored}");
    assert_eq!(
        common::scalar_i64(
            h.conn(),
            &format!(
                "SELECT count(*) FROM platform_api_credentials WHERE id = '{}' AND verifier = '\\x{}'",
                active.id, active.verifier_hex
            )
        )
        .await,
        1
    );
    // A verifier is never REPLACED by another value.
    assert!(common::exec(
        h.conn(),
        &format!(
            "UPDATE platform_api_credentials SET verifier = '\\x{}' WHERE id = '{}'",
            "00".repeat(32),
            active.id
        )
    )
    .await
    .is_err());

    // The hand-off is recorded (digests and counts only).
    let status = run(
        calling_admin,
        &["identity", "status", "--digest", &digest],
        &calling_env,
    );
    assert!(!status.to_string().contains(&active.verifier_hex));
    std::fs::remove_dir_all(&dir).ok();
}

fn secret_hex(token: &str) -> &str {
    token.rsplit('_').next().unwrap()
}

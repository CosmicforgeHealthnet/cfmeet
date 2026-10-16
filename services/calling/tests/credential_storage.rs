//! D. Credential storage: the raw credential is returned once and never persisted; only a
//! keyed verifier is stored; every read path is masked.

mod common;

use calling::{
    domain::{EnvironmentKind, TenantClassification},
    models::api_credentials,
    platform::{credentials::CredentialFilter, error::summarize_db_error, scopes::names},
};
use common::{dump_database, secret_part, Harness};
use hmac::{Hmac, Mac};
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn raw_secret_is_never_persisted_only_a_keyed_verifier() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let token = issued.secret.expose_once().to_string();
    let secret_hex = secret_part(&token).to_string();
    assert_eq!(secret_hex.len(), 64);

    // Search every row of every table for the secret (and its halves).
    let dump = dump_database(h.conn()).await;
    assert!(dump.contains(&issued.credential.public_id), "dump sanity");
    for needle in [
        token.as_str(),
        secret_hex.as_str(),
        &secret_hex[..32],
        &secret_hex[32..],
    ] {
        assert!(
            !dump.contains(needle),
            "raw secret material found in the database"
        );
    }

    // The verifier is present: 32 bytes, keyed algorithm, version 1.
    let row = api_credentials::Entity::find_by_id(issued.credential.id.as_uuid())
        .one(h.conn())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.verifier.0.len(), 32);
    assert_eq!(row.verifier_algorithm, "HMAC_SHA256");
    assert_eq!(row.verifier_key_version, 1);
    assert_eq!(row.last_four, &secret_hex[60..]);

    // It is not an unkeyed hash of the secret or the token.
    let secret_bytes = hex::decode(&secret_hex).unwrap();
    assert_ne!(row.verifier.0, Sha256::digest(&secret_bytes).to_vec());
    assert_ne!(row.verifier.0, Sha256::digest(token.as_bytes()).to_vec());
    // Nor an HMAC under some other key (e.g. an empty pepper).
    let mut unkeyed = Hmac::<Sha256>::new_from_slice(b"").unwrap();
    unkeyed.update(&secret_bytes);
    assert_ne!(row.verifier.0, unkeyed.finalize().into_bytes().to_vec());
    // The Debug output of the row does not reveal the verifier.
    assert!(format!("{row:?}").contains("Verifier([REDACTED])"));
}

#[tokio::test]
async fn creation_result_contains_the_secret_exactly_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let token = issued.secret.expose_once();
    assert!(token.starts_with(&format!("cfmk_live_{}_", issued.credential.public_id)));
    // The authenticated round-trip works with exactly this value.
    h.ctx(token).await;

    // The safe view inside the result never carries the secret.
    let view_json = serde_json::to_string(&issued.credential).unwrap();
    assert!(!view_json.contains(secret_part(token)));
    assert!(!view_json.contains("\"verifier\""));
    // Debug of the whole issuance result is redacted.
    let debug = format!("{issued:?}");
    assert!(!debug.contains(secret_part(token)));
    assert!(debug.contains("[REDACTED]"));
}

#[tokio::test]
async fn read_and_list_views_are_masked() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let token = issued.secret.expose_once().to_string();
    let secret = secret_part(&token).to_string();

    let shown = h.platform.credential(issued.credential.id).await.unwrap();
    let by_public = h
        .platform
        .credential_by_public_id(&issued.credential.public_id)
        .await
        .unwrap();
    let listed = h
        .platform
        .list_credentials(CredentialFilter::Application(c.application.id))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    for view in [&shown, &by_public, &listed[0]] {
        let json = serde_json::to_string(view).unwrap();
        assert!(!json.contains(&secret), "view leaks the secret: {json}");
        assert!(!json.contains("\"verifier\""));
        assert!(view
            .masked
            .starts_with(&format!("cfmk_live_{}_", view.public_id)));
        assert!(view.masked.ends_with(&secret[60..]));
        assert!(view.masked.contains("****"));
        assert_eq!(view.scopes, vec![names::PRINCIPALS_READ.to_string()]);
    }
}

#[tokio::test]
async fn database_error_summaries_never_echo_row_content() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let row = api_credentials::Entity::find_by_id(issued.credential.id.as_uuid())
        .one(h.conn())
        .await
        .unwrap()
        .unwrap();
    // Violate the verifier-length CHECK with a recognisable value: PostgreSQL's error detail
    // would echo the failing row; the summary must not.
    let marker = vec![0xAB; 31];
    let mut bad: api_credentials::ActiveModel = row.into();
    bad.id = Set(uuid::Uuid::new_v4());
    bad.public_id = Set("0123456789abcdef01234567".to_string());
    bad.verifier = Set(api_credentials::Verifier(marker));
    bad.rotated_from_credential_id = Set(None);
    let err = bad.insert(h.conn()).await.unwrap_err();
    let summary = summarize_db_error(&err);
    assert!(
        summary.contains("ck_platform_api_credentials_verifier_length"),
        "{summary}"
    );
    assert!(!summary.to_lowercase().contains("abab"), "{summary}");
    let platform_error = calling::platform::PlatformError::from(err);
    assert!(!platform_error.to_string().to_lowercase().contains("abab"));
}

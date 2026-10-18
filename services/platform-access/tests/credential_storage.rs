//! Credential storage (moved here from Calling with the authority): the raw credential is
//! returned once and never persisted; only a keyed verifier is stored; every read path is
//! masked; database error summaries never echo row content.

mod common;

use axum::http::Method;
use common::{uuid, Harness, TEST_PEPPER};
use hmac::{Hmac, Mac};
use platform_access::{
    domain::{ApplicationId, CredentialId},
    identity::{credentials::CredentialFilter, error::summarize_db_error, PlatformError},
    models::api_credentials,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DatabaseConnection, EntityTrait, Statement,
};
use sha2::{Digest, Sha256};

fn secret_part(token: &str) -> &str {
    token.rsplit('_').next().unwrap()
}

/// Every row of every Platform Access table, as text.
async fn dump_database(conn: &DatabaseConnection) -> String {
    let tables: Vec<String> = conn
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT tablename FROM pg_tables WHERE schemaname = 'public' ORDER BY tablename",
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect();
    let mut out = String::new();
    for table in tables {
        for row in conn
            .query_all(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT t::text FROM {table} t"),
            ))
            .await
            .unwrap()
        {
            out.push_str(&row.try_get_by_index::<String>(0).unwrap());
            out.push('\n');
        }
    }
    out
}

#[tokio::test]
async fn raw_secret_is_never_persisted_only_a_keyed_verifier() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["principals.read"]).await;
    let (credential, token) = h.issue(c.sa("PRODUCTION"), &["principals.read"]).await;
    let secret_hex = secret_part(&token).to_string();
    assert_eq!(secret_hex.len(), 64);

    // Search every row of every table — the change feed and audit included — for the
    // secret and its halves.
    let dump = dump_database(h.conn()).await;
    assert!(
        dump.contains(credential["publicId"].as_str().unwrap()),
        "dump sanity"
    );
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

    let row = api_credentials::Entity::find_by_id(uuid(&credential["id"]))
        .one(h.conn())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.verifier.0.len(), 32);
    assert_eq!(row.verifier_algorithm, "HMAC_SHA256");
    assert_eq!(row.verifier_key_version, 1);
    assert_eq!(row.last_four, &secret_hex[60..]);

    // Keyed: not an unkeyed hash of the secret or the token, nor an HMAC under an empty
    // key — and exactly the Part III construction under the configured pepper.
    let secret_bytes: [u8; 32] = hex::decode(&secret_hex).unwrap().try_into().unwrap();
    assert_ne!(row.verifier.0, Sha256::digest(secret_bytes).to_vec());
    assert_ne!(row.verifier.0, Sha256::digest(token.as_bytes()).to_vec());
    let mut unkeyed = Hmac::<Sha256>::new_from_slice(b"").unwrap();
    unkeyed.update(&secret_bytes);
    assert_ne!(row.verifier.0, unkeyed.finalize().into_bytes().to_vec());
    assert_eq!(
        row.verifier.0,
        common::part_iii_verifier(TEST_PEPPER, "live", &row.public_id, &secret_bytes)
    );
    // Only the credential row holds the verifier: the change feed and audit never do.
    let verifier_hex = hex::encode(&row.verifier.0);
    for table in ["platform_identity_changes", "platform_access_audit"] {
        let text = h
            .conn()
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!("SELECT coalesce(string_agg(t::text, ''), '') FROM {table} t"),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index::<String>(0)
            .unwrap();
        assert!(
            !text.contains(&verifier_hex),
            "{table} carries the verifier"
        );
    }
    // The Debug output of the row does not reveal the verifier.
    assert!(format!("{row:?}").contains("Verifier([REDACTED])"));
}

#[tokio::test]
async fn issuance_returns_the_secret_exactly_once_and_reads_are_masked() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["principals.read"]).await;
    let (credential, token) = h.issue(c.sa("PRODUCTION"), &["principals.read"]).await;
    let public_id = credential["publicId"].as_str().unwrap();
    assert!(token.starts_with(&format!("cfmk_live_{public_id}_")));
    let secret = secret_part(&token).to_string();
    let issued_view = credential.to_string();
    assert!(!issued_view.contains(&secret));
    assert!(!issued_view.contains("\"verifier\""));

    // The administration read and every service-level read path are masked.
    let id = uuid(&credential["id"]);
    let over_http = h
        .admin_ok(
            Method::GET,
            &format!("/internal/v1/admin/credentials/{id}"),
            None,
        )
        .await;
    let shown = h
        .state
        .platform
        .credential(CredentialId::from_uuid(id))
        .await
        .unwrap();
    let by_public = h
        .state
        .platform
        .credential_by_public_id(public_id)
        .await
        .unwrap();
    let listed = h
        .state
        .platform
        .list_credentials(CredentialFilter::Application(ApplicationId::from_uuid(
            c.application_id,
        )))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let mut views = vec![over_http.to_string()];
    for view in [&shown, &by_public, &listed[0]] {
        assert!(view.masked.starts_with(&format!("cfmk_live_{public_id}_")));
        assert!(view.masked.ends_with(&secret[60..]));
        assert!(view.masked.contains("****"));
        assert_eq!(view.scopes, vec!["principals.read".to_string()]);
        views.push(serde_json::to_string(view).unwrap());
    }
    for json in views {
        assert!(!json.contains(&secret), "view leaks the secret: {json}");
        assert!(!json.contains("\"verifier\""));
    }
}

#[tokio::test]
async fn database_error_summaries_never_echo_row_content() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &[]).await;
    let (credential, _) = h.issue(c.sa("PRODUCTION"), &[]).await;
    let row = api_credentials::Entity::find_by_id(uuid(&credential["id"]))
        .one(h.conn())
        .await
        .unwrap()
        .unwrap();
    // Violate the verifier-length CHECK with a recognisable value: PostgreSQL's error detail
    // would echo the failing row; the summary must not.
    let mut bad: api_credentials::ActiveModel = row.into();
    bad.id = Set(uuid::Uuid::new_v4());
    bad.public_id = Set("0123456789abcdef01234567".to_string());
    bad.verifier = Set(api_credentials::Verifier(vec![0xAB; 31]));
    bad.rotated_from_credential_id = Set(None);
    let err = bad.insert(h.conn()).await.unwrap_err();
    let summary = summarize_db_error(&err);
    assert!(
        summary.contains("ck_platform_api_credentials_verifier_length"),
        "{summary}"
    );
    assert!(!summary.to_lowercase().contains("abab"), "{summary}");
    assert!(!PlatformError::from(err)
        .to_string()
        .to_lowercase()
        .contains("abab"));
}

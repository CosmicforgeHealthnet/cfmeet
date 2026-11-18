//! Allowed websites of an environment (ADR 0062 §4; contract
//! `docs/platform/build-internal-contracts.md` §2.3).
//!
//! An environment holds at most 20 exact origins `scheme://host[:port]`: lowercase host,
//! no path / query / fragment / user info / wildcard, no default port (browsers omit it
//! from `Origin`), `https` only — except `http://localhost[:port]` and
//! `http://127.0.0.1[:port]`, which only SANDBOX and TEST environments may use (a
//! PRODUCTION environment never lists a loopback host).
//!
//! Every change appends a row to `platform_environment_origin_changes` (also written by
//! trigger when the effective status of an environment with origins changes); consumers
//! follow that feed (`seq`) and always receive the LATEST state of the environment.

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, DbErr, FromQueryResult, Statement, TransactionTrait};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    environments::EnvironmentView,
    operator::{self, AuditEntry, AuditTargetKind, OperatorAction, OperatorContext},
    PlatformError, PlatformResult, PlatformService,
};
use crate::domain::EnvironmentKind;

pub const MAX_ORIGINS: usize = 20;
/// `https://` + a 253-character host + `:65535`.
pub const MAX_ORIGIN_LEN: usize = 270;
pub const DEFAULT_FEED_PAGE: u64 = 100;
pub const MAX_FEED_PAGE: u64 = 500;

fn loopback(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1"
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Validates one origin for an environment of `kind` (the message is safe to return).
pub fn validate_origin(value: &str, kind: EnvironmentKind) -> Result<String, String> {
    let shown: String = value.chars().take(64).collect();
    let fail = |why: &str| Err(format!("origin {shown:?} {why}"));
    if value.is_empty() || value.len() > MAX_ORIGIN_LEN || !value.is_ascii() {
        return fail("must be 1-270 ASCII characters");
    }
    let (scheme, rest) = if let Some(rest) = value.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = value.strip_prefix("http://") {
        ("http", rest)
    } else {
        return fail("must start with https:// (lowercase)");
    };
    if rest.contains('*') {
        return fail("must not contain a wildcard");
    }
    if rest.contains(['/', '?', '#', '@', '\\', '%', ' ']) || rest.is_empty() {
        return fail(
            "must be exactly scheme://host[:port] (no path, query, fragment or user info)",
        );
    }
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    if host.bytes().any(|b| b.is_ascii_uppercase()) {
        return fail("must have a lowercase host");
    }
    if !valid_host(host) {
        return fail("must have a valid DNS host name");
    }
    if let Some(port) = port {
        let parsed = port
            .parse::<u32>()
            .ok()
            .filter(|p| (1..=65_535).contains(p) && !port.starts_with('0'));
        let Some(p) = parsed else {
            return fail("has an invalid port");
        };
        if (scheme == "https" && p == 443) || (scheme == "http" && p == 80) {
            return fail("must omit the default port");
        }
    }
    if loopback(host) && kind == EnvironmentKind::Production {
        return fail("is a loopback host, which only SANDBOX and TEST environments may use");
    }
    if scheme == "http" && !loopback(host) {
        return fail("must use https (http is allowed only for localhost and 127.0.0.1)");
    }
    Ok(value.to_string())
}

/// Validates a whole list: at most [`MAX_ORIGINS`] distinct origins (duplicates are
/// dropped; order kept).
pub fn validate_origins(values: &[String], kind: EnvironmentKind) -> PlatformResult<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(values.len().min(MAX_ORIGINS));
    for value in values {
        let origin = validate_origin(value, kind).map_err(PlatformError::Validation)?;
        if !out.contains(&origin) {
            out.push(origin);
        }
        if out.len() > MAX_ORIGINS {
            return Err(PlatformError::Validation(format!(
                "an environment allows at most {MAX_ORIGINS} origins"
            )));
        }
    }
    Ok(out)
}

/// The origins of an environment (empty when none were set).
pub async fn origins_of<C: ConnectionTrait>(
    db: &C,
    environment_id: Uuid,
) -> Result<Vec<String>, DbErr> {
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT origins FROM platform_environment_origins WHERE environment_id = $1",
            [environment_id.into()],
        ))
        .await?
        .map(|r| r.try_get::<Vec<String>>("", "origins"))
        .transpose()?
        .unwrap_or_default())
}

/// One change-feed item: the environment's latest state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct OriginFeedItem {
    pub environment_id: Uuid,
    pub origins: Vec<String>,
    /// `ACTIVE` when the environment, its application and its tenant are ACTIVE,
    /// otherwise `SUSPENDED`.
    pub environment_status: String,
    pub seq: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginFeedPage {
    pub items: Vec<OriginFeedItem>,
    /// The cursor to pass as `after` next (the last returned `seq`, or `after`).
    pub next_after: i64,
}

/// At most `limit` feed items with `seq > after`.
pub async fn feed<C: ConnectionTrait>(
    db: &C,
    after: i64,
    limit: Option<u64>,
) -> PlatformResult<OriginFeedPage> {
    let limit = limit.unwrap_or(DEFAULT_FEED_PAGE);
    if limit == 0 || limit > MAX_FEED_PAGE {
        return Err(PlatformError::Validation(format!(
            "limit must be between 1 and {MAX_FEED_PAGE}"
        )));
    }
    if after < 0 {
        return Err(PlatformError::Validation(
            "after must not be negative".to_string(),
        ));
    }
    let items = OriginFeedItem::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT c.environment_id, COALESCE(o.origins, '{}'::text[]) AS origins, \
                CASE WHEN e.status = 'ACTIVE' AND a.status = 'ACTIVE' AND t.status = 'ACTIVE' \
                     THEN 'ACTIVE' ELSE 'SUSPENDED' END AS environment_status, \
                c.seq \
         FROM platform_environment_origin_changes c \
         JOIN platform_environments e ON e.id = c.environment_id \
         JOIN platform_applications a ON a.id = e.application_id \
         JOIN platform_tenants t ON t.id = e.tenant_id \
         LEFT JOIN platform_environment_origins o ON o.environment_id = c.environment_id \
         WHERE c.seq > $1 ORDER BY c.seq LIMIT $2",
        [after.into(), (limit as i64).into()],
    ))
    .all(db)
    .await?;
    let next_after = items.last().map(|i| i.seq).unwrap_or(after);
    Ok(OriginFeedPage { items, next_after })
}

impl PlatformService {
    /// Replaces the origins of `environment` (validated for its kind). An unchanged list
    /// writes nothing; a change writes the row, one feed row and one audit row.
    pub async fn set_environment_origins(
        &self,
        op: &OperatorContext,
        environment: &EnvironmentView,
        origins: &[String],
    ) -> PlatformResult<Vec<String>> {
        let origins = validate_origins(origins, environment.kind)?;
        let now: DateTime<Utc> = self.clock.now();
        let txn = self.db.begin().await?;
        let id = environment.id.as_uuid();
        // Serializes writers of this environment's row (and the feed's commit order).
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 FROM platform_environments WHERE id = $1 FOR UPDATE",
            [id.into()],
        ))
        .await?;
        let current = origins_of(&txn, id).await?;
        let existed = txn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM platform_environment_origins WHERE environment_id = $1",
                [id.into()],
            ))
            .await?
            .is_some();
        if existed && current == origins {
            txn.commit().await?;
            return Ok(origins);
        }
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO platform_environment_origins (environment_id, origins, created_at, updated_at) \
             VALUES ($1, $2, $3, $3) \
             ON CONFLICT (environment_id) DO UPDATE \
               SET origins = EXCLUDED.origins, updated_at = EXCLUDED.updated_at",
            [id.into(), origins.clone().into(), now.into()],
        ))
        .await?;
        txn.execute(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(7239062)",
        ))
        .await?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO platform_environment_origin_changes (environment_id) VALUES ($1)",
            [id.into()],
        ))
        .await?;
        operator::record(
            &txn,
            op,
            &AuditEntry::new(
                OperatorAction::EnvironmentOriginsSet,
                AuditTargetKind::Environment,
                id,
            )
            .scope(
                Some(environment.tenant_id.as_uuid()),
                Some(environment.application_id.as_uuid()),
                Some(id),
            )
            .detail(json!({ "count": origins.len() })),
            now,
        )
        .await?;
        txn.commit().await?;
        crate::metrics::inc("platform_access_environment_origins_set_total", &[]);
        Ok(origins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_exact_https_hosts() {
        for ok in [
            "https://app.example.com",
            "https://app.example.com:8443",
            "https://xn--bcher-kva.example",
            "https://10.1.2.3",
        ] {
            for kind in EnvironmentKind::ALL {
                assert_eq!(validate_origin(ok, *kind).unwrap(), ok, "{ok}");
            }
        }
        for bad in [
            "",
            "app.example.com",
            "HTTPS://app.example.com",
            "https://App.example.com",
            "https://app.example.com/",
            "https://app.example.com/path",
            "https://app.example.com?x=1",
            "https://app.example.com#f",
            "https://user@app.example.com",
            "https://*.example.com",
            "*",
            "https://app.example.com:443",
            "https://app.example.com:0",
            "https://app.example.com:65536",
            "https://app.example.com:080",
            "https://app..example.com",
            "https://-app.example.com",
            "http://app.example.com",
            "ftp://app.example.com",
            "https://[::1]",
        ] {
            assert!(
                validate_origin(bad, EnvironmentKind::Sandbox).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn loopback_is_for_sandbox_and_test_only() {
        for ok in [
            "http://localhost",
            "http://localhost:3000",
            "http://127.0.0.1:5173",
            "https://localhost:8443",
        ] {
            assert!(
                validate_origin(ok, EnvironmentKind::Sandbox).is_ok(),
                "{ok}"
            );
            assert!(validate_origin(ok, EnvironmentKind::Test).is_ok(), "{ok}");
            assert!(
                validate_origin(ok, EnvironmentKind::Production).is_err(),
                "{ok}"
            );
        }
        assert!(validate_origin("http://localhost:80", EnvironmentKind::Sandbox).is_err());
    }

    #[test]
    fn lists_are_bounded_and_deduplicated() {
        let many: Vec<String> = (0..21)
            .map(|i| format!("https://a{i}.example.com"))
            .collect();
        assert!(validate_origins(&many, EnvironmentKind::Sandbox).is_err());
        assert_eq!(
            validate_origins(&many[..20], EnvironmentKind::Sandbox)
                .unwrap()
                .len(),
            20
        );
        let dup = vec![
            "https://a.example.com".to_string(),
            "https://a.example.com".to_string(),
        ];
        assert_eq!(
            validate_origins(&dup, EnvironmentKind::Sandbox).unwrap(),
            vec!["https://a.example.com".to_string()]
        );
    }
}

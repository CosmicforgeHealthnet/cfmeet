//! One PostgreSQL database per service, created on the admin connection and dropped at
//! teardown.

use std::path::Path;

use anyhow::{bail, Context};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

/// `postgres://u:p@h:5432/postgres` + `name` -> `postgres://u:p@h:5432/name`.
pub fn with_db(admin: &str, name: &str) -> String {
    let (base, query) = match admin.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (admin, None),
    };
    let root = base.rsplit_once('/').map(|(r, _)| r).unwrap_or(base);
    match query {
        Some(q) => format!("{root}/{name}?{q}"),
        None => format!("{root}/{name}"),
    }
}

pub async fn create(admin: &str, name: &str) -> anyhow::Result<String> {
    let conn = Database::connect(admin)
        .await
        .context("connecting to TEST_DATABASE_URL")?;
    conn.execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .with_context(|| format!("CREATE DATABASE {name}"))?;
    conn.close().await.ok();
    Ok(with_db(admin, name))
}

pub async fn drop_all(admin: &str, names: &[String]) {
    if names.is_empty() {
        return;
    }
    if let Ok(conn) = Database::connect(admin).await {
        for n in names {
            let _ = conn
                .execute_unprepared(&format!("DROP DATABASE IF EXISTS {n} WITH (FORCE)"))
                .await;
        }
        conn.close().await.ok();
    }
}

/// Runs `<migration> up` against `url` (each service's OWN migration binary).
pub fn migrate(bin: &Path, url: &str) -> anyhow::Result<()> {
    let out = std::process::Command::new(bin)
        .arg("up")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("DATABASE_URL", url)
        .output()
        .with_context(|| format!("running {}", bin.display()))?;
    if !out.status.success() {
        bail!(
            "{} up failed ({}):\n{}\n{}",
            bin.display(),
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

/// A read-only probe connection (usage counters and similar evidence).
pub async fn connect(url: &str) -> anyhow::Result<DatabaseConnection> {
    Ok(Database::connect(url).await?)
}

pub async fn scalar_i64(db: &DatabaseConnection, sql: &str) -> anyhow::Result<i64> {
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_string(),
        ))
        .await?
        .context("no row")?;
    Ok(row.try_get_by_index::<i64>(0)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn database_urls_swap_only_the_name() {
        assert_eq!(
            super::with_db("postgres://u:p@h:5432/postgres", "x"),
            "postgres://u:p@h:5432/x"
        );
        assert_eq!(
            super::with_db("postgres://u:p@h/postgres?sslmode=disable", "y"),
            "postgres://u:p@h/y?sslmode=disable"
        );
    }
}

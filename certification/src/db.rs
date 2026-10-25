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

/// Creates the least-privilege RUNTIME role of `database`, granted exactly as
/// `infra/deployment/postgres/provision-service-database.sh` grants it in production
/// (CONNECT, USAGE on `public`, DML on its tables and sequences, EXECUTE on its functions;
/// no DDL, no TEMP), and returns `(role, url)`. Run after the migrations (which apply as
/// the owner). Every service the stack starts with it proves it runs under its own
/// production privileges, not as a superuser.
pub async fn runtime_role(admin: &str, database: &str) -> anyhow::Result<(String, String)> {
    let role = format!("{database}_rt");
    let password = uuid::Uuid::new_v4().simple().to_string();
    let conn = Database::connect(with_db(admin, database))
        .await
        .with_context(|| format!("connecting to {database}"))?;
    for sql in [
        format!("CREATE ROLE {role} LOGIN NOINHERIT PASSWORD '{password}'"),
        format!("REVOKE ALL ON DATABASE {database} FROM PUBLIC"),
        format!("GRANT CONNECT ON DATABASE {database} TO {role}"),
        "REVOKE CREATE ON SCHEMA public FROM PUBLIC".to_string(),
        format!("GRANT USAGE ON SCHEMA public TO {role}"),
        format!("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {role}"),
        format!("GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO {role}"),
        format!("GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO {role}"),
    ] {
        conn.execute_unprepared(&sql)
            .await
            .with_context(|| format!("runtime role of {database}"))?;
    }
    conn.close().await.ok();
    let (scheme, rest) = admin
        .split_once("://")
        .context("TEST_DATABASE_URL must be a URL")?;
    let host = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
    let url = with_db(&format!("{scheme}://{role}:{password}@{host}"), database);
    Ok((role, url))
}

/// Drops the runtime roles of [`runtime_role`] (after their databases are gone).
pub async fn drop_roles(admin: &str, roles: &[String]) {
    if roles.is_empty() {
        return;
    }
    if let Ok(conn) = Database::connect(admin).await {
        for r in roles {
            let _ = conn
                .execute_unprepared(&format!("DROP ROLE IF EXISTS {r}"))
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

/// Runs `<migration> <args…>` against `url` and returns its output (stdout, then stderr) (the
/// migration CLI of every service: `status`, `up [-n N]`, `down [-n N]`). `Err` carries
/// both output streams when the command fails (a guarded down refuses this way).
pub fn migration(bin: &Path, url: &str, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new(bin)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("DATABASE_URL", url)
        .output()
        .with_context(|| format!("running {}", bin.display()))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        bail!(
            "{} {} failed ({}):\n{text}",
            bin.display(),
            args.join(" "),
            out.status
        );
    }
    Ok(text)
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

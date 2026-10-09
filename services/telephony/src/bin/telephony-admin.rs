//! `telephony-admin`: operator CLI for interconnect configuration (Part E §42, §51, §161).
//!
//! Providers, routes, SIP peer configuration REFERENCES and public relay-number bindings
//! are rows in Telephony's own database; credentials are never accepted here (a provider
//! names its secret file with `--credential-ref`; a SIP peer names its deployment secret
//! with `--secret-ref`). Disabling a provider or a route — or forcing its circuit open —
//! stops new paid legs on it only: Calling, Privacy Relay and application RTC are
//! unaffected.

use std::process::ExitCode;

use chrono::Utc;
use clap::{Parser, Subcommand};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "telephony-admin",
    about = "CF Meet Telephony interconnect administration"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(subcommand)]
    Provider(ProviderCmd),
    #[command(subcommand)]
    Route(RouteCmd),
    #[command(subcommand)]
    SipPeer(SipPeerCmd),
    #[command(subcommand)]
    Number(NumberCmd),
    /// Part F: enterprise SIP devices on the own edge (internal; never paid).
    #[command(subcommand)]
    EnterpriseDevice(EnterpriseDeviceCmd),
    /// Open incidents (restricted operational view; never numbers).
    Incidents,
}

#[derive(Subcommand)]
enum ProviderCmd {
    /// Registers an interconnect (adapter SIP_TRUNK; DETERMINISTIC_TEST only for local/test).
    Add {
        #[arg(long)]
        id: String,
        #[arg(long)]
        adapter: String,
        /// Comma-separated advertised capabilities.
        #[arg(long)]
        capabilities: String,
        #[arg(long, default_value_t = 100)]
        max_concurrent_legs: i32,
        /// Name of the credential FILE in TELECOM_PROVIDER_SECRETS_DIR (never the secret).
        #[arg(long)]
        credential_ref: Option<String>,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    /// Forces the circuit OPEN (stops new legs) or CLOSED.
    Circuit {
        id: String,
        state: String,
    },
    List,
}

#[derive(Subcommand)]
enum RouteCmd {
    Add {
        #[arg(long)]
        id: String,
        #[arg(long)]
        provider: String,
        /// Comma-separated ISO countries.
        #[arg(long)]
        countries: String,
        /// Comma-separated: VOICE_OUTBOUND, VOICE_INBOUND.
        #[arg(long)]
        capabilities: String,
        #[arg(long, default_value_t = 100)]
        priority: i32,
        #[arg(long)]
        sip_peer: Option<String>,
        #[arg(long, default_value_t = 50)]
        max_concurrent_legs: i32,
        #[arg(long)]
        estimated_leg_cost_minor: Option<i64>,
        #[arg(long)]
        cost_currency: Option<String>,
        #[arg(long, default_value_t = 3600)]
        max_call_seconds: i32,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Circuit {
        id: String,
        state: String,
    },
    List,
}

#[derive(Subcommand)]
enum SipPeerCmd {
    Add {
        #[arg(long)]
        id: String,
        #[arg(long)]
        provider: String,
        /// Name of the deployment secret holding the trunk credentials (never the secret).
        #[arg(long)]
        secret_ref: String,
        #[arg(long, default_value = "TLS")]
        transport: String,
        /// Comma-separated CIDRs the peer may send from.
        #[arg(long)]
        allowed_cidrs: String,
        #[arg(long, default_value_t = 100)]
        max_concurrent_dialogs: i32,
    },
    List,
}

#[derive(Subcommand)]
enum NumberCmd {
    /// Binds a platform-owned PUBLIC relay number to the route that serves it.
    Bind {
        #[arg(long)]
        number: String,
        #[arg(long)]
        route: String,
        #[arg(long, default_value = "VOICE_OUTBOUND,VOICE_INBOUND")]
        capabilities: String,
    },
    Disable {
        number: String,
        #[arg(long)]
        route: String,
    },
    List,
}

#[derive(Subcommand)]
enum EnterpriseDeviceCmd {
    /// Registers a device: the opaque reference Calling binds to a principal, the edge
    /// endpoint it authenticates as (its credentials stay in the edge's secret store) and
    /// the scope whose Calling receives its dialogs.
    Add {
        #[arg(long)]
        device_ref: String,
        #[arg(long)]
        edge_endpoint: String,
        #[arg(long)]
        tenant: uuid::Uuid,
        #[arg(long)]
        application: uuid::Uuid,
        #[arg(long)]
        environment: uuid::Uuid,
    },
    Enable {
        device_ref: String,
    },
    Disable {
        device_ref: String,
    },
    List,
}

fn list(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Public relay numbers are shown masked in operator output too.
fn masked(number: &str) -> String {
    let n = number.len();
    if n <= 6 {
        return "•".repeat(n);
    }
    format!("{}{}{}", &number[..4], "•".repeat(n - 6), &number[n - 2..])
}

async fn exec(
    db: &DatabaseConnection,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<u64, String> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
    .await
    .map(|r| r.rows_affected())
    .map_err(|e| telephony::error::summarize_db_error(&e).0)
}

async fn rows(db: &DatabaseConnection, sql: &str) -> Result<Vec<serde_json::Value>, String> {
    let rows = db
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT to_jsonb(t) FROM ({sql}) t"),
        ))
        .await
        .map_err(|e| telephony::error::summarize_db_error(&e).0)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.try_get_by_index::<serde_json::Value>(0).ok())
        .collect())
}

async fn run(db: &DatabaseConnection, command: Command) -> Result<serde_json::Value, String> {
    let now = Utc::now();
    match command {
        Command::Provider(cmd) => match cmd {
            ProviderCmd::Add { id, adapter, capabilities, max_concurrent_legs, credential_ref } => {
                exec(db, "INSERT INTO telecom_providers (id, adapter, capabilities, status, credential_ref, max_concurrent_legs, created_at, updated_at) VALUES ($1, $2, $3, 'DISABLED', $4, $5, $6, $6)",
                    vec![id.clone().into(), adapter.into(), list(&capabilities).into(), credential_ref.into(), max_concurrent_legs.into(), now.into()]).await?;
                Ok(json!({ "provider": id, "status": "DISABLED" }))
            }
            ProviderCmd::Enable { id } => {
                exec(db, "UPDATE telecom_providers SET status = 'ENABLED', updated_at = $2, version = version + 1 WHERE id = $1", vec![id.clone().into(), now.into()]).await?;
                Ok(json!({ "provider": id, "status": "ENABLED" }))
            }
            ProviderCmd::Disable { id } => {
                exec(db, "UPDATE telecom_providers SET status = 'DISABLED', updated_at = $2, version = version + 1 WHERE id = $1", vec![id.clone().into(), now.into()]).await?;
                Ok(json!({ "provider": id, "status": "DISABLED" }))
            }
            ProviderCmd::Circuit { id, state } => {
                exec(db, "UPDATE telecom_providers SET circuit_state = $2, circuit_opened_at = CASE WHEN $2 = 'CLOSED' THEN NULL ELSE $3 END, consecutive_failures = 0, updated_at = $3, version = version + 1 WHERE id = $1",
                    vec![id.clone().into(), state.clone().into(), now.into()]).await?;
                Ok(json!({ "provider": id, "circuit": state }))
            }
            ProviderCmd::List => Ok(json!(rows(db, "SELECT id, adapter, capabilities, status, circuit_state, health, max_concurrent_legs FROM telecom_providers ORDER BY id").await?)),
        },
        Command::Route(cmd) => match cmd {
            RouteCmd::Add { id, provider, countries, capabilities, priority, sip_peer, max_concurrent_legs, estimated_leg_cost_minor, cost_currency, max_call_seconds } => {
                exec(db, "INSERT INTO telecom_routes (id, provider_id, countries, capabilities, priority, status, sip_peer_config_ref_id, max_concurrent_legs, estimated_leg_cost_minor, cost_currency, max_call_seconds, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 'DISABLED', $6, $7, $8, $9, $10, $11, $11)",
                    vec![id.clone().into(), provider.into(), list(&countries).into(), list(&capabilities).into(), priority.into(), sip_peer.into(), max_concurrent_legs.into(), estimated_leg_cost_minor.into(), cost_currency.into(), max_call_seconds.into(), now.into()]).await?;
                Ok(json!({ "route": id, "status": "DISABLED" }))
            }
            RouteCmd::Enable { id } => {
                exec(db, "UPDATE telecom_routes SET status = 'ENABLED', updated_at = $2, version = version + 1 WHERE id = $1", vec![id.clone().into(), now.into()]).await?;
                Ok(json!({ "route": id, "status": "ENABLED" }))
            }
            RouteCmd::Disable { id } => {
                exec(db, "UPDATE telecom_routes SET status = 'DISABLED', updated_at = $2, version = version + 1 WHERE id = $1", vec![id.clone().into(), now.into()]).await?;
                Ok(json!({ "route": id, "status": "DISABLED" }))
            }
            RouteCmd::Circuit { id, state } => {
                exec(db, "UPDATE telecom_routes SET circuit_state = $2, circuit_opened_at = CASE WHEN $2 = 'CLOSED' THEN NULL ELSE $3 END, consecutive_failures = 0, updated_at = $3, version = version + 1 WHERE id = $1",
                    vec![id.clone().into(), state.clone().into(), now.into()]).await?;
                Ok(json!({ "route": id, "circuit": state }))
            }
            RouteCmd::List => Ok(json!(rows(db, "SELECT id, provider_id, countries, capabilities, priority, status, circuit_state, health, max_concurrent_legs, max_call_seconds FROM telecom_routes ORDER BY priority, id").await?)),
        },
        Command::SipPeer(cmd) => match cmd {
            SipPeerCmd::Add { id, provider, secret_ref, transport, allowed_cidrs, max_concurrent_dialogs } => {
                let cidrs = list(&allowed_cidrs);
                exec(db, "INSERT INTO sip_peer_config_refs (id, provider_id, secret_ref, transport, allowed_source_cidrs, max_concurrent_dialogs, created_at, updated_at) VALUES ($1, $2, $3, $4, $5::cidr[], $6, $7, $7)",
                    vec![id.clone().into(), provider.into(), secret_ref.into(), transport.into(), cidrs.into(), max_concurrent_dialogs.into(), now.into()]).await?;
                Ok(json!({ "sipPeer": id }))
            }
            SipPeerCmd::List => Ok(json!(rows(db, "SELECT id, provider_id, secret_ref, transport, allowed_source_cidrs::text[] AS allowed_source_cidrs, max_concurrent_dialogs FROM sip_peer_config_refs ORDER BY id").await?)),
        },
        Command::Number(cmd) => match cmd {
            NumberCmd::Bind { number, route, capabilities } => {
                exec(db, "INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, created_at, updated_at) VALUES ($1, $2, $3, 'ENABLED', $4, $4)",
                    vec![number.clone().into(), route.into(), list(&capabilities).into(), now.into()]).await?;
                Ok(json!({ "number": masked(&number), "status": "ENABLED" }))
            }
            NumberCmd::Disable { number, route } => {
                exec(db, "UPDATE telecom_number_bindings SET status = 'DISABLED', updated_at = $2 WHERE public_e164 = $1 AND route_id = $3", vec![number.clone().into(), now.into(), route.into()]).await?;
                Ok(json!({ "number": masked(&number), "status": "DISABLED" }))
            }
            NumberCmd::List => {
                let list = rows(db, "SELECT public_e164, route_id, capabilities, status FROM telecom_number_bindings ORDER BY route_id, public_e164").await?;
                Ok(json!(list.into_iter().map(|mut r| {
                    let n = r["public_e164"].as_str().unwrap_or("").to_string();
                    r["public_e164"] = json!(masked(&n));
                    r
                }).collect::<Vec<_>>()))
            }
        },
        Command::EnterpriseDevice(cmd) => match cmd {
            EnterpriseDeviceCmd::Add { device_ref, edge_endpoint, tenant, application, environment } => {
                exec(db, "INSERT INTO telephony_enterprise_devices (id, device_ref, edge_endpoint, tenant_id, application_id, environment_id, status, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, 'DISABLED', $7, $7)",
                    vec![uuid::Uuid::new_v4().into(), device_ref.clone().into(), edge_endpoint.into(), tenant.into(), application.into(), environment.into(), now.into()]).await?;
                Ok(json!({ "deviceRef": device_ref, "status": "DISABLED" }))
            }
            EnterpriseDeviceCmd::Enable { device_ref } => {
                exec(db, "UPDATE telephony_enterprise_devices SET status = 'ENABLED', updated_at = $2 WHERE device_ref = $1", vec![device_ref.clone().into(), now.into()]).await?;
                Ok(json!({ "deviceRef": device_ref, "status": "ENABLED" }))
            }
            EnterpriseDeviceCmd::Disable { device_ref } => {
                exec(db, "UPDATE telephony_enterprise_devices SET status = 'DISABLED', updated_at = $2 WHERE device_ref = $1", vec![device_ref.clone().into(), now.into()]).await?;
                Ok(json!({ "deviceRef": device_ref, "status": "DISABLED" }))
            }
            EnterpriseDeviceCmd::List => Ok(json!(rows(db, "SELECT device_ref, edge_endpoint, tenant_id, application_id, environment_id, status FROM telephony_enterprise_devices ORDER BY device_ref").await?)),
        },
        Command::Incidents => Ok(json!(rows(db, "SELECT id, severity, category, status, provider_id, route_id, telephony_leg_id, occurrences, escalation_count, first_seen_at, last_seen_at FROM telephony_incidents WHERE status = 'OPEN' ORDER BY last_seen_at DESC LIMIT 200").await?)),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    dotenvy::dotenv().ok();
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("telephony-admin: DATABASE_URL is required");
        return ExitCode::from(2);
    };
    let mut options = ConnectOptions::new(url);
    options.max_connections(2).sqlx_logging(false);
    let db = match Database::connect(options).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!(
                "telephony-admin: cannot connect ({})",
                telephony::error::summarize_db_error(&e).0
            );
            return ExitCode::from(2);
        }
    };
    match run(&db, cli.command).await {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("telephony-admin: {e}");
            ExitCode::FAILURE
        }
    }
}

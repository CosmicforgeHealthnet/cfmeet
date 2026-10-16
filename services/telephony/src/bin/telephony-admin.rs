//! `telephony-admin`: operator CLI for interconnect configuration (Part E §42, §51, §161).
//!
//! Providers, routes, SIP peer configuration REFERENCES and public relay-number bindings
//! are rows in Telephony's own database; credentials are never accepted here (a provider
//! names its secret file with `--credential-ref`; a SIP peer names its deployment secret
//! with `--secret-ref`). Disabling a provider or a route — or forcing its circuit open —
//! stops new paid legs on it only: Calling, Privacy Relay and application RTC are
//! unaffected.
//!
//! Part H: every mutating command names its operator (`--operator` or `CF_OPERATOR_ID`)
//! and writes a `telephony_operator_audit` row in the SAME transaction as its effect.
//! `destination-classes` manages the pre-toll class rules, `retention` the retention
//! holds, `peers verify` compares the declared peer networks with the edge's mounted
//! trunk `identify` sections. `incidents resolve <id>` closes an OPEN incident (audited
//! in the same transaction).
//!
//! Operator `disable` is the LASTING stop of a route / provider (it stays off until
//! `enable`); `circuit --state OPEN` is the circuit breaker's 60 s probe hold — after it
//! one probe origination is allowed and a success closes the circuit again.

use std::process::ExitCode;

use chrono::Utc;
use clap::{Parser, Subcommand};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::{json, Value};
use telephony::{
    operator_audit::{self, OperatorAction},
    retention::{self, DataClass, HoldReason},
};

#[derive(Parser)]
#[command(
    name = "telephony-admin",
    about = "CF Meet Telephony interconnect administration"
)]
struct Cli {
    /// Operator identity recorded in the audit (required for every mutating command).
    #[arg(long, global = true, env = "CF_OPERATOR_ID")]
    operator: Option<String>,
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
    /// Incidents (restricted operational view; never numbers). Without a subcommand:
    /// the open incidents.
    Incidents {
        #[command(subcommand)]
        command: Option<IncidentsCmd>,
    },
    /// Part H: destination class rules (pre-toll refusal of premium / shared-cost /
    /// satellite / high-risk ranges).
    #[command(subcommand)]
    DestinationClasses(DestinationClassCmd),
    /// Part H: retention policies and holds.
    #[command(subcommand)]
    Retention(RetentionCmd),
    /// Part H: edge peer configuration consistency.
    #[command(subcommand)]
    Peers(PeersCmd),
}

#[derive(Subcommand)]
enum IncidentsCmd {
    /// The open incidents (default).
    List,
    /// Marks an OPEN incident RESOLVED (its condition was handled); audited. A repeat of
    /// the condition opens a new incident.
    Resolve {
        id: uuid::Uuid,
        /// Why it is resolved (a short operational note; never a number or secret).
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Subcommand)]
enum DestinationClassCmd {
    /// Adds or updates one rule: the national-significant prefix of a number RANGE
    /// (never a number) and its class.
    Upsert {
        #[arg(long)]
        country: String,
        #[arg(long)]
        prefix: String,
        /// STANDARD, PREMIUM, SHARED_COST, SATELLITE or HIGH_RISK.
        #[arg(long)]
        class: String,
        /// Where the rule comes from (a dataset name / version).
        #[arg(long)]
        source: String,
    },
    Disable {
        #[arg(long)]
        country: String,
        #[arg(long)]
        prefix: String,
    },
    List {
        #[arg(long)]
        country: Option<String>,
    },
}

#[derive(Subcommand)]
enum RetentionCmd {
    /// The effective retention policies (from the environment).
    Policies,
    /// Places a retention hold on a leg, a provider or a whole data class.
    Hold {
        /// PROVIDER_EVENTS, TELEPHONY_INCIDENTS or NORMALIZED_LEG_EVIDENCE.
        #[arg(long)]
        data_class: String,
        #[arg(long)]
        leg: Option<uuid::Uuid>,
        #[arg(long)]
        provider: Option<String>,
        /// INVESTIGATION, DISPUTE, REGULATORY_REQUEST, INCIDENT_REVIEW or OTHER.
        #[arg(long)]
        reason: String,
    },
    Release {
        id: uuid::Uuid,
    },
    List,
}

#[derive(Subcommand)]
enum PeersCmd {
    /// Compares `sip_peer_config_refs.allowed_source_cidrs` with the `identify`
    /// sections of the mounted trunk files (the edge's enforcement point).
    Verify {
        #[arg(long, default_value = "/run/secrets/edge/trunks")]
        trunks_dir: String,
    },
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

async fn exec<C: ConnectionTrait>(
    db: &C,
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

async fn rows_in<C: ConnectionTrait>(
    db: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT to_jsonb(t) FROM ({sql}) t"),
            values,
        ))
        .await
        .map_err(|e| telephony::error::summarize_db_error(&e).0)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.try_get_by_index::<serde_json::Value>(0).ok())
        .collect())
}

/// Applies one mutation and its audit row in ONE transaction.
async fn mutate(
    db: &DatabaseConnection,
    action: OperatorAction,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<u64, String> {
    let txn = db
        .begin()
        .await
        .map_err(|e| telephony::error::summarize_db_error(&e).0)?;
    let n = exec(&txn, sql, values).await?;
    operator_audit::record(&txn, &action, Utc::now())
        .await
        .map_err(|e| e.to_string())?;
    txn.commit()
        .await
        .map_err(|e| telephony::error::summarize_db_error(&e).0)?;
    Ok(n)
}

/// The audited operator action of a mutating command (refused without an operator).
fn audit(
    operator: Option<&str>,
    action: &'static str,
    target_kind: &'static str,
    target_id: &str,
    detail: Value,
) -> Result<OperatorAction, String> {
    OperatorAction::new(
        operator.unwrap_or(""),
        action,
        target_kind,
        target_id,
        detail,
    )
    .map_err(|e| e.to_string())
}

async fn run(
    db: &DatabaseConnection,
    command: Command,
    operator: Option<&str>,
) -> Result<serde_json::Value, String> {
    let now = Utc::now();
    match command {
        Command::Provider(cmd) => match cmd {
            ProviderCmd::Add { id, adapter, capabilities, max_concurrent_legs, credential_ref } => {
                let a = audit(operator, "PROVIDER_ADD", "PROVIDER", &id, json!({ "adapter": adapter, "capabilities": list(&capabilities), "maxConcurrentLegs": max_concurrent_legs, "credentialRef": credential_ref }))?;
                mutate(db, a, "INSERT INTO telecom_providers (id, adapter, capabilities, status, credential_ref, max_concurrent_legs, created_at, updated_at) VALUES ($1, $2, $3, 'DISABLED', $4, $5, $6, $6)",
                    vec![id.clone().into(), adapter.into(), list(&capabilities).into(), credential_ref.into(), max_concurrent_legs.into(), now.into()]).await?;
                Ok(json!({ "provider": id, "status": "DISABLED" }))
            }
            ProviderCmd::Enable { id } => {
                let a = audit(operator, "PROVIDER_ENABLE", "PROVIDER", &id, json!({ "status": "ENABLED" }))?;
                mutate(db, a, "UPDATE telecom_providers SET status = 'ENABLED', updated_at = $2, version = version + 1 WHERE id = $1", vec![id.clone().into(), now.into()]).await?;
                Ok(json!({ "provider": id, "status": "ENABLED" }))
            }
            ProviderCmd::Disable { id } => {
                let a = audit(operator, "PROVIDER_DISABLE", "PROVIDER", &id, json!({ "status": "DISABLED" }))?;
                mutate(db, a, "UPDATE telecom_providers SET status = 'DISABLED', updated_at = $2, version = version + 1 WHERE id = $1", vec![id.clone().into(), now.into()]).await?;
                Ok(json!({ "provider": id, "status": "DISABLED" }))
            }
            ProviderCmd::Circuit { id, state } => {
                let a = audit(operator, "PROVIDER_CIRCUIT", "PROVIDER", &id, json!({ "circuit": state }))?;
                mutate(db, a, "UPDATE telecom_providers SET circuit_state = $2, circuit_opened_at = CASE WHEN $2 = 'CLOSED' THEN NULL ELSE $3 END, consecutive_failures = 0, updated_at = $3, version = version + 1 WHERE id = $1",
                    vec![id.clone().into(), state.clone().into(), now.into()]).await?;
                Ok(json!({ "provider": id, "circuit": state }))
            }
            ProviderCmd::List => Ok(json!(rows(db, "SELECT id, adapter, capabilities, status, circuit_state, health, max_concurrent_legs FROM telecom_providers ORDER BY id").await?)),
        },
        Command::Route(cmd) => match cmd {
            RouteCmd::Add { id, provider, countries, capabilities, priority, sip_peer, max_concurrent_legs, estimated_leg_cost_minor, cost_currency, max_call_seconds } => {
                let a = audit(operator, "ROUTE_ADD", "ROUTE", &id, json!({ "provider": provider, "countries": list(&countries), "capabilities": list(&capabilities), "priority": priority, "sipPeer": sip_peer, "maxConcurrentLegs": max_concurrent_legs, "maxCallSeconds": max_call_seconds }))?;
                mutate(db, a, "INSERT INTO telecom_routes (id, provider_id, countries, capabilities, priority, status, sip_peer_config_ref_id, max_concurrent_legs, estimated_leg_cost_minor, cost_currency, max_call_seconds, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 'DISABLED', $6, $7, $8, $9, $10, $11, $11)",
                    vec![id.clone().into(), provider.into(), list(&countries).into(), list(&capabilities).into(), priority.into(), sip_peer.into(), max_concurrent_legs.into(), estimated_leg_cost_minor.into(), cost_currency.into(), max_call_seconds.into(), now.into()]).await?;
                Ok(json!({ "route": id, "status": "DISABLED" }))
            }
            RouteCmd::Enable { id } => {
                let a = audit(operator, "ROUTE_ENABLE", "ROUTE", &id, json!({ "status": "ENABLED" }))?;
                mutate(db, a, "UPDATE telecom_routes SET status = 'ENABLED', updated_at = $2, version = version + 1 WHERE id = $1", vec![id.clone().into(), now.into()]).await?;
                Ok(json!({ "route": id, "status": "ENABLED" }))
            }
            RouteCmd::Disable { id } => {
                let a = audit(operator, "ROUTE_DISABLE", "ROUTE", &id, json!({ "status": "DISABLED" }))?;
                mutate(db, a, "UPDATE telecom_routes SET status = 'DISABLED', updated_at = $2, version = version + 1 WHERE id = $1", vec![id.clone().into(), now.into()]).await?;
                Ok(json!({ "route": id, "status": "DISABLED" }))
            }
            RouteCmd::Circuit { id, state } => {
                let a = audit(operator, "ROUTE_CIRCUIT", "ROUTE", &id, json!({ "circuit": state }))?;
                mutate(db, a, "UPDATE telecom_routes SET circuit_state = $2, circuit_opened_at = CASE WHEN $2 = 'CLOSED' THEN NULL ELSE $3 END, consecutive_failures = 0, updated_at = $3, version = version + 1 WHERE id = $1",
                    vec![id.clone().into(), state.clone().into(), now.into()]).await?;
                Ok(json!({ "route": id, "circuit": state }))
            }
            RouteCmd::List => Ok(json!(rows(db, "SELECT id, provider_id, countries, capabilities, priority, status, circuit_state, health, max_concurrent_legs, max_call_seconds FROM telecom_routes ORDER BY priority, id").await?)),
        },
        Command::SipPeer(cmd) => match cmd {
            SipPeerCmd::Add { id, provider, secret_ref, transport, allowed_cidrs, max_concurrent_dialogs } => {
                let cidrs = list(&allowed_cidrs);
                let a = audit(operator, "SIP_PEER_ADD", "SIP_PEER", &id, json!({ "provider": provider, "transport": transport, "allowedCidrs": cidrs, "maxConcurrentDialogs": max_concurrent_dialogs }))?;
                mutate(db, a, "INSERT INTO sip_peer_config_refs (id, provider_id, secret_ref, transport, allowed_source_cidrs, max_concurrent_dialogs, created_at, updated_at) VALUES ($1, $2, $3, $4, $5::cidr[], $6, $7, $7)",
                    vec![id.clone().into(), provider.into(), secret_ref.into(), transport.into(), cidrs.into(), max_concurrent_dialogs.into(), now.into()]).await?;
                Ok(json!({ "sipPeer": id }))
            }
            SipPeerCmd::List => Ok(json!(rows(db, "SELECT id, provider_id, secret_ref, transport, allowed_source_cidrs::text[] AS allowed_source_cidrs, max_concurrent_dialogs FROM sip_peer_config_refs ORDER BY id").await?)),
        },
        Command::Number(cmd) => match cmd {
            NumberCmd::Bind { number, route, capabilities } => {
                let a = audit(operator, "NUMBER_BIND", "NUMBER_BINDING", &route, json!({ "number": masked(&number), "route": route, "capabilities": list(&capabilities) }))?;
                mutate(db, a, "INSERT INTO telecom_number_bindings (public_e164, route_id, capabilities, status, created_at, updated_at) VALUES ($1, $2, $3, 'ENABLED', $4, $4)",
                    vec![number.clone().into(), route.into(), list(&capabilities).into(), now.into()]).await?;
                Ok(json!({ "number": masked(&number), "status": "ENABLED" }))
            }
            NumberCmd::Disable { number, route } => {
                let a = audit(operator, "NUMBER_DISABLE", "NUMBER_BINDING", &route, json!({ "number": masked(&number), "route": route }))?;
                mutate(db, a, "UPDATE telecom_number_bindings SET status = 'DISABLED', updated_at = $2 WHERE public_e164 = $1 AND route_id = $3", vec![number.clone().into(), now.into(), route.into()]).await?;
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
                let a = audit(operator, "ENTERPRISE_DEVICE_ADD", "ENTERPRISE_DEVICE", &device_ref, json!({ "edgeEndpoint": edge_endpoint, "tenant": tenant, "application": application, "environment": environment }))?;
                mutate(db, a, "INSERT INTO telephony_enterprise_devices (id, device_ref, edge_endpoint, tenant_id, application_id, environment_id, status, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, 'DISABLED', $7, $7)",
                    vec![uuid::Uuid::new_v4().into(), device_ref.clone().into(), edge_endpoint.into(), tenant.into(), application.into(), environment.into(), now.into()]).await?;
                Ok(json!({ "deviceRef": device_ref, "status": "DISABLED" }))
            }
            EnterpriseDeviceCmd::Enable { device_ref } => {
                let a = audit(operator, "ENTERPRISE_DEVICE_ENABLE", "ENTERPRISE_DEVICE", &device_ref, json!({ "status": "ENABLED" }))?;
                mutate(db, a, "UPDATE telephony_enterprise_devices SET status = 'ENABLED', updated_at = $2 WHERE device_ref = $1", vec![device_ref.clone().into(), now.into()]).await?;
                Ok(json!({ "deviceRef": device_ref, "status": "ENABLED" }))
            }
            EnterpriseDeviceCmd::Disable { device_ref } => {
                let a = audit(operator, "ENTERPRISE_DEVICE_DISABLE", "ENTERPRISE_DEVICE", &device_ref, json!({ "status": "DISABLED" }))?;
                mutate(db, a, "UPDATE telephony_enterprise_devices SET status = 'DISABLED', updated_at = $2 WHERE device_ref = $1", vec![device_ref.clone().into(), now.into()]).await?;
                Ok(json!({ "deviceRef": device_ref, "status": "DISABLED" }))
            }
            EnterpriseDeviceCmd::List => Ok(json!(rows(db, "SELECT device_ref, edge_endpoint, tenant_id, application_id, environment_id, status FROM telephony_enterprise_devices ORDER BY device_ref").await?)),
        },
        Command::Incidents { command: None | Some(IncidentsCmd::List) } => Ok(json!(rows(db, "SELECT id, severity, category, status, provider_id, route_id, telephony_leg_id, occurrences, escalation_count, first_seen_at, last_seen_at FROM telephony_incidents WHERE status = 'OPEN' ORDER BY last_seen_at DESC LIMIT 200").await?)),
        Command::Incidents { command: Some(IncidentsCmd::Resolve { id, note }) } => {
            let note = note.map(|n| n.chars().filter(|c| !c.is_control()).take(200).collect::<String>());
            if note.as_deref().is_some_and(|n| n.bytes().filter(u8::is_ascii_digit).count() >= 7) {
                return Err("the note must not carry a number".into());
            }
            let a = audit(operator, "INCIDENT_RESOLVE", "INCIDENT", &id.to_string(), json!({ "incident": id, "note": note }))?;
            let txn = db.begin().await.map_err(|e| telephony::error::summarize_db_error(&e).0)?;
            let category = rows_in(&txn, "SELECT category, status FROM telephony_incidents WHERE id = $1 FOR UPDATE", vec![id.into()]).await?;
            let Some(found) = category.into_iter().next() else {
                return Err(format!("incident {id} not found"));
            };
            if found["status"] != "OPEN" {
                return Err(format!("incident {id} is not OPEN"));
            }
            exec(&txn, "UPDATE telephony_incidents SET status = 'RESOLVED', resolved_at = GREATEST($2, last_seen_at) WHERE id = $1 AND status = 'OPEN'", vec![id.into(), now.into()]).await?;
            operator_audit::record(&txn, &a, now).await.map_err(|e| e.to_string())?;
            txn.commit().await.map_err(|e| telephony::error::summarize_db_error(&e).0)?;
            Ok(json!({ "incident": id, "category": found["category"], "status": "RESOLVED" }))
        }
        Command::DestinationClasses(cmd) => match cmd {
            DestinationClassCmd::Upsert { country, prefix, class, source } => {
                let country = country.trim().to_ascii_uppercase();
                let ok_prefix = (1..=15).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_digit());
                if country.len() != 2 || !ok_prefix || telephony::destinations::DestinationClass::parse(&class).is_none() {
                    return Err("country must be ISO alpha-2, prefix 1–15 digits, class one of STANDARD, PREMIUM, SHARED_COST, SATELLITE, HIGH_RISK".into());
                }
                let target = format!("{country}:{prefix}");
                let a = audit(operator, "DESTINATION_CLASS_UPSERT", "DESTINATION_CLASS", &target, json!({ "country": country, "prefixLength": prefix.len(), "class": class, "source": source }))?;
                mutate(db, a, "INSERT INTO telecom_destination_classes (id, country, prefix, class, source, enabled, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, TRUE, $6, $6) \
                               ON CONFLICT (country, prefix) DO UPDATE SET class = EXCLUDED.class, source = EXCLUDED.source, enabled = TRUE, updated_at = EXCLUDED.updated_at",
                    vec![uuid::Uuid::new_v4().into(), country.clone().into(), prefix.clone().into(), class.clone().into(), source.into(), now.into()]).await?;
                Ok(json!({ "country": country, "prefix": prefix, "class": class, "enabled": true }))
            }
            DestinationClassCmd::Disable { country, prefix } => {
                let country = country.trim().to_ascii_uppercase();
                let target = format!("{country}:{prefix}");
                let a = audit(operator, "DESTINATION_CLASS_DISABLE", "DESTINATION_CLASS", &target, json!({ "country": country, "prefixLength": prefix.len() }))?;
                let n = mutate(db, a, "UPDATE telecom_destination_classes SET enabled = FALSE, updated_at = $3 WHERE country = $1 AND prefix = $2",
                    vec![country.clone().into(), prefix.clone().into(), now.into()]).await?;
                Ok(json!({ "country": country, "prefix": prefix, "enabled": false, "updated": n }))
            }
            DestinationClassCmd::List { country } => {
                let filter = country.map(|c| format!(" WHERE country = '{}'", c.trim().to_ascii_uppercase().chars().filter(char::is_ascii_alphabetic).take(2).collect::<String>())).unwrap_or_default();
                Ok(json!(rows(db, &format!("SELECT country, prefix, class, source, enabled, updated_at FROM telecom_destination_classes{filter} ORDER BY country, prefix")).await?))
            }
        },
        Command::Retention(cmd) => match cmd {
            RetentionCmd::Policies => {
                let settings = telephony::service::ExecutionSettings::from_env().map_err(|e| e.to_string())?;
                Ok(json!(retention::policies(&settings).iter().map(|p| json!({
                    "dataClass": p.data_class.as_str(),
                    "action": p.action.as_str(),
                    "bound": p.label(),
                    "description": p.description,
                })).collect::<Vec<_>>()))
            }
            RetentionCmd::Hold { data_class, leg, provider, reason } => {
                let class = DataClass::parse(&data_class).filter(|c| matches!(c, DataClass::ProviderEvents | DataClass::TelephonyIncidents | DataClass::NormalizedLegEvidence))
                    .ok_or("data-class must be PROVIDER_EVENTS, TELEPHONY_INCIDENTS or NORMALIZED_LEG_EVIDENCE")?;
                let reason = HoldReason::parse(&reason).ok_or("reason must be INVESTIGATION, DISPUTE, REGULATORY_REQUEST, INCIDENT_REVIEW or OTHER")?;
                let target = leg.map(|l| l.to_string()).or_else(|| provider.clone()).unwrap_or_else(|| class.as_str().to_string());
                let a = audit(operator, "RETENTION_HOLD", "RETENTION_HOLD", &target, json!({ "dataClass": class.as_str(), "leg": leg, "provider": provider, "reason": reason.as_str() }))?;
                let txn = db.begin().await.map_err(|e| telephony::error::summarize_db_error(&e).0)?;
                let id = retention::place_hold(&txn, class, leg, provider.as_deref(), reason, &a.actor, now).await.map_err(|e| e.to_string())?;
                operator_audit::record(&txn, &a, now).await.map_err(|e| e.to_string())?;
                txn.commit().await.map_err(|e| telephony::error::summarize_db_error(&e).0)?;
                Ok(json!({ "hold": id, "dataClass": class.as_str(), "status": "ACTIVE" }))
            }
            RetentionCmd::Release { id } => {
                let a = audit(operator, "RETENTION_RELEASE", "RETENTION_HOLD", &id.to_string(), json!({ "hold": id }))?;
                let txn = db.begin().await.map_err(|e| telephony::error::summarize_db_error(&e).0)?;
                let released = retention::release_hold(&txn, id, now).await.map_err(|e| e.to_string())?;
                operator_audit::record(&txn, &a, now).await.map_err(|e| e.to_string())?;
                txn.commit().await.map_err(|e| telephony::error::summarize_db_error(&e).0)?;
                Ok(json!({ "hold": id, "status": if released { "RELEASED" } else { "ALREADY_RELEASED_OR_UNKNOWN" } }))
            }
            RetentionCmd::List => Ok(json!(retention::active_holds(db).await.map_err(|e| e.to_string())?.iter().map(|h| h.view()).collect::<Vec<_>>())),
        },
        Command::Peers(cmd) => match cmd {
            PeersCmd::Verify { trunks_dir } => {
                let declared = rows(db, "SELECT id, allowed_source_cidrs::text[] AS cidrs FROM sip_peer_config_refs ORDER BY id").await?;
                let mut database = std::collections::BTreeMap::new();
                for r in declared {
                    let id = r["id"].as_str().unwrap_or("").to_string();
                    let cidrs: Vec<String> = r["cidrs"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                    database.insert(id, cidrs);
                }
                let mut edge = std::collections::BTreeMap::new();
                let entries = std::fs::read_dir(&trunks_dir).map_err(|_| format!("cannot read the trunk directory {trunks_dir}"))?;
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "conf") {
                        if let Ok(text) = std::fs::read_to_string(&path) {
                            for (peer, nets) in telephony::sip::peer_config::parse_identify_sections(&text) {
                                edge.entry(peer).or_insert_with(Vec::new).extend(nets);
                            }
                        }
                    }
                }
                let report = telephony::sip::peer_config::verify(&database, &edge);
                let consistent = report.iter().all(|c| c.outcome == "OK");
                let out = json!({
                    "consistent": consistent,
                    "peers": report.iter().map(|c| json!({ "peer": c.peer, "outcome": c.outcome, "database": c.database_networks, "edge": c.edge_networks })).collect::<Vec<_>>(),
                });
                if consistent { Ok(out) } else { Err(format!("peer networks are inconsistent: {}", serde_json::to_string_pretty(&out).unwrap_or_default())) }
            }
        },
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
    match run(&db, cli.command, cli.operator.as_deref()).await {
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

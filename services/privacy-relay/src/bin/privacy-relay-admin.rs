//! Privacy Relay operator CLI (no network API: an operator with shell access to a
//! Privacy Relay instance and its secrets runs it).
//!
//! ```text
//! privacy-relay-admin contact-vault generate-keyring --purpose encryption|fingerprint --version V --out FILE
//! privacy-relay-admin contact-vault add-key --purpose P --version V --file FILE
//! privacy-relay-admin contact-vault activate-key --purpose P --version V --file FILE
//! privacy-relay-admin contact-vault retire-key --purpose P --version V --file FILE   (DATABASE_URL, keyrings)
//! privacy-relay-admin contact-vault rotation-status            (DATABASE_URL, keyrings)
//! privacy-relay-admin contact-vault reencrypt [--batch N]      (DATABASE_URL, keyrings)
//! privacy-relay-admin contact-vault backfill-fingerprints [--batch N]
//! privacy-relay-admin contact-vault prune-fingerprints --version V
//! privacy-relay-admin relay-number add --number +E164 --capabilities VOICE_INBOUND,VOICE_OUTBOUND --binding REF [--region R] [--available true]
//! privacy-relay-admin relay-number activate|enable|drain|disable --id UUID
//! privacy-relay-admin relay-number list
//! privacy-relay-admin retention hold --reason POLICY_REVIEW|DISPUTE|SECURITY_INVESTIGATION|OTHER --by OPERATOR
//!                                    [--class CLASS|*] [--resource UUID] [--tenant UUID [--application UUID [--environment UUID]]]
//! privacy-relay-admin retention release --id UUID
//! privacy-relay-admin retention list [--all true]
//! privacy-relay-admin incidents list [--all true]
//! privacy-relay-admin incidents resolve --id UUID [--operator OPERATOR]   (or CF_OPERATOR_ID)
//! ```
//!
//! `incidents resolve` changes state: it names its operator (`--operator`, else
//! `CF_OPERATOR_ID`; refused with exit 2 and nothing changed without one) and writes a
//! `privacy_relay_operator_audit` row in the SAME transaction as the resolution.
//! Incident output is the safe summary only (never a number, reference or secret).
//!
//! Relay-number commands need DATABASE_URL and CF_RUNTIME_ENVIRONMENT
//! (RELAY_NUMBER_COOLDOWN_SECONDS applies to a disable that ends an assignment). The
//! provider binding is an opaque reference (never the number); `list` masks numbers.
//!
//! Keyring files are written with mode 0600 and never printed. Every keyring change is
//! atomic (`<file>.tmp` + rename; the live file is never removed first) and keeps a
//! `<file>.bak-<unix-seconds>` copy (mode 0600) of the previous document, which the
//! operator deletes after verifying the new one. Rotation never deletes a key
//! implicitly: `retire-key` removes a version only when it is not active and no stored
//! contact / fingerprint references it (`rotation-status` shows referenced and
//! referenced-but-unreadable versions).

use std::{collections::HashMap, path::Path, process::ExitCode, sync::Arc};

use privacy_relay::{
    clock::SystemClock,
    config::{ContactVaultSettings, FeatureFlag, RelayConfig},
    contact_vault::{
        keyring::{
            activate_key_version, add_key_version, generate_keyring_json, retire_key_version,
            KeyPurpose, KeyVersion,
        },
        secret_file, ContactVault,
    },
    enforcement::UnconfiguredCallingEnforcer,
    incidents,
    relay_numbers::inventory::NewRelayNumber,
    retention::{
        holds::{self, HoldReason, HoldScope},
        DataClass,
    },
    service::RelayService,
};
use sea_orm::{ConnectOptions, Database};

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  privacy-relay-admin contact-vault generate-keyring --purpose encryption|fingerprint --version V --out FILE\n  \
         privacy-relay-admin contact-vault add-key --purpose P --version V --file FILE\n  \
         privacy-relay-admin contact-vault activate-key --purpose P --version V --file FILE\n  \
         privacy-relay-admin contact-vault retire-key --purpose P --version V --file FILE\n  \
         privacy-relay-admin contact-vault rotation-status\n  \
         privacy-relay-admin contact-vault reencrypt [--batch N]\n  \
         privacy-relay-admin contact-vault backfill-fingerprints [--batch N]\n  \
         privacy-relay-admin contact-vault prune-fingerprints --version V\n  \
         privacy-relay-admin relay-number add --number +E164 --capabilities C[,C] --binding REF [--region R] [--available true]\n  \
         privacy-relay-admin relay-number activate|enable|drain|disable --id UUID\n  \
         privacy-relay-admin relay-number list\n  \
         privacy-relay-admin retention hold --reason R --by OPERATOR [--class C|*] [--resource UUID] [--tenant UUID [--application UUID [--environment UUID]]]\n  \
         privacy-relay-admin retention release --id UUID\n  \
         privacy-relay-admin retention list [--all true]\n  \
         privacy-relay-admin incidents list [--all true]\n  \
         privacy-relay-admin incidents resolve --id UUID [--operator OPERATOR]   (or CF_OPERATOR_ID)"
    );
    ExitCode::from(2)
}

fn flags(args: &[String]) -> Option<HashMap<String, String>> {
    let mut out = HashMap::new();
    let mut it = args.iter();
    while let Some(k) = it.next() {
        let key = k.strip_prefix("--")?;
        out.insert(key.to_string(), it.next()?.clone());
    }
    Some(out)
}

fn purpose(value: Option<&String>) -> Option<KeyPurpose> {
    match value.map(String::as_str) {
        Some("encryption") => Some(KeyPurpose::Encryption),
        Some("fingerprint") => Some(KeyPurpose::Fingerprint),
        _ => None,
    }
}

async fn vault() -> Result<ContactVault, String> {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required".to_string())?;
    let settings = ContactVaultSettings::from_env().map_err(|e| e.to_string())?;
    let empty_trust = cf_service_auth::TrustPolicy::from_json(
        &format!(
            "{{\"bundle_version\":1,\"receiver\":\"cfmeet.privacy-relay\",\"runtime_environment\":\"{}\",\"trusted_issuers\":[]}}",
            std::env::var("CF_RUNTIME_ENVIRONMENT").unwrap_or_else(|_| "local".into())
        ),
        &cf_service_auth::ServiceId::privacy_relay(),
        cf_service_auth::RuntimeEnvironment::parse(&std::env::var("CF_RUNTIME_ENVIRONMENT").unwrap_or_else(|_| "local".into()))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let settings = ContactVaultSettings {
        delegation_trust_file: None,
        ..settings
    };
    let material = settings
        .load(
            empty_trust.runtime_environment(),
            &empty_trust,
            Arc::new(cf_service_auth::SystemClock),
        )
        .map_err(|e| e.to_string())?;
    if material.keys.is_none() {
        return Err(
            "CONTACT_VAULT_KEYRING_FILE and CONTACT_FINGERPRINT_KEYRING_FILE are required".into(),
        );
    }
    let _ = url;
    let db = database().await?;
    Ok(ContactVault::new(
        db,
        Arc::new(SystemClock),
        FeatureFlag::new(settings.enabled),
        material.keys,
    ))
}

async fn relay_service() -> Result<RelayService, String> {
    dotenvy::dotenv().ok();
    let config = RelayConfig::from_env().map_err(|e| e.to_string())?;
    let runtime = cf_service_auth::RuntimeEnvironment::parse(
        &std::env::var("CF_RUNTIME_ENVIRONMENT")
            .map_err(|_| "CF_RUNTIME_ENVIRONMENT is required".to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut options = ConnectOptions::new(config.database_url().to_string());
    options.max_connections(2).sqlx_logging(false);
    let db = Database::connect(options)
        .await
        .map_err(|_| "cannot connect to the database".to_string())?;
    Ok(RelayService::new(
        db,
        Arc::new(SystemClock),
        FeatureFlag::new(false),
        runtime,
        Arc::new(UnconfiguredCallingEnforcer),
    )
    .with_relay_number_pool(FeatureFlag::new(false), config.relay_number_cooldown))
}

async fn database() -> Result<sea_orm::DatabaseConnection, String> {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required".to_string())?;
    let mut options = ConnectOptions::new(url);
    options.max_connections(2).sqlx_logging(false);
    Database::connect(options)
        .await
        .map_err(|_| "cannot connect to the database".to_string())
}

/// `retention hold|release|list` (DATABASE_URL). Holds freeze retention for a resource,
/// a scope or a data class; workers skip what an active hold covers.
async fn retention(command: &str, f: &HashMap<String, String>) -> ExitCode {
    let db = match database().await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("privacy-relay-admin: {e}");
            return ExitCode::from(2);
        }
    };
    let now = chrono::Utc::now();
    let uuid = |key: &str| f.get(key).map(|v| uuid::Uuid::parse_str(v));
    let result = match command {
        "hold" => {
            let (Some(reason), Some(by)) = (
                f.get("reason").and_then(|r| HoldReason::parse(r)),
                f.get("by"),
            ) else {
                return usage();
            };
            let class = match f.get("class").map(String::as_str) {
                None | Some("*") => None,
                Some(c) => match DataClass::parse(c) {
                    Some(c) => Some(c),
                    None => return usage(),
                },
            };
            let parse = |key: &str| match uuid(key) {
                None => Ok(None),
                Some(Ok(id)) => Ok(Some(id)),
                Some(Err(_)) => Err(()),
            };
            let (Ok(resource), Ok(tenant), Ok(application), Ok(environment)) = (
                parse("resource"),
                parse("tenant"),
                parse("application"),
                parse("environment"),
            ) else {
                return usage();
            };
            let scope = HoldScope {
                data_class: class,
                resource_id: resource,
                tenant_id: tenant,
                application_id: application,
                environment_id: environment,
            };
            holds::place(&db, &scope, reason, by, now)
                .await
                .map(|id| println!("hold {id} placed"))
        }
        "release" => {
            let Some(Ok(id)) = uuid("id") else {
                return usage();
            };
            holds::release(&db, id, now).await.map(|released| {
                println!("hold {id}: {}", if released { "released" } else { "already released" })
            })
        }
        "list" => holds::list(&db, f.get("all").map(String::as_str) != Some("true"))
            .await
            .map(|rows| {
                for r in rows {
                    println!(
                        "{}\t{}\t{}\tresource {}\ttenant {}\tapplication {}\tenvironment {}\tby {}\tcreated {}\t{}",
                        r.id,
                        r.data_class,
                        r.reason_code,
                        r.resource_id.map(|v| v.to_string()).unwrap_or_else(|| "*".into()),
                        r.tenant_id.map(|v| v.to_string()).unwrap_or_else(|| "*".into()),
                        r.application_id.map(|v| v.to_string()).unwrap_or_else(|| "*".into()),
                        r.environment_id.map(|v| v.to_string()).unwrap_or_else(|| "*".into()),
                        r.created_by,
                        r.created_at.to_rfc3339(),
                        r.released_at
                            .map(|t| format!("released {}", t.to_rfc3339()))
                            .unwrap_or_else(|| "ACTIVE".into()),
                    );
                }
            }),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("privacy-relay-admin: {e}");
            ExitCode::from(1)
        }
    }
}

/// `incidents list|resolve` (DATABASE_URL).
async fn incidents_command(command: &str, f: &HashMap<String, String>) -> ExitCode {
    // Validate the operator before touching the database (a refused resolve changes
    // nothing).
    let operator = f
        .get("operator")
        .cloned()
        .or_else(|| std::env::var("CF_OPERATOR_ID").ok())
        .unwrap_or_default();
    if command == "resolve" && !incidents::valid_operator(&operator) {
        eprintln!(
            "privacy-relay-admin: this command changes state: --operator <id> (or CF_OPERATOR_ID) is required"
        );
        return ExitCode::from(2);
    }
    let db = match database().await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("privacy-relay-admin: {e}");
            return ExitCode::from(2);
        }
    };
    match command {
        "list" => {
            match incidents::list(&db, f.get("all").map(String::as_str) == Some("true")).await {
                Ok(rows) => {
                    for r in rows {
                        println!(
                            "{}\t{}\t{}\t{}\toccurrences {}\tfirst {}\tlast {}\t{}\t{}",
                            r.id,
                            r.category,
                            r.severity,
                            r.status,
                            r.occurrences,
                            r.first_seen_at.to_rfc3339(),
                            r.last_seen_at.to_rfc3339(),
                            r.resolved_by
                                .map(|by| format!("resolved by {by}"))
                                .unwrap_or_else(|| "-".into()),
                            r.summary,
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("privacy-relay-admin: {}", e.code().as_str());
                    ExitCode::from(1)
                }
            }
        }
        "resolve" => {
            let Some(Ok(id)) = f.get("id").map(|v| uuid::Uuid::parse_str(v)) else {
                return usage();
            };
            match incidents::resolve(&db, id, &operator, chrono::Utc::now()).await {
                Ok(row) => {
                    println!("incident {id} ({}) resolved by {operator}", row.category);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    ExitCode::from(1)
                }
            }
        }
        _ => usage(),
    }
}

async fn relay_number(command: &str, f: &HashMap<String, String>) -> ExitCode {
    let service = match relay_service().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("privacy-relay-admin: {e}");
            return ExitCode::from(2);
        }
    };
    let id = || f.get("id").and_then(|v| uuid::Uuid::parse_str(v).ok());
    let result = match command {
        "add" => {
            let (Some(number), Some(caps), Some(binding)) =
                (f.get("number"), f.get("capabilities"), f.get("binding"))
            else {
                return usage();
            };
            let caps: Vec<&str> = caps.split(',').map(str::trim).collect();
            service
                .add_relay_number(NewRelayNumber {
                    number,
                    region: f.get("region").map(String::as_str),
                    capabilities: &caps,
                    provider_binding_ref: binding,
                    available: f.get("available").map(String::as_str) == Some("true"),
                })
                .await
                .map(|id| println!("added {id}"))
        }
        "activate" | "enable" | "drain" | "disable" => {
            let Some(id) = id() else {
                return usage();
            };
            let r = match command {
                "activate" => service.activate_relay_number(id).await,
                "enable" => service.enable_relay_number(id).await,
                "drain" => service.drain_relay_number(id).await,
                _ => service.disable_relay_number(id).await,
            };
            r.map(|_| println!("{command} {id}: done"))
        }
        "list" => service.list_relay_numbers().await.map(|rows| {
            for r in rows {
                println!(
                    "{}\t{}\t{}\t{}\t{}\tgeneration {}",
                    r.id,
                    r.masked(),
                    r.country_iso2,
                    r.capabilities.join(","),
                    r.operational_status,
                    r.generation
                );
            }
        }),
        _ => return usage(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("privacy-relay-admin: {}", e.code().as_str());
            ExitCode::from(1)
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 && args[0] == "relay-number" {
        let Some(f) = flags(&args[2..]) else {
            return usage();
        };
        return relay_number(&args[1], &f).await;
    }
    if args.len() >= 2 && args[0] == "incidents" {
        let Some(f) = flags(&args[2..]) else {
            return usage();
        };
        return incidents_command(&args[1], &f).await;
    }
    if args.len() >= 2 && args[0] == "retention" {
        let Some(f) = flags(&args[2..]) else {
            return usage();
        };
        return retention(&args[1], &f).await;
    }
    if args.len() < 2 || args[0] != "contact-vault" {
        return usage();
    }
    let Some(f) = flags(&args[2..]) else {
        return usage();
    };
    match args[1].as_str() {
        "generate-keyring" => {
            let (Some(p), Some(v), Some(out)) = (
                purpose(f.get("purpose")),
                f.get("version").and_then(|v| KeyVersion::parse(v)),
                f.get("out"),
            ) else {
                return usage();
            };
            let doc = generate_keyring_json(p, &v);
            if let Err(e) = secret_file::write_new(Path::new(out), &doc) {
                eprintln!("privacy-relay-admin: {e}");
                return ExitCode::from(1);
            }
            eprintln!(
                "keyring {} written (mode 0600), active version {v}",
                p.as_str()
            );
        }
        cmd @ ("add-key" | "activate-key") => {
            let (Some(p), Some(v), Some(file)) = (
                purpose(f.get("purpose")),
                f.get("version").and_then(|v| KeyVersion::parse(v)),
                f.get("file"),
            ) else {
                return usage();
            };
            let Ok(current) = std::fs::read_to_string(file).map(zeroize::Zeroizing::new) else {
                eprintln!("privacy-relay-admin: cannot read the keyring file");
                return ExitCode::from(1);
            };
            let updated = if cmd == "add-key" {
                add_key_version(&current, p, &v)
            } else {
                activate_key_version(&current, p, &v)
            };
            match updated {
                Ok(doc) => match secret_file::replace(Path::new(file), &doc) {
                    Ok(backup) => eprintln!(
                        "keyring updated ({cmd} {v}); previous document kept at {} (mode 0600; remove it after verification); \
                         redeploy every instance before the next step",
                        backup.display()
                    ),
                    Err(e) => {
                        eprintln!("privacy-relay-admin: {e} (the keyring file is unchanged)");
                        return ExitCode::from(1);
                    }
                },
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        "retire-key" => {
            let (Some(p), Some(v), Some(file)) = (
                purpose(f.get("purpose")),
                f.get("version").and_then(|v| KeyVersion::parse(v)),
                f.get("file"),
            ) else {
                return usage();
            };
            let Ok(current) = std::fs::read_to_string(file).map(zeroize::Zeroizing::new) else {
                eprintln!("privacy-relay-admin: cannot read the keyring file");
                return ExitCode::from(1);
            };
            // The database decides: a version any stored row still names stays.
            let status = match vault().await {
                Ok(vault) => match vault.rotation_status().await {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("privacy-relay-admin: {e}");
                        return ExitCode::from(1);
                    }
                },
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(2);
                }
            };
            let referenced = match p {
                KeyPurpose::Encryption => &status.referenced_encryption,
                KeyPurpose::Fingerprint => &status.referenced_fingerprint,
            };
            match retire_key_version(&current, p, &v, referenced) {
                Ok(doc) => match secret_file::replace(Path::new(file), &doc) {
                    Ok(backup) => eprintln!(
                        "key version {v} retired from the {} keyring; previous document kept at {} \
                         (mode 0600; backups taken before now still need this version — keep it until they age out)",
                        p.as_str(),
                        backup.display()
                    ),
                    Err(e) => {
                        eprintln!("privacy-relay-admin: {e} (the keyring file is unchanged)");
                        return ExitCode::from(1);
                    }
                },
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        "rotation-status" => match vault().await {
            Ok(v) => match (
                v.key_version_counts().await,
                v.fingerprint_version_counts().await,
                v.rotation_status().await,
            ) {
                (Ok(counts), Ok(fingerprints), Ok(status)) => {
                    for (version, n) in counts {
                        println!("encryption\t{version}\t{n}");
                    }
                    for (version, n) in fingerprints {
                        println!("fingerprint\t{version}\t{n}");
                    }
                    for version in &status.unreadable_encryption {
                        println!("encryption\t{version}\tREFERENCED BUT NOT READABLE");
                    }
                    for version in &status.unreadable_fingerprint {
                        println!("fingerprint\t{version}\tREFERENCED BUT NOT READABLE");
                    }
                    if !status.unreadable_encryption.is_empty()
                        || !status.unreadable_fingerprint.is_empty()
                    {
                        eprintln!("privacy-relay-admin: stored rows reference key versions the keyring cannot read; the service refuses to start until they are restored");
                        return ExitCode::from(1);
                    }
                }
                (Err(e), _, _) | (_, Err(e), _) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(1);
                }
                (_, _, Err(e)) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(1);
                }
            },
            Err(e) => {
                eprintln!("privacy-relay-admin: {e}");
                return ExitCode::from(2);
            }
        },
        "backfill-fingerprints" => {
            let batch = f
                .get("batch")
                .and_then(|b| b.parse::<u64>().ok())
                .unwrap_or(500);
            match vault().await {
                Ok(v) => loop {
                    match v.backfill_fingerprints(batch).await {
                        Ok(r) => {
                            println!(
                                "backfilled {}\tfailed {}\tremaining {}",
                                r.backfilled, r.failed, r.remaining
                            );
                            if r.backfilled == 0 {
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("privacy-relay-admin: {e}");
                            return ExitCode::from(1);
                        }
                    }
                },
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        "prune-fingerprints" => {
            let Some(version) = f.get("version") else {
                return usage();
            };
            match vault().await {
                Ok(v) => match v.prune_fingerprint_version(version).await {
                    Ok(n) => println!("removed {n} fingerprints of {version}"),
                    Err(e) => {
                        eprintln!("privacy-relay-admin: {e}");
                        return ExitCode::from(1);
                    }
                },
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        "reencrypt" => {
            let batch = f
                .get("batch")
                .and_then(|b| b.parse::<u64>().ok())
                .unwrap_or(500);
            match vault().await {
                Ok(v) => loop {
                    match v.rewrap_batch(batch).await {
                        Ok(r) => {
                            println!(
                                "rewrapped {}\tfailed {}\tremaining {}",
                                r.rewrapped, r.failed, r.remaining
                            );
                            if r.rewrapped == 0 {
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("privacy-relay-admin: {e}");
                            return ExitCode::from(1);
                        }
                    }
                },
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        _ => return usage(),
    }
    ExitCode::SUCCESS
}

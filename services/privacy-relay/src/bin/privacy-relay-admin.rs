//! Privacy Relay operator CLI (no network API: an operator with shell access to a
//! Privacy Relay instance and its secrets runs it).
//!
//! ```text
//! privacy-relay-admin contact-vault generate-keyring --purpose encryption|fingerprint --version V --out FILE
//! privacy-relay-admin contact-vault add-key --purpose P --version V --file FILE
//! privacy-relay-admin contact-vault activate-key --purpose P --version V --file FILE
//! privacy-relay-admin contact-vault rotation-status            (DATABASE_URL, keyrings)
//! privacy-relay-admin contact-vault reencrypt [--batch N]      (DATABASE_URL, keyrings)
//! ```
//!
//! Keyring files are written with mode 0600 and never printed. Rotation never deletes a
//! key: an operator removes an old version from the file only after `rotation-status`
//! shows that no contact uses it.

use std::{collections::HashMap, io::Write, path::Path, process::ExitCode, sync::Arc};

use privacy_relay::{
    clock::SystemClock,
    config::{ContactVaultSettings, FeatureFlag},
    contact_vault::{
        keyring::{
            activate_key_version, add_key_version, generate_keyring_json, KeyPurpose, KeyVersion,
        },
        ContactVault,
    },
};
use sea_orm::{ConnectOptions, Database};

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  privacy-relay-admin contact-vault generate-keyring --purpose encryption|fingerprint --version V --out FILE\n  \
         privacy-relay-admin contact-vault add-key --purpose P --version V --file FILE\n  \
         privacy-relay-admin contact-vault activate-key --purpose P --version V --file FILE\n  \
         privacy-relay-admin contact-vault rotation-status\n  \
         privacy-relay-admin contact-vault reencrypt [--batch N]"
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

/// Writes a secret file atomically with mode 0600.
fn write_secret(path: &Path, content: &str, create_new: bool) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    if create_new && path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "refusing to overwrite an existing keyring",
        ));
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
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
    let mut options = ConnectOptions::new(url);
    options.max_connections(2).sqlx_logging(false);
    let db = Database::connect(options)
        .await
        .map_err(|_| "cannot connect to the database".to_string())?;
    Ok(ContactVault::new(
        db,
        Arc::new(SystemClock),
        FeatureFlag::new(settings.enabled),
        material.keys,
    ))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
            if let Err(e) = write_secret(Path::new(out), &doc, true) {
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
                Ok(doc) => {
                    let path = Path::new(file);
                    let _ = std::fs::remove_file(path.with_extension("tmp"));
                    if let Err(e) =
                        std::fs::remove_file(path).and_then(|_| write_secret(path, &doc, false))
                    {
                        eprintln!("privacy-relay-admin: {e}");
                        return ExitCode::from(1);
                    }
                    eprintln!(
                        "keyring updated ({cmd} {v}); redeploy every instance before the next step"
                    );
                }
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        "rotation-status" => match vault().await {
            Ok(v) => match v.key_version_counts().await {
                Ok(counts) => {
                    for (version, n) in counts {
                        println!("{version}\t{n}");
                    }
                }
                Err(e) => {
                    eprintln!("privacy-relay-admin: {e}");
                    return ExitCode::from(1);
                }
            },
            Err(e) => {
                eprintln!("privacy-relay-admin: {e}");
                return ExitCode::from(2);
            }
        },
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

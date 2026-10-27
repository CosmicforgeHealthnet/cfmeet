//! Messaging operator CLI (Part V J). No network API: an operator with shell access to a
//! Messaging instance and its database credentials runs it (DATABASE_URL).
//!
//! ```text
//! messaging-admin kill-switch list
//! messaging-admin kill-switch engage  --switch NAME [--environment UUID] --reason CODE [--operator ID]
//! messaging-admin kill-switch release --switch NAME [--environment UUID] --reason CODE [--operator ID]
//! messaging-admin incidents list [--all true]
//! messaging-admin incidents resolve --id UUID [--operator ID]
//! messaging-admin dead-letters list
//! messaging-admin dead-letters replay --kind outbox|webhook-relay|object-deletion [--id UUID] [--operator ID]
//! messaging-admin kt checkpoint --out FILE        (writes the current tree head, mode 0600)
//! messaging-admin kt verify --checkpoint FILE     (exit 3 on rollback / fork)
//! ```
//!
//! Switch names: MESSAGE_SEND, CONVERSATION_CREATE, E2EE_ONBOARDING, ATTACHMENT_UPLOAD,
//! SMS_BRIDGE, WEBHOOK_RELAY. Switches only stop a capability; none enables anything.
//! State changes name their operator (`--operator`, else `CF_OPERATOR_ID`; refused with
//! exit 2 and nothing changed without one) and are audited in the same transaction.
//! Output never contains message content, a number, a key or a secret.

use std::{collections::HashMap, path::Path, process::ExitCode};

use chrono::Utc;
use messaging::operations::{self, KillSwitch, KtVerdict};
use sea_orm::Database;
use uuid::Uuid;

fn usage() -> ExitCode {
    eprintln!("usage: messaging-admin kill-switch list|engage|release … | incidents list|resolve … | dead-letters list|replay … | kt checkpoint|verify … (see the source header)");
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

fn operator(f: &HashMap<String, String>) -> Option<String> {
    f.get("operator")
        .cloned()
        .or_else(|| std::env::var("CF_OPERATOR_ID").ok())
        .filter(|o| !o.is_empty())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return usage();
    }
    let Some(f) = flags(&args[2..]) else {
        return usage();
    };
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("messaging-admin: DATABASE_URL is required");
        return ExitCode::from(2);
    };
    let db = match Database::connect(url).await {
        Ok(db) => db,
        Err(_) => {
            eprintln!("messaging-admin: cannot connect to the Messaging database");
            return ExitCode::from(1);
        }
    };
    let now = Utc::now();
    let result: Result<ExitCode, String> = match (args[0].as_str(), args[1].as_str()) {
        ("kill-switch", "list") => operations::switches(&db)
            .await
            .map_err(|e| e.to_string())
            .map(|rows| {
                for r in rows {
                    println!("{}", serde_json::to_string(&r).unwrap_or_default());
                }
                ExitCode::SUCCESS
            }),
        ("kill-switch", action @ ("engage" | "release")) => {
            let Some(switch) = f.get("switch").and_then(|s| KillSwitch::parse(s)) else {
                eprintln!("messaging-admin: --switch must be one of MESSAGE_SEND, CONVERSATION_CREATE, E2EE_ONBOARDING, ATTACHMENT_UPLOAD, SMS_BRIDGE, WEBHOOK_RELAY");
                return ExitCode::from(2);
            };
            let environment = match f.get("environment").map(|e| Uuid::parse_str(e)) {
                None => None,
                Some(Ok(e)) => Some(e),
                Some(Err(_)) => {
                    eprintln!("messaging-admin: --environment must be a UUID");
                    return ExitCode::from(2);
                }
            };
            let (Some(op), Some(reason)) = (operator(&f), f.get("reason")) else {
                eprintln!("messaging-admin: --reason and an operator (--operator or CF_OPERATOR_ID) are required");
                return ExitCode::from(2);
            };
            operations::set_switch(&db, switch, environment, action == "engage", reason, &op, now)
                .await
                .map_err(|e| e.to_string())
                .map(|changed| {
                    println!("{}", serde_json::json!({ "switch": switch.as_str(), "environment": environment, "engaged": action == "engage", "changed": changed }));
                    ExitCode::SUCCESS
                })
        }
        ("incidents", "list") => {
            operations::incidents(&db, f.get("all").map(|a| a == "true").unwrap_or(false))
                .await
                .map_err(|e| e.to_string())
                .map(|rows| {
                    for r in rows {
                        println!("{}", serde_json::to_string(&r).unwrap_or_default());
                    }
                    ExitCode::SUCCESS
                })
        }
        ("incidents", "resolve") => {
            let (Some(id), Some(op)) = (
                f.get("id").and_then(|i| Uuid::parse_str(i).ok()),
                operator(&f),
            ) else {
                eprintln!("messaging-admin: --id UUID and an operator (--operator or CF_OPERATOR_ID) are required");
                return ExitCode::from(2);
            };
            operations::resolve(&db, id, &op, now)
                .await
                .map_err(|e| e.to_string())
                .map(|done| {
                    println!(
                        "{}",
                        serde_json::json!({ "incident": id, "resolved": done })
                    );
                    if done {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    }
                })
        }
        ("dead-letters", "list") => operations::dead_letters(&db)
            .await
            .map_err(|e| e.to_string())
            .map(|rows| {
                for (kind, n) in rows {
                    println!(
                        "{}",
                        serde_json::json!({ "kind": kind.as_str(), "deadLetters": n })
                    );
                }
                ExitCode::SUCCESS
            }),
        ("dead-letters", "replay") => {
            let kind = f
                .get("kind")
                .and_then(|k| operations::DeadLetterKind::parse(k));
            let id = match f.get("id").map(|i| Uuid::parse_str(i)) {
                None => None,
                Some(Ok(id)) => Some(id),
                Some(Err(_)) => {
                    eprintln!("messaging-admin: --id must be a UUID");
                    return ExitCode::from(2);
                }
            };
            let (Some(kind), Some(op)) = (kind, operator(&f)) else {
                eprintln!("messaging-admin: --kind outbox|webhook-relay|object-deletion and an operator (--operator or CF_OPERATOR_ID) are required");
                return ExitCode::from(2);
            };
            operations::replay_dead_letters(&db, kind, id, &op, now)
                .await
                .map_err(|e| e.to_string())
                .map(|n| {
                    println!(
                        "{}",
                        serde_json::json!({ "kind": kind.as_str(), "replayed": n })
                    );
                    ExitCode::SUCCESS
                })
        }
        ("kt", "checkpoint") => {
            let Some(out) = f.get("out") else {
                return usage();
            };
            match operations::kt_checkpoint(&db).await {
                Ok(c) => operations::write_checkpoint(Path::new(out), &c)
                    .map_err(|e| format!("cannot write the checkpoint: {e}"))
                    .map(|_| {
                        println!("{}", serde_json::to_string(&c).unwrap_or_default());
                        ExitCode::SUCCESS
                    }),
                Err(e) => Err(e.to_string()),
            }
        }
        ("kt", "verify") => {
            let Some(path) = f.get("checkpoint") else {
                return usage();
            };
            match operations::read_checkpoint(Path::new(path)) {
                Ok(Some(c)) => operations::kt_verify(&db, &c, now)
                    .await
                    .map_err(|e| e.to_string())
                    .map(|v| {
                        println!("{v:?}");
                        if matches!(v, KtVerdict::Consistent { .. }) {
                            ExitCode::SUCCESS
                        } else {
                            ExitCode::from(3)
                        }
                    }),
                Ok(None) => Err("no checkpoint at that path".into()),
                Err(e) => Err(format!("checkpoint unreadable: {e}")),
            }
        }
        _ => return usage(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("messaging-admin: {e}");
            ExitCode::from(1)
        }
    }
}

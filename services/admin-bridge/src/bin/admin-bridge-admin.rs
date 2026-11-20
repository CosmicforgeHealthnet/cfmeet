//! Admin bridge operator tool (CF Meet operators, design §9 stage S3: enrolling HQ and
//! signing the access policy). `DATABASE_URL` is the bridge's own database.
//!
//!   enroll-cert       --fingerprint HEX [--not-before T] --expires T --operator NAME
//!   enroll-hmac       --kid ID --key-file PATH [--not-before T] --expires T --operator NAME
//!   enroll-staff-key  --kid ID --public-key-hex HEX --issuer ISS --audience AUD
//!                     [--not-before T] --expires T --operator NAME
//!   revoke-principal  --kind CLIENT_CERT|HMAC_KEY|STAFF_KEY --key-id ID --operator NAME
//!   revoke-staff      --sub SUB [--session ID] --reason CODE --operator NAME
//!   policy-ceiling    --revision N                      (prints JSON; no database)
//!   policy-sign       --key-file PKCS8_PEM --document FILE   (prints hex; no database)
//!   policy-put        --revision N --document FILE --signature HEX --key-id ID
//!                     --public-key-hex HEX --operator NAME
//!   verify-audit-chain
//!
//! Times are RFC 3339. Only key digests and public keys are stored; an HMAC key file is
//! read, hashed and never written anywhere. Errors never echo a supplied value.

use std::{collections::HashMap, process::ExitCode};

use admin_bridge::{
    contract::model::HQ_PRINCIPAL,
    policy,
    store::{
        pg::{verify_audit_chain, PgStore},
        PolicyRow, PrincipalKind,
    },
    wire::sha256_hex,
};
use chrono::{DateTime, Utc};
use ed25519_dalek::{pkcs8::DecodePrivateKey, SigningKey, VerifyingKey};
use sea_orm::{ConnectionTrait, DbBackend, Statement, Value as DbValue};

const MIN_HMAC_KEY_BYTES: usize = 32;

struct Args {
    command: String,
    flags: HashMap<String, String>,
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("admin-bridge-admin: {msg}");
    ExitCode::from(2)
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().ok_or("missing subcommand")?;
    let mut flags = HashMap::new();
    while let Some(flag) = it.next() {
        let name = flag
            .strip_prefix("--")
            .ok_or("arguments must be --flag value pairs")?
            .to_string();
        let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
        if flags.insert(name.clone(), value).is_some() {
            return Err(format!("--{name} given twice"));
        }
    }
    Ok(Args { command, flags })
}

impl Args {
    fn req(&self, name: &str) -> Result<&str, String> {
        self.flags
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("--{name} is required"))
    }
    fn opt(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }
    fn time(&self, name: &str) -> Result<Option<DateTime<Utc>>, String> {
        self.opt(name)
            .map(|v| {
                DateTime::parse_from_rfc3339(v)
                    .map(|t| t.with_timezone(&Utc))
                    .map_err(|_| format!("--{name} is not RFC 3339"))
            })
            .transpose()
    }
    fn only(&self, allowed: &[&str]) -> Result<(), String> {
        match self.flags.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(k) => Err(format!("--{k} is not an option of {}", self.command)),
            None => Ok(()),
        }
    }
    fn operator(&self) -> Result<String, String> {
        let op = self.req("operator")?.trim();
        if op.is_empty() || op.len() > 128 {
            return Err("--operator must be 1-128 characters".into());
        }
        Ok(op.to_string())
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn key_id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

async fn store() -> Result<PgStore, String> {
    dotenvy::dotenv().ok();
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set".to_string())?;
    PgStore::connect(&url, 2)
        .await
        .map_err(|_| "cannot connect to the database".to_string())
}

async fn exec(store: &PgStore, sql: &str, values: Vec<DbValue>) -> Result<u64, String> {
    store
        .connection()
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .await
        .map(|r| r.rows_affected())
        .map_err(|_| {
            "the database refused the change (duplicate, constraint or trigger)".to_string()
        })
}

#[allow(clippy::too_many_arguments)]
async fn enroll(
    a: &Args,
    kind: PrincipalKind,
    key_id: &str,
    material: &str,
    issuer: Option<&str>,
    audience: Option<&str>,
) -> Result<(), String> {
    let operator = a.operator()?;
    let not_before = a.time("not-before")?.unwrap_or_else(Utc::now);
    let expires = a.time("expires")?.ok_or("--expires is required")?;
    if expires <= not_before {
        return Err("--expires must be after --not-before".into());
    }
    let s = store().await?;
    exec(
        &s,
        "INSERT INTO admin_remote_principals (principal, kind, key_id, material, issuer, audience, not_before, expires_at, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        vec![
            HQ_PRINCIPAL.into(),
            kind.as_str().into(),
            key_id.into(),
            material.into(),
            issuer.map(str::to_owned).into(),
            audience.map(str::to_owned).into(),
            not_before.into(),
            expires.into(),
            operator.into(),
        ],
    )
    .await?;
    println!(
        "{}",
        serde_json::json!({"enrolled": kind.as_str(), "key_id": key_id, "not_before": not_before.to_rfc3339(), "expires_at": expires.to_rfc3339()})
    );
    Ok(())
}

async fn run(a: Args) -> Result<(), String> {
    match a.command.as_str() {
        "enroll-cert" => {
            a.only(&["fingerprint", "not-before", "expires", "operator"])?;
            let fp = a.req("fingerprint")?.to_ascii_lowercase().replace(':', "");
            if !is_hex(&fp, 64) {
                return Err("--fingerprint must be a SHA-256 fingerprint (64 hex)".into());
            }
            enroll(&a, PrincipalKind::ClientCert, &fp, &fp, None, None).await
        }
        "enroll-hmac" => {
            a.only(&["kid", "key-file", "not-before", "expires", "operator"])?;
            let kid = a.req("kid")?;
            if !key_id_ok(kid) {
                return Err("--kid is not a valid key id".into());
            }
            let bytes = zeroize::Zeroizing::new(
                std::fs::read(a.req("key-file")?)
                    .map_err(|_| "cannot read --key-file".to_string())?,
            );
            if bytes.len() < MIN_HMAC_KEY_BYTES {
                return Err(format!(
                    "the HMAC key must be at least {MIN_HMAC_KEY_BYTES} bytes"
                ));
            }
            let digest = sha256_hex(&bytes);
            enroll(&a, PrincipalKind::HmacKey, kid, &digest, None, None).await
        }
        "enroll-staff-key" => {
            a.only(&[
                "kid",
                "public-key-hex",
                "issuer",
                "audience",
                "not-before",
                "expires",
                "operator",
            ])?;
            let kid = a.req("kid")?;
            if !key_id_ok(kid) {
                return Err("--kid is not a valid key id".into());
            }
            let pk = a.req("public-key-hex")?.to_ascii_lowercase();
            let bytes: [u8; 32] = hex::decode(&pk)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or("--public-key-hex must be 64 hex")?;
            VerifyingKey::from_bytes(&bytes)
                .map_err(|_| "--public-key-hex is not an Ed25519 public key".to_string())?;
            let (iss, aud) = (a.req("issuer")?, a.req("audience")?);
            if iss.is_empty() || iss.len() > 128 || aud.is_empty() || aud.len() > 128 {
                return Err("--issuer and --audience must be 1-128 characters".into());
            }
            enroll(&a, PrincipalKind::StaffKey, kid, &pk, Some(iss), Some(aud)).await
        }
        "revoke-principal" => {
            a.only(&["kind", "key-id", "operator"])?;
            a.operator()?;
            let kind = PrincipalKind::parse(a.req("kind")?)
                .ok_or("--kind must be CLIENT_CERT, HMAC_KEY or STAFF_KEY")?;
            let s = store().await?;
            let n = exec(
                &s,
                "UPDATE admin_remote_principals SET revoked_at = now() WHERE kind = $1 AND key_id = $2 AND revoked_at IS NULL",
                vec![kind.as_str().into(), a.req("key-id")?.into()],
            )
            .await?;
            if n == 0 {
                return Err("no live enrolment with that kind and key id".into());
            }
            println!("{}", serde_json::json!({"revoked": kind.as_str()}));
            Ok(())
        }
        "revoke-staff" => {
            a.only(&["sub", "session", "reason", "operator"])?;
            let operator = a.operator()?;
            let s = store().await?;
            exec(
                &s,
                "INSERT INTO admin_revocations (sub, session_id, revoked_at, reason_code, created_by) VALUES ($1, $2, now(), $3, $4)",
                vec![a.req("sub")?.into(), a.opt("session").map(str::to_owned).into(), a.req("reason")?.into(), operator.into()],
            )
            .await?;
            println!(
                "{}",
                serde_json::json!({"revoked": if a.opt("session").is_some() { "SESSION" } else { "STAFF_MEMBER" }})
            );
            Ok(())
        }
        "policy-ceiling" => {
            a.only(&["revision"])?;
            let revision: i64 = a
                .req("revision")?
                .parse()
                .map_err(|_| "--revision must be an integer")?;
            if revision < 1 {
                return Err("--revision must be at least 1".into());
            }
            let issued = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            println!(
                "{}",
                serde_json::to_string_pretty(&policy::ceiling_document(revision, &issued))
                    .expect("json")
            );
            Ok(())
        }
        "policy-sign" => {
            a.only(&["key-file", "document"])?;
            let pem = zeroize::Zeroizing::new(
                std::fs::read_to_string(a.req("key-file")?)
                    .map_err(|_| "cannot read --key-file".to_string())?,
            );
            let key = SigningKey::from_pkcs8_pem(&pem)
                .map_err(|_| "--key-file is not a PKCS#8 Ed25519 private key".to_string())?;
            let doc = std::fs::read(a.req("document")?)
                .map_err(|_| "cannot read --document".to_string())?;
            println!("{}", policy::sign(&key, &doc));
            Ok(())
        }
        "policy-put" => {
            a.only(&[
                "revision",
                "document",
                "signature",
                "key-id",
                "public-key-hex",
                "operator",
            ])?;
            let operator = a.operator()?;
            let revision: i64 = a
                .req("revision")?
                .parse()
                .map_err(|_| "--revision must be an integer")?;
            let document = std::fs::read(a.req("document")?)
                .map_err(|_| "cannot read --document".to_string())?;
            let row = PolicyRow {
                revision,
                document,
                signature: a.req("signature")?.to_ascii_lowercase(),
                key_id: a.req("key-id")?.to_string(),
            };
            let pk: [u8; 32] = hex::decode(a.req("public-key-hex")?)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or("--public-key-hex must be 64 hex")?;
            let key = VerifyingKey::from_bytes(&pk)
                .map_err(|_| "--public-key-hex is not an Ed25519 public key".to_string())?;
            // Same verification the bridge runs at start-up: signature, revision, ceiling.
            policy::verify(&row, &key, &row.key_id, revision)
                .map_err(|e| format!("policy refused: {e}"))?;
            let s = store().await?;
            let digest = sha256_hex(&row.document);
            exec(
                &s,
                "INSERT INTO admin_policy_revisions (revision, document, signature, key_id, document_sha256, created_by) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
                vec![revision.into(), row.document.clone().into(), row.signature.clone().into(), row.key_id.clone().into(), digest.clone().into(), operator.into()],
            )
            .await?;
            println!(
                "{}",
                serde_json::json!({"policy_revision": revision, "document_sha256": digest})
            );
            Ok(())
        }
        "verify-audit-chain" => {
            a.only(&[])?;
            let s = store().await?;
            match verify_audit_chain(&s)
                .await
                .map_err(|_| "cannot read the audit".to_string())?
            {
                Ok(n) => {
                    println!(
                        "{}",
                        serde_json::json!({"audit_chain": "INTACT", "entries": n})
                    );
                    Ok(())
                }
                Err(seq) => Err(format!("audit chain BROKEN at sequence {seq}")),
            }
        }
        _ => Err("unknown subcommand (see the file header for usage)".into()),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => return fail(&e),
    };
    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("admin-bridge-admin: {e}");
            ExitCode::FAILURE
        }
    }
}

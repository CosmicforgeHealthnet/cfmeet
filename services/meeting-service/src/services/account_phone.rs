//! Optional phone binding/sign-in. Disabled until an operator provisions Twilio Verify.
//! The provider confirms control; clients can never submit a "verified" flag. Challenges
//! are one-use, session-bound for binding, rate-limited, and never used to verify email.
use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult, ErrorCode};

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}
fn invalid() -> ApiError {
    ApiError::BadRequest("Invalid or expired phone verification".into())
}
fn unavailable() -> ApiError {
    ApiError::domain(
        ErrorCode::ServiceUnavailable,
        "Phone verification is unavailable",
    )
}

pub fn number(value: &str) -> ApiResult<String> {
    let s = value.trim();
    let bytes = s.as_bytes();
    if !(9..=16).contains(&bytes.len())
        || bytes[0] != b'+'
        || !(b'1'..=b'9').contains(&bytes[1])
        || !bytes[2..].iter().all(u8::is_ascii_digit)
    {
        return Err(ApiError::BadRequest(
            "Use an international phone number, for example +393331234567".into(),
        ));
    }
    Ok(s.to_string())
}

#[async_trait]
pub trait VerificationProvider: Send + Sync {
    async fn start(&self, number: &str) -> ApiResult<String>;
    async fn approved(&self, number: &str, verification: &str, code: &str) -> ApiResult<bool>;
}

pub struct TwilioVerify {
    client: reqwest::Client,
    account: String,
    secret: String,
    service: String,
}
fn sid(value: &str, prefix: &str) -> bool {
    value.len() == 34
        && value.starts_with(prefix)
        && value[2..].bytes().all(|b| b.is_ascii_hexdigit())
}
impl TwilioVerify {
    pub fn from_env() -> ApiResult<Self> {
        if std::env::var("PHONE_ACCOUNTS_ENABLED").ok().as_deref() != Some("true") {
            return Err(ApiError::domain(
                ErrorCode::FeatureDisabled,
                "Phone accounts are not configured",
            ));
        }
        let account = std::env::var("PHONE_VERIFY_ACCOUNT_SID").map_err(|_| unavailable())?;
        let service = std::env::var("PHONE_VERIFY_SERVICE_SID").map_err(|_| unavailable())?;
        let file = std::env::var("PHONE_VERIFY_AUTH_TOKEN_FILE").map_err(|_| unavailable())?;
        let secret = std::fs::read_to_string(file)
            .map_err(|_| unavailable())?
            .trim()
            .to_string();
        if !sid(&account, "AC") || !sid(&service, "VA") || secret.len() < 16 {
            return Err(unavailable());
        }
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| unavailable())?;
        Ok(Self {
            client,
            account,
            secret,
            service,
        })
    }
    async fn post(&self, action: &str, body: &[(&str, &str)]) -> ApiResult<Value> {
        let r = self
            .client
            .post(format!(
                "https://verify.twilio.com/v2/Services/{}/{action}",
                self.service
            ))
            .basic_auth(&self.account, Some(&self.secret))
            .form(body)
            .send()
            .await
            .map_err(|_| unavailable())?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(invalid());
        }
        if !r.status().is_success() {
            return Err(unavailable());
        }
        r.json().await.map_err(|_| unavailable())
    }
}
#[async_trait]
impl VerificationProvider for TwilioVerify {
    async fn start(&self, phone: &str) -> ApiResult<String> {
        let v = self
            .post("Verifications", &[("To", phone), ("Channel", "sms")])
            .await?;
        let id = v["sid"]
            .as_str()
            .filter(|s| sid(s, "VE"))
            .ok_or_else(unavailable)?;
        if v["to"] != phone || v["service_sid"] != self.service || v["status"] != "pending" {
            return Err(unavailable());
        }
        Ok(id.to_string())
    }
    async fn approved(&self, phone: &str, verification: &str, code: &str) -> ApiResult<bool> {
        let v = self
            .post(
                "VerificationCheck",
                &[("VerificationSid", verification), ("Code", code)],
            )
            .await?;
        Ok(v["status"] == "approved"
            && v["valid"] == true
            && v["to"] == phone
            && v["sid"] == verification
            && v["service_sid"] == self.service)
    }
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhoneView {
    pub status: String,
    pub phone_number: Option<String>,
    pub discoverable: bool,
}

pub async fn view(db: &DatabaseConnection, user: Uuid) -> ApiResult<PhoneView> {
    if std::env::var("PHONE_ACCOUNTS_ENABLED").ok().as_deref() != Some("true") {
        return Ok(PhoneView {
            status: "PROVIDER_BLOCKED".into(),
            phone_number: None,
            discoverable: false,
        });
    }
    let row = db
        .query_one(stmt(
            "SELECT phone_e164,discoverable FROM account_phone_bindings WHERE user_id=$1",
            vec![user.into()],
        ))
        .await?;
    Ok(match row {
        Some(r) => PhoneView {
            status: "VERIFIED".into(),
            phone_number: Some(r.try_get("", "phone_e164")?),
            discoverable: r.try_get("", "discoverable")?,
        },
        None => PhoneView {
            status: "NOT_LINKED".into(),
            phone_number: None,
            discoverable: false,
        },
    })
}

pub async fn start(
    db: &DatabaseConnection,
    provider: &impl VerificationProvider,
    user: Uuid,
    session: Option<Uuid>,
    phone: &str,
) -> ApiResult<Uuid> {
    let phone = number(phone)?;
    let txn = db.begin().await?;
    txn.execute(stmt(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,1))",
        vec![user.to_string().into()],
    ))
    .await?;
    txn.execute(stmt(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        vec![phone.clone().into()],
    ))
    .await?;
    txn.execute(stmt(
        "DELETE FROM account_phone_challenges WHERE expires_at<now()-interval '1 day'",
        vec![],
    ))
    .await?;
    let recent = txn.query_one(stmt("SELECT count(*) AS n FROM account_phone_challenges WHERE created_at>now()-interval '15 minutes' AND (user_id=$1 OR phone_e164=$2)", vec![user.into(),phone.clone().into()])).await?.ok_or_else(unavailable)?;
    if recent.try_get::<i64>("", "n")? >= 3 {
        return Err(ApiError::rate_limited("Try phone verification later", 900));
    }
    let verification = provider.start(&phone).await?;
    let id = Uuid::new_v4();
    txn.execute(stmt("INSERT INTO account_phone_challenges(id,user_id,session_id,purpose,phone_e164,provider_id) VALUES($1,$2,$3,$4,$5,$6)",
        vec![id.into(),user.into(),session.into(),if session.is_some(){"BIND"}else{"SIGNIN"}.into(),phone.into(),verification.into()])).await?;
    txn.commit().await?;
    Ok(id)
}

/// A provider approval is consumed while the challenge is row-locked. Neither replay
/// nor a second browser can bind the number or mint a second sign-in from that proof.
pub async fn verify(
    db: &DatabaseConnection,
    provider: &impl VerificationProvider,
    challenge: Uuid,
    binding: Option<(Uuid, Uuid)>,
    code: &str,
) -> ApiResult<Uuid> {
    if code.len() != 6 || !code.bytes().all(|c| c.is_ascii_digit()) {
        return Err(invalid());
    }
    let txn = db.begin().await?;
    let owner = txn
        .query_one(stmt(
            "SELECT user_id FROM account_phone_challenges WHERE id=$1",
            vec![challenge.into()],
        ))
        .await?
        .ok_or_else(invalid)?;
    let account: Uuid = owner.try_get("", "user_id")?;
    txn.execute(stmt(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,1))",
        vec![account.to_string().into()],
    ))
    .await?;
    let row = txn.query_one(stmt("SELECT user_id,session_id,phone_e164,provider_id,purpose FROM account_phone_challenges WHERE id=$1 AND consumed_at IS NULL AND expires_at>now() AND attempts<5 FOR UPDATE", vec![challenge.into()])).await?.ok_or_else(invalid)?;
    let user: Uuid = row.try_get("", "user_id")?;
    let session: Option<Uuid> = row.try_get("", "session_id")?;
    let purpose: String = row.try_get("", "purpose")?;
    if match binding {
        Some((u, s)) => purpose != "BIND" || user != u || session != Some(s),
        None => purpose != "SIGNIN",
    } {
        return Err(invalid());
    }
    let phone: String = row.try_get("", "phone_e164")?;
    let verification: String = row.try_get("", "provider_id")?;
    txn.execute(stmt(
        "UPDATE account_phone_challenges SET attempts=attempts+1 WHERE id=$1",
        vec![challenge.into()],
    ))
    .await?;
    let approved = provider.approved(&phone, &verification, code).await;
    if !matches!(approved, Ok(true)) {
        txn.commit().await?;
        return Err(invalid());
    }
    if binding.is_some() {
        let other = txn
            .query_one(stmt(
                "SELECT 1 FROM account_phone_bindings WHERE phone_e164=$1 AND user_id<>$2",
                vec![phone.clone().into(), user.into()],
            ))
            .await?;
        if other.is_some() {
            return Err(ApiError::BadRequest(
                "This number is already linked to another account".into(),
            ));
        }
        txn.execute(stmt("INSERT INTO account_phone_bindings(user_id,phone_e164) VALUES($1,$2) ON CONFLICT(user_id) DO UPDATE SET phone_e164=EXCLUDED.phone_e164,verified_at=now(),discoverable=false",vec![user.into(),phone.into()])).await?;
    } else {
        // A number removed or replaced after the SMS was sent cannot sign in anymore.
        let current = txn
            .query_one(stmt(
                "SELECT 1 FROM account_phone_bindings WHERE user_id=$1 AND phone_e164=$2 FOR SHARE",
                vec![user.into(), phone.into()],
            ))
            .await?;
        if current.is_none() {
            return Err(invalid());
        }
    }
    txn.execute(stmt(
        "UPDATE account_phone_challenges SET consumed_at=now() WHERE id=$1",
        vec![challenge.into()],
    ))
    .await?;
    txn.commit().await?;
    Ok(user)
}

/// Privacy changes remain in the account domain, rather than the transport layer.
pub async fn set_discoverable(
    db: &DatabaseConnection,
    user: Uuid,
    discoverable: bool,
) -> ApiResult<()> {
    db.execute(stmt(
        "UPDATE account_phone_bindings SET discoverable=$2 WHERE user_id=$1",
        vec![user.into(), discoverable.into()],
    ))
    .await?;
    Ok(())
}

/// Removing a number also retires pending SMS proofs and all current credentials in
/// one transaction. A previously issued binding proof cannot restore the number.
pub async fn remove(db: &DatabaseConnection, user: Uuid) -> ApiResult<()> {
    let txn = db.begin().await?;
    txn.execute(stmt(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,1))",
        vec![user.to_string().into()],
    ))
    .await?;
    txn.execute(stmt(
        "DELETE FROM account_phone_bindings WHERE user_id=$1",
        vec![user.into()],
    ))
    .await?;
    txn.execute(stmt("UPDATE account_phone_challenges SET consumed_at=now() WHERE user_id=$1 AND consumed_at IS NULL", vec![user.into()])).await?;
    crate::services::account_security::revoke_all(
        &txn,
        user,
        crate::services::account_security::RevokeReason::CredentialsChanged,
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

pub async fn find(
    db: &DatabaseConnection,
    phone: &str,
    discovery: bool,
) -> ApiResult<Option<Uuid>> {
    let row = db.query_one(stmt("SELECT b.user_id FROM account_phone_bindings b JOIN users u ON u.id=b.user_id WHERE b.phone_e164=$1 AND (NOT $2 OR b.discoverable) AND u.status::text='active'", vec![number(phone)?.into(),discovery.into()])).await?;
    row.map(|r| r.try_get("", "user_id").map_err(ApiError::from))
        .transpose()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhoneRequest {
    pub phone_number: String,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PhoneConfirm {
    pub challenge_id: Uuid,
    pub code: String,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PhoneChallenge {
    pub challenge_id: Uuid,
    pub message: String,
    pub expires_in_secs: u64,
}
impl PhoneChallenge {
    pub fn sent(id: Uuid) -> Self {
        Self {
            challenge_id: id,
            message: "If this number is eligible, a verification code has been sent".into(),
            expires_in_secs: 600,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_canonical_international_numbers_are_accepted() {
        assert_eq!(number(" +393331234567 ").unwrap(), "+393331234567");
        for n in [
            "3331234567",
            "+00393331234567",
            "+39 3331234567",
            "+12",
            "+1234567890123456",
            "+39333\n1234567",
        ] {
            assert!(number(n).is_err(), "{n}");
        }
    }
}

//! Gap audit A1: an authenticator-app second factor (RFC 6238 TOTP: HMAC-SHA1, 6 digits,
//! 30-second steps; the parameters every common authenticator app supports) and
//! single-use recovery codes.
//!
//! * Enrolment: `setup` stores a new secret UNCONFIRMED and returns it (base32 and an
//!   `otpauth://` URI); `confirm` with a current code turns it on and returns 10 recovery
//!   codes once (stored as SHA-256 only). Turning it off needs a `DISABLE_MFA` step-up.
//! * Sign-in: once confirmed, a proven address (email code / link, Google) no longer
//!   yields tokens directly: the answer is `401 MFA_REQUIRED` with a one-time `mfaTicket`
//!   (5 minutes, at most 5 attempts) that `POST /api/v1/auth/mfa/verify` redeems with an
//!   authenticator code or a recovery code. A code's time step is accepted once.
//! * The secret is sealed at rest: XOR with an HMAC-SHA256 keystream derived from
//!   `MFA_ENCRYPTION_KEY` (or `JWT_SECRET`), the account and a random nonce. The sealed
//!   value names its key, so a rotated key keeps unsealing from
//!   `MFA_PREVIOUS_ENCRYPTION_KEYS` (or `JWT_PREVIOUS_SECRETS`).
use hmac::{Hmac, Mac};
use rand::RngCore;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

const STEP_SECS: i64 = 30;
const SECRET_BYTES: usize = 20;
const TICKET_TTL_SECS: u64 = 300;
const TICKET_ATTEMPTS: i64 = 5;
const RECOVERY_CODES: usize = 10;

fn stmt(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}

/// The sealing key: `MFA_ENCRYPTION_KEY`, else `JWT_SECRET` (also the root of the
/// account-export key, [`crate::services::account_export`]).
pub(crate) fn key() -> String {
    std::env::var("MFA_ENCRYPTION_KEY")
        .or_else(|_| std::env::var("JWT_SECRET"))
        .unwrap_or_default()
}

/// Keys still accepted for unsealing after a rotation (gap audit A15):
/// `MFA_PREVIOUS_ENCRYPTION_KEYS`, else `JWT_PREVIOUS_SECRETS` (comma-separated).
pub(crate) fn previous_keys() -> Vec<String> {
    std::env::var("MFA_PREVIOUS_ENCRYPTION_KEYS")
        .or_else(|_| std::env::var("JWT_PREVIOUS_SECRETS"))
        .unwrap_or_default()
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect()
}

fn keystream(key: &str, user: Uuid, nonce: &[u8]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("any key length");
    mac.update(b"cfmeet-mfa-totp-v1");
    mac.update(user.as_bytes());
    mac.update(nonce);
    mac.finalize().into_bytes().into()
}

/// `v2:<key id>:<nonce>:<ciphertext>`; the key id picks the key after a rotation.
fn seal(user: Uuid, secret: &[u8]) -> String {
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    let key = key();
    let ks = keystream(&key, user, &nonce);
    let ct: Vec<u8> = secret.iter().zip(ks.iter()).map(|(a, b)| a ^ b).collect();
    format!(
        "v2:{}:{}:{}",
        crate::services::auth::key_id(&key),
        hex::encode(nonce),
        hex::encode(ct)
    )
}

fn unseal(user: Uuid, sealed: &str) -> ApiResult<Vec<u8>> {
    let bad = || ApiError::InternalError("unreadable MFA secret".into());
    let parts: Vec<&str> = sealed.split(':').collect();
    let (key, nonce, ct) = match parts.as_slice() {
        ["v1", nonce, ct] => (key(), *nonce, *ct),
        ["v2", kid, nonce, ct] => {
            let key = std::iter::once(key())
                .chain(previous_keys())
                .find(|k| crate::services::auth::key_id(k) == *kid)
                .ok_or_else(bad)?;
            (key, *nonce, *ct)
        }
        _ => return Err(bad()),
    };
    let nonce = hex::decode(nonce).map_err(|_| bad())?;
    let ct = hex::decode(ct).map_err(|_| bad())?;
    let ks = keystream(&key, user, &nonce);
    Ok(ct.iter().zip(ks.iter()).map(|(a, b)| a ^ b).collect())
}

/// RFC 4648 base32 without padding (the authenticator-app format).
pub fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            out.push(ALPHABET[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Decodes RFC 4648 base32 (no padding, case-insensitive); `None` on a bad character.
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let (mut buffer, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for c in text.trim_end_matches('=').chars() {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            out.push((buffer >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

/// The current time step (30 seconds).
pub fn step_now() -> i64 {
    current_step()
}

/// The 6-digit code of `secret` at time step `step` (RFC 4226 / 6238, HMAC-SHA1).
pub fn code_at(secret: &[u8], step: i64) -> String {
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(secret).expect("any key length");
    mac.update(&(step as u64).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let offset = (h[19] & 0x0f) as usize;
    let bin = ((h[offset] as u32 & 0x7f) << 24)
        | ((h[offset + 1] as u32) << 16)
        | ((h[offset + 2] as u32) << 8)
        | h[offset + 3] as u32;
    format!("{:06}", bin % 1_000_000)
}

fn current_step() -> i64 {
    chrono::Utc::now().timestamp() / STEP_SECS
}

fn digest(code: &str) -> String {
    hex::encode(Sha256::digest(
        code.trim().to_ascii_lowercase().replace('-', "").as_bytes(),
    ))
}

/// Whether the account has a CONFIRMED authenticator.
pub async fn enabled<C: ConnectionTrait>(db: &C, user: Uuid) -> ApiResult<bool> {
    Ok(db
        .query_one(stmt(
            "SELECT 1 AS x FROM account_mfa_totp WHERE user_id = $1 AND confirmed_at IS NOT NULL",
            vec![user.into()],
        ))
        .await?
        .is_some())
}

/// The account's MFA state.
pub async fn status<C: ConnectionTrait>(db: &C, user: Uuid) -> ApiResult<serde_json::Value> {
    let enabled = enabled(db, user).await?;
    let remaining = db
        .query_one(stmt(
            "SELECT count(*) AS n FROM account_mfa_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
            vec![user.into()],
        ))
        .await?
        .map(|r| r.try_get::<i64>("", "n"))
        .transpose()?
        .unwrap_or(0);
    Ok(json!({ "totpEnabled": enabled, "recoveryCodesRemaining": remaining }))
}

/// Starts enrolment: a new unconfirmed secret (replacing an unconfirmed one).
pub async fn setup<C: ConnectionTrait>(
    db: &C,
    user: Uuid,
    label: &str,
) -> ApiResult<(String, String)> {
    if enabled(db, user).await? {
        return Err(ApiError::Conflict(
            "An authenticator is already enabled; turn it off first".into(),
        ));
    }
    let mut secret = [0u8; SECRET_BYTES];
    rand::thread_rng().fill_bytes(&mut secret);
    db.execute(stmt(
        "INSERT INTO account_mfa_totp (user_id, secret_sealed) VALUES ($1, $2) \
         ON CONFLICT (user_id) DO UPDATE SET secret_sealed = EXCLUDED.secret_sealed, confirmed_at = NULL, \
           last_used_step = NULL, updated_at = now()",
        vec![user.into(), seal(user, &secret).into()],
    ))
    .await?;
    let b32 = base32(&secret);
    let uri = format!(
        "otpauth://totp/CF%20Meet:{}?secret={b32}&issuer=CF%20Meet&algorithm=SHA1&digits=6&period=30",
        url::form_urlencoded::byte_serialize(label.as_bytes()).collect::<String>()
    );
    Ok((b32, uri))
}

/// Checks `code` against the account's secret (confirmed or not), accepting the current
/// step and one either side, each step at most once.
async fn check_totp<C: ConnectionTrait>(
    db: &C,
    user: Uuid,
    code: &str,
    confirmed: bool,
) -> ApiResult<bool> {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(false);
    }
    let row = db
        .query_one(stmt(
            &format!(
                "SELECT secret_sealed FROM account_mfa_totp WHERE user_id = $1 AND confirmed_at IS {} NULL",
                if confirmed { "NOT" } else { "" }
            ),
            vec![user.into()],
        ))
        .await?;
    let Some(row) = row else { return Ok(false) };
    let secret = unseal(user, &row.try_get::<String>("", "secret_sealed")?)?;
    let now = current_step();
    for step in [now - 1, now, now + 1] {
        if code_at(&secret, step) == code {
            // Each step once (replay of a seen code is refused).
            let claimed = db
                .execute(stmt(
                    "UPDATE account_mfa_totp SET last_used_step = $2, updated_at = now() \
                     WHERE user_id = $1 AND (last_used_step IS NULL OR last_used_step < $2)",
                    vec![user.into(), step.into()],
                ))
                .await?
                .rows_affected();
            return Ok(claimed == 1);
        }
    }
    Ok(false)
}

/// Finishes enrolment with a current code; returns the recovery codes (shown once).
pub async fn confirm<C: ConnectionTrait>(db: &C, user: Uuid, code: &str) -> ApiResult<Vec<String>> {
    if !check_totp(db, user, code, false).await? {
        return Err(ApiError::BadRequest("Invalid or expired code".into()));
    }
    db.execute(stmt(
        "UPDATE account_mfa_totp SET confirmed_at = now(), updated_at = now() WHERE user_id = $1",
        vec![user.into()],
    ))
    .await?;
    db.execute(stmt(
        "DELETE FROM account_mfa_recovery_codes WHERE user_id = $1",
        vec![user.into()],
    ))
    .await?;
    let mut codes = Vec::with_capacity(RECOVERY_CODES);
    for _ in 0..RECOVERY_CODES {
        let mut raw = [0u8; 6];
        rand::thread_rng().fill_bytes(&mut raw);
        let c = base32(&raw).to_ascii_lowercase();
        let pretty = format!("{}-{}", &c[..5], &c[5..]);
        db.execute(stmt(
            "INSERT INTO account_mfa_recovery_codes (user_id, code_hash) VALUES ($1, $2)",
            vec![user.into(), digest(&pretty).into()],
        ))
        .await?;
        codes.push(pretty);
    }
    Ok(codes)
}

/// Turns the second factor off (the caller checked the step-up).
pub async fn disable<C: ConnectionTrait>(db: &C, user: Uuid) -> ApiResult<()> {
    db.execute(stmt(
        "DELETE FROM account_mfa_totp WHERE user_id = $1",
        vec![user.into()],
    ))
    .await?;
    db.execute(stmt(
        "DELETE FROM account_mfa_recovery_codes WHERE user_id = $1",
        vec![user.into()],
    ))
    .await?;
    Ok(())
}

/// A pending sign-in waiting for the second factor.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Pending {
    pub user_id: Uuid,
    pub via: String,
    pub created: bool,
    pub test_account: bool,
}

fn ticket_key(ticket: &str) -> String {
    format!(
        "mfa:ticket:{}",
        hex::encode(Sha256::digest(ticket.as_bytes()))
    )
}

/// Parks a proven sign-in and returns the `401 MFA_REQUIRED` answer carrying its ticket.
pub async fn challenge(state: &AppState, pending: &Pending) -> ApiResult<ApiError> {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let ticket = hex::encode(raw);
    let value =
        serde_json::to_string(pending).map_err(|e| ApiError::InternalError(e.to_string()))?;
    state
        .redis
        .set(&ticket_key(&ticket), &value, Some(TICKET_TTL_SECS))
        .await?;
    Ok(ApiError::MfaRequired { ticket })
}

/// Redeems a ticket with an authenticator code or a recovery code. A wrong code keeps
/// the ticket (at most [`TICKET_ATTEMPTS`]); a right one consumes it.
pub async fn redeem(state: &AppState, ticket: &str, code: &str) -> ApiResult<Pending> {
    let invalid = || ApiError::BadRequest("Invalid or expired code".into());
    let key = ticket_key(ticket);
    let attempts = state
        .redis
        .incr_with_ttl(&format!("{key}:attempts"), TICKET_TTL_SECS)
        .await?;
    if attempts > TICKET_ATTEMPTS {
        state.redis.del(&key).await?;
        return Err(ApiError::TooManyRequests(
            "Too many attempts. Sign in again.".into(),
        ));
    }
    let Some(raw) = state.redis.get(&key).await? else {
        return Err(invalid());
    };
    let pending: Pending = serde_json::from_str(&raw).map_err(|_| invalid())?;
    let ok = if code.trim().len() == 6 {
        check_totp(&state.db, pending.user_id, code, true).await?
    } else {
        state
            .db
            .execute(stmt(
                "UPDATE account_mfa_recovery_codes SET used_at = now() \
                 WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
                vec![pending.user_id.into(), digest(code).into()],
            ))
            .await?
            .rows_affected()
            == 1
    };
    if !ok {
        return Err(invalid());
    }
    if state.redis.take(&key).await?.is_none() {
        return Err(invalid());
    }
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_sha1_vectors() {
        // RFC 6238 Appendix B (SHA1 seed "12345678901234567890"), last 6 digits.
        let seed = b"12345678901234567890";
        assert_eq!(code_at(seed, 59 / 30), "287082");
        assert_eq!(code_at(seed, 1111111109 / 30), "081804");
        assert_eq!(code_at(seed, 2000000000 / 30), "279037");
    }

    #[test]
    fn base32_and_sealing_round_trip() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");
        let user = Uuid::new_v4();
        let secret = [7u8; SECRET_BYTES];
        let sealed = seal(user, &secret);
        assert_eq!(unseal(user, &sealed).unwrap(), secret);
        assert_ne!(unseal(Uuid::new_v4(), &sealed).unwrap(), secret);
    }
}

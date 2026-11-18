//! Gap audit A22: avatar upload. The account keeps one uploaded image (PNG, JPEG or WebP,
//! at most 512 KiB, recognised by its content, not by the declared type). Uploading sets
//! the profile's `avatarUrl` to the image's public, immutable address
//! (`{base}/api/v2/avatars/{avatarId}`), through the same profile update as
//! `PATCH /api/v2/me/profile`, so the Communications profile follows. The image is
//! replaced, never edited: a new upload gets a new address.

use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult, ErrorCode};

/// Largest accepted image.
pub const MAX_AVATAR_BYTES: usize = 512 * 1024;

/// The image type, from its leading bytes.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Validate an upload: the declared type (if any) must match what the bytes are.
pub fn validate(bytes: &[u8], declared: Option<&str>) -> ApiResult<&'static str> {
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
        return Err(ApiError::domain(
            ErrorCode::ValidationError,
            "The avatar must be an image of at most 512 KiB",
        ));
    }
    let kind = sniff(bytes).ok_or_else(|| {
        ApiError::domain(
            ErrorCode::ValidationError,
            "The avatar must be a PNG, JPEG or WebP image",
        )
    })?;
    if let Some(declared) = declared {
        let declared = declared.split(';').next().unwrap_or("").trim();
        if !declared.eq_ignore_ascii_case(kind) {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Content-Type does not match the image",
            ));
        }
    }
    Ok(kind)
}

/// Public base of avatar addresses: `AVATAR_PUBLIC_BASE_URL`, else `SWAGGER_SERVER_URL`
/// (the public API address), else `APP_URL`.
pub fn public_base() -> String {
    ["AVATAR_PUBLIC_BASE_URL", "SWAGGER_SERVER_URL", "APP_URL"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "http://localhost:8080".into())
        .trim_end_matches('/')
        .to_string()
}

/// The image's public address. Profiles only hold `https://` avatar references, so
/// uploads need an https public base.
pub fn address(id: Uuid) -> ApiResult<String> {
    let base = public_base();
    if !base.starts_with("https://") {
        return Err(ApiError::domain(
            ErrorCode::FeatureDisabled,
            "Avatar uploads need an https public address (AVATAR_PUBLIC_BASE_URL)",
        ));
    }
    Ok(format!("{base}/api/v2/avatars/{id}"))
}

/// Store `bytes` as the account's avatar `id`, replacing any earlier one.
pub async fn store<C: TransactionTrait>(
    db: &C,
    user_id: Uuid,
    id: Uuid,
    kind: &str,
    bytes: &[u8],
) -> ApiResult<()> {
    let txn = db.begin().await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM account_avatars WHERE user_id = $1",
        [user_id.into()],
    ))
    .await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO account_avatars (id, user_id, content_type, bytes, sha256) VALUES ($1, $2, $3, $4, $5)",
        [
            id.into(),
            user_id.into(),
            kind.into(),
            bytes.to_vec().into(),
            hex::encode(Sha256::digest(bytes)).into(),
        ],
    ))
    .await?;
    txn.commit().await?;
    Ok(())
}

/// Remove the account's uploaded avatar. Returns whether there was one.
pub async fn remove<C: ConnectionTrait>(db: &C, user_id: Uuid) -> ApiResult<bool> {
    let r = db
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM account_avatars WHERE user_id = $1",
            [user_id.into()],
        ))
        .await?;
    Ok(r.rows_affected() > 0)
}

/// An avatar image: content type, bytes and digest (the entity tag).
pub async fn fetch<C: ConnectionTrait>(
    db: &C,
    id: Uuid,
) -> ApiResult<Option<(String, Vec<u8>, String)>> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT a.content_type, a.bytes, a.sha256 FROM account_avatars a \
             JOIN users u ON u.id = a.user_id \
             WHERE a.id = $1 AND lower(u.status::text) NOT IN ('inactive', 'suspended')",
            [id.into()],
        ))
        .await?;
    row.map(|r| {
        Ok((
            r.try_get("", "content_type")?,
            r.try_get("", "bytes")?,
            r.try_get("", "sha256")?,
        ))
    })
    .transpose()
    .map_err(|e: sea_orm::DbErr| e.into())
}

/// Whether `url` is one of this service's uploaded-avatar addresses.
pub fn is_uploaded(url: &str) -> bool {
    url.starts_with(&format!("{}/api/v2/avatars/", public_base()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_recognised_by_content() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff(b"<svg xmlns="), None);
        assert_eq!(sniff(b"GIF89a"), None);
        assert!(validate(b"\x89PNG\r\n\x1a\nrest", Some("image/png")).is_ok());
        assert!(validate(b"\x89PNG\r\n\x1a\nrest", Some("image/jpeg")).is_err());
        assert!(validate(b"\x89PNG\r\n\x1a\nrest", None).is_ok());
        assert!(validate(&[], None).is_err());
        let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
        big.resize(MAX_AVATAR_BYTES + 1, 0);
        assert!(validate(&big, None).is_err());
    }
}

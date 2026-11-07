use crate::error::{ApiError, ApiResult};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Token type claim values (ACC-001). Access tokens authenticate requests; refresh tokens
/// are accepted only by `POST /api/v1/auth/refresh`.
pub const TOKEN_TYPE_ACCESS: &str = "access";
pub const TOKEN_TYPE_REFRESH: &str = "refresh";

/// Tokens issued before typed tokens existed carry no `typ`. Their lifetime tells them
/// apart exactly: access tokens lived 15 minutes, refresh tokens 7 days.
const LEGACY_ACCESS_MAX_LIFETIME_SECS: i64 = 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Access,
    Refresh,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,  // User ID
    pub role: String, // User role
    pub exp: i64,     // Expiration time
    pub iat: i64,     // Issued at
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_time: Option<i64>, // Credential authentication; never advanced by refresh.
    /// `access` or `refresh`; absent on tokens issued before Blueprint Section 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typ: Option<String>,
    /// Account session id (`account_sessions.id`); absent on guest and legacy tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    /// Refresh-token id for rotation and reuse detection (refresh tokens only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rti: Option<String>,
}

impl Claims {
    /// The token's kind. `None` for an unknown `typ` value (always refused).
    pub fn kind(&self) -> Option<TokenKind> {
        match self.typ.as_deref() {
            Some(TOKEN_TYPE_ACCESS) => Some(TokenKind::Access),
            Some(TOKEN_TYPE_REFRESH) => Some(TokenKind::Refresh),
            Some(_) => None,
            None if self.exp - self.iat <= LEGACY_ACCESS_MAX_LIFETIME_SECS => {
                Some(TokenKind::Access)
            }
            None => Some(TokenKind::Refresh),
        }
    }

    pub fn is_kind(&self, kind: TokenKind) -> bool {
        self.kind() == Some(kind)
    }

    pub fn session_id(&self) -> Option<Uuid> {
        self.sid.as_deref().and_then(|s| Uuid::parse_str(s).ok())
    }

    pub fn refresh_id(&self) -> Option<Uuid> {
        self.rti.as_deref().and_then(|s| Uuid::parse_str(s).ok())
    }
}

pub struct JwtService {
    secret: String,
    access_token_expiry: Duration,
    refresh_token_expiry: Duration,
}

impl JwtService {
    pub fn new(secret: String) -> Self {
        Self {
            secret,
            access_token_expiry: Duration::minutes(15), // 15 minutes
            refresh_token_expiry: Duration::days(7),    // 7 days
        }
    }

    pub fn generate_access_token(&self, user_id: Uuid, role: &str) -> ApiResult<String> {
        self.generate_token(
            user_id,
            role,
            TokenKind::Access,
            Some(Utc::now().timestamp()),
            None,
            None,
        )
    }

    pub fn generate_refresh_token(&self, user_id: Uuid, role: &str) -> ApiResult<String> {
        self.generate_token(
            user_id,
            role,
            TokenKind::Refresh,
            Some(Utc::now().timestamp()),
            None,
            None,
        )
    }

    /// Token pair bound to an account session (registered users).
    pub fn session_tokens(
        &self,
        user_id: Uuid,
        role: &str,
        auth_time: i64,
        session_id: Uuid,
        refresh_id: Uuid,
    ) -> ApiResult<(String, String)> {
        Ok((
            self.generate_token(
                user_id,
                role,
                TokenKind::Access,
                Some(auth_time),
                Some(session_id),
                None,
            )?,
            self.generate_token(
                user_id,
                role,
                TokenKind::Refresh,
                Some(auth_time),
                Some(session_id),
                Some(refresh_id),
            )?,
        ))
    }

    /// Session-less pair (legacy guests only).
    pub fn refreshed_tokens(
        &self,
        user_id: Uuid,
        role: &str,
        auth_time: Option<i64>,
    ) -> ApiResult<(String, String)> {
        Ok((
            self.generate_token(user_id, role, TokenKind::Access, auth_time, None, None)?,
            self.generate_token(user_id, role, TokenKind::Refresh, auth_time, None, None)?,
        ))
    }

    fn generate_token(
        &self,
        user_id: Uuid,
        role: &str,
        kind: TokenKind,
        auth_time: Option<i64>,
        session_id: Option<Uuid>,
        refresh_id: Option<Uuid>,
    ) -> ApiResult<String> {
        let now = Utc::now();
        let (expiry, typ) = match kind {
            TokenKind::Access => (self.access_token_expiry, TOKEN_TYPE_ACCESS),
            TokenKind::Refresh => (self.refresh_token_expiry, TOKEN_TYPE_REFRESH),
        };
        let exp = (now + expiry).timestamp();

        let claims = Claims {
            sub: user_id.to_string(),
            role: role.to_string(),
            exp,
            iat: now.timestamp(),
            auth_time,
            typ: Some(typ.to_string()),
            sid: session_id.map(|s| s.to_string()),
            rti: refresh_id.map(|r| r.to_string()),
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .map_err(|e| ApiError::InternalError(format!("Failed to generate token: {}", e)))
    }

    pub fn verify_token(&self, token: &str) -> ApiResult<Claims> {
        decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &Validation::default(),
        )
        .map(|data| data.claims)
        .map_err(|e| ApiError::Unauthorized(format!("Invalid token: {}", e)))
    }
}

// Password hashing utilities
pub fn hash_password(password: &str) -> ApiResult<String> {
    bcrypt::hash(password, bcrypt::DEFAULT_COST)
        .map_err(|e| ApiError::InternalError(format!("Failed to hash password: {}", e)))
}

pub fn verify_password(password: &str, hash: &str) -> ApiResult<bool> {
    bcrypt::verify(password, hash)
        .map_err(|e| ApiError::InternalError(format!("Failed to verify password: {}", e)))
}

// Generate random 6-digit verification code (numeric only for easy entry)
pub fn generate_verification_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let code: u32 = rng.gen_range(100000..1000000);
    code.to_string()
}

/// Check if user can authenticate based on email verification requirement
///
/// Returns Ok(()) if user can proceed, Err if verification is needed
pub fn check_email_verification(
    user: &crate::models::users::Model,
    require_verification: bool,
) -> ApiResult<()> {
    use crate::models::users::UserStatus;

    // If verification not required, allow all non-inactive users
    if !require_verification {
        return Ok(());
    }

    // If verification required, check status
    if user.status == UserStatus::PendingVerification {
        return Err(ApiError::Forbidden(
            "Email verification required. Please check your email and verify your account."
                .to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod communications_authentication_tests {
    use super::*;
    #[test]
    fn refresh_preserves_credential_authentication_time_and_legacy_absence() {
        let service = JwtService::new("test-only-authentication-time-fixture-0123456789".into());
        let original = Utc::now().timestamp() - 1800;
        let id = Uuid::new_v4();
        let (access, refresh) = service
            .refreshed_tokens(id, "user", Some(original))
            .unwrap();
        for token in [access, refresh] {
            let claims = service.verify_token(&token).unwrap();
            assert_eq!(claims.auth_time, Some(original));
            assert!(claims.iat > original);
        }
        let (access, _) = service.refreshed_tokens(id, "user", None).unwrap();
        assert_eq!(service.verify_token(&access).unwrap().auth_time, None);
    }

    #[test]
    fn token_kinds_are_typed_and_legacy_tokens_are_classified_by_lifetime() {
        let service = JwtService::new("test-only-token-kind-fixture-0123456789abcdef".into());
        let id = Uuid::new_v4();
        let access = service.generate_access_token(id, "user").unwrap();
        let refresh = service.generate_refresh_token(id, "user").unwrap();
        assert_eq!(
            service.verify_token(&access).unwrap().kind(),
            Some(TokenKind::Access)
        );
        assert_eq!(
            service.verify_token(&refresh).unwrap().kind(),
            Some(TokenKind::Refresh)
        );
        let sid = Uuid::new_v4();
        let (a, r) = service
            .session_tokens(id, "user", 1, sid, Uuid::new_v4())
            .unwrap();
        let (a, r) = (
            service.verify_token(&a).unwrap(),
            service.verify_token(&r).unwrap(),
        );
        assert_eq!((a.session_id(), r.session_id()), (Some(sid), Some(sid)));
        assert!(a.refresh_id().is_none() && r.refresh_id().is_some());

        let legacy = |lifetime: i64| Claims {
            sub: id.to_string(),
            role: "user".into(),
            exp: 10_000 + lifetime,
            iat: 10_000,
            auth_time: None,
            typ: None,
            sid: None,
            rti: None,
        };
        assert_eq!(legacy(15 * 60).kind(), Some(TokenKind::Access));
        assert_eq!(legacy(7 * 24 * 3600).kind(), Some(TokenKind::Refresh));
        let mut unknown = legacy(900);
        unknown.typ = Some("id".into());
        assert_eq!(unknown.kind(), None);
    }
}

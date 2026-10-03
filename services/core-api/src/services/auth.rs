use crate::error::{ApiError, ApiResult};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,  // User ID
    pub role: String, // User role
    pub exp: i64,     // Expiration time
    pub iat: i64,     // Issued at
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
        self.generate_token(user_id, role, self.access_token_expiry)
    }

    pub fn generate_refresh_token(&self, user_id: Uuid, role: &str) -> ApiResult<String> {
        self.generate_token(user_id, role, self.refresh_token_expiry)
    }

    fn generate_token(&self, user_id: Uuid, role: &str, expiry: Duration) -> ApiResult<String> {
        let now = Utc::now();
        let exp = (now + expiry).timestamp();

        let claims = Claims {
            sub: user_id.to_string(),
            role: role.to_string(),
            exp,
            iat: now.timestamp(),
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
            "Email verification required. Please check your email and verify your account.".to_string()
        ));
    }
    
    Ok(())
}

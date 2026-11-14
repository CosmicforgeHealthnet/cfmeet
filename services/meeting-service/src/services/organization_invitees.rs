//! Account lookups for organization invitations (Session 3): which account an email
//! address belongs to (the ALREADY_MEMBER check) and whether a resolved account may be
//! invited at all. Never relayed to clients; the handler decides what to disclose.

use sea_orm::{sea_query::Expr, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::models::users::{self, UserRole, UserStatus};

/// The account registered with `email` (case-insensitive), if any.
pub async fn account_with_email<C: ConnectionTrait>(
    conn: &C,
    email: &str,
) -> Result<Option<Uuid>, DbErr> {
    let normalized = email.trim().to_ascii_lowercase();
    Ok(users::Entity::find()
        .filter(Expr::cust_with_values("lower(email) = $1", [normalized]))
        .one(conn)
        .await?
        .map(|u| u.id))
}

/// The lower-cased email of `user` when the account can receive an organization
/// invitation: a registered (non-guest) account that is active or awaiting email
/// verification. `None` for unknown, guest, inactive and suspended accounts.
pub async fn invitable_email<C: ConnectionTrait>(
    conn: &C,
    user: Uuid,
) -> Result<Option<String>, DbErr> {
    Ok(users::Entity::find_by_id(user)
        .one(conn)
        .await?
        .filter(|u| {
            u.role != UserRole::Guest
                && matches!(
                    u.status,
                    UserStatus::Active | UserStatus::PendingVerification
                )
        })
        .map(|u| u.email.trim().to_ascii_lowercase()))
}

//! Gap audit M8: per-account creation budgets for meeting resources.
//!
//! Each signed-in account may create at most a configured number of meeting spaces,
//! instant meetings and conference occurrences per hour (fixed windows in Redis, keyed by
//! a hash of the account id). Exceeding a budget answers `429 RATE_LIMITED`; the budgets
//! bound room and storage exhaustion by one account without affecting normal use.
//!
//! Machine callers (platform credentials) are governed by the API Gateway's plan quotas
//! and are not counted here. Redis unavailable: the request proceeds (availability first;
//! logged), exactly as the email budget does.

use crate::{
    error::{ApiError, ApiResult, ErrorCode},
    services::actor::ActorContext,
    state::AppState,
};

/// One creation budget: the rate-limiter action and its hourly default.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub action: &'static str,
    pub env: &'static str,
    pub default_per_hour: i64,
}

pub const SPACES: Budget = Budget {
    action: "meeting:space-create",
    env: "MEETING_SPACE_CREATES_PER_HOUR",
    default_per_hour: 60,
};
pub const INSTANT_MEETINGS: Budget = Budget {
    action: "meeting:instant-create",
    env: "MEETING_INSTANT_CREATES_PER_HOUR",
    default_per_hour: 60,
};
pub const CONFERENCES: Budget = Budget {
    action: "meeting:conference-create",
    env: "MEETING_CONFERENCE_CREATES_PER_HOUR",
    default_per_hour: 240,
};

/// The budget's hourly limit: its environment override (a positive integer) or default.
pub fn per_hour(budget: Budget) -> i64 {
    std::env::var(budget.env)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(budget.default_per_hour)
}

/// Spends one unit of `budget` for the signed-in account (no-op for other actors).
pub async fn spend(state: &AppState, actor: &ActorContext, budget: Budget) -> ApiResult<()> {
    let Some(user) = actor.registered() else {
        return Ok(());
    };
    match state
        .rate_limiter
        .check_action_rate(
            budget.action,
            &user.user_id.to_string(),
            per_hour(budget),
            3600,
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(ApiError::TooManyRequests(_)) => Err(ApiError::domain(
            ErrorCode::RateLimited,
            "Too many meetings created recently; try again later",
        )),
        Err(e) => {
            tracing::warn!(action = budget.action, "creation budget unavailable: {e}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_come_from_the_environment_or_the_defaults() {
        let b = Budget {
            action: "x",
            env: "CFMEET_TEST_CREATION_BUDGET_UNSET",
            default_per_hour: 7,
        };
        assert_eq!(per_hour(b), 7);
        std::env::set_var("CFMEET_TEST_CREATION_BUDGET_SET", " 3 ");
        assert_eq!(
            per_hour(Budget {
                env: "CFMEET_TEST_CREATION_BUDGET_SET",
                ..b
            }),
            3
        );
        std::env::set_var("CFMEET_TEST_CREATION_BUDGET_BAD", "-1");
        assert_eq!(
            per_hour(Budget {
                env: "CFMEET_TEST_CREATION_BUDGET_BAD",
                ..b
            }),
            7
        );
    }
}

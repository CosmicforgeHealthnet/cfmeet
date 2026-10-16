//! Lifecycle and validity rules shared by contexts and relay sessions.
//!
//! Authority is evaluated at READ time against the server clock — never only by the
//! expiry worker (which materializes final state and evidence but is not the security
//! boundary):
//!
//! * before `starts_at`: no authority;
//! * ACTIVE: authority until `expires_at` (exclusive);
//! * SUSPENDED: no new authority;
//! * COMPLETED: new authority only while `now < grace_expires_at` AND the policy's
//!   `grace_allows_new_calls`; completion without grace ends new authority;
//! * REVOKED / EXPIRED: never;
//! * after the final expiry (`grace_expires_at`, else `expires_at`): never, whatever the
//!   stored status.

use chrono::{DateTime, Duration, Utc};

use super::policy::ContextPolicy;
use crate::{
    error::{RelayError, RelayResult},
    vocabulary::{AuthorizationReason, CommunicationContextStatus},
};

/// Longest accepted validity window (`starts_at` .. `expires_at`).
pub fn max_validity() -> Duration {
    Duration::days(366)
}

/// Longest accepted completion grace after `expires_at`.
pub fn max_grace() -> Duration {
    Duration::days(30)
}

/// Lifecycle status shared by contexts and sessions.
pub type LifecycleStatus = CommunicationContextStatus;

/// Why a validity window grants no new authority now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowDenial {
    Revoked,
    Expired,
    Suspended,
    NotStarted,
    /// COMPLETED without (remaining) grace authority.
    NotActive,
    /// COMPLETED inside grace, but the policy denies new calls during grace.
    GraceDenied,
}

#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub status: LifecycleStatus,
    pub starts_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub grace_expires_at: Option<DateTime<Utc>>,
}

impl Window {
    pub fn final_expires_at(&self) -> DateTime<Utc> {
        self.grace_expires_at.unwrap_or(self.expires_at)
    }

    /// Whether ALL authority has ended at `now` (final expiry), whatever the status.
    pub fn finally_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.final_expires_at()
    }

    /// New-communication authority at `now`: the instant it ends, or why there is none.
    pub fn authority(
        &self,
        policy: &ContextPolicy,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, WindowDenial> {
        match self.status {
            LifecycleStatus::Revoked => return Err(WindowDenial::Revoked),
            LifecycleStatus::Expired => return Err(WindowDenial::Expired),
            _ => {}
        }
        if self.finally_expired(now) {
            return Err(WindowDenial::Expired);
        }
        if now < self.starts_at {
            return Err(WindowDenial::NotStarted);
        }
        match self.status {
            LifecycleStatus::Suspended => Err(WindowDenial::Suspended),
            LifecycleStatus::Active => {
                if now < self.expires_at {
                    Ok(self.expires_at)
                } else {
                    Err(WindowDenial::Expired)
                }
            }
            LifecycleStatus::Completed => match self.grace_expires_at {
                Some(grace) if now < grace => {
                    if policy.grace_allows_new_calls {
                        Ok(grace)
                    } else {
                        Err(WindowDenial::GraceDenied)
                    }
                }
                Some(_) => Err(WindowDenial::Expired),
                None => Err(WindowDenial::NotActive),
            },
            LifecycleStatus::Revoked | LifecycleStatus::Expired => unreachable!("handled above"),
        }
    }
}

impl WindowDenial {
    pub fn context_reason(&self) -> AuthorizationReason {
        match self {
            WindowDenial::Revoked => AuthorizationReason::ContextRevoked,
            WindowDenial::Expired => AuthorizationReason::ContextExpired,
            WindowDenial::Suspended => AuthorizationReason::ContextSuspended,
            WindowDenial::NotStarted => AuthorizationReason::ContextNotStarted,
            WindowDenial::NotActive => AuthorizationReason::ContextNotActive,
            WindowDenial::GraceDenied => AuthorizationReason::GracePolicyDenied,
        }
    }

    pub fn session_reason(&self) -> AuthorizationReason {
        match self {
            WindowDenial::Expired => AuthorizationReason::SessionExpired,
            WindowDenial::NotStarted => AuthorizationReason::SessionNotStarted,
            WindowDenial::GraceDenied => AuthorizationReason::GracePolicyDenied,
            WindowDenial::Revoked | WindowDenial::Suspended | WindowDenial::NotActive => {
                AuthorizationReason::SessionNotActive
            }
        }
    }
}

/// Validates a validity window: `starts_at < expires_at <= grace_expires_at`, bounded.
pub fn validate_validity(
    starts_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    grace_expires_at: Option<DateTime<Utc>>,
) -> RelayResult<()> {
    if starts_at >= expires_at {
        return Err(RelayError::Validation("startsAt must be before expiresAt."));
    }
    if expires_at - starts_at > max_validity() {
        return Err(RelayError::Validation(
            "The validity window is too long (at most 366 days).",
        ));
    }
    if let Some(grace) = grace_expires_at {
        if grace < expires_at {
            return Err(RelayError::Validation(
                "graceExpiresAt must not be before expiresAt.",
            ));
        }
        if grace - expires_at > max_grace() {
            return Err(RelayError::Validation(
                "The grace period is too long (at most 30 days).",
            ));
        }
    }
    Ok(())
}

/// The external context reference is consumer-defined and opaque, but bounded and never
/// contact data (mirrors the database `privacy_relay_safe_reference`).
pub fn validate_external_reference(value: &str) -> RelayResult<()> {
    const INVALID: RelayError = RelayError::Validation(
        "externalContextReference must be 1-128 characters [A-Za-z0-9._:-], start with a letter or digit, and must not be contact data.",
    );
    let bytes = value.as_bytes();
    let shape = !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
    if !shape {
        return Err(INVALID);
    }
    let digits = bytes.iter().filter(|b| b.is_ascii_digit()).count();
    let phone_like = bytes
        .iter()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b':' | b'-'))
        && digits >= 7;
    let lower = value.to_ascii_lowercase();
    let uri_like = ["tel:", "sip:", "sips:", "mailto:", "callto:", "wa:", "sms:"]
        .iter()
        .any(|p| lower.starts_with(p));
    if phone_like || uri_like {
        return Err(INVALID);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(minutes: i64) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2031-03-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
            + Duration::minutes(minutes)
    }

    fn window(status: LifecycleStatus, grace: Option<i64>) -> Window {
        Window {
            status,
            starts_at: t(0),
            expires_at: t(60),
            grace_expires_at: grace.map(t),
        }
    }

    #[test]
    fn authority_is_time_aware() {
        let allow = ContextPolicy {
            grace_allows_new_calls: true,
            ..ContextPolicy::default()
        };
        let deny = ContextPolicy::default();
        let active = window(LifecycleStatus::Active, None);
        assert_eq!(
            active.authority(&deny, t(-1)),
            Err(WindowDenial::NotStarted)
        );
        assert_eq!(active.authority(&deny, t(0)), Ok(t(60)));
        assert_eq!(active.authority(&deny, t(60)), Err(WindowDenial::Expired));
        // ACTIVE past expires_at but inside a configured grace: grace applies only after
        // completion.
        assert_eq!(
            window(LifecycleStatus::Active, Some(90)).authority(&allow, t(70)),
            Err(WindowDenial::Expired)
        );
        let completed = window(LifecycleStatus::Completed, Some(90));
        assert_eq!(completed.authority(&allow, t(70)), Ok(t(90)));
        assert_eq!(
            completed.authority(&deny, t(70)),
            Err(WindowDenial::GraceDenied)
        );
        assert_eq!(
            completed.authority(&allow, t(90)),
            Err(WindowDenial::Expired)
        );
        assert_eq!(
            window(LifecycleStatus::Completed, None).authority(&allow, t(10)),
            Err(WindowDenial::NotActive)
        );
        assert_eq!(
            window(LifecycleStatus::Suspended, None).authority(&allow, t(10)),
            Err(WindowDenial::Suspended)
        );
        assert_eq!(
            window(LifecycleStatus::Revoked, None).authority(&allow, t(10)),
            Err(WindowDenial::Revoked)
        );
    }

    #[test]
    fn validity_and_references_are_bounded() {
        assert!(validate_validity(t(0), t(60), None).is_ok());
        assert!(validate_validity(t(60), t(60), None).is_err());
        assert!(validate_validity(t(0), t(60), Some(t(59))).is_err());
        assert!(validate_validity(t(0), t(60), Some(t(60))).is_ok());
        for ok in [
            "delivery-8812",
            "order:2031:A17",
            "case_77",
            "x",
            "ORD-1234567X",
        ] {
            assert!(validate_external_reference(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "+2348012345678",
            "2348012345678",
            "080-1234-5678",
            "alice@example.com",
            "sip:alice",
            "tel:2348012345678",
            "has space",
            &"a".repeat(129),
            "-leading",
        ] {
            assert!(validate_external_reference(bad).is_err(), "{bad}");
        }
    }
}

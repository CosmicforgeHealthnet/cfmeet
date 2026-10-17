//! ORIGINATE intake (Part E §35, §75, §165–§168).
//!
//! Calling sends one versioned, idempotent command per authorized public leg attempt.
//! Telephony refuses it unless it carries COMPLETE, CURRENT authority proof:
//!
//! * the Calling Call / CallLeg / attempt / route identity and the route plan version;
//! * the Call authorization evidence (id, policy version);
//! * the Privacy Relay decision (id, context / session ids and versions, validity) —
//!   the leg's authority ends when that validity ends;
//! * the Contact Resolution Grant of the protected destination (Telephony alone can
//!   exchange it, once, for this leg);
//! * the spend reservation (id, reserved amount, currency, maximum duration);
//! * the idempotency key, correlation id and expected generation.
//!
//! The command is persisted (with the leg) BEFORE any provider action; workers execute
//! it later. Nothing here touches a provider, a telephone number or Privacy Relay.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    domain::{InterconnectClass, MediaAttachment, PublicRouteClass},
    error::{ErrorCode, TelephonyError, TelephonyResult},
};

pub const SCHEMA_VERSION: &str = "1";
/// Maximum validity window of the authority evidence a command may rely on.
pub const MAX_AUTHORITY_WINDOW_SECONDS: i64 = 6 * 3600;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CallRef {
    pub call_id: Uuid,
    pub call_leg_id: Uuid,
    pub attempt_id: Uuid,
    pub route_id: Uuid,
    pub route_class: PublicRouteClass,
    pub route_policy_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopeRef {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationProof {
    pub call_authorization_id: Uuid,
    pub call_policy_version: String,
    pub privacy_decision_id: Uuid,
    pub context_id: Uuid,
    pub context_version: i64,
    pub relay_session_id: Uuid,
    pub relay_session_version: i64,
    pub decided_at: DateTime<Utc>,
    pub valid_until: DateTime<Utc>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtectedDestination {
    pub contact_reference: Uuid,
    pub grant_id: Uuid,
    /// The grant secret: short-lived, single-execution, useless to anyone but Telephony.
    pub grant_secret: String,
    pub grant_expires_at: DateTime<Utc>,
    pub country: String,
}

impl std::fmt::Debug for ProtectedDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtectedDestination")
            .field("contact_reference", &self.contact_reference)
            .field("grant_id", &self.grant_id)
            .field("country", &self.country)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpendProof {
    pub reservation_id: Uuid,
    pub reserved_amount_minor: i64,
    pub currency: String,
    pub max_duration_seconds: i32,
}

/// `cfmeet.telephony.command` OriginateCommand v1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OriginateCommand {
    pub schema_version: String,
    pub command_id: Uuid,
    pub idempotency_key: String,
    pub correlation_id: String,
    pub expected_generation: i32,
    pub call: CallRef,
    pub scope: ScopeRef,
    pub authorization: AuthorizationProof,
    pub destination: ProtectedDestination,
    pub spend: SpendProof,
    pub media: MediaAttachment,
    /// Ordered candidate Telephony routes from the Routing Engine (may be empty: then
    /// Telephony chooses among eligible routes).
    #[serde(default)]
    pub candidate_routes: Vec<String>,
    /// Part IV L: the DCN this call presents — Numbering's single-use CALLER_IDENTITY
    /// grant (audience Telephony) for exactly this execution. Never a number: Telephony
    /// learns the identity only from Numbering's verification at execution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presented_identity: Option<PresentedIdentity>,
    /// Part IV L closure: the interconnect class Calling's route policy selected. PUBLIC
    /// (default) never uses a synthetic test route; SYNTHETIC_TEST uses ONLY synthetic test
    /// routes, presents a synthetic DCN and needs the synthetic interconnect enabled.
    #[serde(default)]
    pub interconnect_class: InterconnectClass,
}

/// A DCN presented as the caller identity (Part IV L).
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PresentedIdentity {
    pub number_resource_id: Uuid,
    /// The compact routing grant (bearer; never logged, never echoed).
    pub grant: String,
}

impl std::fmt::Debug for PresentedIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresentedIdentity")
            .field("number_resource_id", &self.number_resource_id)
            .finish_non_exhaustive()
    }
}

/// The single operation a CALLER_IDENTITY grant is bound to: this Calling leg's
/// origination command (`call.callLegId`, `commandId`). Calling requests the grant with
/// exactly this reference.
pub fn dcn_operation_ref(call_leg_id: Uuid, command_id: Uuid) -> String {
    format!("leg:{}:{}", call_leg_id.simple(), command_id.simple())
}

fn safe_token(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
}

fn refuse(code: ErrorCode) -> TelephonyError {
    TelephonyError::Refused(code)
}

impl OriginateCommand {
    /// Shape and proof validation at intake time `now`. Never touches the database.
    pub fn validate(&self, now: DateTime<Utc>) -> TelephonyResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(TelephonyError::Validation("unsupported schemaVersion"));
        }
        if !safe_token(&self.idempotency_key, 128) {
            return Err(TelephonyError::Validation("invalid idempotencyKey"));
        }
        if !safe_token(&self.correlation_id, 64) {
            return Err(TelephonyError::Validation("invalid correlationId"));
        }
        if !(1..=64).contains(&self.expected_generation) {
            return Err(TelephonyError::Validation(
                "expectedGeneration must be 1-64",
            ));
        }
        if !safe_token(&self.call.route_policy_version, 64)
            || !safe_token(&self.authorization.call_policy_version, 64)
        {
            return Err(TelephonyError::Validation("invalid policy version"));
        }
        if self.candidate_routes.len() > 8
            || self.candidate_routes.iter().any(|r| !safe_token(r, 64))
        {
            return Err(TelephonyError::Validation("invalid candidateRoutes"));
        }
        let c = &self.destination.country;
        if c.len() != 2 || !c.bytes().all(|b| b.is_ascii_uppercase()) {
            return Err(TelephonyError::Validation("invalid destination country"));
        }
        // Authorization evidence: complete, internally consistent, current.
        let a = &self.authorization;
        if a.context_version < 1 || a.relay_session_version < 1 {
            return Err(refuse(ErrorCode::AuthorizationProofInvalid));
        }
        if a.decided_at > now + Duration::seconds(5)
            || a.valid_until <= a.decided_at
            || a.valid_until - a.decided_at > Duration::seconds(MAX_AUTHORITY_WINDOW_SECONDS)
        {
            return Err(refuse(ErrorCode::AuthorizationProofInvalid));
        }
        if a.valid_until <= now {
            return Err(refuse(ErrorCode::AuthorizationProofExpired));
        }
        // The grant: present, plausible, not expired, not longer-lived than the decision.
        let g = &self.destination;
        if g.grant_secret.len() < 32
            || g.grant_secret.len() > 128
            || !g
                .grant_secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(refuse(ErrorCode::AuthorizationProofInvalid));
        }
        if g.grant_expires_at <= now {
            return Err(refuse(ErrorCode::AuthorizationProofExpired));
        }
        // A synthetic test interconnect carries a synthetic DCN only (Numbering decides
        // whether the number is synthetic at execution time).
        if self.interconnect_class == InterconnectClass::SyntheticTest
            && self.presented_identity.is_none()
        {
            return Err(TelephonyError::Validation(
                "a SYNTHETIC_TEST interconnect needs a presented DCN",
            ));
        }
        if let Some(p) = &self.presented_identity {
            if p.grant.is_empty()
                || p.grant.len() > 4096
                || p.grant.split('.').count() != 3
                || !p
                    .grant
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            {
                return Err(refuse(ErrorCode::AuthorizationProofInvalid));
            }
        }
        // Spend: a real reservation with a positive allowance and a duration cap.
        let s = &self.spend;
        if s.reserved_amount_minor <= 0
            || s.currency.len() != 3
            || !s.currency.bytes().all(|b| b.is_ascii_uppercase())
            || !(30..=86_400).contains(&s.max_duration_seconds)
        {
            return Err(refuse(ErrorCode::SpendReservationRequired));
        }
        Ok(())
    }

    /// Digest of the request without its secret: replays with a different body under
    /// the same idempotency key are IDEMPOTENCY_CONFLICT.
    pub fn digest(&self) -> String {
        let mut copy = self.clone();
        copy.destination.grant_secret = String::new();
        copy.destination.grant_expires_at = DateTime::<Utc>::UNIX_EPOCH;
        copy.destination.grant_id = Uuid::nil();
        if let Some(p) = copy.presented_identity.as_mut() {
            // The grant is re-issued per dispatch; the DCN itself is part of the request.
            p.grant = String::new();
        }
        hex::encode(Sha256::digest(
            serde_json::to_vec(&copy).unwrap_or_default(),
        ))
    }

    /// The bounded, secret-free payload persisted with the command.
    pub fn stored_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": self.schema_version,
            "candidateRoutes": self.candidate_routes,
            "routeClass": self.call.route_class.as_str(),
            "routeId": self.call.route_id,
            "routePolicyVersion": self.call.route_policy_version,
            "callPolicyVersion": self.authorization.call_policy_version,
            "contextVersion": self.authorization.context_version,
            "relaySessionVersion": self.authorization.relay_session_version,
            "authorityValidUntil": self.authorization.valid_until,
            "media": self.media.as_str(),
            "destinationCountry": self.destination.country,
            "presentedNumberResourceId": self.presented_identity.as_ref().map(|p| p.number_resource_id),
            "interconnectClass": self.interconnect_class.as_str(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn command(now: DateTime<Utc>) -> OriginateCommand {
        OriginateCommand {
            presented_identity: None,
            interconnect_class: InterconnectClass::Public,
            schema_version: "1".into(),
            command_id: Uuid::new_v4(),
            idempotency_key: "leg-1-attempt-1".into(),
            correlation_id: "corr_1".into(),
            expected_generation: 1,
            call: CallRef {
                call_id: Uuid::new_v4(),
                call_leg_id: Uuid::new_v4(),
                attempt_id: Uuid::new_v4(),
                route_id: Uuid::new_v4(),
                route_class: PublicRouteClass::AppToPstn,
                route_policy_version: "ROUTING_POLICY_V1.r1".into(),
            },
            scope: ScopeRef {
                tenant_id: Uuid::new_v4(),
                application_id: Uuid::new_v4(),
                environment_id: Uuid::new_v4(),
            },
            authorization: AuthorizationProof {
                call_authorization_id: Uuid::new_v4(),
                call_policy_version: "PRIVACY_RELAY_V1".into(),
                privacy_decision_id: Uuid::new_v4(),
                context_id: Uuid::new_v4(),
                context_version: 1,
                relay_session_id: Uuid::new_v4(),
                relay_session_version: 1,
                decided_at: now,
                valid_until: now + Duration::minutes(30),
            },
            destination: ProtectedDestination {
                contact_reference: Uuid::new_v4(),
                grant_id: Uuid::new_v4(),
                grant_secret: "g".repeat(43),
                grant_expires_at: now + Duration::seconds(120),
                country: "NG".into(),
            },
            spend: SpendProof {
                reservation_id: Uuid::new_v4(),
                reserved_amount_minor: 500,
                currency: "NGN".into(),
                max_duration_seconds: 3600,
            },
            media: MediaAttachment::Application,
            candidate_routes: vec!["ng-primary".into()],
        }
    }

    #[test]
    fn complete_current_proof_is_accepted() {
        let now = Utc::now();
        command(now).validate(now).unwrap();
    }

    #[test]
    fn missing_or_stale_proof_is_refused() {
        let now = Utc::now();
        let code = |c: OriginateCommand| c.validate(now).unwrap_err().code();
        let mut c = command(now);
        c.authorization.decided_at = now - Duration::minutes(10);
        c.authorization.valid_until = now - Duration::seconds(1);
        assert_eq!(code(c), ErrorCode::AuthorizationProofExpired);
        let mut c = command(now);
        c.authorization.context_version = 0;
        assert_eq!(code(c), ErrorCode::AuthorizationProofInvalid);
        let mut c = command(now);
        c.authorization.decided_at = now + Duration::minutes(5);
        assert_eq!(code(c), ErrorCode::AuthorizationProofInvalid);
        let mut c = command(now);
        c.destination.grant_secret = String::new();
        assert_eq!(code(c), ErrorCode::AuthorizationProofInvalid);
        let mut c = command(now);
        c.destination.grant_expires_at = now;
        assert_eq!(code(c), ErrorCode::AuthorizationProofExpired);
        let mut c = command(now);
        c.spend.reserved_amount_minor = 0;
        assert_eq!(code(c), ErrorCode::SpendReservationRequired);
        let mut c = command(now);
        c.spend.max_duration_seconds = 0;
        assert_eq!(code(c), ErrorCode::SpendReservationRequired);
        let mut c = command(now);
        c.schema_version = "2".into();
        assert_eq!(code(c), ErrorCode::ValidationError);
    }

    #[test]
    fn unknown_fields_and_caller_ids_are_refused_by_the_parser() {
        let now = Utc::now();
        let mut v = serde_json::to_value(command(now)).unwrap();
        v["fromNumber"] = serde_json::json!("+2348000000000");
        assert!(serde_json::from_value::<OriginateCommand>(v).is_err());
        let mut v = serde_json::to_value(command(now)).unwrap();
        v["destination"]["e164"] = serde_json::json!("+2348000000000");
        assert!(serde_json::from_value::<OriginateCommand>(v).is_err());
    }

    #[test]
    fn digests_ignore_the_grant_and_debug_never_shows_it() {
        let now = Utc::now();
        let a = command(now);
        let mut b = a.clone();
        b.destination.grant_secret = "h".repeat(43);
        assert_eq!(a.digest(), b.digest());
        assert!(!format!("{a:?}").contains(&"g".repeat(43)));
        let mut c = a.clone();
        c.destination.country = "GH".into();
        assert_ne!(a.digest(), c.digest());
        assert!(!a.stored_payload().to_string().contains("ggg"));
    }
}

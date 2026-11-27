//! Bridge error codes: the complete, published list (`signing-profile.md`). Messages
//! never echo a request value.
//!
//! HQ parses the generated "## Error codes" table (`| `CODE` | HTTP | meaning |`) and fails
//! its CI on an unmapped code: tell HQ Track B before adding a code, and say whether it
//! means "nothing was done" for a command ([`ErrorCode::nothing_done`]).

macro_rules! codes {
    ($($variant:ident => ($code:literal, $status:literal, $meaning:literal)),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorCode { $($variant),+ }
        impl ErrorCode {
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$variant),+];
            pub const fn code(self) -> &'static str { match self { $(ErrorCode::$variant => $code),+ } }
            pub const fn status(self) -> u16 { match self { $(ErrorCode::$variant => $status),+ } }
            pub const fn meaning(self) -> &'static str { match self { $(ErrorCode::$variant => $meaning),+ } }
        }
    };
}

codes! {
    BridgeDisabled => ("BRIDGE_DISABLED", 503, "ADMIN_BRIDGE_ENABLED is off"),
    BridgeUnavailable => ("BRIDGE_UNAVAILABLE", 503, "the bridge's own database is unavailable; nothing was done"),
    RequestShapeInvalid => ("REQUEST_SHAPE_INVALID", 400, "path too long, encoded or dot segments, or a malformed request line"),
    BodyTooLarge => ("BODY_TOO_LARGE", 413, "body over 32 KiB"),
    ClientCertificateRefused => ("CLIENT_CERTIFICATE_REFUSED", 401, "no client certificate, wrong URI name, or fingerprint not enrolled and live"),
    HqKeyUnknown => ("HQ_KEY_UNKNOWN", 401, "X-HQ-Key-ID is not an enrolled, live HMAC key"),
    SignatureInvalid => ("SIGNATURE_INVALID", 401, "HMAC v2 headers missing or signature mismatch"),
    TimestampSkew => ("TIMESTAMP_SKEW", 401, "X-Service-Timestamp more than 30 s from bridge time"),
    StaffAssertionInvalid => ("STAFF_ASSERTION_INVALID", 401, "staff assertion malformed, wrong header, unknown kid, bad signature, wrong iss / aud / product, or lifetime over 60 s"),
    StaffAssertionExpired => ("STAFF_ASSERTION_EXPIRED", 401, "staff assertion outside nbf / exp"),
    RequestDigestMismatch => ("REQUEST_DIGEST_MISMATCH", 401, "request_digest does not match this request"),
    Revoked => ("REVOKED", 401, "staff member or session revoked"),
    Replay => ("REPLAY", 409, "nonce or jti already used"),
    CapabilityNotFound => ("CAPABILITY_NOT_FOUND", 404, "no capability for this method and path"),
    QueryInvalid => ("QUERY_INVALID", 400, "query parameters not exactly the capability's shape, in order, with valid values"),
    PolicyUnavailable => ("POLICY_UNAVAILABLE", 503, "no verified access policy at the pinned revision"),
    PurposeNotAllowed => ("PURPOSE_NOT_ALLOWED", 403, "purpose not allowed for this capability or role"),
    PermissionDenied => ("PERMISSION_DENIED", 403, "permission missing from the assertion or not granted to any asserted role by the access policy"),
    ScopeDenied => ("SCOPE_DENIED", 403, "market, organization, zone or tenant outside the assertion scopes or the enrolled scope"),
    RecentMfaRequired => ("RECENT_MFA_REQUIRED", 403, "R2+ needs auth_time within 300 s"),
    StrongAuthRequired => ("STRONG_AUTH_REQUIRED", 403, "R3 / R4 need a strong acr"),
    ApprovalRequired => ("APPROVAL_REQUIRED", 403, "TWO_PERSON capability without an approval claim"),
    ApprovalInvalid => ("APPROVAL_INVALID", 403, "approval for another capability, requester is not sub, approver is sub, or lifetime over 43200 s"),
    ApprovalExpired => ("APPROVAL_EXPIRED", 403, "approval expires_at has passed"),
    ApprovalReused => ("APPROVAL_REUSED", 409, "approval_id already used"),
    CapabilityDisabled => ("CAPABILITY_DISABLED", 503, "capability not served in this build, its module switch is off, or writes are off"),
    IdempotencyKeyRequired => ("IDEMPOTENCY_KEY_REQUIRED", 400, "command without a valid Idempotency-Key"),
    IdempotencyConflict => ("IDEMPOTENCY_CONFLICT", 409, "Idempotency-Key reused with a different request"),
    VersionConflict => ("VERSION_CONFLICT", 409, "another operation already holds this target's expected version (in flight or succeeded); nothing was done: read back and use the current version"),
    CommandBodyInvalid => ("COMMAND_BODY_INVALID", 400, "command body is not exactly the capability's request schema"),
    RateLimited => ("RATE_LIMITED", 429, "per staff member, permission and scope limit reached"),
    AuditUnavailable => ("AUDIT_UNAVAILABLE", 503, "the audit row could not be written; nothing was done and no data is returned"),
    DatasetNotFound => ("DATASET_NOT_FOUND", 404, "unknown dataset id"),
    ResourceNotFound => ("RESOURCE_NOT_FOUND", 404, "the owning service has no such resource"),
    OwnerUnavailable => ("OWNER_UNAVAILABLE", 503, "the owning service did not answer a read"),
    OwnerRefused => ("OWNER_REFUSED", 422, "the owning service refused the command (FAILED)"),
    UnknownOutcome => ("UNKNOWN_OUTCOME", 502, "a command was sent and no clear answer came back; settle by reading back, never resend"),
    Internal => ("INTERNAL", 500, "unexpected bridge failure"),
}

impl ErrorCode {
    /// For a command: true when this code GUARANTEES the owner was never called (nothing
    /// was done; HQ may record FAILED). Every 4xx, plus the bridge-side 5xx refusals that
    /// are raised before the owner call. Any other 5xx after a command was sent is an
    /// unknown outcome, settled only by reading back. Command handlers (A3+) must never
    /// return one of these codes once the owner has been called.
    pub const fn nothing_done(self) -> bool {
        self.status() < 500
            || matches!(
                self,
                ErrorCode::BridgeDisabled
                    | ErrorCode::BridgeUnavailable
                    | ErrorCode::CapabilityDisabled
                    | ErrorCode::PolicyUnavailable
                    | ErrorCode::AuditUnavailable
            )
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    /// Agreed with HQ Track B: exactly these 5xx codes mean "nothing was done".
    #[test]
    fn the_nothing_done_5xx_codes_are_exactly_the_agreed_five() {
        let five: Vec<&str> = ErrorCode::ALL
            .iter()
            .filter(|e| e.status() >= 500 && e.nothing_done())
            .map(|e| e.code())
            .collect();
        assert_eq!(
            five,
            [
                "BRIDGE_DISABLED",
                "BRIDGE_UNAVAILABLE",
                "POLICY_UNAVAILABLE",
                "CAPABILITY_DISABLED",
                "AUDIT_UNAVAILABLE"
            ]
        );
        for e in [
            ErrorCode::UnknownOutcome,
            ErrorCode::Internal,
            ErrorCode::OwnerUnavailable,
        ] {
            assert!(!e.nothing_done(), "{}", e.code());
        }
    }
}

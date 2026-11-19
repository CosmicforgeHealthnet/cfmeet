//! Operator attribution for requests relayed by the HQ admin bridge (ADR 0068).
//!
//! The bridge calls an owning service with ITS OWN service token (`cfmeet.admin-bridge`);
//! the HQ staff token is never forwarded. Who acted travels in dedicated headers so the
//! owner can audit the person, not the service:
//!
//! * `X-CF-Operator: hq:<staff>` — the acting staff member; required on every request
//!   from the bridge;
//! * `X-CF-Approver: hq:<staff>`, `X-CF-Approval-Ref`, `X-CF-Intent-Digest` — the second
//!   person of a two-person approval (R3 / R4 actions), all three or none.
//!
//! Only `cfmeet.admin-bridge` may send these headers. From any other caller their presence
//! is an error, never silently ignored, so a misconfigured or compromised service cannot
//! impersonate HQ staff. The approver must differ from the operator. Error messages never
//! echo a header value.

use http::{HeaderMap, HeaderName};

use crate::{
    headers::{single, HeaderFault},
    identity::{services, ServiceId},
};

/// `X-CF-Operator: hq:<staff>` — the acting HQ staff member.
pub const OPERATOR: HeaderName = HeaderName::from_static("x-cf-operator");
/// `X-CF-Approver: hq:<staff>` — the approving HQ staff member (a different person).
pub const APPROVER: HeaderName = HeaderName::from_static("x-cf-approver");
/// `X-CF-Approval-Ref` — HQ's reference of the single-use two-person approval.
pub const APPROVAL_REF: HeaderName = HeaderName::from_static("x-cf-approval-ref");
/// `X-CF-Intent-Digest` — hex sha256 of the approved intent (64 lower-case hex).
pub const INTENT_DIGEST: HeaderName = HeaderName::from_static("x-cf-intent-digest");

/// Every operator-attribution header, in a fixed order.
pub const ALL_HEADERS: [HeaderName; 4] = [OPERATOR, APPROVER, APPROVAL_REF, INTENT_DIGEST];

/// Prefix of every staff reference.
pub const STAFF_REF_PREFIX: &str = "hq:";
const STAFF_ID_MAX: usize = 128;
const APPROVAL_REF_MAX: usize = 128;

/// Why operator attribution was refused. Messages name the header, never its value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OperatorAttributionError {
    /// Attribution headers arrived from a caller other than `cfmeet.admin-bridge`.
    #[error("operator attribution headers are accepted only from cfmeet.admin-bridge")]
    ForeignCaller,
    /// The admin bridge sent no `X-CF-Operator`.
    #[error("X-CF-Operator is required")]
    MissingOperator,
    #[error("{header} is sent more than once")]
    Duplicate { header: &'static str },
    #[error("{header} is malformed")]
    Malformed { header: &'static str },
    /// Some, but not all, of `X-CF-Approver`, `X-CF-Approval-Ref`, `X-CF-Intent-Digest`.
    #[error("X-CF-Approver, X-CF-Approval-Ref and X-CF-Intent-Digest must be sent together")]
    IncompleteApproval,
    /// The approver is the operator.
    #[error("the approver must be a different person than the operator")]
    SelfApproval,
    /// The action needs a second person and none was supplied.
    #[error("this action requires a second-person approval (X-CF-Approver)")]
    SecondPersonRequired,
}

/// A validated `hq:<staff>` reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StaffRef(String);

impl StaffRef {
    /// `hq:` followed by 1–128 of `[A-Za-z0-9._:@-]`.
    pub fn parse(value: &str) -> Option<Self> {
        let id = value.strip_prefix(STAFF_REF_PREFIX)?;
        (!id.is_empty()
            && id.len() <= STAFF_ID_MAX
            && id.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'-')
            }))
        .then(|| Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for StaffRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The second person of a two-person approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub approver: StaffRef,
    /// 1–128 of `[A-Za-z0-9._:-]`.
    pub approval_ref: String,
    /// 64 lower-case hex characters.
    pub intent_digest: String,
}

/// Who acted on a request relayed by the admin bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorAttribution {
    pub operator: StaffRef,
    pub approval: Option<Approval>,
}

impl OperatorAttribution {
    /// Reads the attribution of a request whose service token was ALREADY verified as
    /// `caller`.
    ///
    /// * caller `cfmeet.admin-bridge`: `Ok(Some(_))`; `X-CF-Operator` is required and the
    ///   approval headers are all-or-nothing with a different approver;
    /// * any other caller: `Ok(None)` without attribution headers, and
    ///   [`OperatorAttributionError::ForeignCaller`] if any is present.
    pub fn from_headers(
        caller: &ServiceId,
        headers: &HeaderMap,
    ) -> Result<Option<Self>, OperatorAttributionError> {
        if caller.as_str() != services::ADMIN_BRIDGE {
            return if ALL_HEADERS.iter().any(|h| headers.contains_key(h)) {
                Err(OperatorAttributionError::ForeignCaller)
            } else {
                Ok(None)
            };
        }
        let operator = optional(headers, &OPERATOR, "X-CF-Operator")?
            .ok_or(OperatorAttributionError::MissingOperator)?;
        let operator = StaffRef::parse(operator).ok_or(OperatorAttributionError::Malformed {
            header: "X-CF-Operator",
        })?;
        let approver = optional(headers, &APPROVER, "X-CF-Approver")?;
        let approval_ref = optional(headers, &APPROVAL_REF, "X-CF-Approval-Ref")?;
        let intent_digest = optional(headers, &INTENT_DIGEST, "X-CF-Intent-Digest")?;
        let approval = match (approver, approval_ref, intent_digest) {
            (None, None, None) => None,
            (Some(approver), Some(approval_ref), Some(intent_digest)) => {
                let approver =
                    StaffRef::parse(approver).ok_or(OperatorAttributionError::Malformed {
                        header: "X-CF-Approver",
                    })?;
                if !valid_approval_ref(approval_ref) {
                    return Err(OperatorAttributionError::Malformed {
                        header: "X-CF-Approval-Ref",
                    });
                }
                if !valid_digest(intent_digest) {
                    return Err(OperatorAttributionError::Malformed {
                        header: "X-CF-Intent-Digest",
                    });
                }
                if approver == operator {
                    return Err(OperatorAttributionError::SelfApproval);
                }
                Some(Approval {
                    approver,
                    approval_ref: approval_ref.to_string(),
                    intent_digest: intent_digest.to_string(),
                })
            }
            _ => return Err(OperatorAttributionError::IncompleteApproval),
        };
        Ok(Some(Self { operator, approval }))
    }

    /// The approval of an R3 / R4 action, or [`OperatorAttributionError::SecondPersonRequired`].
    pub fn require_second_person(&self) -> Result<&Approval, OperatorAttributionError> {
        self.approval
            .as_ref()
            .ok_or(OperatorAttributionError::SecondPersonRequired)
    }
}

fn optional<'a>(
    headers: &'a HeaderMap,
    name: &HeaderName,
    label: &'static str,
) -> Result<Option<&'a str>, OperatorAttributionError> {
    match single(headers, name) {
        Ok(v) => Ok(Some(v)),
        Err(HeaderFault::Missing) => Ok(None),
        Err(HeaderFault::Duplicate) => Err(OperatorAttributionError::Duplicate { header: label }),
        Err(HeaderFault::Malformed) => Err(OperatorAttributionError::Malformed { header: label }),
    }
}

fn valid_approval_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= APPROVAL_REF_MAX
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn headers(pairs: &[(&HeaderName, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in pairs {
            h.append((*name).clone(), HeaderValue::from_str(value).unwrap());
        }
        h
    }

    fn bridge() -> ServiceId {
        ServiceId::admin_bridge()
    }

    fn approved(approver: &str) -> HeaderMap {
        headers(&[
            (&OPERATOR, "hq:alice"),
            (&APPROVER, approver),
            (&APPROVAL_REF, "apr_01:x-y.z"),
            (&INTENT_DIGEST, DIGEST),
        ])
    }

    #[test]
    fn operator_only_from_the_bridge() {
        let a = OperatorAttribution::from_headers(&bridge(), &headers(&[(&OPERATOR, "hq:alice")]))
            .unwrap()
            .unwrap();
        assert_eq!(a.operator.as_str(), "hq:alice");
        assert!(a.approval.is_none());
        assert_eq!(
            a.require_second_person(),
            Err(OperatorAttributionError::SecondPersonRequired)
        );
    }

    #[test]
    fn full_approval_is_accepted() {
        let a = OperatorAttribution::from_headers(&bridge(), &approved("hq:bob@corp"))
            .unwrap()
            .unwrap();
        let approval = a.require_second_person().unwrap();
        assert_eq!(approval.approver.as_str(), "hq:bob@corp");
        assert_eq!(approval.approval_ref, "apr_01:x-y.z");
        assert_eq!(approval.intent_digest, DIGEST);
    }

    #[test]
    fn self_approval_is_refused() {
        assert_eq!(
            OperatorAttribution::from_headers(&bridge(), &approved("hq:alice")),
            Err(OperatorAttributionError::SelfApproval)
        );
    }

    #[test]
    fn bridge_must_name_the_operator() {
        assert_eq!(
            OperatorAttribution::from_headers(&bridge(), &HeaderMap::new()),
            Err(OperatorAttributionError::MissingOperator)
        );
        let mut h = approved("hq:bob");
        h.remove(&OPERATOR);
        assert_eq!(
            OperatorAttribution::from_headers(&bridge(), &h),
            Err(OperatorAttributionError::MissingOperator)
        );
    }

    #[test]
    fn other_callers_without_headers_have_no_attribution() {
        for caller in [ServiceId::operator_console(), ServiceId::meeting_service()] {
            assert_eq!(
                OperatorAttribution::from_headers(&caller, &HeaderMap::new()),
                Ok(None)
            );
        }
    }

    #[test]
    fn attribution_headers_from_other_callers_are_refused() {
        for header in ALL_HEADERS.iter() {
            let value = if *header == INTENT_DIGEST {
                DIGEST
            } else {
                "hq:alice"
            };
            for caller in [ServiceId::operator_console(), ServiceId::meeting_service()] {
                assert_eq!(
                    OperatorAttribution::from_headers(&caller, &headers(&[(header, value)])),
                    Err(OperatorAttributionError::ForeignCaller),
                    "{header}"
                );
            }
        }
    }

    #[test]
    fn approval_headers_are_all_or_nothing() {
        for missing in [&APPROVER, &APPROVAL_REF, &INTENT_DIGEST] {
            let mut h = approved("hq:bob");
            h.remove(missing);
            assert_eq!(
                OperatorAttribution::from_headers(&bridge(), &h),
                Err(OperatorAttributionError::IncompleteApproval),
                "{missing}"
            );
        }
        let h = headers(&[(&OPERATOR, "hq:alice"), (&APPROVER, "hq:bob")]);
        assert_eq!(
            OperatorAttribution::from_headers(&bridge(), &h),
            Err(OperatorAttributionError::IncompleteApproval)
        );
    }

    #[test]
    fn staff_refs_are_validated() {
        let long = format!("hq:{}", "a".repeat(129));
        for bad in [
            "alice",
            "hq:",
            "HQ:alice",
            "hq:al ice",
            "hq:al/ice",
            "hq:alice\u{e9}",
            " hq:alice",
            long.as_str(),
        ] {
            assert!(StaffRef::parse(bad).is_none(), "{bad:?}");
        }
        let max = format!("hq:{}", "a".repeat(128));
        for good in ["hq:a", "hq:A.b_c:d@e-f", "hq:123", max.as_str()] {
            assert!(StaffRef::parse(good).is_some(), "{good:?}");
        }
    }

    #[test]
    fn malformed_operator_and_approver_are_refused() {
        assert_eq!(
            OperatorAttribution::from_headers(&bridge(), &headers(&[(&OPERATOR, "alice")])),
            Err(OperatorAttributionError::Malformed {
                header: "X-CF-Operator"
            })
        );
        assert_eq!(
            OperatorAttribution::from_headers(&bridge(), &approved("bob")),
            Err(OperatorAttributionError::Malformed {
                header: "X-CF-Approver"
            })
        );
    }

    #[test]
    fn approval_ref_is_validated() {
        let long = "a".repeat(129);
        for bad in ["", "a b", "a/b", "a@b", long.as_str()] {
            let mut h = approved("hq:bob");
            h.insert(APPROVAL_REF, HeaderValue::from_str(bad).unwrap());
            assert_eq!(
                OperatorAttribution::from_headers(&bridge(), &h),
                Err(OperatorAttributionError::Malformed {
                    header: "X-CF-Approval-Ref"
                }),
                "{bad:?}"
            );
        }
        let mut h = approved("hq:bob");
        h.insert(
            APPROVAL_REF,
            HeaderValue::from_str(&"a".repeat(128)).unwrap(),
        );
        assert!(OperatorAttribution::from_headers(&bridge(), &h).is_ok());
    }

    #[test]
    fn intent_digest_is_64_lower_hex() {
        let upper = DIGEST.to_ascii_uppercase();
        let short = &DIGEST[..63];
        let long = format!("{DIGEST}0");
        let non_hex = format!("{}g", &DIGEST[..63]);
        for bad in [upper.as_str(), short, long.as_str(), non_hex.as_str()] {
            let mut h = approved("hq:bob");
            h.insert(INTENT_DIGEST, HeaderValue::from_str(bad).unwrap());
            assert_eq!(
                OperatorAttribution::from_headers(&bridge(), &h),
                Err(OperatorAttributionError::Malformed {
                    header: "X-CF-Intent-Digest"
                }),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn duplicate_headers_are_refused() {
        for header in ALL_HEADERS.iter() {
            let mut h = approved("hq:bob");
            let value = h.get(header).unwrap().clone();
            h.append(header.clone(), value);
            assert!(
                matches!(
                    OperatorAttribution::from_headers(&bridge(), &h),
                    Err(OperatorAttributionError::Duplicate { .. })
                ),
                "{header}"
            );
        }
    }

    #[test]
    fn non_ascii_header_values_are_malformed() {
        let mut h = HeaderMap::new();
        h.insert(OPERATOR, HeaderValue::from_bytes(b"hq:\xffalice").unwrap());
        assert_eq!(
            OperatorAttribution::from_headers(&bridge(), &h),
            Err(OperatorAttributionError::Malformed {
                header: "X-CF-Operator"
            })
        );
    }

    #[test]
    fn errors_never_echo_values() {
        let secret = "hq:secret-staff-id";
        let err = OperatorAttribution::from_headers(
            &bridge(),
            &headers(&[
                (&OPERATOR, secret),
                (&APPROVER, secret),
                (&APPROVAL_REF, "r"),
                (&INTENT_DIGEST, DIGEST),
            ]),
        )
        .unwrap_err();
        assert!(!err.to_string().contains("secret"));
        let err = OperatorAttribution::from_headers(
            &ServiceId::operator_console(),
            &headers(&[(&OPERATOR, secret)]),
        )
        .unwrap_err();
        assert!(!err.to_string().contains("secret"));
    }
}

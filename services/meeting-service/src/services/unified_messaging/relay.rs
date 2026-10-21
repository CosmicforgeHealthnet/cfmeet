//! Part V H §104 — the CF Meet web / mobile client's Messaging API, relayed by Meeting.
//!
//! Messaging's customer API is for application BACKENDS (an API credential plus the
//! principal they act for). CF Meet's own clients therefore reach it through Meeting:
//! Meeting verifies the CF Meet login, resolves the account's `NATIVE_CF_USER`
//! principal and forwards the request to Messaging's delegated surface with that
//! principal SIGNED into its service token. The client never holds a credential and can
//! never act for anyone else: the acting principal is not taken from the request.
//!
//! Only the end-user operations in [`ROUTES`] are relayed: an explicit allowlist of
//! method, path shape and query keys. Administration (policy, legal holds, exports,
//! audit, moderation, SMS bridges) is never reachable this way, and Messaging re-checks
//! every operation against the acting principal's membership and role anyway.
//!
//! The directory is the CF Meet boundary for "who may I start a conversation with":
//! active members of the organizations the caller belongs to (verified memberships), so
//! the relay never becomes an account-enumeration oracle.

use axum::http::Method;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{all, chat_error, Acting, Peer, PrincipalKind, UnifiedMessaging};
use crate::error::{ApiError, ApiResult, ErrorCode};

/// One path segment of a relayed route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seg {
    Lit(&'static str),
    Id,
}

use Seg::{Id, Lit};

/// One relayed operation: method, path shape (under `/v1/`), accepted query keys and
/// whether the client's `Idempotency-Key` is forwarded.
#[derive(Debug)]
pub struct RelayRoute {
    method: Method,
    path: &'static [Seg],
    query: &'static [&'static str],
    idempotency_key: bool,
}

const fn r(
    method: Method,
    path: &'static [Seg],
    query: &'static [&'static str],
    idempotency_key: bool,
) -> RelayRoute {
    RelayRoute {
        method,
        path,
        query,
        idempotency_key,
    }
}

const PAGE: &[&str] = &["limit", "cursor"];
const MESSAGES: &[&str] = &["afterSequence", "beforeSequence", "limit"];

/// The end-user operations of `cfmeet.messaging.http-api` the CF Meet client may use.
pub static ROUTES: &[RelayRoute] = &[
    // Conversations, membership, messages, read state.
    r(Method::GET, &[Lit("conversations")], PAGE, false),
    r(Method::POST, &[Lit("conversations")], &[], true),
    r(Method::GET, &[Lit("conversations"), Id], &[], false),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("members")],
        PAGE,
        false,
    ),
    r(
        Method::POST,
        &[Lit("conversations"), Id, Lit("members")],
        &[],
        false,
    ),
    r(
        Method::DELETE,
        &[Lit("conversations"), Id, Lit("members"), Id],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("messages")],
        MESSAGES,
        false,
    ),
    r(
        Method::POST,
        &[Lit("conversations"), Id, Lit("messages")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("messages"), Id],
        &[],
        false,
    ),
    r(
        Method::DELETE,
        &[Lit("conversations"), Id, Lit("messages"), Id],
        &[],
        false,
    ),
    r(
        Method::PUT,
        &[Lit("conversations"), Id, Lit("read-cursor")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("unread")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("events")],
        &["afterEventSequence", "limit"],
        false,
    ),
    // The MLS Delivery Service (ciphertext, commits, Welcomes, KeyPackage claims).
    r(
        Method::POST,
        &[Lit("conversations"), Id, Lit("mls"), Lit("commits")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("mls"), Lit("state")],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[
            Lit("conversations"),
            Id,
            Lit("mls"),
            Lit("key-package-claims"),
        ],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("devices"), Id, Lit("welcomes")],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[
            Lit("messaging"),
            Lit("devices"),
            Id,
            Lit("welcomes"),
            Id,
            Lit("consume"),
        ],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("principals"), Id, Lit("devices")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("key-transparency"), Lit("proofs")],
        &["deviceId", "fromTreeSize"],
        false,
    ),
    // Attachments (MANAGED: scanned on the server; PRIVATE_E2EE: ciphertext only).
    r(
        Method::POST,
        &[Lit("conversations"), Id, Lit("attachments")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("attachments"), Id],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[
            Lit("conversations"),
            Id,
            Lit("attachments"),
            Id,
            Lit("finalize"),
        ],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[
            Lit("conversations"),
            Id,
            Lit("attachments"),
            Id,
            Lit("download-grants"),
        ],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[
            Lit("conversations"),
            Id,
            Lit("attachments"),
            Id,
            Lit("thumbnail-grants"),
        ],
        &[],
        false,
    ),
    // Device-to-device history transfer (opaque sealed bytes only).
    r(
        Method::POST,
        &[Lit("messaging"), Lit("history-transfers")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("history-transfers")],
        &["deviceId"],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("history-transfers"), Id],
        &[],
        false,
    ),
    r(
        Method::PUT,
        &[
            Lit("messaging"),
            Lit("history-transfers"),
            Id,
            Lit("sealed-history"),
        ],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[
            Lit("messaging"),
            Lit("history-transfers"),
            Id,
            Lit("sealed-history"),
        ],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[
            Lit("messaging"),
            Lit("history-transfers"),
            Id,
            Lit("complete"),
        ],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[
            Lit("messaging"),
            Lit("history-transfers"),
            Id,
            Lit("cancel"),
        ],
        &[],
        false,
    ),
];

/// Matches a client path (relative, e.g. `v1/conversations/<id>/messages`) against the
/// allowlist. Every `Id` segment must be a canonical UUID.
pub fn route_for(method: &Method, path: &str) -> Option<&'static RelayRoute> {
    let rest = path.strip_prefix("v1/")?;
    let segs: Vec<&str> = rest.split('/').collect();
    ROUTES.iter().find(|r| {
        r.method == *method
            && r.path.len() == segs.len()
            && r.path.iter().zip(&segs).all(|(p, s)| match p {
                Lit(l) => l == s,
                Id => Uuid::parse_str(s).is_ok_and(|u| u.hyphenated().to_string() == *s),
            })
    })
}

/// A relayed request (the handler parsed it; nothing here is trusted for identity).
pub struct RelayRequest<'a> {
    pub method: Method,
    pub path: &'a str,
    pub query: Vec<(String, String)>,
    pub idempotency_key: Option<String>,
    pub body: Option<Value>,
}

impl UnifiedMessaging {
    /// Relays one allowlisted operation for a VERIFIED CF Meet account and returns
    /// Messaging's status and body unchanged (its typed error envelope included).
    pub async fn relay(&self, user_id: Uuid, req: RelayRequest<'_>) -> ApiResult<(u16, Value)> {
        let Some(route) = route_for(&req.method, req.path) else {
            return Err(ApiError::domain(
                ErrorCode::EndpointNotFound,
                "This Messaging operation is not available to CF Meet clients",
            ));
        };
        for (k, _) in &req.query {
            if !route.query.contains(&k.as_str()) {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "Unsupported query parameter",
                ));
            }
        }
        let principal = self.principal_for_user(user_id).await?;
        let acting = Acting {
            principal_id: principal,
            kind: PrincipalKind::NativeCfUser,
        };
        let query: Vec<(&str, &str)> = req
            .query
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let headers: Vec<(&str, &str)> = match (&req.idempotency_key, route.idempotency_key) {
            (Some(k), true) => vec![("idempotency-key", k.as_str())],
            _ => vec![],
        };
        let body = match (&req.method, req.body) {
            (&Method::GET | &Method::DELETE, _) => None,
            (_, Some(b)) => Some(b),
            (_, None) => Some(json!({})),
        };
        self.gateway
            .call_raw_with_headers(
                Peer::Messaging,
                Some(acting),
                req.method,
                &format!("/internal/v1/delegated/{}", req.path),
                &query,
                &headers,
                body.as_ref(),
            )
            .await
            .map_err(|e| chat_error(&e))
    }

    /// The caller's own Messaging identity: principal and the scope ids every client
    /// encryption binds (tenant, application, environment). Identifiers, not secrets.
    pub async fn me(&self, user_id: Uuid) -> ApiResult<Value> {
        let principal = self.principal_for_user(user_id).await?;
        let (tenant, application, environment) = self.gateway.scope_ids().ok_or_else(|| {
            ApiError::domain(
                ErrorCode::ChatUnavailable,
                "Messaging scope is not configured",
            )
        })?;
        Ok(json!({
            "principalId": principal,
            "tenantId": tenant,
            "applicationId": application,
            "environmentId": environment,
        }))
    }

    /// Colleagues the caller may start a conversation with: ACTIVE members of the
    /// caller's verified organizations (at most 200; optional name filter).
    pub async fn directory(
        &self,
        user_id: Uuid,
        organizations: &[Uuid],
        search: Option<&str>,
    ) -> ApiResult<Value> {
        if organizations.is_empty() {
            return Ok(json!({ "people": [] }));
        }
        let pattern = search.map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| {
            let escaped: String = s
                .chars()
                .take(64)
                .flat_map(|c| match c {
                    '%' | '_' | '\\' => vec!['\\', c],
                    c => vec![c],
                })
                .collect();
            format!("%{escaped}%")
        });
        let rows = all(
            &self.db,
            "SELECT DISTINCT u.id, u.username, l.principal_id FROM organization_memberships m \
               JOIN users u ON u.id = m.user_id \
               LEFT JOIN communication_principal_links l ON l.subject_kind = 'USER' AND l.subject_id = u.id \
                 AND l.environment_id = $3 \
              WHERE m.organization_id = ANY($1) AND m.status = 'ACTIVE' AND u.id <> $2 \
                AND ($4::text IS NULL OR u.username ILIKE $4) \
              ORDER BY u.username LIMIT 200",
            vec![
                organizations.to_vec().into(),
                user_id.into(),
                self.environment_id().into(),
                pattern.into(),
            ],
        )
        .await?;
        let people: Vec<Value> = rows
            .iter()
            .map(|r| -> ApiResult<Value> {
                let id: Uuid = r.try_get_by_index(0)?;
                let name: String = r.try_get_by_index(1)?;
                let principal: Option<Uuid> = r.try_get_by_index(2)?;
                Ok(json!({ "userId": id, "displayName": name, "principalId": principal }))
            })
            .collect::<ApiResult<_>>()?;
        Ok(json!({ "people": people }))
    }

    /// The principals of directory colleagues (created on first use), so the client can
    /// add them to a conversation. A user outside the caller's organizations is refused
    /// exactly like an unknown one.
    pub async fn resolve_colleagues(
        &self,
        user_id: Uuid,
        organizations: &[Uuid],
        user_ids: &[Uuid],
    ) -> ApiResult<Value> {
        if user_ids.is_empty() || user_ids.len() > 100 {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Between 1 and 100 people",
            ));
        }
        let allowed: Vec<Uuid> = if organizations.is_empty() {
            vec![]
        } else {
            all(
                &self.db,
                "SELECT DISTINCT m.user_id FROM organization_memberships m \
                 WHERE m.organization_id = ANY($1) AND m.status = 'ACTIVE' AND m.user_id = ANY($2)",
                vec![organizations.to_vec().into(), user_ids.to_vec().into()],
            )
            .await?
            .iter()
            .map(|r| r.try_get_by_index::<Uuid>(0))
            .collect::<Result<_, _>>()?
        };
        let mut out = Vec::new();
        for u in user_ids {
            if *u == user_id || !allowed.contains(u) {
                return Err(ApiError::NotFound(
                    "No such colleague in your organizations".into(),
                ));
            }
            out.push(json!({ "userId": u, "principalId": self.principal_for_user(*u).await? }));
        }
        Ok(json!({ "people": out }))
    }

    /// Display names of principals the caller can see: the caller and colleagues of its
    /// verified organizations (others are simply not named).
    pub async fn name_principals(
        &self,
        user_id: Uuid,
        organizations: &[Uuid],
        principal_ids: &[Uuid],
    ) -> ApiResult<Value> {
        if principal_ids.len() > 256 {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "At most 256 principals",
            ));
        }
        let rows = all(
            &self.db,
            "SELECT DISTINCT l.principal_id, u.username FROM communication_principal_links l \
               JOIN users u ON u.id = l.subject_id \
              WHERE l.subject_kind = 'USER' AND l.environment_id = $1 AND l.principal_id = ANY($2) \
                AND (u.id = $3 OR EXISTS (SELECT 1 FROM organization_memberships m \
                     WHERE m.user_id = u.id AND m.status = 'ACTIVE' AND m.organization_id = ANY($4)))",
            vec![
                self.environment_id().into(),
                principal_ids.to_vec().into(),
                user_id.into(),
                organizations.to_vec().into(),
            ],
        )
        .await?;
        let names: Vec<Value> = rows
            .iter()
            .map(|r| -> ApiResult<Value> {
                let p: Uuid = r.try_get_by_index(0)?;
                let n: String = r.try_get_by_index(1)?;
                Ok(json!({ "principalId": p, "displayName": n }))
            })
            .collect::<ApiResult<_>>()?;
        Ok(json!({ "principals": names }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: &str = "6f1d2c3b-4a59-4e68-9f70-8a91b2c3d4e5";

    #[test]
    fn only_end_user_operations_are_relayed() {
        for (m, p) in [
            (Method::GET, "v1/conversations".to_string()),
            (Method::POST, format!("v1/conversations/{C}/messages")),
            (Method::POST, format!("v1/conversations/{C}/mls/commits")),
            (Method::GET, format!("v1/messaging/devices/{C}/welcomes")),
            (
                Method::PUT,
                format!("v1/messaging/history-transfers/{C}/sealed-history"),
            ),
        ] {
            assert!(route_for(&m, &p).is_some(), "{m} {p}");
        }
        for (m, p) in [
            // Administration and compliance are never relayed.
            (Method::PUT, "v1/messaging/policy".to_string()),
            (Method::GET, "v1/messaging/policy".to_string()),
            (Method::POST, "v1/messaging/legal-holds".to_string()),
            (Method::POST, "v1/messaging/exports".to_string()),
            (Method::GET, "v1/messaging/audit".to_string()),
            (Method::POST, "v1/messaging/moderation/actions".to_string()),
            (Method::POST, format!("v1/conversations/{C}/sms-bridges")),
            (Method::POST, format!("v1/conversations/{C}/bans")),
            (
                Method::GET,
                format!("v1/conversations/{C}/attachments/{C}/content"),
            ),
            // Wrong method, shape, or a non-canonical id.
            (Method::DELETE, format!("v1/conversations/{C}")),
            (Method::GET, format!("v1/conversations/{C}/messages/")),
            (
                Method::GET,
                format!("v1/conversations/{}/messages", C.to_uppercase()),
            ),
            (Method::GET, format!("v1/conversations/{C}/../policy")),
            (Method::GET, "v1/conversations/./x".to_string()),
            (Method::GET, "v1/conversations/not-a-uuid".to_string()),
            (Method::GET, "internal/v1/anything".to_string()),
            (Method::GET, "v2/conversations".to_string()),
        ] {
            assert!(route_for(&m, &p).is_none(), "{m} {p}");
        }
    }
}

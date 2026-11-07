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

/// Who counts as the caller's colleague.
///
/// * `Organizations` (legacy / shadow): ACTIVE members, in the local membership table, of
///   the caller's verified organizations.
/// * `Users` (Organizations authoritative): the ACTIVE, non-GUEST members Organizations
///   listed for the caller's verified organizations (the local membership table is no
///   longer maintained after cutover, so it is never read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColleagueScope {
    Organizations(Vec<Uuid>),
    Users(Vec<Uuid>),
}

impl ColleagueScope {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Organizations(v) | Self::Users(v) => v.is_empty(),
        }
    }
}

/// One path segment of a relayed route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seg {
    Lit(&'static str),
    Id,
    Reaction,
}

use Seg::{Id, Lit, Reaction};

/// One relayed operation: method, path shape (under `/v1/`), accepted query keys and
/// whether the client's `Idempotency-Key` is forwarded.
#[derive(Debug)]
pub struct RelayRoute {
    method: Method,
    path: &'static [Seg],
    query: &'static [&'static str],
    idempotency_key: bool,
}

impl RelayRoute {
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// The Messaging customer path template (`/v1/...`, `{id}` / `{reaction}` holes).
    pub fn template(&self) -> String {
        let mut out = String::from("/v1");
        for seg in self.path {
            out.push('/');
            match seg {
                Lit(l) => out.push_str(l),
                Id => out.push_str("{id}"),
                Reaction => out.push_str("{reaction}"),
            }
        }
        out
    }

    /// The canonical (Messaging contract) query keys forwarded for this operation.
    pub fn query_keys(&self) -> &'static [&'static str] {
        self.query
    }

    /// Rewrites accepted compatibility spellings to the canonical key and refuses any
    /// key the operation does not take (and a canonical key given twice by spelling).
    pub fn canonical_query(
        &self,
        query: Vec<(String, String)>,
    ) -> ApiResult<Vec<(String, String)>> {
        let mut out: Vec<(String, String)> = Vec::with_capacity(query.len());
        for (k, v) in query {
            let key = QUERY_ALIASES
                .iter()
                .find(|(alias, canonical)| *alias == k && self.query.contains(canonical))
                .map(|(_, canonical)| canonical.to_string())
                .unwrap_or(k);
            if !self.query.contains(&key.as_str()) {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "Unsupported query parameter",
                ));
            }
            if out.iter().any(|(seen, _)| *seen == key) {
                return Err(ApiError::domain(
                    ErrorCode::ValidationError,
                    "A query parameter was given more than once",
                ));
            }
            out.push((key, v));
        }
        Ok(out)
    }
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
/// `GET v1/conversations`: the Messaging contract's filters, keyset page and ordering.
const CONVERSATIONS: &[&str] = &[
    "lifecycleState",
    "contextClass",
    "limit",
    "cursor",
    "orderBy",
];
const MESSAGES: &[&str] = &["afterSequence", "beforeSequence", "limit", "threadRootId"];
/// `GET v1/conversations/{id}/events`: Messaging's canonical replay cursor is `after`.
const EVENTS: &[&str] = &["after", "limit"];

/// Compatibility spellings the relay accepts and rewrites to Messaging's canonical query
/// key before forwarding (MSG-001: early web builds sent the WebSocket frame's
/// `afterEventSequence` on the REST replay). Only the canonical key is ever forwarded.
pub const QUERY_ALIASES: &[(&str, &str)] = &[("afterEventSequence", "after")];

/// Every HTTP method the allowlist uses; `routes::v2` mounts exactly these on
/// `/api/v2/messaging/relay/*path` (a unit test keeps the two in step).
pub const RELAY_METHODS: &[Method] = &[
    Method::GET,
    Method::POST,
    Method::PUT,
    Method::PATCH,
    Method::DELETE,
];

/// The end-user operations of `cfmeet.messaging.http-api` the CF Meet client may use.
pub static ROUTES: &[RelayRoute] = &[
    // Conversations, membership, messages, read state.
    r(Method::GET, &[Lit("conversations")], CONVERSATIONS, false),
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
        &["reason"],
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
        &["scope"],
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
        EVENTS,
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
    // Section 8: existing end-user domain operations; authority stays in Messaging.
    r(Method::PATCH, &[Lit("conversations"), Id], &[], false),
    r(
        Method::PATCH,
        &[Lit("conversations"), Id, Lit("members"), Id],
        &[],
        false,
    ),
    r(
        Method::PATCH,
        &[Lit("conversations"), Id, Lit("messages"), Id],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[
            Lit("conversations"),
            Id,
            Lit("messages"),
            Id,
            Lit("revisions"),
        ],
        &[],
        false,
    ),
    r(Method::GET, &[Lit("messaging"), Lit("blocks")], &[], false),
    r(Method::POST, &[Lit("messaging"), Lit("blocks")], &[], false),
    r(
        Method::DELETE,
        &[Lit("messaging"), Lit("blocks"), Id],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[Lit("messaging"), Lit("client-sessions"), Id, Lit("revoke")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("presence"), Lit("settings")],
        &[],
        false,
    ),
    r(
        Method::PUT,
        &[Lit("messaging"), Lit("presence"), Lit("settings")],
        &[],
        false,
    ),
    r(
        Method::PUT,
        &[
            Lit("messaging"),
            Lit("devices"),
            Id,
            Lit("push-registration"),
        ],
        &[],
        false,
    ),
    r(
        Method::DELETE,
        &[
            Lit("messaging"),
            Lit("devices"),
            Id,
            Lit("push-registration"),
        ],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("search")],
        &["q", "limit", "beforeSequence"],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("search")],
        &["q", "limit"],
        false,
    ),
    r(
        Method::POST,
        &[Lit("conversations"), Id, Lit("reports")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("bans")],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[Lit("conversations"), Id, Lit("bans")],
        &[],
        false,
    ),
    r(
        Method::DELETE,
        &[Lit("conversations"), Id, Lit("bans"), Id],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[
            Lit("conversations"),
            Id,
            Lit("messages"),
            Id,
            Lit("reactions"),
        ],
        &[],
        false,
    ),
    r(
        Method::PUT,
        &[
            Lit("conversations"),
            Id,
            Lit("messages"),
            Id,
            Lit("reactions"),
            Reaction,
        ],
        &[],
        false,
    ),
    r(
        Method::DELETE,
        &[
            Lit("conversations"),
            Id,
            Lit("messages"),
            Id,
            Lit("reactions"),
            Reaction,
        ],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("pins")],
        &[],
        false,
    ),
    r(
        Method::PUT,
        &[Lit("conversations"), Id, Lit("pins"), Id],
        &[],
        false,
    ),
    r(
        Method::DELETE,
        &[Lit("conversations"), Id, Lit("pins"), Id],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[Lit("messaging"), Lit("backups")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("backups"), Lit("latest")],
        &[],
        false,
    ),
    r(
        Method::DELETE,
        &[Lit("messaging"), Lit("backups"), Id],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[Lit("messaging"), Lit("backups"), Id, Lit("finalize")],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[Lit("messaging"), Lit("backups"), Id, Lit("download-grants")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("conversations"), Id, Lit("messages"), Id, Lit("thread")],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[Lit("conversations"), Id, Lit("invitation"), Lit("accept")],
        &[],
        false,
    ),
    r(
        Method::POST,
        &[Lit("conversations"), Id, Lit("invitation"), Lit("decline")],
        &[],
        false,
    ),
    r(
        Method::GET,
        &[Lit("messaging"), Lit("invitations")],
        &[],
        false,
    ),
];

// Bound the path segment independently of the domain reaction validation. Reject
// decoded delimiters, traversal, control characters and double-encoded escapes.
fn safe_reaction_segment(raw: &str) -> bool {
    if raw.len() > 192 {
        return false;
    }
    let Ok(value) = urlencoding::decode(raw) else {
        return false;
    };
    !value.is_empty()
        && value.len() <= 64
        && !value.chars().any(|c| {
            c.is_control() || c.is_whitespace() || matches!(c, '/' | '\\' | '?' | '#' | '%' | '.')
        })
}

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
                Reaction => safe_reaction_segment(s),
            })
    })
}

/// Documentation of the existing native transport, derived from the real allowlist.
/// The canonical resource schema stays Messaging-owned; native clients authenticate
/// through their Account session and never receive a service credential.
pub fn native_public_path(method: &str, template: &str) -> Option<String> {
    let direct = match (method, template) {
        ("post", "/v1/messaging/devices/registration-challenges") => {
            Some("/api/v2/messaging/devices/registration-challenges")
        }
        ("post", "/v1/messaging/devices") => Some("/api/v2/messaging/devices"),
        ("post", "/v1/messaging/devices/{deviceId}/key-packages") => {
            Some("/api/v2/messaging/devices/{deviceId}/key-packages")
        }
        ("post", "/v1/messaging/devices/{deviceId}/revoke") => {
            Some("/api/v2/messaging/devices/{deviceId}/revoke")
        }
        ("post", "/v1/messaging/client-tokens") => Some("/api/v2/messaging/client-tokens"),
        ("post", "/v1/messaging/client-sessions/revoke-all") => {
            Some("/api/v2/messaging/client-sessions/revoke-all")
        }
        _ => None,
    };
    if let Some(path) = direct {
        return Some(path.into());
    }
    let path = template
        .trim_start_matches('/')
        .split('/')
        .map(|part| {
            if part == "{reaction}" {
                ":example:"
            } else if part.starts_with('{') && part.ends_with('}') {
                "00000000-0000-0000-0000-000000000001"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    let method = Method::from_bytes(method.to_uppercase().as_bytes()).ok()?;
    route_for(&method, &path).map(|_| format!("/api/v2/messaging/relay{template}"))
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
        let canonical = route.canonical_query(req.query)?;
        let principal = self.principal_for_user(user_id).await?;
        let acting = Acting {
            principal_id: principal,
            kind: PrincipalKind::NativeCfUser,
        };
        let query: Vec<(&str, &str)> = canonical
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
        scope: &ColleagueScope,
        search: Option<&str>,
    ) -> ApiResult<Value> {
        if scope.is_empty() {
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
        let rows = match scope {
            ColleagueScope::Organizations(organizations) => {
                all(
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
                .await?
            }
            ColleagueScope::Users(people) => {
                all(
                    &self.db,
                    "SELECT u.id, u.username, l.principal_id FROM users u \
                       LEFT JOIN communication_principal_links l ON l.subject_kind = 'USER' AND l.subject_id = u.id \
                         AND l.environment_id = $3 \
                      WHERE u.id = ANY($1) AND u.id <> $2 \
                        AND ($4::text IS NULL OR u.username ILIKE $4) \
                      ORDER BY u.username LIMIT 200",
                    vec![
                        people.to_vec().into(),
                        user_id.into(),
                        self.environment_id().into(),
                        pattern.into(),
                    ],
                )
                .await?
            }
        };
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
        scope: &ColleagueScope,
        user_ids: &[Uuid],
    ) -> ApiResult<Value> {
        if user_ids.is_empty() || user_ids.len() > 100 {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "Between 1 and 100 people",
            ));
        }
        let allowed: Vec<Uuid> = match scope {
            _ if scope.is_empty() => vec![],
            ColleagueScope::Organizations(organizations) => all(
                &self.db,
                "SELECT DISTINCT m.user_id FROM organization_memberships m \
                 WHERE m.organization_id = ANY($1) AND m.status = 'ACTIVE' AND m.user_id = ANY($2)",
                vec![organizations.to_vec().into(), user_ids.to_vec().into()],
            )
            .await?
            .iter()
            .map(|r| r.try_get_by_index::<Uuid>(0))
            .collect::<Result<_, _>>()?,
            ColleagueScope::Users(people) => user_ids
                .iter()
                .filter(|u| people.contains(u))
                .copied()
                .collect(),
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
        scope: &ColleagueScope,
        principal_ids: &[Uuid],
    ) -> ApiResult<Value> {
        if principal_ids.len() > 256 {
            return Err(ApiError::domain(
                ErrorCode::ValidationError,
                "At most 256 principals",
            ));
        }
        let rows = match scope {
            ColleagueScope::Organizations(organizations) => {
                all(
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
                .await?
            }
            ColleagueScope::Users(people) => {
                all(
                    &self.db,
                    "SELECT DISTINCT l.principal_id, u.username FROM communication_principal_links l \
                       JOIN users u ON u.id = l.subject_id \
                      WHERE l.subject_kind = 'USER' AND l.environment_id = $1 AND l.principal_id = ANY($2) \
                        AND (u.id = $3 OR u.id = ANY($4))",
                    vec![
                        self.environment_id().into(),
                        principal_ids.to_vec().into(),
                        user_id.into(),
                        people.to_vec().into(),
                    ],
                )
                .await?
            }
        };
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
            (Method::PATCH, format!("v1/conversations/{C}/members/{C}")),
            (Method::POST, format!("v1/conversations/{C}/bans")),
            (
                Method::PUT,
                format!("v1/conversations/{C}/messages/{C}/reactions/:thumbsup:"),
            ),
            (
                Method::PUT,
                format!("v1/conversations/{C}/messages/{C}/reactions/%F0%9F%91%8D"),
            ),
            (Method::GET, "v1/messaging/invitations".into()),
            (Method::PUT, "v1/messaging/presence/settings".into()),
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
    /// MSG-001/002/003: every relayed operation takes exactly the query keys of the
    /// Messaging consumer contract (walked from the bundled contract, so a contract change
    /// fails here until the allowlist follows), and every allowlisted operation exists in
    /// that contract.
    #[test]
    fn every_relayed_operation_takes_exactly_the_messaging_contract_query() {
        use std::collections::BTreeSet;
        let contract: Value = serde_json::from_str(include_str!(
            "../../../../../packages/contracts/bundled/messaging/http/v1.openapi.json"
        ))
        .unwrap();
        let params = |v: &Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    let p = match p.get("$ref").and_then(Value::as_str) {
                        Some(r) => {
                            &contract["components"]["parameters"][r.rsplit('/').next().unwrap()]
                        }
                        None => p,
                    };
                    (p["in"] == "query").then(|| p["name"].as_str().unwrap().to_string())
                })
                .collect()
        };
        let mut matched = BTreeSet::new();
        for (path, item) in contract["paths"].as_object().unwrap() {
            for (method, op) in item.as_object().unwrap() {
                let m = match method.as_str() {
                    "get" => Method::GET,
                    "post" => Method::POST,
                    "put" => Method::PUT,
                    "patch" => Method::PATCH,
                    "delete" => Method::DELETE,
                    _ => continue,
                };
                let concrete = path
                    .trim_start_matches('/')
                    .split('/')
                    .map(|p| match p {
                        "{reaction}" => ":thumbsup:",
                        p if p.starts_with('{') => C,
                        p => p,
                    })
                    .collect::<Vec<_>>()
                    .join("/");
                let Some(route) = route_for(&m, &concrete) else {
                    continue;
                };
                let mut want: BTreeSet<String> = params(&op["parameters"]).into_iter().collect();
                want.extend(params(&item["parameters"]));
                let have: BTreeSet<String> =
                    route.query_keys().iter().map(|k| k.to_string()).collect();
                assert_eq!(have, want, "{m} {path}: relay query allowlist differs");
                matched.insert((m.to_string(), route.template()));
            }
        }
        for r in ROUTES {
            assert!(
                matched.contains(&(r.method().to_string(), r.template())),
                "{} {} is relayed but is not a Messaging contract operation",
                r.method(),
                r.template()
            );
        }
        // Every method the allowlist uses is mounted by the Meeting router.
        let methods: BTreeSet<String> = ROUTES.iter().map(|r| r.method().to_string()).collect();
        let mounted: BTreeSet<String> = RELAY_METHODS.iter().map(|m| m.to_string()).collect();
        assert_eq!(methods, mounted);
    }

    #[test]
    fn the_replay_cursor_is_forwarded_under_its_canonical_name() {
        let events = route_for(&Method::GET, &format!("v1/conversations/{C}/events")).unwrap();
        let q = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            events
                .canonical_query(q(&[("afterEventSequence", "7"), ("limit", "50")]))
                .unwrap(),
            q(&[("after", "7"), ("limit", "50")])
        );
        assert_eq!(
            events.canonical_query(q(&[("after", "7")])).unwrap(),
            q(&[("after", "7")])
        );
        // Both spellings at once are ambiguous; unknown keys stay refused.
        assert!(events
            .canonical_query(q(&[("after", "7"), ("afterEventSequence", "8")]))
            .is_err());
        assert!(events.canonical_query(q(&[("principalId", C)])).is_err());
        // The alias belongs to the replay route only.
        let messages = route_for(&Method::GET, &format!("v1/conversations/{C}/messages")).unwrap();
        assert!(messages
            .canonical_query(q(&[("afterEventSequence", "7")]))
            .is_err());
        let delete = route_for(
            &Method::DELETE,
            &format!("v1/conversations/{C}/messages/{C}"),
        )
        .unwrap();
        assert_eq!(
            delete.canonical_query(q(&[("scope", "ME")])).unwrap(),
            q(&[("scope", "ME")])
        );
    }

    #[test]
    fn reaction_paths_cannot_escape_the_domain_or_smuggle_a_query() {
        for value in [
            "/",
            "..",
            "%2f",
            "%5c",
            "%252f",
            "%00",
            "x?admin=true",
            "x#fragment",
            " ",
        ] {
            assert!(!safe_reaction_segment(value), "{value}");
            assert!(route_for(
                &Method::PUT,
                &format!("v1/conversations/{C}/messages/{C}/reactions/{value}")
            )
            .is_none());
        }
    }
}

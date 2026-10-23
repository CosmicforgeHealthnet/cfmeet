//! Default-deny public routing foundation.
//!
//! The gateway serves ONLY routes of a validated [`RouteTable`]; every other customer
//! path is NOT_FOUND before any credential is examined. A route maps a public `/v1/...`
//! pattern (literal segments and `{param}` segments) for one method and protocol (REST or
//! WebSocket) to one upstream service's `/internal/v1/...` path.
//!
//! Construction refuses, so they can never be routed:
//!
//! * upstreams outside [`CUSTOMER_UPSTREAMS`] — Platform Access, Telephony, Privacy Relay,
//!   the operator console and the gateway itself are never customer-reachable;
//! * public patterns outside `/v1/`, or naming `internal`, `admin`, `operator`,
//!   `provider-callbacks`, `callbacks` or `metrics`;
//! * upstream paths outside `/internal/v1/` or under an administration namespace;
//! * duplicate ids or (method, pattern) pairs, and WebSocket routes while
//!   `API_GATEWAY_WEBSOCKETS_ENABLED` is off.
//!
//! The production catalogue ([`foundation_catalogue`]) contains one foundation route. The
//! Section 51 route catalogue is NOT part of this foundation.

use std::{collections::BTreeSet, time::Duration};

use axum::http::Method;
use cf_service_auth::{services, ServiceId};

/// Upstreams that may ever receive customer traffic from the gateway (each still needs its
/// own receiver-side permission granting `cfmeet.api-gateway`; today only the Developer
/// Platform has one). Domain services keep final resource authorization.
pub const CUSTOMER_UPSTREAMS: &[&str] = &[
    services::DEVELOPER_PLATFORM,
    services::CALLING,
    services::MESSAGING,
    services::NUMBERING,
    services::SMS_GATEWAY,
    services::MEETING_SERVICE,
];

/// Path segments that are never customer-routable.
const FORBIDDEN_SEGMENTS: &[&str] = &[
    "internal",
    "admin",
    "operator",
    "provider-callbacks",
    "callbacks",
    "metrics",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Rest,
    WebSocket,
}

#[derive(Debug, Clone)]
pub struct RouteDefinition {
    /// Stable catalogue id, e.g. `developer-platform.context`.
    pub id: &'static str,
    pub method: Method,
    /// Public pattern, e.g. `/v1/developer-platform/context`.
    pub pattern: &'static str,
    pub protocol: Protocol,
    pub upstream: &'static str,
    /// Upstream path template (same `{param}` names), under `/internal/v1/`.
    pub upstream_path: &'static str,
    /// Scope the credential must hold (Platform Access scope registry); `None`: an
    /// authenticated FULL-authority credential suffices.
    pub required_scope: Option<&'static str>,
    /// Per-route deadline (default: the gateway's upstream timeout).
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    #[error("route {0}: upstream {1} is not a customer-facing service")]
    ForbiddenUpstream(String, String),
    #[error("route {0}: the public pattern must be a normalized /v1/ path without internal, administrative or callback segments")]
    ForbiddenPattern(String),
    #[error("route {0}: the upstream path must be a normalized /internal/v1/ path outside administration, with the pattern's parameters")]
    ForbiddenUpstreamPath(String),
    #[error("route {0} is defined twice")]
    Duplicate(String),
    #[error("route {0}: WebSocket routes require API_GATEWAY_WEBSOCKETS_ENABLED")]
    WebSocketsDisabled(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Param(String),
}

fn parse_pattern(pattern: &str, prefix: &str) -> Option<Vec<Segment>> {
    let rest = pattern.strip_prefix(prefix)?;
    if rest.is_empty() || rest.ends_with('/') || pattern.len() > 256 {
        return None;
    }
    rest.split('/')
        .map(|s| {
            if let Some(name) = s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                (!name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
                    .then(|| Segment::Param(name.to_string()))
            } else {
                (!s.is_empty()
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
                .then(|| Segment::Literal(s.to_string()))
            }
        })
        .collect()
}

fn params(segments: &[Segment]) -> BTreeSet<&str> {
    segments
        .iter()
        .filter_map(|s| match s {
            Segment::Param(p) => Some(p.as_str()),
            Segment::Literal(_) => None,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct CompiledRoute {
    definition: RouteDefinition,
    public: Vec<Segment>,
    upstream: Vec<Segment>,
    upstream_service: ServiceId,
}

/// A validated, immutable route table.
#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    routes: Vec<CompiledRoute>,
}

/// A matched route with its bound parameters.
#[derive(Debug, Clone)]
pub struct RouteMatch<'a> {
    pub definition: &'a RouteDefinition,
    pub upstream: &'a ServiceId,
    /// The upstream path with parameters substituted.
    pub upstream_path: String,
}

#[derive(Debug, Clone)]
pub enum Lookup<'a> {
    Matched(RouteMatch<'a>),
    /// The path exists for other methods only.
    MethodNotAllowed,
    /// Default deny.
    NoRoute,
}

/// A bound parameter value: `[A-Za-z0-9_-]{1,128}` (never a path, never encoded).
fn valid_param_value(v: &str) -> bool {
    (1..=128).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl RouteTable {
    /// The empty table: everything is NOT_FOUND.
    pub fn deny_all() -> Self {
        Self::default()
    }

    pub fn new(definitions: Vec<RouteDefinition>, websockets: bool) -> Result<Self, RouteError> {
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut routes = Vec::new();
        for d in definitions {
            let id = d.id.to_string();
            if !CUSTOMER_UPSTREAMS.contains(&d.upstream) {
                return Err(RouteError::ForbiddenUpstream(id, d.upstream.to_string()));
            }
            let public = parse_pattern(d.pattern, "/v1/")
                .filter(|segments| {
                    segments.iter().all(|s| match s {
                        Segment::Literal(l) => !FORBIDDEN_SEGMENTS.contains(&l.as_str()),
                        Segment::Param(_) => true,
                    })
                })
                .ok_or_else(|| RouteError::ForbiddenPattern(id.clone()))?;
            let upstream = parse_pattern(d.upstream_path, "/internal/v1/")
                .filter(|segments| {
                    !matches!(segments.first(), Some(Segment::Literal(l)) if l == "admin")
                        && segments
                            .iter()
                            .all(|s| !matches!(s, Segment::Literal(l) if l == "admin" || l == "operator"))
                })
                .filter(|segments| params(segments) == params(&public))
                .ok_or_else(|| RouteError::ForbiddenUpstreamPath(id.clone()))?;
            if d.protocol == Protocol::WebSocket && !websockets {
                return Err(RouteError::WebSocketsDisabled(id));
            }
            if !ids.insert(id.clone()) || !keys.insert((d.method.to_string(), d.pattern)) {
                return Err(RouteError::Duplicate(id));
            }
            let upstream_service = ServiceId::parse(d.upstream)
                .map_err(|_| RouteError::ForbiddenUpstream(id.clone(), d.upstream.into()))?;
            routes.push(CompiledRoute {
                definition: d,
                public,
                upstream,
                upstream_service,
            });
        }
        Ok(Self { routes })
    }

    pub fn len(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    pub fn definitions(&self) -> impl Iterator<Item = &RouteDefinition> {
        self.routes.iter().map(|r| &r.definition)
    }

    pub fn lookup(&self, method: &Method, path: &str) -> Lookup<'_> {
        let Some(rest) = path.strip_prefix("/v1/") else {
            return Lookup::NoRoute;
        };
        let parts: Vec<&str> = rest.split('/').collect();
        let mut path_exists = false;
        for route in &self.routes {
            if route.public.len() != parts.len() {
                continue;
            }
            let mut bound = Vec::new();
            let matched = route.public.iter().zip(&parts).all(|(s, p)| match s {
                Segment::Literal(l) => l == p,
                Segment::Param(name) => {
                    bound.push((name.as_str(), *p));
                    valid_param_value(p)
                }
            });
            if !matched {
                continue;
            }
            if route.definition.method != method {
                path_exists = true;
                continue;
            }
            let upstream_path = std::iter::once("/internal/v1".to_string())
                .chain(route.upstream.iter().map(|s| {
                    match s {
                        Segment::Literal(l) => l.clone(),
                        Segment::Param(name) => bound
                            .iter()
                            .find(|(n, _)| n == name)
                            .map(|(_, v)| v.to_string())
                            .unwrap_or_default(),
                    }
                }))
                .collect::<Vec<_>>()
                .join("/");
            return Lookup::Matched(RouteMatch {
                definition: &route.definition,
                upstream: &route.upstream_service,
                upstream_path,
            });
        }
        if path_exists {
            Lookup::MethodNotAllowed
        } else {
            Lookup::NoRoute
        }
    }
}

/// The production route catalogue of this foundation: one diagnostic route through the
/// Developer Platform. The functional catalogue is Section 51.
pub fn foundation_catalogue() -> Vec<RouteDefinition> {
    vec![RouteDefinition {
        id: "developer-platform.context",
        method: Method::GET,
        pattern: "/v1/developer-platform/context",
        protocol: Protocol::Rest,
        upstream: services::DEVELOPER_PLATFORM,
        upstream_path: "/internal/v1/gateway/context",
        required_scope: None,
        timeout: None,
    }]
}

/// The table the flags select: deny-all unless public routing is enabled.
pub fn table_for(public_routing: bool, websockets: bool) -> Result<RouteTable, RouteError> {
    if public_routing {
        RouteTable::new(foundation_catalogue(), websockets)
    } else {
        Ok(RouteTable::deny_all())
    }
}

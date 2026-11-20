//! Request shape, the route table (built once from the catalog: one table, one path) and
//! the exact query-shape parser.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::contract::model::*;
use crate::errors::ErrorCode;

pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_BODY_BYTES: usize = 32 * 1024;

/// Step 1: the path must be plain (no percent-encoding, no dot or empty segments).
pub fn check_shape(method: &str, path: &str) -> Result<(), ErrorCode> {
    if !matches!(method, "GET" | "POST") {
        return Err(ErrorCode::CapabilityNotFound);
    }
    if path.len() > MAX_PATH_BYTES
        || !path.starts_with('/')
        || path.contains('%')
        || path.contains('\\')
    {
        return Err(ErrorCode::RequestShapeInvalid);
    }
    if path
        .split('/')
        .skip(1)
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(ErrorCode::RequestShapeInvalid);
    }
    if path.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        return Err(ErrorCode::RequestShapeInvalid);
    }
    Ok(())
}

#[derive(Debug, Clone)]
enum Seg {
    Lit(&'static str),
    Param(&'static str, ParamFormat),
}

pub struct RouteTable {
    routes: Vec<(Method, Vec<Seg>, Arc<Capability>)>,
}

pub struct Matched {
    pub capability: Arc<Capability>,
    pub params: BTreeMap<&'static str, String>,
}

fn valid_param(format: ParamFormat, v: &str) -> bool {
    match format {
        ParamFormat::Uuid => {
            v.len() == 36
                && v.bytes().enumerate().all(|(i, b)| match i {
                    8 | 13 | 18 | 23 => b == b'-',
                    _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
                })
        }
        ParamFormat::Opaque => {
            !v.is_empty()
                && v.len() <= 128
                && v.as_bytes()[0].is_ascii_alphanumeric()
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
        }
        ParamFormat::DatasetId => v.strip_prefix("cfmeet.").is_some_and(|r| {
            !r.is_empty() && r.len() <= 48 && r.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        }),
    }
}

impl RouteTable {
    pub fn new(caps: Vec<Capability>) -> Self {
        let routes = caps
            .into_iter()
            .map(|c| {
                let segs = c
                    .path
                    .split('/')
                    .skip(1)
                    .map(
                        |s| match s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                            Some(name) => {
                                let p = c
                                    .path_params
                                    .iter()
                                    .find(|p| p.name == name)
                                    .expect("declared");
                                Seg::Param(p.name, p.format)
                            }
                            None => Seg::Lit(s),
                        },
                    )
                    .collect();
                (c.method, segs, Arc::new(c))
            })
            .collect();
        RouteTable { routes }
    }

    pub fn capabilities(&self) -> impl Iterator<Item = &Arc<Capability>> {
        self.routes.iter().map(|(_, _, c)| c)
    }

    pub fn by_id(&self, id: &str) -> Option<&Arc<Capability>> {
        self.capabilities().find(|c| c.id == id)
    }

    /// Step 8: method + path (relative to the base) to a capability. Literal segments
    /// win over parameters; a parameter value that fails its format does not match.
    pub fn lookup(&self, method: &str, path: &str) -> Option<Matched> {
        let rel = path.strip_prefix(BRIDGE_BASE)?;
        let parts: Vec<&str> = rel.split('/').skip(1).collect();
        let method = Method::parse(method)?;
        let mut best: Option<(usize, Matched)> = None;
        for (m, segs, cap) in &self.routes {
            if *m != method || segs.len() != parts.len() {
                continue;
            }
            let mut params = BTreeMap::new();
            let mut literals = 0;
            let ok = segs.iter().zip(&parts).all(|(s, p)| match s {
                Seg::Lit(l) => {
                    literals += 1;
                    l == p
                }
                Seg::Param(name, f) => {
                    params.insert(*name, p.to_string());
                    valid_param(*f, p)
                }
            });
            if ok && best.as_ref().is_none_or(|(n, _)| literals > *n) {
                best = Some((
                    literals,
                    Matched {
                        capability: cap.clone(),
                        params,
                    },
                ));
            }
        }
        best.map(|(_, m)| m)
    }
}

/// Decoded, validated query of one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub market: String,
    pub organization: String,
    pub zone: String,
    pub tenant: String,
    pub purpose: Purpose,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub start: Option<DateTime<Utc>>,
    pub end: Option<DateTime<Utc>>,
    pub q: Option<String>,
}

fn percent_decode(v: &str) -> Option<String> {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn scope_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.as_bytes()[0].is_ascii_alphanumeric()
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'@' | b'-'))
}

fn cursor_ok(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 512
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._~:@+=-".contains(&b))
}

fn search_ok(v: &str) -> bool {
    let n = v.chars().count();
    (1..=128).contains(&n)
        && v.chars()
            .all(|c| c.is_alphanumeric() || " ._@:-".contains(c))
}

fn utc(v: &str) -> Option<DateTime<Utc>> {
    if !v.ends_with('Z') {
        return None;
    }
    DateTime::parse_from_rfc3339(v)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Parses the raw query string against the capability's shape: parameters in the exact
/// shape order, no repeats, no extras, every required one present, every value valid.
pub fn parse_query(
    raw: Option<&str>,
    shape: QueryShape,
    cap_purposes: &[Purpose],
    now: DateTime<Utc>,
) -> Result<Query, ErrorCode> {
    let bad = ErrorCode::QueryInvalid;
    let order = shape.params();
    let mut got: BTreeMap<&str, String> = BTreeMap::new();
    let mut position = 0usize;
    for pair in raw.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').ok_or(bad)?;
        let idx = order[position..].iter().position(|n| *n == k).ok_or(bad)? + position;
        position = idx + 1;
        let name = order[idx];
        got.insert(name, percent_decode(v).ok_or(bad)?);
    }
    for r in shape.required() {
        if !got.contains_key(r) {
            return Err(bad);
        }
    }
    let take = |k: &str| got.get(k).cloned();
    let market = take("market").ok_or(bad)?;
    let organization = take("organization").ok_or(bad)?;
    let zone = take("zone").ok_or(bad)?;
    let tenant = take("tenant").ok_or(bad)?;
    if !scope_ok(&market)
        || organization != PRODUCT
        || !matches!(zone.as_str(), "production" | "sandbox")
        || !scope_ok(&tenant)
    {
        return Err(bad);
    }
    let purpose = Purpose::parse(&take("purpose").ok_or(bad)?).ok_or(bad)?;
    if !cap_purposes.contains(&purpose) {
        return Err(ErrorCode::PurposeNotAllowed);
    }
    let limit = match take("limit") {
        None => None,
        Some(v) => {
            let n: u32 = v.parse().map_err(|_| bad)?;
            if !(1..=MAX_LIMIT).contains(&n) || v.starts_with('0') {
                return Err(bad);
            }
            Some(n)
        }
    };
    let cursor = take("cursor");
    if cursor.as_deref().is_some_and(|c| !cursor_ok(c)) {
        return Err(bad);
    }
    let q = take("q");
    if q.as_deref().is_some_and(|v| !search_ok(v)) {
        return Err(bad);
    }
    let start = take("start").map(|v| utc(&v).ok_or(bad)).transpose()?;
    let end = take("end").map(|v| utc(&v).ok_or(bad)).transpose()?;
    if let (Some(s), Some(e)) = (start, end) {
        if e <= s
            || e - s > Duration::days(MAX_WINDOW_DAYS as i64)
            || e > now + Duration::seconds(60)
        {
            return Err(bad);
        }
    }
    Ok(Query {
        market,
        organization,
        zone,
        tenant,
        purpose,
        limit,
        cursor,
        start,
        end,
        q,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::catalog::catalog;

    fn table() -> RouteTable {
        RouteTable::new(catalog())
    }

    #[test]
    fn shape_refuses_encoded_dotted_and_long_paths() {
        assert!(check_shape("GET", "/internal/cfmeet-admin/v1/capabilities").is_ok());
        for p in [
            "/a/%2e%2e/b",
            "/a/../b",
            "/a/./b",
            "/a//b",
            "/a/b/",
            "relative",
            "/a\\b",
            "/a b",
        ] {
            assert_eq!(
                check_shape("GET", p),
                Err(ErrorCode::RequestShapeInvalid),
                "{p}"
            );
        }
        assert_eq!(
            check_shape("GET", &format!("/{}", "a".repeat(MAX_PATH_BYTES))),
            Err(ErrorCode::RequestShapeInvalid)
        );
        assert_eq!(
            check_shape("DELETE", "/x"),
            Err(ErrorCode::CapabilityNotFound)
        );
    }

    #[test]
    fn every_catalog_route_matches_itself_and_nothing_else_matches() {
        let t = table();
        for c in catalog() {
            let concrete = c.path_params.iter().fold(c.path.to_string(), |p, pp| {
                p.replace(
                    &format!("{{{}}}", pp.name),
                    match pp.format {
                        ParamFormat::Uuid => "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a",
                        ParamFormat::Opaque => "id-1",
                        ParamFormat::DatasetId => "cfmeet.engagement_daily",
                    },
                )
            });
            let m = t
                .lookup(c.method.as_str(), &format!("{BRIDGE_BASE}{concrete}"))
                .unwrap_or_else(|| panic!("{}", c.id));
            assert_eq!(m.capability.id, c.id);
        }
        assert!(t.lookup("GET", &format!("{BRIDGE_BASE}/nope")).is_none());
        assert!(t
            .lookup("POST", &format!("{BRIDGE_BASE}/capabilities"))
            .is_none());
        assert!(t.lookup("GET", "/other/capabilities").is_none());
        // a uuid parameter refuses a non-uuid value
        assert!(t
            .lookup("GET", &format!("{BRIDGE_BASE}/accounts/users/not-a-uuid"))
            .is_none());
        assert!(t
            .lookup("GET", &format!("{BRIDGE_BASE}/datasets/other.thing"))
            .is_none());
    }

    fn now() -> DateTime<Utc> {
        utc("2026-11-19T12:00:00Z").unwrap()
    }

    const BASE: &str =
        "market=GLOBAL&organization=cfmeet&zone=production&tenant=cfmeet-first-party";

    #[test]
    fn query_must_follow_the_exact_shape() {
        let p = &[Purpose::MetricsRead];
        let ok = format!("{BASE}&purpose=metrics-read&limit=31&start=2026-11-01T00%3A00%3A00Z&end=2026-11-19T00:00:00Z");
        let q = parse_query(Some(&ok), QueryShape::ScopedWindow, p, now()).unwrap();
        assert_eq!((q.limit, q.market.as_str()), (Some(31), "GLOBAL"));
        let refused = [
            // out of order
            "organization=cfmeet&market=GLOBAL&zone=production&tenant=t&purpose=metrics-read&start=2026-11-01T00:00:00Z&end=2026-11-02T00:00:00Z".to_string(),
            // repeated
            format!("{BASE}&purpose=metrics-read&purpose=metrics-read&start=2026-11-01T00:00:00Z&end=2026-11-02T00:00:00Z"),
            // extra
            format!("{BASE}&purpose=metrics-read&start=2026-11-01T00:00:00Z&end=2026-11-02T00:00:00Z&x=1"),
            // missing required end
            format!("{BASE}&purpose=metrics-read&start=2026-11-01T00:00:00Z"),
            // window over 31 days
            format!("{BASE}&purpose=metrics-read&start=2026-10-01T00:00:00Z&end=2026-11-19T00:00:00Z"),
            // end in the future
            format!("{BASE}&purpose=metrics-read&start=2026-11-19T00:00:00Z&end=2026-11-20T00:00:00Z"),
            // non-UTC
            format!("{BASE}&purpose=metrics-read&start=2026-11-01T00:00:00+01:00&end=2026-11-02T00:00:00Z"),
            // limit out of range
            format!("{BASE}&purpose=metrics-read&limit=101&start=2026-11-01T00:00:00Z&end=2026-11-02T00:00:00Z"),
            format!("{BASE}&purpose=metrics-read&limit=007&start=2026-11-01T00:00:00Z&end=2026-11-02T00:00:00Z"),
        ];
        for r in &refused {
            assert_eq!(
                parse_query(Some(r), QueryShape::ScopedWindow, p, now()),
                Err(ErrorCode::QueryInvalid),
                "{r}"
            );
        }
        assert_eq!(
            parse_query(Some("market=GLOBAL&organization=karryam&zone=production&tenant=t&purpose=metrics-read"), QueryShape::Scoped, p, now()),
            Err(ErrorCode::QueryInvalid)
        );
        assert_eq!(
            parse_query(
                Some(&format!("{BASE}&purpose=support-review")),
                QueryShape::Scoped,
                p,
                now()
            ),
            Err(ErrorCode::PurposeNotAllowed)
        );
        assert_eq!(
            parse_query(
                Some(&format!("{BASE}&purpose=nope")),
                QueryShape::Scoped,
                p,
                now()
            ),
            Err(ErrorCode::QueryInvalid)
        );
        // optional params may be omitted, order still holds
        assert!(parse_query(
            Some(&format!("{BASE}&purpose=metrics-read&cursor=abc")),
            QueryShape::ScopedPage,
            p,
            now()
        )
        .is_ok());
        assert!(parse_query(
            Some(&format!("{BASE}&purpose=metrics-read&q=Ada%20L")),
            QueryShape::ScopedSearch,
            p,
            now()
        )
        .is_ok());
        assert!(parse_query(
            Some(&format!("{BASE}&purpose=metrics-read&q=a%3Cb")),
            QueryShape::ScopedSearch,
            p,
            now()
        )
        .is_err());
    }
}

//! Provider-neutral inventory search (Part IV Workstream H §142.1–§142.2).
//!
//! A search answers from Numbering's OWN canonical inventory: Number Resources with their
//! newest observation, capability evidence (resolved by the E resolver) and KYC
//! requirements (F). Before the first page of a query, the providers serving the caller's
//! inventory class are asked for fresh offers — at most once per query and interval,
//! claimed atomically so concurrent searches never fan out to a provider — and the offers
//! are ingested as canonical resources (G).
//!
//! * The caller's environment decides the inventory class: SANDBOX / TEST see synthetic
//!   test inventory only, PRODUCTION real inventory only (never synthetic stock), and a
//!   real provider is consulted only for an environment allowed live telecom.
//! * Only numbers that could be reserved appear: AVAILABLE / REASSIGNABLE, offered by
//!   their newest observation, with no current reservation or open assignment — never a
//!   quarantined, uncertain, reserved or assigned number (no oracle on other tenants).
//! * Every result is an OBSERVATION, never a guarantee: it carries its observation time
//!   and freshness, and an expired observation is reported `STALE` (not reservable until
//!   observed again). A provider that cannot be reached is reported, and cached results
//!   are never presented as live stock.
//! * Enumeration is bounded: a country is required, pages are at most
//!   `search_page_max` (keyset cursors bound to the query), and each trusted environment
//!   has a per-minute search quota shared by every instance.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine;
use chrono::{DateTime, Duration, DurationRound, Utc};
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::Acquisition;
use crate::{
    domain::RequestContext,
    numbers::{
        catalog::{CountryEntryRow, TypeEntryRow},
        resolution::CapabilityResolution,
        resources::ResourceRow,
        stmt, Capability, DomainError, DomainResult, EnvironmentKind, InventoryClass, NumberType,
        OwnerScope, OwnershipClass,
    },
    providers::{InventoryQuery, ProviderOperation},
};

/// A search as asked (validated by [`Acquisition::search`]).
#[derive(Debug, Clone, Default)]
pub struct SearchRequest {
    pub country: String,
    pub region: Option<String>,
    pub number_type: Option<NumberType>,
    pub capabilities: Vec<Capability>,
    /// Leading digits of the E.164 number (with or without `+`).
    pub prefix: Option<String>,
    pub limit: Option<u64>,
    pub cursor: Option<String>,
}

/// Whether the result could be reserved now.
pub const AVAILABLE: &str = "AVAILABLE";
/// The newest observation has expired: shown, but not reservable until observed again.
pub const STALE: &str = "STALE";

/// What the KYC requirements in force say about a result (F).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KycIndicator {
    /// `NOT_REQUIRED`, `REQUIRED`, `REQUIRED_FOR_OWNERSHIP_CLASSES` or `UNKNOWN` (no
    /// requirements published: eligibility fails closed).
    pub status: &'static str,
    /// The ownership classes that require KYC (`REQUIRED_FOR_OWNERSHIP_CLASSES`).
    pub ownership_classes: Vec<String>,
    /// The number itself is flagged KYC_REQUIRED.
    pub number_flagged: bool,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub resource: ResourceRow,
    pub resolution: CapabilityResolution,
    pub kyc: KycIndicator,
    /// [`AVAILABLE`] or [`STALE`].
    pub availability: &'static str,
}

/// How the inventory behind a page was obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryFreshness {
    pub inventory_class: InventoryClass,
    /// `REFRESHED` (a provider was asked now), `CACHED` (asked recently, within the
    /// refresh interval), `PROVIDER_UNAVAILABLE` (a provider could not be reached: results
    /// are the cached observations), `NOT_CONFIGURED` (no provider serves this class) or
    /// `NOT_REQUESTED` (a later page: no provider is asked).
    pub refresh: &'static str,
    pub refreshed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct SearchPage {
    pub results: Vec<SearchResult>,
    pub next_cursor: Option<String>,
    pub inventory: InventoryFreshness,
}

#[derive(Debug, Clone)]
pub enum SearchOutcome {
    Page(Box<SearchPage>),
    /// The environment's search quota is exhausted.
    Throttled {
        retry_after_seconds: u64,
    },
}

/// The class an environment may use.
pub fn class_of(kind: EnvironmentKind) -> InventoryClass {
    match kind {
        EnvironmentKind::Production => InventoryClass::Real,
        _ => InventoryClass::Synthetic,
    }
}

fn invalid(message: &str) -> DomainError {
    DomainError::Validation(message.to_string())
}

/// `+` and 1–15 digits.
fn normalize_prefix(prefix: &str) -> DomainResult<String> {
    let digits = prefix.strip_prefix('+').unwrap_or(prefix);
    if digits.is_empty() || digits.len() > 15 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("a prefix is 1–15 digits"));
    }
    Ok(format!("+{digits}"))
}

fn valid_region(region: &str) -> bool {
    (1..=100).contains(&region.len())
        && region
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | ' ' | '_'))
}

#[derive(Debug, Clone)]
struct Normalized {
    country: String,
    region: Option<String>,
    number_type: Option<NumberType>,
    capabilities: Vec<Capability>,
    prefix: Option<String>,
    limit: u64,
    after: Option<(String, Uuid)>,
    digest: String,
}

fn digest(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

fn encode_cursor(e164: &str, id: Uuid, digest: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({ "e": e164, "i": id, "q": &digest[..16] }).to_string())
}

fn decode_cursor(cursor: &str, digest: &str) -> DomainResult<(String, Uuid)> {
    let bad = || invalid("the cursor does not belong to this search");
    if cursor.len() > 256 {
        return Err(bad());
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| bad())?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    if v["q"].as_str() != Some(&digest[..16]) {
        return Err(bad());
    }
    let e164 = v["e"].as_str().filter(|e| e.len() <= 16).ok_or_else(bad)?;
    let id = v["i"]
        .as_str()
        .and_then(|i| Uuid::parse_str(i).ok())
        .ok_or_else(bad)?;
    Ok((e164.to_string(), id))
}

impl Acquisition {
    fn normalize(&self, request: &SearchRequest) -> DomainResult<Normalized> {
        crate::numbers::catalog::valid_country(&request.country)?;
        let region = match request.region.as_deref().map(str::trim) {
            Some(r) if !valid_region(r) => return Err(invalid("invalid region")),
            Some(r) => Some(r.to_string()),
            None => None,
        };
        if request.number_type == Some(NumberType::Unclassified) {
            return Err(invalid("search a classified number type"));
        }
        if request.capabilities.len() > Capability::ALL.len() {
            return Err(invalid("too many capabilities"));
        }
        let mut capabilities = request.capabilities.clone();
        capabilities.sort();
        capabilities.dedup();
        let prefix = request
            .prefix
            .as_deref()
            .map(normalize_prefix)
            .transpose()?;
        let limit = request.limit.unwrap_or(20);
        if !(1..=self.settings.search_page_max).contains(&limit) {
            return Err(invalid("the page size is out of range"));
        }
        let caps: Vec<&str> = capabilities.iter().map(|c| c.as_str()).collect();
        let digest = digest(&[
            &request.country,
            region.as_deref().unwrap_or(""),
            request.number_type.map(|t| t.as_str()).unwrap_or(""),
            &caps.join(","),
            prefix.as_deref().unwrap_or(""),
        ]);
        let after = request
            .cursor
            .as_deref()
            .map(|c| decode_cursor(c, &digest))
            .transpose()?;
        Ok(Normalized {
            country: request.country.clone(),
            region,
            number_type: request.number_type,
            capabilities,
            prefix,
            limit,
            after,
            digest,
        })
    }

    /// Counts one search against the environment's quota (fixed one-minute windows).
    async fn take_quota(&self, scope: &OwnerScope) -> DomainResult<Option<u64>> {
        let now = self.now();
        let window = now.duration_trunc(Duration::minutes(1)).unwrap_or(now);
        let key = format!(
            "{}:{}:{}",
            scope.tenant_id.simple(),
            scope.application_id.simple(),
            scope.environment_id.simple()
        );
        #[derive(FromQueryResult)]
        struct Count {
            requests: i32,
        }
        let count = Count::find_by_statement(stmt(
            "INSERT INTO number_search_windows (scope_key, window_start, requests) VALUES ($1, $2, 1) \
             ON CONFLICT (scope_key, window_start) DO UPDATE SET requests = number_search_windows.requests + 1 \
             RETURNING requests",
            vec![key.into(), window.into()],
        ))
        .one(self.numbers.db())
        .await?
        .map(|c| c.requests)
        .unwrap_or(1);
        if count > self.settings.search_requests_per_minute {
            crate::metrics::inc("numbering_search_throttled_total", &[]);
            let retry = (window + Duration::minutes(1) - now).num_seconds().max(1);
            return Ok(Some(retry as u64));
        }
        Ok(None)
    }

    /// Forgets quota windows that ended before `before` (bounded).
    pub async fn purge_search_windows(&self, before: DateTime<Utc>) -> DomainResult<u64> {
        Ok(self
            .numbers
            .db()
            .execute(stmt(
                "DELETE FROM number_search_windows WHERE window_start < $1",
                vec![before.into()],
            ))
            .await?
            .rows_affected())
    }

    /// Asks every provider serving `class` for fresh offers of `query` (claimed per
    /// provider and query; at most once per refresh interval).
    async fn refresh(
        &self,
        ctx: &RequestContext,
        class: InventoryClass,
        query: &InventoryQuery,
    ) -> DomainResult<InventoryFreshness> {
        let Some(commands) = &self.providers else {
            return Ok(InventoryFreshness {
                inventory_class: class,
                refresh: "NOT_CONFIGURED",
                refreshed_at: None,
            });
        };
        let synthetic = class == InventoryClass::Synthetic;
        let providers: Vec<_> = commands
            .registry()
            .descriptors()
            .into_iter()
            .filter(|d| d.synthetic == synthetic && d.supports(ProviderOperation::Search))
            .filter(|d| d.countries.is_empty() || d.countries.contains(&query.country))
            .collect();
        if providers.is_empty() {
            return Ok(InventoryFreshness {
                inventory_class: class,
                refresh: "NOT_CONFIGURED",
                refreshed_at: None,
            });
        }
        let caps: Vec<&str> = query.capabilities.iter().map(|c| c.as_str()).collect();
        let key = digest(&[
            &query.country,
            query.region.as_deref().unwrap_or(""),
            query.number_type.map(|t| t.as_str()).unwrap_or(""),
            &caps.join(","),
        ]);
        let mut refreshed = false;
        let mut unavailable = false;
        let mut last_success: Option<DateTime<Utc>> = None;
        for p in providers {
            let now = self.now();
            #[derive(FromQueryResult)]
            struct Claimed {
                #[allow(dead_code)]
                provider_id: String,
            }
            let claimed = Claimed::find_by_statement(stmt(
                "INSERT INTO number_inventory_refreshes (provider_id, query_key, country_code, inventory_class, \
                   claimed_until, last_attempt_at, last_outcome, next_refresh_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, 'IN_PROGRESS', $6) \
                 ON CONFLICT (provider_id, query_key) DO UPDATE SET claimed_until = EXCLUDED.claimed_until, \
                   last_attempt_at = EXCLUDED.last_attempt_at, last_outcome = 'IN_PROGRESS' \
                 WHERE number_inventory_refreshes.next_refresh_at <= $6 \
                   AND (number_inventory_refreshes.claimed_until IS NULL OR number_inventory_refreshes.claimed_until <= $6) \
                 RETURNING provider_id",
                vec![
                    p.provider_id.clone().into(),
                    key.clone().into(),
                    query.country.clone().into(),
                    class.as_str().into(),
                    (now + self.settings.refresh_claim).into(),
                    now.into(),
                ],
            ))
            .one(self.numbers.db())
            .await?;
            if claimed.is_none() {
                #[derive(FromQueryResult)]
                struct State {
                    last_success_at: Option<DateTime<Utc>>,
                    last_outcome: String,
                }
                if let Some(s) = State::find_by_statement(stmt(
                    "SELECT last_success_at, last_outcome FROM number_inventory_refreshes WHERE provider_id = $1 AND query_key = $2",
                    vec![p.provider_id.clone().into(), key.clone().into()],
                ))
                .one(self.numbers.db())
                .await?
                {
                    last_success = last_success.max(s.last_success_at);
                    unavailable |= s.last_outcome == "UNAVAILABLE";
                }
                continue;
            }
            let outcome = commands.ingest_inventory(ctx, &p.provider_id, query).await;
            let finished = self.now();
            let (label, offers, next) = match outcome {
                Ok(rows) => (
                    "SUCCEEDED",
                    rows.len() as i32,
                    finished + self.settings.refresh_interval,
                ),
                Err(DomainError::Precondition {
                    code: "OPERATION_UNSUPPORTED",
                    ..
                }) => ("UNSUPPORTED", 0, finished + self.settings.refresh_interval),
                Err(DomainError::Precondition { .. }) => (
                    "UNAVAILABLE",
                    0,
                    finished + self.settings.refresh_failure_backoff,
                ),
                Err(e) => {
                    // A local failure: release the claim and report it.
                    self.numbers
                        .db()
                        .execute(stmt(
                            "UPDATE number_inventory_refreshes SET claimed_until = NULL, last_outcome = 'UNAVAILABLE', \
                               next_refresh_at = $3 WHERE provider_id = $1 AND query_key = $2",
                            vec![p.provider_id.clone().into(), key.clone().into(), finished.into()],
                        ))
                        .await?;
                    return Err(e);
                }
            };
            self.numbers
                .db()
                .execute(stmt(
                    "UPDATE number_inventory_refreshes SET claimed_until = NULL, last_outcome = $3, offers = $4, \
                       next_refresh_at = $5, last_success_at = CASE WHEN $3 = 'SUCCEEDED' THEN $6 ELSE last_success_at END \
                     WHERE provider_id = $1 AND query_key = $2",
                    vec![
                        p.provider_id.clone().into(),
                        key.clone().into(),
                        label.into(),
                        offers.into(),
                        next.into(),
                        finished.into(),
                    ],
                ))
                .await?;
            crate::metrics::inc("numbering_inventory_refreshes_total", &[("outcome", label)]);
            match label {
                "SUCCEEDED" => {
                    refreshed = true;
                    last_success = Some(finished);
                }
                "UNAVAILABLE" => unavailable = true,
                _ => {}
            }
        }
        Ok(InventoryFreshness {
            inventory_class: class,
            refresh: if unavailable {
                "PROVIDER_UNAVAILABLE"
            } else if refreshed {
                "REFRESHED"
            } else {
                "CACHED"
            },
            refreshed_at: last_success,
        })
    }

    /// Searches the caller's inventory class (see the module rules).
    pub async fn search(
        &self,
        ctx: &RequestContext,
        scope: &OwnerScope,
        live_telecom_allowed: bool,
        request: &SearchRequest,
    ) -> DomainResult<SearchOutcome> {
        let q = self.normalize(request)?;
        if let Some(retry_after_seconds) = self.take_quota(scope).await? {
            return Ok(SearchOutcome::Throttled {
                retry_after_seconds,
            });
        }
        let class = class_of(scope.environment_kind);
        let inventory = if q.after.is_some() {
            InventoryFreshness {
                inventory_class: class,
                refresh: "NOT_REQUESTED",
                refreshed_at: None,
            }
        } else if class == InventoryClass::Real && !live_telecom_allowed {
            // Real providers are never consulted for an environment without live telecom.
            InventoryFreshness {
                inventory_class: class,
                refresh: "NOT_CONFIGURED",
                refreshed_at: None,
            }
        } else {
            self.refresh(
                ctx,
                class,
                &InventoryQuery {
                    country: q.country.clone(),
                    number_type: q.number_type,
                    capabilities: q.capabilities.clone(),
                    region: q.region.clone(),
                    limit: 100,
                },
            )
            .await?
        };
        let now = self.now();
        let (after_e164, after_id) = q.after.clone().unzip();
        let caps: Vec<String> = q
            .capabilities
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        let rows = ResourceRow::find_by_statement(stmt(
            "SELECT r.* FROM number_resources r \
             JOIN LATERAL (SELECT availability FROM number_inventory_snapshots s \
                            WHERE s.number_resource_id = r.id ORDER BY s.observed_at DESC, s.sequence DESC LIMIT 1) o ON true \
             WHERE r.country_code = $1 AND (r.inventory_source = 'SYNTHETIC_TEST') = $2 \
               AND r.service_class = 'DCN' \
               AND r.state IN ('AVAILABLE', 'REASSIGNABLE') AND o.availability = 'AVAILABLE' \
               AND NOT EXISTS (SELECT 1 FROM number_reservations x WHERE x.number_resource_id = r.id \
                                AND x.status IN ('PENDING', 'HELD', 'UNKNOWN')) \
               AND NOT EXISTS (SELECT 1 FROM dcn_assignments a WHERE a.number_resource_id = r.id AND a.status <> 'CLOSED') \
               AND ($3::text IS NULL OR r.region = $3) \
               AND ($4::text IS NULL OR r.number_type = $4) \
               AND ($5::text IS NULL OR left(r.e164, length($5)) = $5) \
               AND NOT EXISTS (SELECT 1 FROM unnest($6::text[]) AS want(c) WHERE NOT EXISTS ( \
                     SELECT 1 FROM number_capabilities nc WHERE nc.number_resource_id = r.id AND nc.capability = want.c \
                        AND nc.effective_to IS NULL AND nc.status IN ('VERIFIED', 'DECLARED') \
                        AND nc.effective_from <= $7 AND (nc.expires_at IS NULL OR nc.expires_at > $7))) \
               AND ($8::text IS NULL OR (r.e164, r.id) > ($8, $9)) \
             ORDER BY r.e164, r.id LIMIT $10",
            vec![
                q.country.clone().into(),
                (class == InventoryClass::Synthetic).into(),
                q.region.clone().into(),
                q.number_type.map(|t| t.as_str().to_string()).into(),
                q.prefix.clone().into(),
                caps.into(),
                now.into(),
                after_e164.into(),
                after_id.into(),
                (q.limit as i64 + 1).into(),
            ],
        ))
        .all(self.numbers.db())
        .await?;
        let more = rows.len() as u64 > q.limit;
        let page: Vec<ResourceRow> = rows.into_iter().take(q.limit as usize).collect();
        let next_cursor = more.then(|| {
            let last = page.last().expect("a full page");
            encode_cursor(&last.e164, last.id, &q.digest)
        });
        let mut kyc_cache: BTreeMap<String, (&'static str, Vec<String>)> = BTreeMap::new();
        let mut results = Vec::with_capacity(page.len());
        for resource in page {
            // The authoritative resolver (catalog AND evidence): a capability the catalog
            // excludes or real evidence does not verify is never offered as supported.
            let resolution = self.numbers.resolve_capabilities(resource.id).await?;
            if q.capabilities.iter().any(|c| !resolution.get(*c).supported) {
                continue;
            }
            let (status, classes) = match kyc_cache.get(&resource.number_type) {
                Some(v) => v.clone(),
                None => {
                    let v = self
                        .kyc_requirement_status(
                            &resource.country_code,
                            class,
                            &resource.number_type,
                            now,
                        )
                        .await?;
                    kyc_cache.insert(resource.number_type.clone(), v.clone());
                    v
                }
            };
            let flagged = resource
                .restriction_flags
                .iter()
                .any(|f| f == "KYC_REQUIRED");
            let availability = if resource.is_fresh(now) {
                AVAILABLE
            } else {
                STALE
            };
            results.push(SearchResult {
                kyc: KycIndicator {
                    status: if flagged && status == "NOT_REQUIRED" {
                        "REQUIRED"
                    } else {
                        status
                    },
                    ownership_classes: classes,
                    number_flagged: flagged,
                },
                availability,
                resolution,
                resource,
            });
        }
        crate::metrics::inc(
            "numbering_searches_total",
            &[("refresh", inventory.refresh)],
        );
        Ok(SearchOutcome::Page(Box::new(SearchPage {
            results,
            next_cursor,
            inventory,
        })))
    }

    /// Which ownership classes the base KYC requirements in force make KYC mandatory for
    /// (`UNKNOWN` when nothing is published: eligibility then fails closed).
    async fn kyc_requirement_status(
        &self,
        country: &str,
        class: InventoryClass,
        number_type: &str,
        now: DateTime<Utc>,
    ) -> DomainResult<(&'static str, Vec<String>)> {
        #[derive(FromQueryResult)]
        struct Set {
            ownership_class: Option<String>,
            kyc_mandatory: bool,
        }
        let sets = Set::find_by_statement(stmt(
            "SELECT ownership_class, kyc_mandatory FROM number_kyc_requirements WHERE country_code = $1 \
               AND inventory_class = $2 AND status = 'CURRENT' AND scope_kind = 'BASE' \
               AND (number_type IS NULL OR number_type = $3) AND effective_from <= $4 \
               AND (effective_to IS NULL OR effective_to > $4)",
            vec![
                country.into(),
                class.as_str().into(),
                number_type.into(),
                now.into(),
            ],
        ))
        .all(self.numbers.db())
        .await?;
        if sets.is_empty() {
            return Ok(("UNKNOWN", vec![]));
        }
        if sets
            .iter()
            .any(|s| s.kyc_mandatory && s.ownership_class.is_none())
        {
            return Ok(("REQUIRED", vec![]));
        }
        let classes: BTreeSet<String> = sets
            .iter()
            .filter(|s| s.kyc_mandatory)
            .filter_map(|s| s.ownership_class.clone())
            .collect();
        if classes.is_empty() {
            Ok(("NOT_REQUIRED", vec![]))
        } else if classes.len() == OwnershipClass::ALL.len() {
            Ok(("REQUIRED", vec![]))
        } else {
            Ok((
                "REQUIRED_FOR_OWNERSHIP_CLASSES",
                classes.into_iter().collect(),
            ))
        }
    }

    /// The countries of the caller's inventory class that are supported now.
    pub async fn countries(&self, scope: &OwnerScope) -> DomainResult<Vec<CountryEntryRow>> {
        let class = class_of(scope.environment_kind);
        let now = self.now();
        let rows = CountryEntryRow::find_by_statement(stmt(
            "SELECT * FROM number_country_catalog WHERE inventory_class = $1 AND status = 'CURRENT' \
               AND support_status <> 'UNSUPPORTED' ORDER BY country_code",
            vec![class.as_str().into()],
        ))
        .all(self.numbers.db())
        .await?;
        Ok(rows
            .into_iter()
            .filter(|c| c.is_effective_at(now))
            .filter(|c| class == InventoryClass::Synthetic || c.verification_status == "VERIFIED")
            .collect())
    }

    /// The generic number-type entries of a supported country (provider applicability
    /// stays internal).
    pub async fn number_types(
        &self,
        scope: &OwnerScope,
        country: &str,
    ) -> DomainResult<Vec<TypeEntryRow>> {
        crate::numbers::catalog::valid_country(country)?;
        let class = class_of(scope.environment_kind);
        if !self
            .countries(scope)
            .await?
            .iter()
            .any(|c| c.country_code == country)
        {
            return Err(DomainError::NotFound("country"));
        }
        let now = self.now();
        Ok(self
            .numbers
            .number_type_entries(country, class)
            .await?
            .into_iter()
            .filter(|e| e.provider_id.is_none() && e.is_effective_at(now))
            .filter(|e| class == InventoryClass::Synthetic || e.verification_status == "VERIFIED")
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_cursors_are_bounded_and_bound_to_their_query() {
        assert_eq!(normalize_prefix("1555").unwrap(), "+1555");
        assert_eq!(normalize_prefix("+1555").unwrap(), "+1555");
        for bad in ["", "+", "12a", "1234567890123456", "+1 555"] {
            assert!(normalize_prefix(bad).is_err(), "{bad}");
        }
        let d = digest(&["US", "", "", "", ""]);
        let other = digest(&["US", "", "MOBILE", "", ""]);
        let id = Uuid::new_v4();
        let c = encode_cursor("+15550100001", id, &d);
        assert_eq!(decode_cursor(&c, &d).unwrap(), ("+15550100001".into(), id));
        assert!(decode_cursor(&c, &other).is_err());
        assert!(decode_cursor("not-a-cursor", &d).is_err());
        // Length-prefixed parts never collide.
        assert_ne!(digest(&["ab", "c"]), digest(&["a", "bc"]));
    }
}

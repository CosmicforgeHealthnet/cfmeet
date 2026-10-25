//! The producer-side usage exporter (Part VI §55 §11.4).
//!
//! A domain service implements [`EventSource`] (immutable facts it already records, read
//! in `(position time, key)` order) and / or [`IntervalSource`] (resources stored or
//! active over time — storage bytes, active numbers — metered per completed UTC day) and
//! runs [`Exporter::run`]. The exporter:
//!
//! * asks the Developer Platform for the stream's durable watermark
//!   (`GET /internal/v1/usage/streams/{stream}`), so a restart, an outage or a lost
//!   response resumes exactly where the last ACKNOWLEDGED batch ended (backlog recovery;
//!   a missing watermark replays the producer's full history: historical backfill);
//! * exports only facts older than the settle window (late commits are not skipped);
//! * posts batches (`POST /internal/v1/usage/facts`) whose acknowledgement advances the
//!   watermark in the same Developer Platform transaction; duplicates are harmless
//!   (`(producer, source_fact_id)` dedupe), conflicting replays are refused there;
//! * reports its own per-day totals for shadow reconciliation
//!   (`POST /internal/v1/usage/reconciliations`).
//!
//! The producer is the AUTHENTICATED service (service-auth `developer-event.publish`):
//! nothing in a fact names it.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use cf_service_auth::{client::InternalServiceClient, OutboundServiceRequest, ServiceId};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{overlap_seconds, UsageFact, MAX_BATCH};

/// A stream position: facts are ordered by `(at, key)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub at: DateTime<Utc>,
    pub key: String,
}

/// One per-day producer total for reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Total {
    pub environment_id: Uuid,
    pub usage_type: &'static str,
    pub quantity: u64,
    pub count: u64,
}

/// Immutable facts the service already records (event facts).
#[async_trait]
pub trait EventSource: Send + Sync {
    /// Stable stream name (`[a-z][a-z0-9_.-]*`).
    fn stream(&self) -> &'static str;
    /// Up to `limit` facts with position `> after` and `<= until`, in position order.
    async fn facts_after(
        &self,
        after: Option<&Position>,
        until: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<(Position, UsageFact)>, String>;
    /// Totals of the facts positioned in `[start, end)`.
    async fn totals(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<Total>, String>;
}

/// A resource stored / active over time.
#[derive(Debug, Clone)]
pub struct ActiveInterval {
    /// Safe reference (`kind:uuid`).
    pub resource_ref: String,
    pub usage_type: &'static str,
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub environment_kind: Option<String>,
    pub active_from: DateTime<Utc>,
    pub active_until: Option<DateTime<Utc>>,
    /// Bytes for STORAGE_BYTE_TIME; ignored for NUMBER_ACTIVE.
    pub weight: u64,
    pub dimensions: BTreeMap<String, Value>,
}

/// Resources metered per completed UTC day (interval facts).
#[async_trait]
pub trait IntervalSource: Send + Sync {
    fn stream(&self) -> &'static str;
    /// The earliest `active_from` (None: nothing to meter yet).
    async fn earliest(&self) -> Result<Option<DateTime<Utc>>, String>;
    /// Resources whose active interval overlaps `[start, end)`.
    async fn active(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<ActiveInterval>, String>;
}

/// The interval facts of one completed day.
pub fn interval_facts(intervals: &[ActiveInterval], day: NaiveDate) -> Vec<UsageFact> {
    let start = day.and_hms_opt(0, 0, 0).expect("midnight").and_utc();
    let end = start + ChronoDuration::days(1);
    intervals
        .iter()
        .filter_map(|i| {
            let secs = overlap_seconds(i.active_from, i.active_until, start, end) as u64;
            if secs == 0 {
                return None;
            }
            let (quantity, unit) = match i.usage_type {
                "STORAGE_BYTE_TIME" => (i.weight.saturating_mul(secs), "BYTE_SECOND"),
                _ => (secs * 1000, "MILLISECOND"),
            };
            if quantity == 0 {
                return None;
            }
            let lo = i.active_from.max(start);
            let hi = i.active_until.unwrap_or(end).min(end);
            Some(
                UsageFact {
                    source_fact_id: format!("{}:{}", i.resource_ref, day.format("%Y%m%d")),
                    usage_type: i.usage_type.to_string(),
                    unit: unit.to_string(),
                    quantity,
                    tenant_id: i.tenant_id,
                    application_id: i.application_id,
                    environment_id: i.environment_id,
                    environment_kind: i.environment_kind.clone(),
                    resource_ref: i.resource_ref.clone(),
                    occurred_at: start,
                    period_start: Some(lo),
                    period_end: Some(hi),
                    dimensions: i.dimensions.clone(),
                    digest: String::new(),
                }
                .normalized()
                .sealed(),
            )
        })
        .collect()
}

/// Exporter settings (`CFMEET_USAGE_EXPORT_*`).
#[derive(Debug, Clone)]
pub struct ExportSettings {
    /// `CFMEET_USAGE_EXPORT_ENABLED` (default false).
    pub enabled: bool,
    /// `CFMEET_USAGE_EXPORT_INTERVAL_SECONDS` (default 30).
    pub interval: Duration,
    /// `CFMEET_USAGE_EXPORT_SETTLE_SECONDS` (default 30): only facts older than this are
    /// exported (a late-committing transaction is never skipped).
    pub settle: Duration,
    /// `CFMEET_USAGE_EXPORT_BATCH` (default 200, at most 500).
    pub batch: usize,
    /// `CFMEET_USAGE_EXPORT_BACKFILL_DAYS` (default 400): how far back interval metering
    /// starts for a stream without a watermark.
    pub backfill_days: i64,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: Duration::from_secs(30),
            settle: Duration::from_secs(30),
            batch: 200,
            backfill_days: 400,
        }
    }
}

impl ExportSettings {
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut s = Self::default();
        let num = |name: &str, min: u64, max: u64| -> Result<Option<u64>, String> {
            match get(name) {
                None => Ok(None),
                Some(v) => v
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (min..=max).contains(n))
                    .map(Some)
                    .ok_or_else(|| format!("{name} must be an integer in {min}..={max}")),
            }
        };
        if let Some(v) = get("CFMEET_USAGE_EXPORT_ENABLED") {
            s.enabled = match v.trim() {
                "true" | "1" => true,
                "false" | "0" | "" => false,
                _ => return Err("CFMEET_USAGE_EXPORT_ENABLED must be true or false".into()),
            };
        }
        if let Some(v) = num("CFMEET_USAGE_EXPORT_INTERVAL_SECONDS", 1, 3600)? {
            s.interval = Duration::from_secs(v);
        }
        if let Some(v) = num("CFMEET_USAGE_EXPORT_SETTLE_SECONDS", 0, 3600)? {
            s.settle = Duration::from_secs(v);
        }
        if let Some(v) = num("CFMEET_USAGE_EXPORT_BATCH", 1, MAX_BATCH as u64)? {
            s.batch = v as usize;
        }
        if let Some(v) = num("CFMEET_USAGE_EXPORT_BACKFILL_DAYS", 1, 3650)? {
            s.backfill_days = v as i64;
        }
        Ok(s)
    }
}

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassReport {
    pub exported: u64,
    pub duplicates: u64,
    pub refused: u64,
    pub reconciled: u64,
}

/// The Developer Platform side of the export.
#[async_trait]
pub trait UsageSink: Send + Sync {
    async fn watermark(&self, stream: &str) -> Result<Option<Position>, String>;
    /// Records `facts` and advances the stream to `advance_to` (one transaction).
    async fn publish(
        &self,
        stream: &str,
        facts: &[UsageFact],
        advance_to: &Position,
    ) -> Result<PassReport, String>;
    async fn reconcile(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        totals: &[Total],
    ) -> Result<u64, String>;
}

/// The Developer Platform over authenticated internal HTTP (`developer-event.publish`).
pub struct DeveloperPlatformSink {
    client: Arc<InternalServiceClient>,
}

impl DeveloperPlatformSink {
    pub fn new(client: Arc<InternalServiceClient>) -> Self {
        Self { client }
    }
}

fn parse_position(v: &Value) -> Option<Position> {
    let at = DateTime::parse_from_rfc3339(v.get("at")?.as_str()?)
        .ok()?
        .with_timezone(&Utc);
    Some(Position {
        at,
        key: v.get("key")?.as_str()?.to_string(),
    })
}

fn position_json(p: &Position) -> Value {
    json!({ "at": p.at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true), "key": p.key })
}

#[async_trait]
impl UsageSink for DeveloperPlatformSink {
    async fn watermark(&self, stream: &str) -> Result<Option<Position>, String> {
        let req = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        let r = self
            .client
            .get(&req, &format!("/internal/v1/usage/streams/{stream}"))
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = r.json().map_err(|e| e.to_string())?;
        Ok(v.get("position").and_then(parse_position))
    }

    async fn publish(
        &self,
        stream: &str,
        facts: &[UsageFact],
        advance_to: &Position,
    ) -> Result<PassReport, String> {
        let req = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        let body =
            json!({ "stream": stream, "facts": facts, "advanceTo": position_json(advance_to) });
        let r = self
            .client
            .post_json(&req, "/internal/v1/usage/facts", &body)
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = r.json().map_err(|e| e.to_string())?;
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Ok(PassReport {
            exported: n("recorded"),
            duplicates: n("duplicates"),
            refused: n("refused"),
            reconciled: 0,
        })
    }

    async fn reconcile(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        totals: &[Total],
    ) -> Result<u64, String> {
        let req = OutboundServiceRequest::new_operation(ServiceId::developer_platform());
        let entries: Vec<Value> = totals
            .iter()
            .map(|t| {
                json!({ "environmentId": t.environment_id, "usageType": t.usage_type,
                        "quantity": t.quantity, "factCount": t.count })
            })
            .collect();
        let body = json!({
            "windowStart": start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "windowEnd": end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "entries": entries,
        });
        let r = self
            .client
            .post_json(&req, "/internal/v1/usage/reconciliations", &body)
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = r.json().map_err(|e| e.to_string())?;
        Ok(v.get("mismatches").and_then(Value::as_u64).unwrap_or(0))
    }
}

/// The exporter of one service.
pub struct Exporter {
    sink: Arc<dyn UsageSink>,
    events: Vec<Arc<dyn EventSource>>,
    intervals: Vec<Arc<dyn IntervalSource>>,
    settings: ExportSettings,
    /// `(stream, day)` already reconciled by this process.
    reconciled: std::sync::Mutex<std::collections::HashSet<(&'static str, NaiveDate)>>,
}

fn day_start(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_opt(0, 0, 0).expect("midnight").and_utc()
}

impl Exporter {
    pub fn new(sink: Arc<dyn UsageSink>, settings: ExportSettings) -> Self {
        Self {
            sink,
            events: Vec::new(),
            intervals: Vec::new(),
            settings,
            reconciled: Default::default(),
        }
    }

    pub fn with_events(mut self, s: Arc<dyn EventSource>) -> Self {
        self.events.push(s);
        self
    }

    pub fn with_intervals(mut self, s: Arc<dyn IntervalSource>) -> Self {
        self.intervals.push(s);
        self
    }

    /// Exports everything settled at `now` (all streams), then reconciles the last
    /// completed UTC day of every event stream.
    pub async fn run_once(&self, now: DateTime<Utc>) -> Result<PassReport, String> {
        let mut report = PassReport::default();
        let until =
            now - ChronoDuration::from_std(self.settings.settle).unwrap_or(ChronoDuration::zero());
        for source in &self.events {
            let mut after = self.sink.watermark(source.stream()).await?;
            loop {
                let batch = source
                    .facts_after(after.as_ref(), until, self.settings.batch)
                    .await?;
                let Some((last, _)) = batch.last() else {
                    break;
                };
                let last = last.clone();
                let facts: Vec<UsageFact> = batch.into_iter().map(|(_, f)| f).collect();
                let r = self.sink.publish(source.stream(), &facts, &last).await?;
                report.exported += r.exported;
                report.duplicates += r.duplicates;
                report.refused += r.refused;
                let full = facts.len() >= self.settings.batch;
                after = Some(last);
                if !full {
                    break;
                }
            }
            // Shadow reconciliation of yesterday (UTC).
            let yesterday = now.date_naive() - ChronoDuration::days(1);
            let (start, end) = (
                day_start(yesterday),
                day_start(yesterday + ChronoDuration::days(1)),
            );
            let fresh = !self
                .reconciled
                .lock()
                .unwrap()
                .contains(&(source.stream(), yesterday));
            if end <= until && fresh {
                self.reconciled
                    .lock()
                    .unwrap()
                    .insert((source.stream(), yesterday));
                let totals = source.totals(start, end).await?;
                if !totals.is_empty() {
                    self.sink.reconcile(start, end, &totals).await?;
                    report.reconciled += totals.len() as u64;
                }
            }
        }
        for source in &self.intervals {
            let today = now.date_naive();
            let watermark = self.sink.watermark(source.stream()).await?;
            let mut day = match watermark {
                Some(p) => p.at.date_naive() + ChronoDuration::days(1),
                None => match source.earliest().await? {
                    Some(t) => t
                        .date_naive()
                        .max(today - ChronoDuration::days(self.settings.backfill_days)),
                    None => continue,
                },
            };
            // Only COMPLETED days (settled) are metered.
            while day_start(day + ChronoDuration::days(1)) <= until {
                let start = day_start(day);
                let active = source
                    .active(start, start + ChronoDuration::days(1))
                    .await?;
                let facts = interval_facts(&active, day);
                let position = Position {
                    at: start,
                    key: "day".into(),
                };
                for chunk in facts.chunks(self.settings.batch.max(1)) {
                    let r = self.sink.publish(source.stream(), chunk, &position).await?;
                    report.exported += r.exported;
                    report.duplicates += r.duplicates;
                    report.refused += r.refused;
                }
                if facts.is_empty() {
                    self.sink.publish(source.stream(), &[], &position).await?;
                }
                // The interval facts are their own reconciliation: totals per day.
                let mut totals: BTreeMap<(Uuid, &'static str), Total> = BTreeMap::new();
                for f in &facts {
                    let ty = crate::definition(&f.usage_type)
                        .map(|d| d.name)
                        .unwrap_or("STORAGE_BYTE_TIME");
                    let e = totals.entry((f.environment_id, ty)).or_insert(Total {
                        environment_id: f.environment_id,
                        usage_type: ty,
                        quantity: 0,
                        count: 0,
                    });
                    e.quantity += f.quantity;
                    e.count += 1;
                }
                if !totals.is_empty() {
                    let t: Vec<Total> = totals.into_values().collect();
                    self.sink
                        .reconcile(start, start + ChronoDuration::days(1), &t)
                        .await?;
                    report.reconciled += t.len() as u64;
                }
                day += ChronoDuration::days(1);
            }
        }
        Ok(report)
    }

    /// Runs [`run_once`](Self::run_once) every interval forever (failures are logged and
    /// retried on the next pass: the watermark only moves on acknowledgement).
    pub async fn run(self: Arc<Self>) {
        loop {
            match self.run_once(Utc::now()).await {
                Ok(r) if r != PassReport::default() => {
                    tracing::info!(
                        exported = r.exported,
                        duplicates = r.duplicates,
                        refused = r.refused,
                        reconciled = r.reconciled,
                        "usage facts exported to the Developer Platform"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "usage export deferred (backlog kept)"),
            }
            tokio::time::sleep(self.settings.interval).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct MemorySink {
        marks: Mutex<BTreeMap<String, Position>>,
        facts: Mutex<BTreeMap<String, UsageFact>>,
        down: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl UsageSink for MemorySink {
        async fn watermark(&self, stream: &str) -> Result<Option<Position>, String> {
            if self.down.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("down".into());
            }
            Ok(self.marks.lock().unwrap().get(stream).cloned())
        }
        async fn publish(
            &self,
            stream: &str,
            facts: &[UsageFact],
            advance_to: &Position,
        ) -> Result<PassReport, String> {
            if self.down.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("down".into());
            }
            let mut r = PassReport::default();
            let mut all = self.facts.lock().unwrap();
            for f in facts {
                if all.insert(f.source_fact_id.clone(), f.clone()).is_some() {
                    r.duplicates += 1;
                } else {
                    r.exported += 1;
                }
            }
            self.marks
                .lock()
                .unwrap()
                .insert(stream.to_string(), advance_to.clone());
            Ok(r)
        }
        async fn reconcile(
            &self,
            _: DateTime<Utc>,
            _: DateTime<Utc>,
            _: &[Total],
        ) -> Result<u64, String> {
            Ok(0)
        }
    }

    struct Fixed(Vec<(Position, UsageFact)>);

    #[async_trait]
    impl EventSource for Fixed {
        fn stream(&self) -> &'static str {
            "test.events"
        }
        async fn facts_after(
            &self,
            after: Option<&Position>,
            until: DateTime<Utc>,
            limit: usize,
        ) -> Result<Vec<(Position, UsageFact)>, String> {
            Ok(self
                .0
                .iter()
                .filter(|(p, _)| after.is_none_or(|a| p > a) && p.at <= until)
                .take(limit)
                .cloned()
                .collect())
        }
        async fn totals(&self, _: DateTime<Utc>, _: DateTime<Utc>) -> Result<Vec<Total>, String> {
            Ok(vec![])
        }
    }

    fn fact(i: usize, at: DateTime<Utc>) -> (Position, UsageFact) {
        let f = UsageFact {
            source_fact_id: format!("sms:{i}"),
            usage_type: "SMS_SEGMENT".into(),
            unit: "SEGMENT".into(),
            quantity: 1,
            tenant_id: Uuid::nil(),
            application_id: Uuid::nil(),
            environment_id: Uuid::nil(),
            environment_kind: None,
            resource_ref: format!("sms:{i}"),
            occurred_at: at,
            period_start: None,
            period_end: None,
            dimensions: BTreeMap::new(),
            digest: String::new(),
        }
        .sealed();
        (
            Position {
                at,
                key: format!("{i:05}"),
            },
            f,
        )
    }

    #[tokio::test]
    async fn export_resumes_from_the_acknowledged_watermark_and_skips_unsettled_facts() {
        let now = Utc::now();
        let old = now - ChronoDuration::minutes(10);
        let mut facts: Vec<_> = (0..7).map(|i| fact(i, old)).collect();
        facts.push(fact(99, now)); // not settled yet
        let sink = Arc::new(MemorySink::default());
        let settings = ExportSettings {
            batch: 3,
            ..ExportSettings::default()
        };
        let exporter = Exporter::new(sink.clone(), settings.clone())
            .with_events(Arc::new(Fixed(facts.clone())));
        // Outage: nothing moves, nothing is lost.
        sink.down.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(exporter.run_once(now).await.is_err());
        sink.down.store(false, std::sync::atomic::Ordering::SeqCst);
        let r = exporter.run_once(now).await.unwrap();
        assert_eq!((r.exported, r.duplicates), (7, 0));
        // A second pass exports nothing new; the unsettled fact follows once settled.
        assert_eq!(exporter.run_once(now).await.unwrap().exported, 0);
        let later = now + ChronoDuration::minutes(5);
        assert_eq!(exporter.run_once(later).await.unwrap().exported, 1);
        assert_eq!(sink.facts.lock().unwrap().len(), 8);
    }

    #[test]
    fn interval_facts_meter_each_completed_day() {
        let day = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let start = day_start(day);
        let i = ActiveInterval {
            resource_ref: "attachment:1".into(),
            usage_type: "STORAGE_BYTE_TIME",
            tenant_id: Uuid::nil(),
            application_id: Uuid::nil(),
            environment_id: Uuid::nil(),
            environment_kind: Some("SANDBOX".into()),
            active_from: start + ChronoDuration::hours(12),
            active_until: None,
            weight: 1000,
            dimensions: BTreeMap::new(),
        };
        let f = interval_facts(std::slice::from_ref(&i), day);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].quantity, 1000 * 43_200);
        assert_eq!(f[0].source_fact_id, "attachment:1:20261001");
        assert!(f[0].validate(crate::producers::MESSAGING).is_ok());
        let mut n = i;
        n.usage_type = "NUMBER_ACTIVE";
        n.resource_ref = "assignment:1".into();
        let f = interval_facts(&[n], day);
        assert_eq!(f[0].quantity, 43_200_000);
        assert!(f[0].validate(crate::producers::NUMBERING).is_ok());
    }
}

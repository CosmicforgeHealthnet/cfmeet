//! Business hours (Part F §58, §77–§84): a schedule in an IANA time zone with an OPEN and
//! a CLOSED route.
//!
//! * Local wall-clock evaluation through `chrono-tz` (the IANA database): an instant is
//!   converted to the schedule's local date and time, so DST transitions (a skipped
//!   spring hour, a repeated autumn hour) and historical offset changes are the library's
//!   job — this module never adds or subtracts offsets itself.
//! * Weekly windows: ISO day of week + local `HH:MM` start / end. A window whose end is
//!   before its start crosses midnight into the next day (Friday 22:00-02:00). Windows
//!   may not overlap anywhere on the week (checked on the 7-day circle).
//! * Exceptions: a local date that is CLOSED or has CUSTOM_HOURS (windows inside that
//!   date). An exception governs its whole local date: neither that date's weekly
//!   windows nor a previous day's window crossing midnight apply on it, and a window
//!   crossing midnight from an exception date never spills into the next day.
//! * Evaluation is a pure, deterministic function of (schedule, instant): OPEN / CLOSED,
//!   why, the local date / time used, and the route it selects.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveDate, Offset, Timelike, Utc};
use chrono_tz::Tz;
use sea_orm::{ConnectionTrait, FromQueryResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    audit::{self, AuditDetails, AuditFact, AuditOperation, AuditResource},
    check_version, clean_name,
    destinations::{self, Destination, DestinationType},
    exec,
    policy::{self, AdminCapability, ResourceStatus},
    scope_values, stmt, time, AdminActor, EnterpriseError, EnterpriseResult, EnterpriseService,
    PageRequest,
};
use crate::calls::command::CallScope;

/// Maximum weekly windows of one schedule.
pub const MAX_WEEKLY_WINDOWS: usize = 42;
/// Maximum exception dates of one schedule.
pub const MAX_EXCEPTIONS: usize = 400;
/// Maximum custom windows of one exception date.
pub const MAX_EXCEPTION_WINDOWS: usize = 6;

const DAY: i32 = 24 * 60;
const WEEK: i32 = 7 * DAY;

text_enum!(
    /// ISO days of the week.
    DayOfWeek {
        Monday = "MONDAY",
        Tuesday = "TUESDAY",
        Wednesday = "WEDNESDAY",
        Thursday = "THURSDAY",
        Friday = "FRIDAY",
        Saturday = "SATURDAY",
        Sunday = "SUNDAY",
    }
);

impl DayOfWeek {
    /// 0 = Monday … 6 = Sunday.
    fn index(self) -> i32 {
        Self::ALL.iter().position(|d| *d == self).unwrap_or(0) as i32
    }

    fn from_index(i: i32) -> Self {
        Self::ALL[i.rem_euclid(7) as usize]
    }
}

text_enum!(
    /// What an exception date does.
    ExceptionKind {
        Closed = "CLOSED",
        CustomHours = "CUSTOM_HOURS",
    }
);

text_enum!(
    /// The evaluation result.
    HoursStatus {
        Open = "OPEN",
        Closed = "CLOSED",
    }
);

text_enum!(
    /// Why the evaluation is OPEN / CLOSED (safe, bounded).
    HoursReason {
        WeeklyWindow = "WEEKLY_WINDOW",
        OutsideWeeklyWindows = "OUTSIDE_WEEKLY_WINDOWS",
        ExceptionClosed = "EXCEPTION_CLOSED",
        ExceptionHours = "EXCEPTION_HOURS",
        OutsideExceptionHours = "OUTSIDE_EXCEPTION_HOURS",
    }
);

/// One weekly open window (API shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WeeklyWindow {
    pub day_of_week: DayOfWeek,
    /// Local `HH:MM`.
    pub start: String,
    /// Local `HH:MM` (`24:00` = end of day); before `start` = the next day.
    pub end: String,
}

/// One open window inside an exception date (API shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DayWindow {
    pub start: String,
    pub end: String,
}

/// One exception date (API shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HoursException {
    /// Local date `YYYY-MM-DD`.
    pub date: NaiveDate,
    pub kind: ExceptionKind,
    #[serde(default)]
    pub windows: Vec<DayWindow>,
}

/// `HH:MM` → minutes of the day (`24:00` only where `end` is allowed).
fn parse_hhmm(s: &str, allow_end_of_day: bool) -> Option<i32> {
    let (h, m) = s.split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    if h == 24 && m == 0 && allow_end_of_day {
        return Some(DAY);
    }
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

fn fmt_hhmm(minutes: i32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// A validated, evaluable schedule.
#[derive(Debug, Clone)]
pub struct Schedule {
    tz: Tz,
    /// (day 0..6, start minute, end minute); end < start crosses midnight.
    weekly: Vec<(i32, i32, i32)>,
    exceptions: BTreeMap<NaiveDate, (ExceptionKind, Vec<(i32, i32)>)>,
}

/// The result of evaluating a schedule at an instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub status: HoursStatus,
    pub reason: HoursReason,
    pub local_date: NaiveDate,
    /// Local `HH:MM`.
    pub local_time: String,
    /// The local UTC offset at the instant (`+01:00`).
    pub utc_offset: String,
}

/// An IANA time-zone identifier the library knows (never a raw offset).
pub fn parse_timezone(name: &str) -> EnterpriseResult<Tz> {
    if name.is_empty() || name.len() > 64 {
        return Err(EnterpriseError::Validation(
            "timezone must be an IANA time-zone identifier.",
        ));
    }
    name.parse::<Tz>()
        .map_err(|_| EnterpriseError::Validation("timezone must be an IANA time-zone identifier."))
}

impl Schedule {
    /// Validates the API shape into an evaluable schedule.
    pub fn build(
        timezone: &str,
        weekly: &[WeeklyWindow],
        exceptions: &[HoursException],
    ) -> EnterpriseResult<Self> {
        let tz = parse_timezone(timezone)?;
        if weekly.len() > MAX_WEEKLY_WINDOWS {
            return Err(EnterpriseError::Validation(
                "a schedule has at most 42 weekly windows.",
            ));
        }
        let mut parsed = Vec::with_capacity(weekly.len());
        for w in weekly {
            let start = parse_hhmm(&w.start, false);
            let end = parse_hhmm(&w.end, true);
            let (Some(start), Some(end)) = (start, end) else {
                return Err(EnterpriseError::Validation(
                    "window times must be HH:MM (end may be 24:00).",
                ));
            };
            if start == end {
                return Err(EnterpriseError::Validation(
                    "a window's start and end must differ.",
                ));
            }
            parsed.push((w.day_of_week.index(), start, end));
        }
        // Overlaps on the 7-day circle (a window crossing Sunday midnight wraps to Monday).
        let mut spans: Vec<(i32, i32)> = Vec::new();
        for &(day, start, end) in &parsed {
            let from = day * DAY + start;
            let to = if end > start {
                day * DAY + end
            } else {
                (day + 1) * DAY + end
            };
            if to > WEEK {
                spans.push((from, WEEK));
                spans.push((0, to - WEEK));
            } else {
                spans.push((from, to));
            }
        }
        spans.sort();
        if spans.windows(2).any(|w| w[1].0 < w[0].1) {
            return Err(EnterpriseError::Validation(
                "weekly windows must not overlap.",
            ));
        }
        if exceptions.len() > MAX_EXCEPTIONS {
            return Err(EnterpriseError::Validation(
                "a schedule has at most 400 exception dates.",
            ));
        }
        let mut by_date = BTreeMap::new();
        for ex in exceptions {
            let windows = match ex.kind {
                ExceptionKind::Closed if ex.windows.is_empty() => Vec::new(),
                ExceptionKind::Closed => {
                    return Err(EnterpriseError::Validation(
                        "a CLOSED exception has no windows.",
                    ))
                }
                ExceptionKind::CustomHours => {
                    if ex.windows.is_empty() || ex.windows.len() > MAX_EXCEPTION_WINDOWS {
                        return Err(EnterpriseError::Validation(
                            "a CUSTOM_HOURS exception has 1-6 windows.",
                        ));
                    }
                    let mut ws = Vec::with_capacity(ex.windows.len());
                    for w in &ex.windows {
                        match (parse_hhmm(&w.start, false), parse_hhmm(&w.end, true)) {
                            (Some(s), Some(e)) if e > s => ws.push((s, e)),
                            _ => {
                                return Err(EnterpriseError::Validation(
                                    "an exception window is HH:MM-HH:MM inside its date.",
                                ))
                            }
                        }
                    }
                    ws.sort();
                    if ws.windows(2).any(|w| w[1].0 < w[0].1) {
                        return Err(EnterpriseError::Validation(
                            "exception windows must not overlap.",
                        ));
                    }
                    ws
                }
            };
            if by_date.insert(ex.date, (ex.kind, windows)).is_some() {
                return Err(EnterpriseError::Validation(
                    "an exception date appears twice.",
                ));
            }
        }
        Ok(Self {
            tz,
            weekly: parsed,
            exceptions: by_date,
        })
    }

    pub fn timezone(&self) -> Tz {
        self.tz
    }

    /// OPEN / CLOSED at `at` (deterministic).
    pub fn evaluate(&self, at: DateTime<Utc>) -> Evaluation {
        let local = at.with_timezone(&self.tz);
        let date = local.date_naive();
        let minute = (local.hour() * 60 + local.minute()) as i32;
        let day = local.weekday().num_days_from_monday() as i32;
        let (status, reason) = match self.exceptions.get(&date) {
            Some((ExceptionKind::Closed, _)) => (HoursStatus::Closed, HoursReason::ExceptionClosed),
            Some((_, windows)) => {
                if windows.iter().any(|&(s, e)| s <= minute && minute < e) {
                    (HoursStatus::Open, HoursReason::ExceptionHours)
                } else {
                    (HoursStatus::Closed, HoursReason::OutsideExceptionHours)
                }
            }
            None => {
                let yesterday_is_exception = date
                    .pred_opt()
                    .is_some_and(|d| self.exceptions.contains_key(&d));
                let open = self.weekly.iter().any(|&(d, s, e)| {
                    if e > s {
                        d == day && s <= minute && minute < e
                    } else {
                        (d == day && minute >= s)
                            || (!yesterday_is_exception
                                && DayOfWeek::from_index(d + 1).index() == day
                                && minute < e)
                    }
                });
                if open {
                    (HoursStatus::Open, HoursReason::WeeklyWindow)
                } else {
                    (HoursStatus::Closed, HoursReason::OutsideWeeklyWindows)
                }
            }
        };
        let offset = local.offset().fix().local_minus_utc();
        let sign = if offset < 0 { '-' } else { '+' };
        let offset = offset.abs();
        Evaluation {
            status,
            reason,
            local_date: date,
            local_time: fmt_hhmm(minute),
            utc_offset: format!("{sign}{:02}:{:02}", offset / 3600, (offset % 3600) / 60),
        }
    }
}

/// Kinds the OPEN route may take.
pub fn open_types() -> &'static [DestinationType] {
    &[
        DestinationType::EnterpriseMember,
        DestinationType::Principal,
        DestinationType::Extension,
        DestinationType::RingGroup,
        DestinationType::Queue,
        DestinationType::Ivr,
    ]
}

/// Kinds the CLOSED route may take (as configured kinds become available).
pub fn closed_types() -> &'static [DestinationType] {
    &[
        DestinationType::EnterpriseMember,
        DestinationType::Principal,
        DestinationType::Extension,
        DestinationType::RingGroup,
        DestinationType::Queue,
        DestinationType::Ivr,
        DestinationType::PublicContact,
    ]
}

#[derive(Debug, Clone, FromQueryResult)]
pub(crate) struct HoursRow {
    pub id: Uuid,
    pub name: String,
    pub timezone: String,
    pub open_destination_type: String,
    pub open_destination_id: Uuid,
    pub closed_destination_type: String,
    pub closed_destination_id: Uuid,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

impl HoursRow {
    pub(crate) fn open(&self) -> Option<Destination> {
        Some(Destination::new(
            DestinationType::parse(&self.open_destination_type)?,
            self.open_destination_id,
        ))
    }

    pub(crate) fn closed(&self) -> Option<Destination> {
        Some(Destination::new(
            DestinationType::parse(&self.closed_destination_type)?,
            self.closed_destination_id,
        ))
    }
}

const COLUMNS: &str = "id, name, timezone, open_destination_type, open_destination_id, \
    closed_destination_type, closed_destination_id, status, version, created_at, updated_at, \
    disabled_at";

#[derive(Debug, Clone, FromQueryResult)]
struct WindowRow {
    day_of_week: Option<i16>,
    exception_date: Option<NaiveDate>,
    start_minute: i16,
    end_minute: i16,
}

#[derive(Debug, Clone, FromQueryResult)]
struct ExceptionRow {
    exception_date: NaiveDate,
    kind: String,
}

/// The schedule as stored (API shape).
#[derive(Debug, Clone)]
pub(crate) struct StoredSchedule {
    pub weekly: Vec<WeeklyWindow>,
    pub exceptions: Vec<HoursException>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessHoursView {
    pub business_hours_id: Uuid,
    pub name: String,
    pub timezone: String,
    pub open_destination: Option<Destination>,
    pub closed_destination: Option<Destination>,
    pub weekly_windows: Vec<WeeklyWindow>,
    pub exceptions: Vec<HoursException>,
    pub status: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BusinessHoursPage {
    pub business_hours: Vec<BusinessHoursView>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationView {
    pub business_hours_id: Uuid,
    pub version: i64,
    pub at: String,
    pub status: HoursStatus,
    pub reason: HoursReason,
    pub timezone: String,
    pub local_date: NaiveDate,
    pub local_time: String,
    pub utc_offset: String,
    /// The route the evaluation selects (OPEN → open route, CLOSED → closed route).
    pub destination: Option<Destination>,
}

/// Create / replace input.
#[derive(Debug, Clone)]
pub struct HoursSettings {
    pub name: String,
    pub timezone: String,
    pub open_destination: Destination,
    pub closed_destination: Destination,
    pub weekly_windows: Vec<WeeklyWindow>,
    pub exceptions: Vec<HoursException>,
}

pub(crate) async fn hours_row<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    id: Uuid,
    lock: bool,
) -> EnterpriseResult<HoursRow> {
    let mut values = scope_values(scope);
    values.push(id.into());
    HoursRow::find_by_statement(stmt(
        &format!(
            "SELECT {COLUMNS} FROM enterprise_business_hours \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3 AND id = $4{}",
            if lock { " FOR UPDATE" } else { "" }
        ),
        values,
    ))
    .one(db)
    .await?
    .ok_or(EnterpriseError::NotFound)
}

/// The stored windows / exceptions of a schedule.
pub(crate) async fn stored_schedule<C: ConnectionTrait>(
    db: &C,
    rule_id: Uuid,
) -> EnterpriseResult<StoredSchedule> {
    let windows = WindowRow::find_by_statement(stmt(
        "SELECT day_of_week, exception_date, start_minute, end_minute \
           FROM enterprise_business_hours_windows WHERE rule_id = $1 \
          ORDER BY exception_date NULLS FIRST, day_of_week, start_minute",
        vec![rule_id.into()],
    ))
    .all(db)
    .await?;
    let exceptions = ExceptionRow::find_by_statement(stmt(
        "SELECT exception_date, kind FROM enterprise_business_hours_exceptions \
          WHERE rule_id = $1 ORDER BY exception_date",
        vec![rule_id.into()],
    ))
    .all(db)
    .await?;
    let mut weekly = Vec::new();
    let mut custom: BTreeMap<NaiveDate, Vec<DayWindow>> = BTreeMap::new();
    for w in windows {
        let start = fmt_hhmm(w.start_minute as i32);
        let end = fmt_hhmm(w.end_minute as i32);
        match (w.day_of_week, w.exception_date) {
            (Some(d), None) => weekly.push(WeeklyWindow {
                day_of_week: DayOfWeek::from_index(d as i32 - 1),
                start,
                end,
            }),
            (None, Some(date)) => custom
                .entry(date)
                .or_default()
                .push(DayWindow { start, end }),
            _ => {}
        }
    }
    let exceptions = exceptions
        .into_iter()
        .map(|e| HoursException {
            date: e.exception_date,
            kind: ExceptionKind::parse(&e.kind).unwrap_or(ExceptionKind::Closed),
            windows: custom.remove(&e.exception_date).unwrap_or_default(),
        })
        .collect();
    Ok(StoredSchedule { weekly, exceptions })
}

/// Loads and builds the evaluable schedule of a rule.
pub(crate) async fn schedule_of<C: ConnectionTrait>(
    db: &C,
    row: &HoursRow,
) -> EnterpriseResult<Schedule> {
    let stored = stored_schedule(db, row.id).await?;
    Schedule::build(&row.timezone, &stored.weekly, &stored.exceptions)
}

async fn view<C: ConnectionTrait>(db: &C, row: HoursRow) -> EnterpriseResult<BusinessHoursView> {
    let stored = stored_schedule(db, row.id).await?;
    Ok(BusinessHoursView {
        open_destination: row.open(),
        closed_destination: row.closed(),
        business_hours_id: row.id,
        name: row.name,
        timezone: row.timezone,
        weekly_windows: stored.weekly,
        exceptions: stored.exceptions,
        status: row.status,
        version: row.version,
        created_at: time(row.created_at),
        updated_at: time(row.updated_at),
        disabled_at: row.disabled_at.map(time),
    })
}

/// Replaces the stored windows / exceptions (inside the caller's transaction).
async fn write_schedule<C: ConnectionTrait>(
    db: &C,
    scope: CallScope,
    rule_id: Uuid,
    settings: &HoursSettings,
    now: DateTime<Utc>,
) -> EnterpriseResult<()> {
    exec(
        db,
        "DELETE FROM enterprise_business_hours_exceptions WHERE rule_id = $1",
        vec![rule_id.into()],
    )
    .await?;
    exec(
        db,
        "DELETE FROM enterprise_business_hours_windows WHERE rule_id = $1",
        vec![rule_id.into()],
    )
    .await?;
    for w in &settings.weekly_windows {
        let mut values = vec![Uuid::new_v4().into()];
        values.extend(scope_values(scope));
        values.extend([
            rule_id.into(),
            ((w.day_of_week.index() + 1) as i16).into(),
            (parse_hhmm(&w.start, false).unwrap_or(0) as i16).into(),
            (parse_hhmm(&w.end, true).unwrap_or(DAY) as i16).into(),
        ]);
        exec(
            db,
            "INSERT INTO enterprise_business_hours_windows (id, tenant_id, application_id, \
             environment_id, rule_id, day_of_week, start_minute, end_minute) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            values,
        )
        .await?;
    }
    for ex in &settings.exceptions {
        let mut values = scope_values(scope);
        values.extend([
            rule_id.into(),
            ex.date.into(),
            ex.kind.as_str().into(),
            now.into(),
        ]);
        exec(
            db,
            "INSERT INTO enterprise_business_hours_exceptions (tenant_id, application_id, \
             environment_id, rule_id, exception_date, kind, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            values,
        )
        .await?;
        for w in &ex.windows {
            let mut values = vec![Uuid::new_v4().into()];
            values.extend(scope_values(scope));
            values.extend([
                rule_id.into(),
                ex.date.into(),
                (parse_hhmm(&w.start, false).unwrap_or(0) as i16).into(),
                (parse_hhmm(&w.end, true).unwrap_or(DAY) as i16).into(),
            ]);
            exec(
                db,
                "INSERT INTO enterprise_business_hours_windows (id, tenant_id, application_id, \
                 environment_id, rule_id, exception_date, start_minute, end_minute) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                values,
            )
            .await?;
        }
    }
    Ok(())
}

impl EnterpriseService {
    async fn validate_hours(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        scope: CallScope,
        settings: &HoursSettings,
    ) -> EnterpriseResult<String> {
        let name = clean_name(&settings.name, 100, "name must be 1-100 characters.")?;
        Schedule::build(
            &settings.timezone,
            &settings.weekly_windows,
            &settings.exceptions,
        )?;
        destinations::validate(txn, scope, settings.open_destination, open_types()).await?;
        destinations::validate(txn, scope, settings.closed_destination, closed_types()).await?;
        if settings.closed_destination.kind == DestinationType::PublicContact {
            policy::require_capability(txn, scope, AdminCapability::ManagePublicForwarding).await?;
            let rules = policy::effective_policy(txn, scope, None).await?.rules;
            if !(rules.forwarding_allowed && rules.public_forwarding_allowed) {
                return Err(EnterpriseError::PolicyDenied(
                    "The enterprise policy does not permit public forwarding.",
                ));
            }
        }
        Ok(name)
    }

    pub async fn create_business_hours(
        &self,
        actor: &AdminActor,
        settings: HoursSettings,
    ) -> EnterpriseResult<BusinessHoursView> {
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let name = self.validate_hours(&txn, actor.scope, &settings).await?;
        let id = Uuid::new_v4();
        let now = self.now();
        let mut values = vec![id.into()];
        values.extend(scope_values(actor.scope));
        values.extend([
            name.into(),
            settings.timezone.clone().into(),
            settings.open_destination.kind.as_str().into(),
            settings.open_destination.id.into(),
            settings.closed_destination.kind.as_str().into(),
            settings.closed_destination.id.into(),
            now.into(),
        ]);
        exec(
            &txn,
            "INSERT INTO enterprise_business_hours (id, tenant_id, application_id, environment_id, \
             name, timezone, open_destination_type, open_destination_id, closed_destination_type, \
             closed_destination_id, status, version, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'ACTIVE', 1, $11, $11)",
            values,
        )
        .await?;
        write_schedule(&txn, actor.scope, id, &settings, now).await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::BusinessHours,
                resource_id: id,
                operation: AuditOperation::Create,
                old_version: None,
                new_version: Some(1),
                details: AuditDetails::new()
                    .with("weeklyWindows", settings.weekly_windows.len())
                    .with("exceptions", settings.exceptions.len()),
            },
            now,
        )
        .await?;
        let out = view(&txn, hours_row(&txn, actor.scope, id, false).await?).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[("resource", "BUSINESS_HOURS"), ("operation", "CREATE")],
        );
        Ok(out)
    }

    pub async fn get_business_hours(
        &self,
        scope: CallScope,
        id: Uuid,
    ) -> EnterpriseResult<BusinessHoursView> {
        view(self.db(), hours_row(self.db(), scope, id, false).await?).await
    }

    pub async fn list_business_hours(
        &self,
        scope: CallScope,
        status: Option<ResourceStatus>,
        page: &PageRequest,
    ) -> EnterpriseResult<BusinessHoursPage> {
        let limit = page.limit()?;
        let mut values = scope_values(scope);
        let mut sql = format!(
            "SELECT {COLUMNS} FROM enterprise_business_hours \
             WHERE tenant_id = $1 AND application_id = $2 AND environment_id = $3"
        );
        if let Some(status) = status {
            values.push(status.as_str().into());
            sql.push_str(&format!(" AND status = ${}", values.len()));
        }
        if let Some((at, id)) = page.after()? {
            values.push(at.into());
            values.push(id.into());
            sql.push_str(&format!(
                " AND (created_at, id) > (${}, ${})",
                values.len() - 1,
                values.len()
            ));
        }
        sql.push_str(&format!(" ORDER BY created_at, id LIMIT {}", limit + 1));
        let mut rows = HoursRow::find_by_statement(stmt(&sql, values))
            .all(self.db())
            .await?;
        let next_cursor = if rows.len() as u64 > limit {
            rows.truncate(limit as usize);
            rows.last()
                .map(|r| super::cursor::encode(r.created_at, r.id))
        } else {
            None
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(view(self.db(), row).await?);
        }
        Ok(BusinessHoursPage {
            business_hours: out,
            next_cursor,
        })
    }

    /// Replaces the whole schedule (name, zone, routes, windows, exceptions) atomically.
    pub async fn replace_business_hours(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
        settings: HoursSettings,
    ) -> EnterpriseResult<BusinessHoursView> {
        let txn = self.begin().await?;
        policy::require_capability(&txn, actor.scope, AdminCapability::ManageRouting).await?;
        let row = hours_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status != "ACTIVE" {
            return Err(EnterpriseError::Conflict(
                "disabled business hours cannot be changed",
            ));
        }
        let name = self.validate_hours(&txn, actor.scope, &settings).await?;
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_business_hours SET name = $2, timezone = $3, \
             open_destination_type = $4, open_destination_id = $5, closed_destination_type = $6, \
             closed_destination_id = $7, version = version + 1, \
             updated_at = GREATEST(updated_at, $8) WHERE id = $1",
            vec![
                id.into(),
                name.into(),
                settings.timezone.clone().into(),
                settings.open_destination.kind.as_str().into(),
                settings.open_destination.id.into(),
                settings.closed_destination.kind.as_str().into(),
                settings.closed_destination.id.into(),
                now.into(),
            ],
        )
        .await?;
        write_schedule(&txn, actor.scope, id, &settings, now).await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::BusinessHours,
                resource_id: id,
                operation: AuditOperation::ReplaceSchedule,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new()
                    .with("weeklyWindows", settings.weekly_windows.len())
                    .with("exceptions", settings.exceptions.len())
                    .with("timezoneChanged", row.timezone != settings.timezone),
            },
            now,
        )
        .await?;
        let out = view(&txn, hours_row(&txn, actor.scope, id, false).await?).await?;
        txn.commit().await?;
        crate::metrics::inc(
            "calling_enterprise_config_changes_total",
            &[
                ("resource", "BUSINESS_HOURS"),
                ("operation", "REPLACE_SCHEDULE"),
            ],
        );
        Ok(out)
    }

    pub async fn disable_business_hours(
        &self,
        actor: &AdminActor,
        id: Uuid,
        expected_version: i64,
    ) -> EnterpriseResult<BusinessHoursView> {
        let txn = self.begin().await?;
        let row = hours_row(&txn, actor.scope, id, true).await?;
        check_version(expected_version, row.version)?;
        if row.status == "DISABLED" {
            return Err(EnterpriseError::Conflict(
                "the business hours are already disabled",
            ));
        }
        let now = self.now();
        exec(
            &txn,
            "UPDATE enterprise_business_hours SET status = 'DISABLED', disabled_at = $2, \
             version = version + 1, updated_at = GREATEST(updated_at, $2) WHERE id = $1",
            vec![id.into(), now.into()],
        )
        .await?;
        audit::record(
            &txn,
            actor,
            AuditFact {
                resource: AuditResource::BusinessHours,
                resource_id: id,
                operation: AuditOperation::Disable,
                old_version: Some(row.version),
                new_version: Some(row.version + 1),
                details: AuditDetails::new(),
            },
            now,
        )
        .await?;
        let out = view(&txn, hours_row(&txn, actor.scope, id, false).await?).await?;
        txn.commit().await?;
        Ok(out)
    }

    /// Evaluates a schedule at `at` (the current time when absent).
    pub async fn evaluate_business_hours(
        &self,
        scope: CallScope,
        id: Uuid,
        at: Option<DateTime<Utc>>,
    ) -> EnterpriseResult<EvaluationView> {
        let row = hours_row(self.db(), scope, id, false).await?;
        let schedule = schedule_of(self.db(), &row).await?;
        let at = at.unwrap_or_else(|| self.now());
        let e = schedule.evaluate(at);
        let destination = match e.status {
            HoursStatus::Open => row.open(),
            HoursStatus::Closed => row.closed(),
        };
        Ok(EvaluationView {
            business_hours_id: row.id,
            version: row.version,
            at: time(at),
            status: e.status,
            reason: e.reason,
            timezone: row.timezone,
            local_date: e.local_date,
            local_time: e.local_time,
            utc_offset: e.utc_offset,
            destination,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn minute_before(at: DateTime<Utc>) -> DateTime<Utc> {
        at - Duration::minutes(1)
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    fn w(day: DayOfWeek, start: &str, end: &str) -> WeeklyWindow {
        WeeklyWindow {
            day_of_week: day,
            start: start.into(),
            end: end.into(),
        }
    }

    fn weekdays_9_to_5(tz: &str) -> Schedule {
        let days = [
            DayOfWeek::Monday,
            DayOfWeek::Tuesday,
            DayOfWeek::Wednesday,
            DayOfWeek::Thursday,
            DayOfWeek::Friday,
        ];
        let weekly: Vec<_> = days.iter().map(|d| w(*d, "09:00", "17:00")).collect();
        Schedule::build(tz, &weekly, &[]).unwrap()
    }

    fn open(s: &Schedule, at: DateTime<Utc>) -> bool {
        s.evaluate(at).status == HoursStatus::Open
    }

    #[test]
    fn only_iana_zones_and_valid_windows() {
        assert!(parse_timezone("Europe/London").is_ok());
        assert!(parse_timezone("Africa/Lagos").is_ok());
        for bad in ["", "+01:00", "GMT+1x", "Mars/Olympus", "../etc", "UTC+1"] {
            assert!(parse_timezone(bad).is_err(), "{bad}");
        }
        let bad_windows = [
            vec![w(DayOfWeek::Monday, "9:00", "17:00")],
            vec![w(DayOfWeek::Monday, "09:00", "09:00")],
            vec![w(DayOfWeek::Monday, "24:00", "10:00")],
            vec![w(DayOfWeek::Monday, "09:00", "25:00")],
            vec![w(DayOfWeek::Monday, "09:60", "10:00")],
            // Overlaps (same day; crossing midnight into the next day's window; Sunday
            // night wrapping into Monday morning).
            vec![
                w(DayOfWeek::Monday, "09:00", "12:00"),
                w(DayOfWeek::Monday, "11:00", "13:00"),
            ],
            vec![
                w(DayOfWeek::Friday, "22:00", "02:00"),
                w(DayOfWeek::Saturday, "01:00", "05:00"),
            ],
            vec![
                w(DayOfWeek::Sunday, "23:00", "03:00"),
                w(DayOfWeek::Monday, "02:00", "04:00"),
            ],
        ];
        for windows in bad_windows {
            assert!(
                Schedule::build("UTC", &windows, &[]).is_err(),
                "{windows:?}"
            );
        }
        // Adjacent windows are fine.
        assert!(Schedule::build(
            "UTC",
            &[
                w(DayOfWeek::Monday, "09:00", "12:00"),
                w(DayOfWeek::Monday, "12:00", "24:00"),
                w(DayOfWeek::Tuesday, "00:00", "08:00"),
            ],
            &[]
        )
        .is_ok());
        let date = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap();
        let ex = |kind, windows: Vec<DayWindow>| HoursException {
            date,
            kind,
            windows,
        };
        let dw = |s: &str, e: &str| DayWindow {
            start: s.into(),
            end: e.into(),
        };
        for bad in [
            vec![ex(ExceptionKind::Closed, vec![dw("09:00", "10:00")])],
            vec![ex(ExceptionKind::CustomHours, vec![])],
            vec![ex(ExceptionKind::CustomHours, vec![dw("22:00", "02:00")])],
            vec![ex(
                ExceptionKind::CustomHours,
                vec![dw("09:00", "11:00"), dw("10:00", "12:00")],
            )],
            vec![
                ex(ExceptionKind::Closed, vec![]),
                ex(ExceptionKind::Closed, vec![]),
            ],
        ] {
            assert!(Schedule::build("UTC", &[], &bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn weekly_windows_in_the_local_zone() {
        // Lagos is UTC+1 all year (no DST).
        let s = weekdays_9_to_5("Africa/Lagos");
        // Monday 2026-10-05: 09:00 local = 08:00 UTC.
        assert!(!open(&s, utc(2026, 10, 5, 7, 59)));
        assert!(open(&s, utc(2026, 10, 5, 8, 0)));
        assert!(open(&s, utc(2026, 10, 5, 15, 59)));
        assert!(!open(&s, utc(2026, 10, 5, 16, 0)));
        // Saturday.
        assert!(!open(&s, utc(2026, 10, 10, 12, 0)));
        let e = s.evaluate(utc(2026, 10, 5, 8, 0));
        assert_eq!(e.reason, HoursReason::WeeklyWindow);
        assert_eq!(e.local_time, "09:00");
        assert_eq!(e.utc_offset, "+01:00");
        assert_eq!(e.local_date, NaiveDate::from_ymd_opt(2026, 10, 5).unwrap());
        // Deterministic.
        assert_eq!(s.evaluate(utc(2026, 10, 5, 8, 0)), e);
    }

    #[test]
    fn spring_forward_skips_an_hour_of_wall_clock() {
        // Europe/London, Sunday 2026-03-29: 01:00 GMT -> 02:00 BST.
        let s = Schedule::build(
            "Europe/London",
            &[w(DayOfWeek::Sunday, "01:00", "03:00")],
            &[],
        )
        .unwrap();
        // 00:59 UTC = 00:59 GMT (before the window).
        assert!(!open(&s, utc(2026, 3, 29, 0, 59)));
        // 01:00 UTC = 02:00 BST (local 01:00-02:00 never exists): open.
        let e = s.evaluate(utc(2026, 3, 29, 1, 0));
        assert_eq!(
            (e.status, e.local_time.as_str(), e.utc_offset.as_str()),
            (HoursStatus::Open, "02:00", "+01:00")
        );
        assert!(open(&s, utc(2026, 3, 29, 1, 59)));
        // 02:00 UTC = 03:00 BST: closed. The window lasted ONE real hour.
        assert!(!open(&s, utc(2026, 3, 29, 2, 0)));
        // New York springs forward on 2026-03-08 (02:00 EST -> 03:00 EDT).
        let ny = weekdays_9_to_5("America/New_York");
        // Monday 2026-03-09 09:00 EDT = 13:00 UTC (it was 14:00 UTC the week before).
        assert!(open(&ny, utc(2026, 3, 9, 13, 0)));
        assert!(!open(&ny, utc(2026, 3, 9, 12, 59)));
        assert!(!open(&ny, utc(2026, 3, 2, 13, 30)));
        assert!(open(&ny, utc(2026, 3, 2, 14, 0)));
    }

    #[test]
    fn autumn_back_repeats_an_hour_of_wall_clock() {
        // Europe/London, Sunday 2026-10-25: 02:00 BST -> 01:00 GMT (01:xx happens twice).
        let s = Schedule::build(
            "Europe/London",
            &[w(DayOfWeek::Sunday, "01:00", "02:00")],
            &[],
        )
        .unwrap();
        assert!(!open(&s, utc(2026, 10, 24, 23, 59))); // 00:59 BST
        let first = s.evaluate(utc(2026, 10, 25, 0, 0)); // 01:00 BST
        assert_eq!(
            (first.status, first.utc_offset.as_str()),
            (HoursStatus::Open, "+01:00")
        );
        assert!(open(&s, utc(2026, 10, 25, 0, 59))); // 01:59 BST
        let second = s.evaluate(utc(2026, 10, 25, 1, 0)); // 01:00 GMT (again)
        assert_eq!(
            (
                second.status,
                second.local_time.as_str(),
                second.utc_offset.as_str()
            ),
            (HoursStatus::Open, "01:00", "+00:00")
        );
        assert!(open(&s, utc(2026, 10, 25, 1, 59))); // 01:59 GMT
        assert!(!open(&s, utc(2026, 10, 25, 2, 0))); // 02:00 GMT: two real hours open.
                                                     // Southern hemisphere: Sydney leaves DST on 2026-04-05 (03:00 AEDT -> 02:00 AEST).
        let syd = weekdays_9_to_5("Australia/Sydney");
        // Monday 2026-04-06 09:00 AEST = Sunday 23:00 UTC.
        assert!(open(&syd, utc(2026, 4, 5, 23, 0)));
        assert!(!open(&syd, utc(2026, 4, 5, 22, 59)));
    }

    #[test]
    fn windows_cross_midnight_and_the_week() {
        let s = Schedule::build(
            "UTC",
            &[
                w(DayOfWeek::Friday, "22:00", "02:00"),
                w(DayOfWeek::Sunday, "23:00", "01:00"),
            ],
            &[],
        )
        .unwrap();
        // Friday 2026-10-09.
        assert!(!open(&s, utc(2026, 10, 9, 21, 59)));
        assert!(open(&s, utc(2026, 10, 9, 22, 0)));
        assert!(open(&s, utc(2026, 10, 10, 1, 59))); // Saturday 01:59
        assert!(!open(&s, utc(2026, 10, 10, 2, 0)));
        assert!(!open(&s, utc(2026, 10, 11, 1, 0))); // Sunday 01:00: not Friday's
                                                     // Sunday 23:00 -> Monday 01:00 (wraps the week).
        assert!(open(&s, utc(2026, 10, 11, 23, 30)));
        assert!(open(&s, utc(2026, 10, 12, 0, 30)));
        assert!(!open(&s, utc(2026, 10, 12, 1, 0)));
    }

    #[test]
    fn exceptions_govern_their_whole_local_date() {
        let fri = NaiveDate::from_ymd_opt(2026, 12, 25).unwrap(); // a Friday
        let thu = NaiveDate::from_ymd_opt(2026, 12, 24).unwrap();
        let mut weekly = vec![w(DayOfWeek::Friday, "22:00", "02:00")];
        for d in [DayOfWeek::Thursday, DayOfWeek::Friday] {
            weekly.push(w(d, "09:00", "17:00"));
        }
        let s = Schedule::build(
            "Europe/London",
            &weekly,
            &[
                HoursException {
                    date: fri,
                    kind: ExceptionKind::Closed,
                    windows: vec![],
                },
                HoursException {
                    date: thu,
                    kind: ExceptionKind::CustomHours,
                    windows: vec![DayWindow {
                        start: "09:00".into(),
                        end: "12:00".into(),
                    }],
                },
            ],
        )
        .unwrap();
        // Christmas Eve: custom 09:00-12:00 only (GMT in December).
        assert_eq!(
            s.evaluate(utc(2026, 12, 24, 10, 0)).reason,
            HoursReason::ExceptionHours
        );
        let late = s.evaluate(utc(2026, 12, 24, 13, 0));
        assert_eq!(
            (late.status, late.reason),
            (HoursStatus::Closed, HoursReason::OutsideExceptionHours)
        );
        // Christmas Day: closed all day, including its evening window...
        let xmas = s.evaluate(utc(2026, 12, 25, 10, 0));
        assert_eq!(
            (xmas.status, xmas.reason),
            (HoursStatus::Closed, HoursReason::ExceptionClosed)
        );
        assert!(!open(&s, utc(2026, 12, 25, 23, 0)));
        // ...and its window never spills into Saturday.
        assert!(!open(&s, utc(2026, 12, 26, 1, 0)));
        // An ordinary Friday night still spills.
        assert!(open(&s, utc(2026, 12, 19, 1, 0)));
    }

    #[test]
    fn changing_the_zone_moves_the_boundaries() {
        // The same instant: open in Lagos (10:00 WAT), closed in Los Angeles (02:00 PDT).
        let at = utc(2026, 10, 5, 9, 0);
        assert!(open(&weekdays_9_to_5("Africa/Lagos"), at));
        assert!(!open(&weekdays_9_to_5("America/Los_Angeles"), at));
        // Between the US (2026-03-08) and EU (2026-03-29) transitions the London - New York
        // gap is 4 h, not 5 h: 13:30 UTC on Monday 2026-03-16 is 09:30 in New York.
        let ny = weekdays_9_to_5("America/New_York");
        assert!(open(&ny, utc(2026, 3, 16, 13, 30)));
        assert!(!open(&ny, utc(2026, 3, 23, 12, 30)));
        assert_eq!(
            minute_before(utc(2026, 3, 16, 13, 0)),
            utc(2026, 3, 16, 12, 59)
        );
        assert!(!open(&ny, minute_before(utc(2026, 3, 16, 13, 0))));
        // A zone whose rules changed: Istanbul moved to permanent +03:00 in 2016.
        let ist = weekdays_9_to_5("Europe/Istanbul");
        assert_eq!(ist.evaluate(utc(2026, 1, 5, 6, 0)).utc_offset, "+03:00");
        assert!(open(&ist, utc(2026, 1, 5, 6, 0)));
        assert!(!open(&ist, utc(2026, 1, 5, 5, 59)));
    }
}

//! First-party calendar endpoints (Blueprint Section 10): the cross-space agenda and
//! RFC 5545 exports. Meeting Service v2 stays the only scheduling authority.

use axum::{
    extract::{Query, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
    Json,
};
use uuid::Uuid;

use super::{views, V2Path};
use crate::{
    dto::v2::*,
    error::{ApiError, ApiResult, ErrorCode},
    services::actor::ActorContext,
    state::AppState,
};

/// List the caller's occurrences across meeting spaces (agenda / calendar range).
///
/// Returns scheduled occurrences whose start lies in `[from, to)` (default: the next 31 days;
/// at most 366 days), ordered by start, across every meeting space the caller owns, is an
/// active member of, holds an active invitation for (bound to the caller's verified email),
/// is registered for, or took part in. A machine caller gets its own environment's meetings.
/// Recurring occurrences beyond the 30-day materialization horizon are included with
/// `materialized: false` and `conferenceId: null`; `occurrenceKey` is stable, so the entry keeps
/// its identity once the occurrence is materialized. Filters: `status` and `role` (the caller's
/// relation: OWNER, MEMBER, INVITEE, REGISTRANT, PARTICIPANT, ORGANIZATION), both
/// comma-separated. `organizationId` restricts the agenda to that organization and adds its
/// organization-visible meetings; the caller must be a verified member, and an unknown or
/// unauthorized organization gets the same PERMISSION_DENIED. Paginate with `limit` (1-200,
/// default 50) and the opaque `nextCursor`. Only managers of a space (FULL view) see
/// management detail; titles of HEALTHCARE / HIGH_SECURITY spaces are omitted otherwise.
#[utoipa::path(
    get, path = "/api/v2/me/conferences", tag = "Conferences (v2)", operation_id = "v2_my_conferences",
    params(
        ("from" = Option<String>, Query, description = "RFC 3339, inclusive (default now)"),
        ("to" = Option<String>, Query, description = "RFC 3339, exclusive (default from + 31 days, at most 366 days)"),
        ("status" = Option<String>, Query, description = "Comma-separated ConferenceStatus values"),
        ("role" = Option<String>, Query, description = "Comma-separated AgendaRelation values"),
        ("organizationId" = Option<Uuid>, Query, description = "Organization of which the caller is a verified member"),
        ("cursor" = Option<String>, Query, description = "Opaque cursor from nextCursor"),
        ("limit" = Option<u32>, Query, description = "Page size, 1-200 (default 50)"),
    ),
    responses(
        (status = 200, description = "Agenda page", body = AgendaResponse),
        (status = 400, description = "VALIDATION_ERROR", body = ErrorEnvelope),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
        (status = 403, description = "PERMISSION_DENIED (organizationId)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn my_conferences(
    State(state): State<AppState>,
    actor: ActorContext,
    query: Result<Query<AgendaQuery>, axum::extract::rejection::QueryRejection>,
) -> ApiResult<Json<AgendaResponse>> {
    let Query(q) = query
        .map_err(|_| ApiError::domain(ErrorCode::ValidationError, "Invalid query parameters"))?;
    let page = state.meeting.calendar.agenda(&actor, q).await?;
    Ok(Json(AgendaResponse {
        items: page.entries.iter().map(views::agenda_item).collect(),
        next_cursor: page.next_cursor,
    }))
}

fn ics_response(body: String, filename: &str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/calendar; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    response
}

/// Export one conference occurrence as an iCalendar (RFC 5545) file.
///
/// One VEVENT with a stable UID (per occurrence, unchanged by materialization), SEQUENCE that
/// increases with every change, DTSTART/DTEND in the meeting timezone (TZID + VTIMEZONE; UTC for
/// UTC meetings; a start-only occurrence lasts 60 minutes) and STATUS:CANCELLED once cancelled.
/// Allowed for the space owner and members, admitted participants, holders of an active
/// invitation bound to their verified email, webinar registrants and the owning machine caller;
/// others get PERMISSION_DENIED. The file carries the meeting link but never a passcode,
/// invitation token or provider identifier; the title of a HEALTHCARE / HIGH_SECURITY meeting is
/// replaced unless the caller manages it. Served with `Cache-Control: no-store`.
#[utoipa::path(
    get, path = "/api/v2/conferences/{id}/calendar.ics", tag = "Conferences (v2)", operation_id = "v2_conference_calendar",
    params(("id" = Uuid, Path, description = "Conference id")),
    responses(
        (status = 200, description = "iCalendar file", content_type = "text/calendar", body = String),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "CONFERENCE_NOT_FOUND", body = ErrorEnvelope),
        (status = 409, description = "CONFERENCE_STATE_CONFLICT (no scheduled time)", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []), ("participant_token" = []))
)]
pub async fn conference_calendar(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Response> {
    let body = state
        .meeting
        .calendar
        .conference_ics(&actor, id, &state.app_url)
        .await?;
    Ok(ics_response(body, "conference.ics"))
}

/// Export a meeting space's schedule as an iCalendar (RFC 5545) file.
///
/// The active recurring series as one VEVENT with RRULE (UNTIL in UTC), EXDATE for cancelled
/// occurrences and RECURRENCE-ID overrides for rescheduled ones, all in the meeting timezone with
/// a generated VTIMEZONE; plus the space's one-off scheduled occurrences (up to 200, from 90 days
/// ago; cancelled ones with STATUS:CANCELLED). UIDs are stable per series / occurrence and
/// SEQUENCE increases on every change. Allowed for the space owner and members, holders of an
/// active space invitation bound to their verified email, webinar registrants and the owning
/// machine caller. No passcodes, invitation tokens or provider identifiers. Served with
/// `Cache-Control: no-store`.
#[utoipa::path(
    get, path = "/api/v2/meeting-spaces/{id}/calendar.ics", tag = "Meeting Spaces (v2)", operation_id = "v2_meeting_space_calendar",
    params(("id" = Uuid, Path, description = "Meeting space id")),
    responses(
        (status = 200, description = "iCalendar file", content_type = "text/calendar", body = String),
        (status = 403, description = "PERMISSION_DENIED", body = ErrorEnvelope),
        (status = 404, description = "MEETING_NOT_FOUND", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn meeting_space_calendar(
    State(state): State<AppState>,
    actor: ActorContext,
    V2Path(id): V2Path<Uuid>,
) -> ApiResult<Response> {
    let body = state
        .meeting
        .calendar
        .space_ics(&actor, id, &state.app_url)
        .await?;
    Ok(ics_response(body, "meeting.ics"))
}

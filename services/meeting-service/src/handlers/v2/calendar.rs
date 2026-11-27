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

fn feed_state(state: Option<crate::services::calendar_feed::FeedState>) -> CalendarFeedResponse {
    CalendarFeedResponse {
        active: state.is_some(),
        token: None,
        path: None,
        created_at: state.as_ref().map(|s| s.created_at),
        last_used_at: state.and_then(|s| s.last_used_at),
    }
}

/// My calendar feed (audit MF2).
///
/// Whether the caller has a private calendar feed, when it was created and last read. The feed
/// URL itself is shown only when it is created.
#[utoipa::path(
    get, path = "/api/v2/me/calendar-feed", tag = "Conferences (v2)", operation_id = "v2_get_calendar_feed",
    responses(
        (status = 200, description = "Feed state", body = CalendarFeedResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_calendar_feed(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<CalendarFeedResponse>> {
    let user = actor.require_registered()?.user_id;
    let current = crate::services::calendar_feed::state(&state.db, user).await?;
    Ok(Json(feed_state(current)))
}

/// Create (or replace) my calendar feed (audit MF2).
///
/// Returns a private iCalendar subscription URL listing the caller's meetings (the same
/// meetings as `GET /api/v2/me/conferences`, from 30 days ago to 180 days ahead, at most 500;
/// HEALTHCARE / HIGH_SECURITY meetings are not included). Subscribe to it from Google Calendar
/// ("From URL"), Outlook ("Subscribe from web") or Apple Calendar; it is read-only and updates
/// when the calendar app refreshes it. The token is shown once (only its hash is stored);
/// creating the feed again replaces the previous URL, which stops working.
#[utoipa::path(
    post, path = "/api/v2/me/calendar-feed", tag = "Conferences (v2)", operation_id = "v2_create_calendar_feed",
    responses(
        (status = 201, description = "Feed created (token shown once)", body = CalendarFeedResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_calendar_feed(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<(axum::http::StatusCode, Json<CalendarFeedResponse>)> {
    let user = actor.require_registered()?.user_id;
    let (token, created_at) = crate::services::calendar_feed::create(&state.db, user).await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(CalendarFeedResponse {
            active: true,
            path: Some(crate::services::calendar_feed::feed_path(&token)),
            token: Some(token),
            created_at: Some(created_at),
            last_used_at: None,
        }),
    ))
}

/// Delete my calendar feed (audit MF2).
///
/// Revokes the caller's calendar feed URL at once; calendar apps subscribed to it stop
/// receiving updates. Deleting a feed that does not exist is a no-op.
#[utoipa::path(
    delete, path = "/api/v2/me/calendar-feed", tag = "Conferences (v2)", operation_id = "v2_delete_calendar_feed",
    responses(
        (status = 200, description = "Feed revoked (or none existed)", body = CalendarFeedResponse),
        (status = 401, description = "AUTHENTICATION_REQUIRED", body = ErrorEnvelope),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_calendar_feed(
    State(state): State<AppState>,
    actor: ActorContext,
) -> ApiResult<Json<CalendarFeedResponse>> {
    let user = actor.require_registered()?.user_id;
    crate::services::calendar_feed::revoke(&state.db, user).await?;
    Ok(Json(feed_state(None)))
}

/// Read a calendar feed (audit MF2).
///
/// The iCalendar (RFC 5545, `METHOD:PUBLISH`, UTC times) of the account the feed token belongs
/// to. No sign-in: the token in the path is the credential (`calf_…`, followed by `.ics`). An
/// unknown or revoked token, or an account that is no longer active, is 404. Served with
/// `Cache-Control: no-store`; it never contains passcodes, invitation tokens or provider
/// identifiers.
#[utoipa::path(
    get, path = "/api/v2/calendar-feeds/{file}", tag = "Conferences (v2)", operation_id = "v2_read_calendar_feed",
    params(("file" = String, Path, description = "The feed token followed by `.ics`")),
    responses(
        (status = 200, description = "iCalendar feed", content_type = "text/calendar", body = String),
        (status = 404, description = "CALENDAR_FEED_NOT_FOUND (unknown, replaced or revoked token, or an inactive account)", body = ErrorEnvelope),
    ),
    security(())
)]
pub async fn read_calendar_feed(
    State(state): State<AppState>,
    axum::extract::Path(file): axum::extract::Path<String>,
) -> ApiResult<Response> {
    let token = file.strip_suffix(".ics").unwrap_or(&file);
    let body = crate::services::calendar_feed::render(
        &state.db,
        &state.meeting.calendar,
        token,
        &state.app_url,
    )
    .await?;
    let mut response = ics_response(body, "cfmeet.ics");
    // A subscription, not a download.
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline; filename=\"cfmeet.ics\""),
    );
    Ok(response)
}

# Meetings calendar — Blueprint v1.0 Section 10

Meeting Service v2 stays the only scheduling authority. There is no calendar service and no
second scheduling store: the agenda and the calendar files are read from `meeting_spaces`,
`conference_sessions`, `meeting_recurrence_rules` and `meeting_recurrence_exceptions`.

Code: `services/meeting-service/src/services/calendar.rs` (agenda, canonical conference,
stale expiry), `services/ics.rs` (RFC 5545 rendering), `services/idempotency.rs`,
`handlers/v2/calendar.rs`. Tests: `tests/meetings_calendar.rs`.

## API

| Method | Path | Change |
|---|---|---|
| GET | `/api/v2/me/conferences?from&to&status&role&organizationId&cursor&limit` | NEW — cross-space agenda (MTG-005, ORG-011, MTG-008) |
| GET | `/api/v2/conferences/{id}/calendar.ics` | NEW — one occurrence (MTG-007) |
| GET | `/api/v2/meeting-spaces/{id}/calendar.ics` | NEW — series + one-off occurrences (MTG-007) |
| GET | `/api/v2/meeting-spaces?cursor&limit` | EXTENDED — optional pagination, constant query count (MTG-018) |
| POST | `/api/v2/meeting-spaces`, `/api/v2/meeting-spaces/{id}/conferences` | EXTENDED — `Idempotency-Key` (SEC-014), past-start rule |
| POST | `/api/v2/conferences/{id}/reschedule` | EXTENDED — keeps the duration, create-time validation, optional `version` (MTG-009) |
| PUT / DELETE | `/api/v2/meeting-spaces/{id}/recurrence` | EXTENDED — in-place series edit; end retires rescheduled occurrences (MTG-014) |
| GET | `/api/v2/conferences/{id}`, `/api/v2/resolve/{identifier}`, `/api/v2/meeting-spaces/{id}` | CHANGED — one summary-privacy rule (MTG-011) |

v2 operations: 174 → 177. Every existing request and response stays valid: new request
fields and query parameters are optional, `nextCursor` on the space list appears only when
paginating.

Gateway classification (`infra/api-gateway/public-routes.json`): the agenda is
`PUBLIC_CUSTOMER`, like the meeting reads it extends (`GET /api/v2/meeting-spaces`,
`/meeting-spaces/{id}/conferences`, `/conferences/{id}`, `/me/organizations`); it supports the
gateway's machine callers (an environment's machine caller gets its own environment's meetings).
The two `calendar.ics` downloads are `EXCLUDED` with the new reason
`CALENDAR_FILE_DIRECT_INGRESS`: the gateway's route grammar rejects file-extension segments
(`ForbiddenPattern`), so they stay on the Meeting Service ingress for signed-in app users; the
JSON agenda is the routed customer API. The gateway relays `Idempotent-Replayed`.

## Rules chosen

**Agenda scope.** A registered caller sees the occurrences of spaces it owns, is an active
member of (by user or verified email), holds an active, unexhausted invitation for that is
bound to its verified email (space-wide or for that conference), is registered for
(webinars), or took part in (participant row not removed/banned/denied). A machine caller
sees its environment's spaces. Strict (HEALTHCARE / HIGH_SECURITY / `requireVerifiedRevocation`)
spaces are left out when the session's revocation status could not be verified (fail closed,
as for every other read). Only *scheduled* occurrences are listed (`scheduledStartAt` in
`[from, to)`); unscheduled on-demand conferences stay in the per-space list.

**Organization calendar.** `organizationId` is authorized per request from the actor's
`verified_organizations` (all `ORGANIZATIONS_AUTHORITY_MODE`s; never a client-supplied role).
It restricts the agenda to that organization and adds the organization's meetings a member may
see: `ORGANIZATION_ONLY`, `PUBLIC`, `PUBLIC_PASSCODE`, `AUTHENTICATED` — never `INVITE_ONLY`,
HEALTHCARE, HIGH_SECURITY or healthcare-appointment meetings. Unknown and unauthorized
organizations get the identical `403 PERMISSION_DENIED` (same message).

**Pagination.** Ordered by `(scheduledStartAt, meetingSpaceId, occurrenceKey)` (bytewise);
`limit` 1–200 (default 50); `nextCursor` is opaque. Range default 31 days, at most 366 days.
The agenda is read in one REPEATABLE READ, read-only transaction with a constant number of
queries (occurrences, series, exceptions) — no N+1.

**Beyond the horizon (MTG-008).** Recurring occurrences later than a series'
`materialized_until` are computed from the rule and its exceptions (`materialized: false`,
`conferenceId: null`). Their `occurrenceKey` is the materializer's key
(`rrule:<rule>:<original start UTC>`), so a later materialization keeps the identity and never
duplicates. Hard bounds per request: at most `limit + 1` virtual occurrences per series, at most
500 series (more is a `VALIDATION_ERROR` asking for a narrower filter), at most 20,000 rule
iterations per series.

**Past starts (MTG-010).** A `scheduledStartAt` more than 5 minutes in the past is rejected
(`VALIDATION_ERROR`) on create and reschedule; the end must be after the start and at most 24
hours later. (Legacy v1 rejected `start <= now + 1 min`; v2 tolerates clock skew instead.)

**Effective window and stale occurrences (MTG-010).** A start-only occurrence lasts 60 minutes.
It can be started from `earlyStartMinutes` before the start until `lateStartGraceMinutes` after
the effective end. After that it is *stale*: never the canonical conference, cannot be started,
and the scheduler expires it (`CANCELLED`, `termination_reason = CANCELLED`,
`cancelled_by = system:expired`, one `conference.cancelled` event; `FOR UPDATE SKIP LOCKED`,
200 per tick). No new termination reason, so the previous image reads the rows unchanged.

**Canonical conference.** One shared rule (`resolve`, space reads, the space list): LIVE (v2
first, latest start) > the next unstarted occurrence whose start window is still open
(unscheduled ones last) > the latest one. One query for any number of spaces.

**Reschedule (MTG-009).** An omitted `scheduledEndAt` keeps the duration (a start-only occurrence
stays start-only); the end is never cleared. Same validation as create. Optional `version` →
`MEETING_VERSION_CONFLICT`. Only CREATED occurrences can be moved (a prepared room is not
silently moved). Reschedule and cancel serialize on the row lock: either order leaves a consistent
`CANCELLED` occurrence.

**Series edit (MTG-014).** `PUT …/recurrence` on an active series edits it in place (same rule id
and occurrence keys): future occurrences that are still occurrences of the edited rule are kept
(plain ones get the new duration, rescheduled ones keep their time) with their exceptions;
occurrences that no longer match are deleted (no history) or cancelled and their exception
records dropped; past occurrences are never touched. The rule version advances by 1 + the dropped
exception versions so the calendar `SEQUENCE` never decreases. `DELETE …/recurrence` retires every
unstarted future occurrence, rescheduled ones included. Not built: "this and following" splits.

**Timezones (MTG-012).** IANA zones throughout; recurrence is wall-clock in the space timezone. A
nonexistent local time (spring forward) occurs at the shifted instant (02:30 → 03:30 EDT); a
repeated local time (fall back) occurs at the first instant. A series cannot *start* at a
nonexistent or ambiguous local time (`VALIDATION_ERROR`).

**Summary privacy (MTG-011).** Callers without the FULL view see the scheduled start/end in the
conference SUMMARY view and in the space's `latestConference` — except for HEALTHCARE /
HIGH_SECURITY profiles, HEALTHCARE meeting type and healthcare-appointment access, where both
views withhold all timing (id and status only). Guests of ordinary meetings get their countdown;
appointment times are not disclosed to code holders. `startedAt`/`endedAt` stay FULL-only in the
conference view. The agenda shows the schedule to every related caller (it is their own
calendar); HEALTHCARE / HIGH_SECURITY titles only to managers.

**ICS (MTG-007).** RFC 5545, CRLF, folded at 75 octets, escaped text, `METHOD:PUBLISH`. Times in
the meeting timezone with `TZID` and a generated `VTIMEZONE` (the zone's real transitions over
the exported window); UTC meetings use `…Z`. Series: one master `VEVENT` with `RRULE` (`UNTIL`
rewritten to UTC), `EXDATE` per cancelled occurrence, a `RECURRENCE-ID` override per rescheduled
occurrence. UIDs: `series-<rule>@cfmeet.com`, `occurrence-<rule>-<original UTC>@cfmeet.com`
(single-occurrence export, stable across materialization), `conference-<id>@cfmeet.com`.
`SEQUENCE` = conference `version - 1`, or rule `version - 1` + exception versions. Cancelled →
`STATUS:CANCELLED`. A start-only occurrence gets a 60-minute `DTEND`. Authorized for the owner,
members, admitted participants, email-bound invitees, registrants and the owning machine caller;
everyone else gets `403`. The file holds the meeting code and join URL only — never a passcode,
invitation token or provider id; HEALTHCARE / HIGH_SECURITY titles become "CF Meet meeting" for
non-managers. `Cache-Control: no-store`. Importing both a series file and a single-occurrence file
of the same series yields two entries (different UIDs by design).

**Idempotency (SEC-014).** `Idempotency-Key` (1–128 visible ASCII) on space and conference
creation, scoped to the caller (user, or environment + service account) and operation (and space
for conferences), kept 24 hours as SHA-256 digests with the digest of the canonical JSON body.
Same key and body → the same resource, `201`, `Idempotent-Replayed: true`; different body →
`409 IDEMPOTENCY_CONFLICT`; concurrent first requests → exactly one creates. Expired records are
purged by the scheduler. Instant meetings and legacy v1 writes are not covered.

## Migration

A NEW additive history `meetings_calendar_migrations` (`meetings-calendar-migration up`,
`m20261107_000001_meetings_calendar`): table `meeting_idempotency_keys` and read indexes
(`conference_sessions(scheduled_start_at)`, participations by user, email-bound invitations,
registrations by user). No existing table or column changes, so the previous image keeps
starting; the new image refuses to start without it (`meetings_calendar_ready`). Wired into the
Dockerfile, `infra/deployment/backend_runtime.py`, `communications_release.py` (after account
security, `binaries[1:]` semantics kept), certification stacks, the backup catalog and the test
template.

## Client mapping (web / mobile)

* **Lists and dashboard (MTG-004):** show `latestConference.scheduledStartAt` (or agenda
  `scheduledStartAt`) formatted in `meetingSpace.timezone` *and* the viewer's zone — never
  `createdAt`.
* **Calendar views:** one `GET /api/v2/me/conferences?from&to` per visible range; follow
  `nextCursor`. Key entries by `(meetingSpaceId, occurrenceKey)`; when `conferenceId` is null the
  occurrence is not yet materialized (show it; actions such as join/start need the conference and
  become available once materialized, or use `PUT …/recurrence/exceptions` for cancel/reschedule).
* **Organization calendar:** pass `organizationId` of an organization from
  `GET /api/v2/me/organizations`; treat 403 as "not available" (never as "does not exist").
* **Create / reschedule:** convert `datetime-local` input in the *space* timezone (not the browser
  zone); send `Idempotency-Key` (a UUID per user action) on create; on reschedule you may omit
  `scheduledEndAt` to keep the duration and send `version` to detect concurrent edits.
* **Pre-join (FE-001, MTG-013):** read `access.*` and `consents.*` from
  `GET /api/v2/conferences/{id}/pre-join`; the countdown uses `conference.scheduledStartAt` from
  `resolve` (now present in the SUMMARY view for ordinary meetings; absent for healthcare /
  high-security — show "waiting for host" without a time).
* **Add to calendar:** link to `…/calendar.ics` with the user's bearer token (download, not a
  subscription URL; files are `no-store`).
* **Recurrence editor:** `PUT …/recurrence` now edits in place — exceptions survive; tell users
  that occurrences no longer produced by the new rule are removed.

## Findings

Closed: MTG-005, MTG-008, MTG-009, MTG-010, MTG-011, MTG-018, ORG-011 (listing), SEC-014
(v2 space/conference creation). Partial: MTG-007 (export only; no personal feed, no import or
external sync), MTG-012 (backend timezone semantics; the web UI conversion is a frontend task),
MTG-014 (in-place edit and end; no "this and following"), MTG-016 (listing; web create form is
frontend). Not in scope: MTG-006 (notifications/reminders), MTG-013/015/019/021 (frontend),
MTG-017/020.

# ADR 0071 — Events belong to Messaging

Status: Accepted (product owner decision 2026-11-27: "nothing here goes under meeting
service"; Status, Channels, Communities and Events are Messaging features).

## Context

CF Meet users plan things with the people they chat with: a birthday dinner with friends,
a weekly stand-up of a group, a call with a contact. The owner's brief (item 19-20 of the
2026-11-28 handover) asks for personal, conversation, community and organization events with
RSVP, reminders, recurrence, ICS export and links to meetings, and an events dashboard
(upcoming, today, past, mine, invitations, accepted; list and calendar; search and filters).

The Meeting Service already schedules meetings. A second scheduler there would couple a
chat feature to the meeting app again, which ADR 0069 has just undone for Status and Channels.

## Decisions

1. **Messaging owns Events.** They live in `services/messaging/src/calendar/` (the name
   `events` is taken by Messaging's domain-event outbox): events, people, RSVP, reminders,
   recurrence, ICS, notices. Messaging's own database, under a separate additive migration
   history `messaging_events_seaql_migrations` (`m20261218_000046_events`): a previous
   Messaging image never reads it, so a rollback still boots. The customer API is
   `/v1/messaging/events/**` in `cfmeet.messaging.http-api` 1.26.0. Every operation is
   `EXCLUDED` from the public API Gateway (`FIRST_PARTY_EVENTS`): invitations are CF Meet
   personal contacts and CF Meet conversations.
2. **Reached through the relay only.** The CF Meet clients call
   `/api/v2/messaging/relay/v1/messaging/events/...`. The Meeting Service holds only allowlist
   entries in `relay.rs` `ROUTES` (idempotency-key forwarding for event creation) and forwards
   with the signed-in account's principal.
3. **Online events link to a meeting; there is no second scheduler.** The web picks one of the
   user's CF Meet meetings or creates one through the existing Meeting API, and the event
   stores only `meetingId` and / or `meetingUrl` (`https` only). Messaging never calls the
   Meeting Service and never reads its database. `CALL` events are joined as a call of the
   event's conversation (or of the organizer for personal events) by the client.
4. **Scopes and who may see an event** (one gate, `calendar::visible`; anything else is
   `404 NOT_FOUND`, indistinguishable from an event that does not exist):
   * `PERSONAL`: the organizer and the people on the event (co-organizers, invitees). An
     invitee must be one of the inviter's mutual, unblocked contacts at invitation time
     (Communications Hub `contacts/eligible`, as ADR 0069 decision 3; Communications
     unreachable is `503`). A Messaging block between the viewer and the organizer hides the
     event and refuses an invitation.
   * `CONVERSATION`: the organizer and the ACTIVE members of the conversation, decided live
     (leaving the conversation removes access at once; a person on the event who is no longer
     a member does not see it). The organizer must be a member allowed to send when creating
     it; co-organizers and invitees must be ACTIVE members.
   * `ORGANIZATION`: **not available yet.** Messaging has no Organizations membership check
     for a signed-in person (organizations reach Messaging only for moderation, in the
     organization's own scope, ADR 0056). Creating one is
     `422 CONTEXT_NOT_ALLOWED / EVENT_SCOPE_NOT_AVAILABLE`. Until an Organizations
     integration exists, a team's event is a `CONVERSATION` event of its team channel.
   * `COMMUNITY`: a reserved value for Communities (not built yet), refused the same way.
5. **People and RSVP.** One organizer, at most 10 co-organizers (organizer only changes them),
   at most 500 invitees. RSVP `GOING` / `MAYBE` / `NOT_GOING` applies to the whole series; a
   conversation member who answers without an invitation is recorded as `MEMBER`. Counts:
   going, maybe, not going and awaiting (invitees without an answer; for a conversation event,
   ACTIVE members without an answer). Organizer and co-organizers edit, invite and cancel; only
   the organizer deletes. A cancelled or finished event takes no answers (`409`).
6. **Time and recurrence.** `startsAt` / `endsAt` are UTC instants with an IANA `timeZone`
   (validated against the bundled tz database) and `allDay` (both instants local midnights).
   Recurrence is an RRULE subset: `FREQ=DAILY|WEEKLY|MONTHLY`, `INTERVAL` 1-99, `COUNT` 1-730
   or `UNTIL` (a local date, inclusive, at most 5 years after the start), `BYDAY` (WEEKLY only;
   the start must fall on one of the days). Occurrences are expanded server-side in the
   event's time zone (a weekly 10:00 stays 10:00 across daylight-saving changes; a local time
   skipped by a change moves forward; a MONTHLY day missing from a month is skipped, as RFC
   5545 does). `seriesEndsAt` (the last occurrence's end, `NULL` for an open series) bounds
   every query. Per-occurrence exceptions (moving or cancelling one occurrence) are not
   supported.
7. **Queries.** `GET /v1/messaging/events?view=` returns OCCURRENCES ordered by start (`PAST`
   newest first) with an opaque keyset `cursor` and `limit` (1-100, default 30):
   `UPCOMING` (not yet ended), `TODAY` (overlapping the caller's day: the client sends its local
   day as `from` / `to`, since the relay forwards at most 8 query keys; without them the UTC day), `PAST` (ended),
   `MINE` (organizer or co-organizer, not yet ended), `INVITATIONS` (invited or a conversation
   member, no answer yet, active, not organizer), `ACCEPTED` (`GOING`, not yet ended). `from`
   / `to` (at most 92 days apart) replace the time window for the month calendar. Filters:
   `conversationId`, `type`, `q` (prefix search over title, description and location,
   PostgreSQL full text, `simple` configuration). Indexes cover the organizer, the
   conversation, the series end, the people's principal and the search vector.
8. **Reminders and notices.** Each person sets their own reminders (`minutesBefore`, at most 5,
   0 to 40320). A reminder keeps its next fire time (`next_fire_at`), recomputed on every
   change of the event; a worker fires due reminders (at most 15 minutes late, else the
   occurrence is skipped) and advances them. Invitations, cancellations and reminders are
   written to `messaging_event_notices` and relayed to the Notification Service exactly like
   status notices (ids only, addressed to the recipient's CF Meet account, `SKIPPED` when the
   recipient can no longer see the event or answered `NOT_GOING`), only with
   `MESSAGING_EVENT_NOTICES=on`. The catalogue types `messaging.event.invited`,
   `messaging.event.cancelled` and `messaging.event.reminder` are registered in the
   Notification Service with their routing rows seeded **OFF** (see Activation).
9. **ICS.** `GET /v1/messaging/events/{eventId}/ics` (one event) and
   `GET /v1/messaging/events/calendar` (the caller's events: not ended, or ended in the last 90
   days, at most 500) return `{filename, contentType, content}`: the relay carries JSON only,
   and the client saves `content` as a `.ics` file. RFC 5545: CRLF, folded lines, escaped
   text, `TZID` local times (UTC as `Z`), `VALUE=DATE` for all-day events, `RRULE`, `SEQUENCE`
   (bumped on every edit), `STATUS:CANCELLED`, `URL` / `X-CFMEET-MEETING-ID` for the meeting.
   No names, e-mail addresses or principal ids are exported.
10. **Cover images** use the ADR 0069 decision 5 media store with purpose `EVENT` (images only,
    8 MiB): `…/events/media/**`, readable by whoever can see an event using it; unused covers
    are removed by the media sweep after 24 hours; a deleted event's cover at once. The
    `EVENT` purpose is added to the media table's check by the events migration (the
    previous image never selects it, and its sweep never removes a completed `EVENT` upload).
11. **Idempotency and limits.** Creation takes an optional `Idempotency-Key`
    (`messaging_idempotency_keys`, 24 hours, the acting principal in the digest). Limits per
    person: 60 created per hour, 120 writes per minute, 120 reads per minute, 500 organized
    events not yet ended.

## API

All under `/v1/messaging/events`, acting principal required; reads `messages.read`, writes
`messages.send`.

| Operation | Route | Body / query |
|---|---|---|
| `createEvent` | `POST /events` (`Idempotency-Key`) | `EventCreateRequest` → `201 Event` (`200` replay) |
| `listEvents` | `GET /events` | `view`, `from`, `to`, `conversationId`, `q`, `type`, `limit`, `cursor` → `EventPage` |
| `exportEventCalendar` | `GET /events/calendar` | → `EventCalendarFile` |
| `getEvent` | `GET /events/{eventId}` | → `Event` (with `upcomingOccurrences`) |
| `updateEvent` | `PATCH /events/{eventId}` | `EventUpdateRequest` → `Event` |
| `deleteEvent` | `DELETE /events/{eventId}` | → `EventDeleted` |
| `cancelEvent` | `POST /events/{eventId}/cancel` | `{reason?}` → `Event` |
| `respondToEvent` | `PUT /events/{eventId}/rsvp` | `{response}` → `Event` |
| `listEventAttendees` | `GET /events/{eventId}/attendees` | → `EventAttendees` |
| `inviteToEvent` | `POST /events/{eventId}/invitations` | `{principalIds}` → `EventAttendees` |
| `removeEventInvitee` | `DELETE /events/{eventId}/invitations/{principalId}` | → `EventAttendees` |
| `setEventReminders` | `PUT /events/{eventId}/reminders` | `{minutesBefore}` → `EventReminders` |
| `exportEvent` | `GET /events/{eventId}/ics` | → `EventCalendarFile` |
| cover media | `POST /events/media`, `GET` / `DELETE /events/media/{mediaId}`, `POST …/parts/{part}`, `POST …/complete`, `POST …/download-grants` | as status media (images only) |

`EventCreateRequest`: `scope` (`PERSONAL` / `CONVERSATION` / `ORGANIZATION` / `COMMUNITY`),
`conversationId` (CONVERSATION), `title` (1-200), `description` (≤ 5000), `location` (≤ 500),
`type` (`ONLINE` / `PHYSICAL` / `CALL`), `meetingId` (≤ 64, `[A-Za-z0-9_-]`), `meetingUrl`
(`https`, ≤ 2048; ONLINE needs `meetingId` or `meetingUrl`), `coverMediaId`, `startsAt`,
`endsAt` (after the start, at most 31 days later, not in the past for a single event),
`timeZone` (IANA, default `UTC`), `allDay`, `recurrence` (`frequency`, `interval`, `count`,
`until` `YYYY-MM-DD`, `byDay` `["MO", …]`), `coOrganizerPrincipalIds` (≤ 10),
`inviteePrincipalIds` (≤ 500), `remindersMinutesBefore` (the organizer's own, ≤ 5).
`EventUpdateRequest`: the same fields except `scope`, `conversationId`, invitees and reminders
(`null` clears an optional field), plus `version` (`409 VERSION_CONFLICT` when stale).

`EventAttendees`: `eventId`, `counts`, `attendees` (`principalId`, `role`, `response`,
`respondedAt`). `EventReminders`: `eventId`, `minutesBefore`. `EventDeleted`: `eventId`,
`deleted`. The inbox action of the three notices is `OPEN_EVENT` (subject kind `EVENT`, catalogue
#43-#45; the web opens `/dashboard/events/{eventId}`).

`Event`: `eventId`, `scope`, `conversationId`, `title`, `description`, `location`, `type`,
`meetingId`, `meetingUrl`, `coverMediaId`, `startsAt`, `endsAt`, `timeZone`, `allDay`,
`recurrence`, `rrule`, `seriesEndsAt`, `state` (`ACTIVE` / `CANCELLED`), `cancelledAt`,
`cancelReason`, `organizerPrincipalId`, `coOrganizerPrincipalIds`, `myRole`, `myResponse`,
`myReminders`, `counts` (`going`, `maybe`, `notGoing`, `awaiting`), `canEdit`, `canDelete`,
`version`, `createdAt`, `updatedAt`, `nextOccurrence` and, on `getEvent`,
`upcomingOccurrences` (at most 10). `EventPage`: `items` (`{startsAt, endsAt, event}`),
`nextCursor`.

## Activation (release steps, all off by default)

1. Deploy (the migration runs with the release; nothing is notified).
2. Notification Service: set the routing rows `MESSAGING_EVENTS` / `IN_APP` and `PUSH` to
   enabled for `messaging.event.invited`, `messaging.event.cancelled` and
   `messaging.event.reminder` (seeded OFF by `m20261218_000001_messaging_event_notices`).
3. Messaging: `MESSAGING_EVENT_NOTICES=on` (needs `CF_NOTIFICATIONS_BASE_URL`), restart.
   Until then invitations, cancellations and reminders are shown in the app only (the
   dashboard's Invitations view, the event page) and nothing is written to the outbox.

## Known limitations

* Event text is stored server-side and readable by the server; nothing is labelled end-to-end
  encrypted.
* No organization events until Messaging has an Organizations integration (decision 4).
* RSVP, cancellation and edits apply to the whole series (no per-occurrence exceptions).
* Deleting an event sends no notice (cancel first to tell people).
* No realtime push of event changes (clients refresh); the Messaging WebSocket's
  per-principal events are the natural later channel.

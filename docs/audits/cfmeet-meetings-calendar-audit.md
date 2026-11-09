# CF Meet — Meeting Scheduling and Calendar Assessment

**Audit phase:** Phase 1 — audit only, no implementation · **Repository:** `CosmicforgeHealthnet/cfmeet` · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` (`staging` = `main`) · **Date:** 2026-11-06

**Related:** [executive report](cfmeet-platform-comprehensive-audit.md) · [feature gap register](cfmeet-feature-gap-register.md) · [API inventory](cfmeet-api-inventory.csv) · [implementation plan (proposal)](cfmeet-recommended-implementation-plan.md)

> **Method and evidence rules.** Every finding was established by reading the code at the baseline commit. The file and line references are to that commit. No application code, schema, contract, configuration or deployment was changed, and no production host was contacted. No test suites were run by this audit: the sandbox has no service databases, and building would write into the repository. A feature is `COMPLETE_VERIFIED` only where an automated test exercises it against a real database or real stack **in CI**. Those CI runs were read, not re-executed. The latest backend CI evidence is on `e717d018`, an ancestor of the baseline whose six later commits change only docs, infra and workflows. The lead auditor re-checked the highest-severity claims directly; see "Lead-auditor verification" below.

## Lead-auditor verification and cross-checks

- **MTG-004 (the date/time defect the owner noticed): verified.** `apps/web/src/components/dashboard/MeetingsContent.tsx:249-254` renders `formatShortDateForDisplay(space.createdAt)` next to a `<Calendar>` icon. The scheduled time, which the backend returns as `latestConference.scheduledStartAt` on `GET /api/v2/meeting-spaces`, is never read there. The schedule itself is persisted (TIMESTAMPTZ) and returned correctly. **The defect is display, not storage.**
- **MTG-001: verified.** Mobile still builds v1 bodies with `scheduled_at` / `duration_minutes`, for example `apps/mobile/app/(meetings)/[id]/edit.tsx:54-55,108-109` and `app/(tabs)/index.tsx:42`.
- **MTG-003 / FE-003: independently reproduced.** The lead auditor matched every mobile `/api/v2` call against the statically extracted backend routes. 35 distinct mobile v2 call sites target routes that do not exist; the table below lists them with the real backend path. None of them reaches the backend successfully.
- **Space vs Conference inventory.** The API inventory confirms that v2 has no cross-space listing. The only occurrence listing is `GET /api/v2/meeting-spaces/:id/conferences`, and there is no `/api/v2/me/conferences`, `/calendar` or `/agenda` route anywhere in the 1,066 registered operations. This independently confirms MTG-005.
- **Legacy flag.** The v1 write surface (23 routes) sits behind `legacy_write_guard` (`services/meeting-service/src/routes/mod.rs:65-77`). Turning on `FEATURE_LEGACY_MEETING_WRITE_DISABLED` does not affect web scheduling, which already uses v2, apart from the dead legacy branch in MTG-019. It breaks every mobile meeting write.

### Mobile v2 calls that hit non-existent backend routes

| Mobile call | Call site | Real backend route |
|---|---|---|
| `DELETE /api/v2/conferences/:conferenceId/chat/:messageId` | `apps/mobile/features/meetings/v2/api.ts:313` | `/messages` |
| `DELETE /api/v2/conferences/:conferenceId/participants/:participantId/lock/audio` | `apps/mobile/features/meetings/v2/api.ts:601` | `/audio-lock` |
| `DELETE /api/v2/conferences/:conferenceId/participants/:participantId/lock/video` | `apps/mobile/features/meetings/v2/api.ts:615` | `/video-lock` |
| `DELETE /api/v2/conferences/:conferenceId/qna/:questionId/pin` | `apps/mobile/features/meetings/v2/api.ts:559` | `/questions` |
| `DELETE /api/v2/conferences/:conferenceId/qna/:questionId/vote` | `apps/mobile/features/meetings/v2/api.ts:526` | `/questions` |
| `DELETE /api/v2/conferences/:conferenceId/spotlight/:participantId` | `apps/mobile/features/meetings/v2/api.ts:587` | `/spotlights` |
| `GET /api/v2/conferences/:conferenceId/chat` | `apps/mobile/features/meetings/v2/api.ts:297` | `/messages` |
| `GET /api/v2/conferences/:conferenceId/hand` | `apps/mobile/features/meetings/v2/api.ts:352` | `/hand-raise` |
| `GET /api/v2/conferences/:conferenceId/participants/:participantId` | `apps/mobile/features/meetings/v2/api.ts:219` | no backend equivalent (`GET /participants/:id`, hand lower by id) |
| `GET /api/v2/conferences/:conferenceId/qna` | `apps/mobile/features/meetings/v2/api.ts:504` | `/questions` |
| `GET /api/v2/conferences/:conferenceId/waiting` | `apps/mobile/features/meetings/v2/api.ts:268` | `/waiting-room` |
| `GET /api/v2/spaces/:id` | `apps/mobile/features/meetings/v2/api.ts:94` | `/api/v2/meeting-spaces` |
| `GET /api/v2/spaces/:spaceId/conferences/:conferenceId` | `apps/mobile/features/meetings/v2/api.ts:132` | `/api/v2/meeting-spaces` |
| `GET /api/v2/spaces/:spaceId/conferences` | `apps/mobile/features/meetings/v2/api.ts:125` | `/api/v2/meeting-spaces` |
| `GET /api/v2/spaces` | `apps/mobile/features/meetings/v2/api.ts:89` | `/api/v2/meeting-spaces` |
| `POST /api/v2/conferences/:conferenceId/breakouts/announce` | `apps/mobile/features/meetings/v2/api.ts:415` | `/announcement` |
| `POST /api/v2/conferences/:conferenceId/chat` | `apps/mobile/features/meetings/v2/api.ts:305` | `/messages` |
| `POST /api/v2/conferences/:conferenceId/hand/:participantId/lower` | `apps/mobile/features/meetings/v2/api.ts:345` | `/hand-raise` |
| `POST /api/v2/conferences/:conferenceId/hand/lower` | `apps/mobile/features/meetings/v2/api.ts:338` | `/hand-raise` |
| `POST /api/v2/conferences/:conferenceId/hand/raise` | `apps/mobile/features/meetings/v2/api.ts:331` | `/hand-raise` |
| `POST /api/v2/conferences/:conferenceId/participants/:participantId/lock/audio` | `apps/mobile/features/meetings/v2/api.ts:594` | `/audio-lock` |
| `POST /api/v2/conferences/:conferenceId/participants/:participantId/lock/video` | `apps/mobile/features/meetings/v2/api.ts:608` | `/video-lock` |
| `POST /api/v2/conferences/:conferenceId/polls/:pollId/respond` | `apps/mobile/features/meetings/v2/api.ts:483` | `/responses` |
| `POST /api/v2/conferences/:conferenceId/qna/:questionId/answer` | `apps/mobile/features/meetings/v2/api.ts:537` | `/questions` |
| `POST /api/v2/conferences/:conferenceId/qna/:questionId/dismiss` | `apps/mobile/features/meetings/v2/api.ts:545` | `/questions` |
| `POST /api/v2/conferences/:conferenceId/qna/:questionId/pin` | `apps/mobile/features/meetings/v2/api.ts:552` | `/questions` |
| `POST /api/v2/conferences/:conferenceId/qna/:questionId/vote` | `apps/mobile/features/meetings/v2/api.ts:519` | `/questions` |
| `POST /api/v2/conferences/:conferenceId/qna` | `apps/mobile/features/meetings/v2/api.ts:511` | `/questions` |
| `POST /api/v2/conferences/:conferenceId/spotlight` | `apps/mobile/features/meetings/v2/api.ts:579` | `/spotlights` |
| `POST /api/v2/conferences/:conferenceId/waiting/admit` | `apps/mobile/features/meetings/v2/api.ts:275` | `/waiting-room` |
| `POST /api/v2/conferences/:conferenceId/waiting/deny` | `apps/mobile/features/meetings/v2/api.ts:283` | `/waiting-room` |
| `POST /api/v2/instant` | `apps/mobile/features/meetings/v2/api.ts:114` | `/api/v2/instant-meetings` |
| `POST /api/v2/spaces/:id/archive` | `apps/mobile/features/meetings/v2/api.ts:104` | `/api/v2/meeting-spaces` |
| `POST /api/v2/spaces/:spaceId/conferences` | `apps/mobile/features/meetings/v2/api.ts:139` | `/api/v2/meeting-spaces` |
| `POST /api/v2/spaces` | `apps/mobile/features/meetings/v2/api.ts:99` | `/api/v2/meeting-spaces` |

### Severity reconciliation in the master register
- **MTG-001** — also reported as FE-002, FE-022
- **MTG-002** — also reported as FE-004
- **MTG-003** — also reported as FE-003
- **MTG-016** — register severity **P1**: Severity raised to P1 from merged duplicate. — also reported as ORG-010



Repository @ 586b88cb (staging == origin/main). Audit only; nothing in the repo was modified.
Backend CI status checked read-only: `gh run list --workflow backend.yml --branch main` → last 3 runs `success`
(2026-11-06 07:43Z, run 42182739607). `backend.yml` "test" job runs `cargo test --workspace` against real
PostgreSQL 16 + Redis 7 (`TEST_DATABASE_URL`), so backend DB tests cited below are CI-executed. Real LiveKit tests
(`tests/livekit_certification.rs`) are `#[ignore]` and not part of CI; the CI E2E uses the in-process LiveKit mock.

## Scope inspected

Backend `services/meeting-service`:
- Routes: `src/routes/{mod,meetings,participants,api,v2}.rs`; guard `src/middleware/legacy_write.rs`.
- Handlers: `src/handlers/meetings.rs` (v1), `src/handlers/v2/{spaces,conferences,participants,invitations,members,views,organizations}.rs`, `src/handlers/admin.rs` (meeting listing).
- DTOs: `src/dto/meetings.rs` (v1), `src/dto/v2.rs`; domain enums `src/domain/mod.rs`.
- Services: `meeting_space.rs`, `conference.rs`, `recurrence.rs`, `admission.rs` (join), `compat.rs` (resolver), `invitations.rs`, `meeting_core.rs` (v2 write gate), `email.rs`, `platform/events.rs`, `communications.rs`.
- Workers: `conference_scheduler.rs`, `meeting_auto_start.rs`, `meeting_auto_end.rs`, `outbox_relay.rs`, `mod.rs`.
- Models: `meetings.rs`, `conference_sessions.rs`, `meeting_recurrence_rules.rs`, `meeting_recurrence_exceptions.rs`.
- Migrations: `m20260116_000002_create_meetings.rs`, `m20261003_000001_phase2_meeting_domain.rs`, `m20261004_000002_recurrence_exceptions.rs`, `phase2_backfill.rs`.
- Config: `src/config/features.rs`; `.env.example`; `docker-compose.prod.yml`.
- Utils: `src/utils/datetime.rs`, `src/utils/format.rs`.
- Tests: `tests/api_v2_e2e.rs`, `tests/system_e2e.rs`, `tests/recurrence_exceptions.rs`, `tests/communications_meetings.rs`, `tests/legacy_retirement.rs`, `tests/common/mod.rs`; `.github/workflows/backend.yml`.
Contracts/SDKs: `packages/contracts/openapi/meeting-service.json`, `packages/contracts/bundled/public/meetings/v1.openapi.json`, `infra/api-gateway/route-manifest.json`, `sdks/typescript/src/generated/{operations,models}.ts`, `sdks/dart/lib/src/generated/{operations,contracts}.dart`.
Communications hub: `services/communications/src/meetings.rs` (meeting links only).
Web `apps/web/src`: `services/meeting.service.ts`, `services/v2/{spaces,conferences}.service.ts`, `lib/meeting-scheduling.ts`, `types/v2/index.ts`, `types/meeting.ts`, `components/dashboard/{MeetingsContent,DashboardContent,ScheduleContent,EditMeetingContent,OrganizationsContent}.tsx`, `components/meeting-spaces/{CreateMeetingSpaceContent,MeetingSpaceWorkspace,RecurrencePanel,InvitationsPanel}.tsx`, `components/conferences/{ConferencesPanel,ConferenceWorkspace,ParticipantsPanel}.tsx`, `components/room/{pre-join-screen,live-room,footer-controls,ChatProvider}.tsx`, `app/dashboard/**/page.tsx`, `hooks/*`, `tests/scheduling-v2.test.mjs`.
Mobile `apps/mobile`: `app/(tabs)/{index,schedule,meetings}.tsx`, `app/(meetings)/[id]/edit.tsx`, `app/(room)/[id]/{index,live}.tsx`, `features/meetings/{api/meetings.api.ts,hooks/use-meetings.ts,types.ts,schemas/meeting.schema.ts,components/*}`, `features/meetings/v2/{api.ts,types.ts,components/*}`, `features/room/{api/room.api.ts,components/*,hooks/*}`, `lib/api/client.ts`, `lib/constants`.
Spec: `CFMeet_Phase2_Frontend_Product_Design_Requirements_v1.0.pdf` (F-02..F-06, J-02, J-03, AT-02/03, "External calendar sync = Optional"); `docs/phase2-certification/12-rollout-plan.md:141` ("Calendar providers | Optional").

## Resource model: Meeting Space vs Conference (v2) vs legacy Meeting (v1)

| Concept | Table (migration) | Key schedule fields | Lifecycle | Owner of timezone |
|---|---|---|---|---|
| Legacy meeting (v1) | `meetings` (`m20260116_000002_create_meetings.rs:25-66`) | `start_time TIMESTAMP NOT NULL`, `end_time TIMESTAMP` (naive, UTC by convention). **No timezone column** | `scheduled/ongoing/ended/cancelled` (`models/meetings.rs:25`) | Request-only (`CreateMeetingRequest.timezone`), converted then discarded |
| Meeting Space (v2, reusable config) | `meeting_spaces` (`m20261003_000001…:18-61`) | `timezone VARCHAR(64) NOT NULL DEFAULT 'UTC'` (:30), `schedule_policy JSONB` (:51 early-start/late-grace/auto-end). **No start/end** | `ACTIVE/ARCHIVED/DELETED` | Space |
| Recurrence rule | `meeting_recurrence_rules` (:66-87) | `rrule`, `timezone` (copied from space), `dtstart_local TIMESTAMP`, `duration_minutes 1..1440`, `materialized_until` | `ACTIVE/ENDED`; one active per space (`uq_recurrence_one_active_per_space`) | Copied from space at set time; re-anchored on space TZ change (`recurrence.rs:328`) |
| Recurrence exception | `meeting_recurrence_exceptions` (`m20261004_000002…:14-28`) | `original_start_local`, `kind CANCELLED/RESCHEDULED`, `new_start_at/new_end_at TIMESTAMPTZ` | keyed `(recurrence_rule_id, original_start_local)` | Rule |
| Conference (occurrence) | `conference_sessions` (:89-140) | `scheduled_start_at/scheduled_end_at TIMESTAMPTZ`, `original_start_at`, `started_at`, `ended_at`, `cancelled_at`; CHECK end>start (:126) | `CREATED→PREPARING→WAITING→LIVE→ENDING→ENDED`, `CANCELLED`, `FAILED` (`domain/mod.rs:119-128`); one LIVE per space (:132) | **None** (UTC instants only) |

Relationships: space 1–N conferences (`meeting_space_id`), space 1–N rules (1 active), rule 1–N conferences (`recurrence_rule_id`) and 1–N exceptions; legacy meeting 1–1 space and 1–1 conference via `legacy_meeting_id` (backfill only).

Inconsistencies (detailed in findings): schedule lives on the conference, timezone lives on the space (and only drives recurrence); the frontend lists **spaces** (`GET /api/v2/meeting-spaces`) and shows `createdAt`, not the conference schedule; space SUMMARY view exposes `latestConference.scheduledStartAt` while conference SUMMARY view hides it; legacy meetings created after the one-time backfill have no space/conference at all.

## Field-level table

| Field | Request DTO | Entity / column (migration) | Response DTO(s) | Swagger (`openapi/meeting-service.json`) | TS SDK | Dart SDK | Web type | Mobile type |
|---|---|---|---|---|---|---|---|---|
| v1 `start_time` | `CreateMeetingRequest.start_time: NaiveDateTime` local (`dto/meetings.rs:68`), `UpdateMeetingRequest.start_time` (:95) | `meetings.start_time TIMESTAMP NOT NULL` (`m20260116_000002…:48`) | `MeetingResponse.start_time` "…Z" (`dto/meetings.rs:147`), `InstantMeetingResponse`, `PublicMeetingInfoResponse` | yes (v1 paths present) | not exposed (v1 not in SDK op list) | not exposed | `types/meeting.ts:8,26` (only legacy, unused for create) | **absent** — mobile sends/reads `scheduled_at` (`features/meetings/types.ts:9,28`) |
| v1 `end_time` | `end_time: Option<NaiveDateTime>` (:72,99) | `meetings.end_time TIMESTAMP` (:49) | `MeetingResponse.end_time` (:150) | yes | – | – | `types/meeting.ts:9,27` | **absent** — mobile uses `duration_minutes` (:10,29) |
| v1 `timezone` | `CreateMeetingRequest.timezone: String` **required** (:76), Update optional (:103) | **not persisted** | **not returned** | request only | – | – | `types/meeting.ts:28` | **absent** (never sent → 422) |
| v2 space `timezone` | `CreateMeetingSpaceRequest.timezone` (`dto/v2.rs:97`), `UpdateMeetingSpaceRequest.timezone`, `InstantMeetingRequest.timezone` (:177) | `meeting_spaces.timezone` (`m20261003_000001…:30`) validated IANA (`meeting_space.rs:95`) | `MeetingSpaceResponse.timezone` **FULL view only** (`views.rs:45`) | yes | `models.ts:4674,4756` | untyped Map | `types/v2/index.ts:192,277` | `features/meetings/v2/types.ts:136` (v2 module unused by screens) |
| `scheduledStartAt` | `CreateConferenceRequest` (`dto/v2.rs:383-387`), `RescheduleConferenceRequest` (required, :390), `RecurrenceExceptionRequest` (:876) | `conference_sessions.scheduled_start_at TIMESTAMPTZ` (:101) | `ConferenceResponse` (FULL only, `views.rs:96`), `ConferenceSummary` inside `MeetingSpaceResponse.latestConference` (**any view**, `views.rs:68,75`) | yes (`ConferenceSummary`, `ConferenceResponse`, `CreateConferenceRequest`) | `models.ts:4582,4597,4640` | untyped | `types/v2/index.ts:259,385,390,406` | `v2/types.ts:157,219,230` (unused); legacy screens use `scheduled_at` |
| `scheduledEndAt` | same as above (optional) | `conference_sessions.scheduled_end_at` (:102) | same | yes | yes | untyped | yes | v2 only (unused) |
| `startedAt` / `endedAt` | – | `started_at`, `ended_at` | `ConferenceResponse` (FULL), `ConferenceSummary` | yes | yes | untyped | yes | v2 only |
| `originalStartAt` / `isException` | – (exception uses `originalStartAt`) | `original_start_at`, `is_exception` | `ConferenceResponse` (FULL) | yes | yes | untyped | `types/v2/index.ts:415` | v2 only |
| `rrule` | `SetRecurrenceRequest.rrule` (`dto/v2.rs:485`) | `meeting_recurrence_rules.rrule` (:69) | `RecurrenceResponse.rrule` | yes | yes | untyped | `types/v2/index.ts:437,445` | **none** |
| `startLocal` ↔ `dtstart_local` | `SetRecurrenceRequest.start_local` (NaiveDateTime, space-TZ wall time) | column **`dtstart_local`** (:71) — name differs from API | `RecurrenceResponse.startLocal` | yes (`format` string) | yes | untyped | `types/v2/index.ts:438,447` | none |
| `durationMinutes` | `SetRecurrenceRequest.duration_minutes` 1–1440 | `duration_minutes` CHECK (:72) | `RecurrenceResponse.durationMinutes` | yes | yes | untyped | yes | none (legacy `duration_minutes` is a different, unsupported v1 field) |
| recurrence `timezone` | **not accepted** (taken from space) | `meeting_recurrence_rules.timezone` (:70) | `RecurrenceResponse.timezone`, `RecurrenceExceptionResponse.timezone` | yes | yes | untyped | yes | none |
| `schedulePolicy` (early/late/autoEnd) | Create/Update space (`dto/v2.rs:53`) | `meeting_spaces.schedule_policy JSONB` (:51) | `MeetingSpaceResponse.schedulePolicy` FULL only | yes | yes | untyped | not edited in web UI | none |
| list filter `from`/`to` | `ListConferencesQuery` (`dto/v2.rs:397`) on `GET /meeting-spaces/{id}/conferences` only | filters `scheduled_start_at` (`conference.rs:384-389`) | – | yes (query params) | `operations.ts:365` query from/to | `operations.dart` | used (`ConferencesPanel.tsx:16`) | not used |

## Feature matrix

| Feature | Intended | Backend implementation | APIs available | APIs missing | DB fields | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| Instant meeting (v2) | Space + LIVE conference in one call | `meeting_space.rs:471-540`; handler `spaces.rs` `create_instant_meeting` | POST /api/v2/instant-meetings | – | conference `kind=INSTANT`, `scheduled_start_at=now`, no end | Web `DashboardContent.tsx` → `spaces.service.ts:100` then `/{meetingCode}` | `communications_meetings.rs` (instant via hub, real DB), `api_v2_e2e`; web unit tests only | COMPLETE_VERIFIED (backend); web IMPLEMENTED_UNVERIFIED | Keep |
| Instant meeting (mobile) | Same on mobile | v1 `handlers/meetings.rs:59-180` | POST /api/v1/meetings/instant (410 when flag on) | mobile should call v2 | `meetings` | `app/(tabs)/index.tsx:58-67` reads `response.meeting.id` / `join_data` — backend returns `{success,data}` | none | CONTRACT_MISMATCH | MTG-001 |
| Select future date/time | Pick start/end | `conference.rs:297-351` | POST /api/v2/meeting-spaces/{id}/conferences | – | `scheduled_start_at/end_at` | Web `ConferencesPanel.tsx:20-37` (datetime-local, browser TZ) ; mobile legacy form broken | `api_v2_e2e.rs:124` e2e03, `system_e2e.rs:59` | COMPLETE_VERIFIED (backend) / web IMPLEMENTED_UNVERIFIED / mobile CONTRACT_MISMATCH | MTG-001, MTG-012 |
| Set timezone | Meeting timezone | space `timezone` (validated) ; occurrence has none | POST/PATCH /meeting-spaces | per-occurrence TZ | `meeting_spaces.timezone` | Web create/edit forms | e2e via `system_e2e` (Africa/Lagos) | PARTIALLY_IMPLEMENTED | MTG-012 |
| Set duration | Duration / end | end time ≤24h on create (`conference.rs:304-316`); recurrence `durationMinutes` | as above | – | end column / duration_minutes | Web start+end; mobile `duration_minutes` (not accepted by v1) | e2e03 | COMPLETE_VERIFIED (backend) | MTG-009 for reschedule gap |
| Schedule and save | Persist occurrence | as above | as above | – | yes | Web yes | e2e03 | COMPLETE_VERIFIED (backend) | – |
| Retrieve exact scheduled date/time | Round-trip | `views.rs:96-97` (FULL), `views.rs:68,75` (summary in space) | GET /conferences/{id}, GET /meeting-spaces/{id}/conferences, GET /meeting-spaces (latestConference), GET /resolve/{id} | – | yes (timestamptz µs; JS sends ms → exact) | Web list/dashboard show **`createdAt`** not schedule (`MeetingsContent.tsx:249-254`, `DashboardContent.tsx:169-172`); ConferencesPanel shows schedule in browser TZ | `system_e2e.rs:384-425` asserts stored starts across DST; `api_v2_e2e.rs:216,266` | Backend COMPLETE_VERIFIED; list display FRONTEND_ONLY_GAP; summary-view mismatch CONTRACT_MISMATCH | MTG-004, MTG-011 |
| Edit / reschedule | Move occurrence | `conference.rs:768-823` (CREATED only) | POST /api/v2/conferences/{id}/reschedule | version/optimistic concurrency | yes | Web `ConferencesPanel.tsx:63` | `api_v2_e2e.rs:237` | COMPLETE_VERIFIED with defects | MTG-009 |
| Cancel | Cancel occurrence | `conference.rs:712-766` | POST /api/v2/conferences/{id}/cancel | – | `cancelled_at/by`, `termination_reason` | Web yes | e2e03 | COMPLETE_VERIFIED | – |
| Recurring meetings | RRULE series | `recurrence.rs:47-112,551-620`; materialize horizon 30 d (`config/features.rs:158`) | PUT/GET/DELETE /meeting-spaces/{id}/recurrence | occurrence preview/expansion beyond horizon | rules table | Web `RecurrencePanel.tsx` (raw RRULE text) ; mobile none | `recurrence_exceptions.rs` rx01–rx03 (DST, re-anchor), `system_e2e.rs:384` | COMPLETE_VERIFIED (backend), PARTIALLY (UX/expansion) | MTG-008, MTG-015 |
| Modify series | Change series | Replace only (ends old rule, new rule) `recurrence.rs:551-620` | PUT …/recurrence | "this and following", series edit preserving exceptions | – | Web "Replace series" | rx02 | PARTIALLY_IMPLEMENTED | MTG-014 |
| Modify single occurrence | Exception | `recurrence.rs:623-760`; also via cancel/reschedule | PUT …/recurrence/exceptions; /conferences/{id}/cancel,/reschedule | – | exceptions table | Web ExceptionEditor (raw ISO timestamp) | rx01, rx03 | COMPLETE_VERIFIED (backend) | MTG-015 |
| Invite participants | Members / invitation links / email | `invitations.rs`, members service | POST /meeting-spaces/{id}/invitations, /members, POST /invitations/{token}/resolve | email delivery, ICS invite | `meeting_invitations`, `meeting_members` | Web InvitationsPanel ("does not send an email") | `api_v2_e2e.rs:509` e2e12, `system_e2e` m_e2e_01 | COMPLETE_VERIFIED (links) / MISSING (email) | MTG-006 |
| Schedule-change notifications | Email/push on reschedule/cancel | outbox events `conference.rescheduled/cancelled` → realtime only (`realtime.rs:402-414`); platform events don't map them (`platform/events.rs:118-127`) | none | notification fan-out | outbox_events | none | – | MISSING | MTG-006 |
| Reminders | Pre-meeting reminder job | none (`conference_scheduler.rs:12` "No auto-start… reminders are never authorization dependencies"); `send_notification_email` has no caller | none | reminder worker | none | none | – | MISSING | MTG-006 |
| List upcoming meetings | Agenda for me | per-space only + `latestConference` (single) | GET /meeting-spaces, GET /meeting-spaces/{id}/conferences?from= | GET /me/conferences (cross-space), status filter, pagination | yes | Web list shows spaces w/ createdAt; per-space panel | – | PARTIALLY_IMPLEMENTED | MTG-005 |
| List past meetings | History | per-space list with `to=` (no status filter) | same | cross-space history | yes | Per-space ConferencesPanel date filter | – | PARTIALLY_IMPLEMENTED | MTG-005 |
| Join at appropriate time | Window enforcement | start window `conference.rs:873-893` (early 15 min / late grace 60 min); join requires LIVE (`admission.rs:628-630`); no auto-start | POST /conferences/{id}/start, /join | – | schedule_policy | Pre-join countdown disables button (`pre-join-screen.tsx:582-606`), no start control in pre-join | e2e03 (window refusal), `conference.rs:995` unit | COMPLETE_VERIFIED (backend), UX PARTIAL | MTG-013 |
| Meeting history | Past conferences + attendance | list conferences; `/attendance`; artifacts | GET /conferences/{id}/attendance, /artifacts, /recordings | cross-space | yes | Web ConferenceWorkspace (participants, recordings); attendance not shown | `system_e2e` m_e2e_09 | IMPLEMENTED_UNVERIFIED (UI) | MTG-005 |
| Personal calendar (day/week/month) | Range queries across spaces | none cross-space | per-space from/to only | GET /me/conferences?from&to | – | none (`/dashboard/schedule` redirects to `/dashboard/meetings`) | – | MISSING | MTG-005 |
| Organization calendar | Org meetings | `organization_id` on space; no org listing (only membership queries `organizations.rs:117,311`) | none | GET /organizations/{id}/conferences | `meeting_spaces.organization_id` | none | `organization_domain_access.rs` (access only) | MISSING | MTG-016 |
| Recurring expansion in range | Virtual occurrences | materialized rows only within 30 d | – | expansion | – | – | `recurrence.rs:895` unit | PARTIALLY_IMPLEMENTED | MTG-008 |
| Timezone & DST | Wall-time stability | rrule with TZID (`recurrence.rs:79-91`), re-anchor on TZ change | – | – | – | Web shows browser TZ only | rx01/rx02, m_e2e_06 | COMPLETE_VERIFIED (recurrence); display PARTIAL | MTG-012 |
| ICS export/import / calendar invitations | .ics | none (`grep -rln "VCALENDAR\|text/calendar"` → no source hits) | none | GET /conferences/{id}/calendar.ics, GET /me/calendar.ics | – | none | – | MISSING | MTG-007 |
| External calendars (Google/Outlook) | Sync | none (only Google OAuth login) | none | provider link + sync | – | none | – | DEFERRED_BY_OWNER (spec "Optional") | MTG-007 |
| Admission / waiting room | Lobby + admit/deny | `admission.rs`, `handlers/v2/participants.rs` | GET /conferences/{id}/waiting-room, POST …/admit, …/deny, …/return-to-waiting | – | `conference_participants.admission_state` | Web ParticipantsPanel + pre-join polling; mobile legacy (broken) | e2e07_08, e2e09, m_e2e_01 | COMPLETE_VERIFIED (backend) | MTG-002 for mobile |
| Host controls | lock/unlock/mute/remove/ban/role | `conference.rs:580-623`, participants handlers | /lock,/unlock,/participants/{pid}/{mute,remove,ban,role} | – | – | Web | e2e13–16 | COMPLETE_VERIFIED | – |
| End meeting | End for all | `conference.rs:625-710` | POST /conferences/{id}/end | – | ended_at, termination_reason | Web ConferenceRow; mobile calls v1 `/meetings//end` | e2e18, m_e2e_01 | COMPLETE_VERIFIED (backend) / mobile CONTRACT_MISMATCH | MTG-002 |
| Auto-end | End after scheduled end + grace | `conference_scheduler.rs:68-114` | worker | – | schedule_policy | – | `failure_injection_m.rs:284` | COMPLETE_VERIFIED | MTG-009 (end wiped on reschedule disables it) |
| Stale/missed occurrence expiry | Mark never-started occurrences | none (index `idx_conference_scheduler_start` unused) | – | expiry transition | – | – | – | MISSING | MTG-010 |
| Meeting links / resolve | Code/link → space + canonical conference | `compat.rs:94-172` | GET /resolve/{identifier}, GET /meeting-spaces/{id} | – | meeting_code | Web/mobile pre-join | e2e02 | COMPLETE_VERIFIED; canonical choice defect | MTG-010 |
| Participant tokens | Guest credential | join response `participantToken` | header X-Participant-Token | – | `credential_hash` | Web `storageStore.setParticipantToken` | `credential_replay.rs`, e2e14 | COMPLETE_VERIFIED | – |
| LiveKit join | Media credential | `admission.rs` RtcOutcome; `rtc_provider.rs` | join response `rtc` | – | rtc_room_* | Web live room; mobile live room | CI uses LiveKit mock; real LiveKit tests `#[ignore]` | IMPLEMENTED_UNVERIFIED (real media) | – |
| Meeting chat linkage | Conference chat | `handlers/v2/collaboration.rs` messages | GET/POST /conferences/{id}/messages | – | – | Web v2 branch; legacy ChatProvider in fallback | collab_* tests | COMPLETE_VERIFIED (backend) | MTG-019 |
| Recordings link | Recording per conference | artifacts handlers | /conferences/{id}/recordings | – | meeting_artifacts | Web ConferenceRecordingsPanel | m_e2e_09 | COMPLETE_VERIFIED (backend, mock storage) | – |
| Organization meeting scheduling | Create org-scoped meeting | `organization_id` + `ORGANIZATION_ONLY` validated (`meeting_space.rs:153,452`) | POST /meeting-spaces (+ /conferences) | – | yes | Web create form has no org/ORGANIZATION_ONLY option | `organization_domain_access.rs` | FRONTEND_ONLY_GAP | MTG-016 |
| Legacy v1 scheduling | Back-compat | `handlers/meetings.rs:303-390` | POST/PUT/DELETE /api/v1/meetings (410 when flag on) | v2 mirroring of new legacy rows | meetings | Web: none for create; mobile: all | `legacy_retirement.rs`, `legacy_v1_*` | PARTIALLY_IMPLEMENTED | MTG-017 |

## Findings

### MTG-001 — Mobile meeting creation, scheduling, listing, editing and instant meeting are broken (wrong v1 contract, and retired by the legacy-write flag)
- Service/module: apps/mobile `features/meetings/*`, `app/(tabs)/{index,schedule,meetings}.tsx`, `app/(meetings)/[id]/edit.tsx` → meeting-service v1 `/api/v1/meetings*`.
- Expected: Mobile users can create instant/scheduled meetings, list them, and see/edit date/time, using the supported v2 API (spec: "no new meeting feature built on v1", PDF line ~2336).
- Actual:
  1. Request mismatch: mobile `CreateMeetingRequest = {title, description, scheduled_at, duration_minutes, is_private}` (`features/meetings/types.ts:25-31`, `schemas/meeting.schema.ts:3-15`, posted by `schedule-form.tsx:91`). Backend `CreateMeetingRequest` requires `start_time: NaiveDateTime` and `timezone: String` (`services/meeting-service/src/dto/meetings.rs:58-83`) and uses plain `axum::Json` (`handlers/meetings.rs:303-306`) → missing-field deserialization rejection (axum 422) for every scheduled meeting. Same for update (`app/(meetings)/[id]/edit.tsx:108-109` sends `scheduled_at`, `duration_minutes`).
  2. Response mismatch: backend wraps in `{success, data, meta}` (`dto/common.rs:10-20`); mobile reads `response.data.meeting` (`meetings.api.ts:29-31,38-41`), `data.meetings` (`meeting-list.tsx:32`, `app/(tabs)/index.tsx` `meetingsData?.meetings`), `response.meeting.id`/`join_data` after instant (`app/(tabs)/index.tsx:58-66`) — backend instant returns `data.{id, join_token, livekit_url, room_name}` (`dto/meetings.rs:26-46`). Lists render empty; instant navigation reads `undefined.id` (crash/undefined route param). `meeting-card.tsx:57,63` formats `meeting.scheduled_at`, a field the backend never returns (`start_time`).
  3. Flag: all those calls are writes on `/api/v1/meetings`, wrapped by `legacy_writes()` (`routes/mod.rs:63-71`) → `410 LEGACY_WRITE_DISABLED` when `FEATURE_LEGACY_MEETING_WRITE_DISABLED=true` (`middleware/legacy_write.rs:26-44`). Default is `false` (`config/features.rs:75`, `.env.example:92`), so today mobile fails on (1)/(2); after retirement it fails on (3) too.
  4. Even a successfully created v1 meeting is not joinable through mobile's own pre-join, which resolves via v2 (`features/room/components/pre-join-screen.tsx:14,34`) and needs a backfilled space (see MTG-017).
- Evidence: `grep -n "scheduled_at\|duration_minutes" apps/mobile/features/meetings/types.ts` ; `sed -n 58,83p services/meeting-service/src/dto/meetings.rs`; `grep -rn "meetingsApi\." apps/mobile/features`.
- API/Swagger refs: `packages/contracts/openapi/meeting-service.json` `/api/v1/meetings` (CreateMeetingRequest requires start_time, timezone).
- Affected frontend workflow: Mobile Home "New Meeting", Schedule tab, Meetings tab list, Edit meeting.
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P0 (critical broken workflow — mobile cannot schedule or list meetings)
- Business impact: Mobile scheduling product is non-functional; any mobile release exposes the defect to all users.
- Recommended remedy: Reuse existing v2 endpoints (no new backend): instant → `POST /api/v2/instant-meetings`; schedule → `POST /api/v2/meeting-spaces` then `POST /api/v2/meeting-spaces/{id}/conferences` (or reuse an existing space); list → `GET /api/v2/meeting-spaces` + per-space `GET …/conferences`; edit → `POST /api/v2/conferences/{id}/reschedule`. Fix `features/meetings/v2/api.ts` paths first (MTG-003) and mount the existing (unused) `features/meetings/v2/components/schedule-form.tsx`. Remove the v1 meeting client from mobile.
- Dependencies: MTG-003, MTG-005 (a cross-space upcoming endpoint makes the mobile list efficient).
- Complexity: MEDIUM
- Regression risk: LOW backend (none), MEDIUM mobile.
- Confidence: CONFIRMED

### MTG-002 — Mobile live room drives a v2 conference through legacy v1 participant/waiting/chat/media/end endpoints
- Service/module: apps/mobile `features/room/api/room.api.ts:4-14`, `features/room/components/{participants-panel,room-controls}.tsx`, `features/room/hooks/use-media-controls.ts:62-66`; `app/(room)/[id]/live.tsx:4-7`.
- Expected: After a v2 join, host admits/denies via `/api/v2/conferences/{id}/waiting-room/*`, chats via `/conferences/{id}/messages`, ends via `/conferences/{id}/end`.
- Actual: mobile pre-join maps the v2 join to legacy `JoinMeetingData` with `meeting_id = meetingSpaceId`, `room_name = conference.id`, no `id` (`pre-join-screen.tsx:39-51`). The live room then calls v1 `/meetings/{meetingSpaceId}/waiting`, `/participants`, `/chat`, `/participants/{pid}/audio` and `/meetings/{joinData.id}/end` with `joinData.id === undefined` (`room-controls.tsx:22`). A space id is not a legacy `meetings.id` → 404; writes → 410 when the flag is on. `isHost = joinData?.role === "host"` (`room-controls.tsx:39`) excludes OWNER (role is lower-cased v2 role `owner`). v2 equivalents exist in `features/room/v2/components/*` but `live.tsx` mounts the legacy components.
- Evidence: `grep -n "meetingsApi" apps/mobile/features/room/api/room.api.ts`; `sed -n 36,52p apps/mobile/features/room/components/pre-join-screen.tsx`.
- API/Swagger refs: v2 `/api/v2/conferences/{id}/waiting-room/admit|deny`, `/end`, `/messages`.
- Affected frontend workflow: Mobile host admit, chat, mute, end (Journey D on mobile).
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P1
- Business impact: Mobile hosts cannot admit waiting guests or end meetings; guests in waiting rooms stall.
- Recommended remedy: Mount `features/room/v2/components/room-sidebar.tsx` (already uses v2) and switch end/admit/media to v2 conference endpoints keyed by `conference.id`. No backend change.
- Dependencies: MTG-003
- Complexity: MEDIUM
- Regression risk: MEDIUM (mobile only)
- Confidence: CONFIRMED

### MTG-003 — Mobile v2 client targets non-existent paths
- Service/module: `apps/mobile/features/meetings/v2/api.ts:81-143`.
- Expected: `/api/v2/meeting-spaces`, `/api/v2/instant-meetings`, `/api/v2/conferences/{id}`.
- Actual: `V2_BASE = ""` with paths `/spaces`, `/spaces/{id}`, `/spaces/{id}/archive`, `/instant`, `/spaces/{id}/conferences`, `/spaces/{id}/conferences/{cid}` → v2 router fallback `ENDPOINT_NOT_FOUND` (`routes/v2.rs:503-505`). Only `resolve`, `join`, `pre-join`, `leave`, lifecycle paths are correct. These functions are only used by the unmounted `features/meetings/v2/components/*` (grep shows no screen imports them).
- Evidence: `sed -n 81,143p apps/mobile/features/meetings/v2/api.ts`; route list `services/meeting-service/src/routes/v2.rs:38-110`.
- API/Swagger refs: `v2_create_meeting_space`, `v2_create_instant_meeting`, `v2_get_conference`.
- Affected frontend workflow: Any future mobile v2 scheduling.
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P1 (blocks the MTG-001 fix)
- Business impact: The ready-made mobile v2 schedule form cannot work as written.
- Recommended remedy: Correct paths; consider generating the mobile client from the TS SDK (`sdks/typescript`) to avoid drift.
- Dependencies: –
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-004 — Web meeting list and dashboard show the space creation date, not the scheduled date/time
- Service/module: apps/web `components/dashboard/MeetingsContent.tsx:249-254`, `components/dashboard/DashboardContent.tsx:167-172`.
- Expected: Spec F-02: "List owned/member spaces, next occurrence, LIVE badge… Cards open correct latest/live occurrence". After scheduling, the user sees the scheduled date/time.
- Actual: Cards render a calendar icon + `formatShortDateForDisplay(space.createdAt)`. `latestConference` (which the backend already returns with `scheduledStartAt/EndAt/status`, `views.rs:68-82`) is typed in web (`types/v2/index.ts:297`) but never read (`grep -rn latestConference apps/web/src` → only the type). `createdAt` is also FULL-view only (`views.rs:63`), so members with summary view see no date at all. The only place a schedule is visible is inside each space's ConferencesPanel (`ConferencesPanel.tsx:54`).
- Evidence: `grep -n "createdAt\|Calendar" apps/web/src/components/dashboard/MeetingsContent.tsx`.
- API/Swagger refs: `MeetingSpaceResponse.latestConference` (`ConferenceSummary`) present in Swagger and TS SDK (`models.ts:4906`).
- Affected frontend workflow: Dashboard "Your Meeting Spaces", `/dashboard/meetings` list (the "retrieve scheduled date/time" symptom).
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P1
- Business impact: Users believe the schedule was lost or wrong; no LIVE / next-occurrence indication on cards.
- Recommended remedy: Reuse `latestConference` (status badge + `scheduledStartAt` formatted in space TZ and viewer TZ). For multiple upcoming occurrences use MTG-005 endpoint. No backend change required for the basic fix.
- Dependencies: MTG-010 (canonical conference choice must be correct), MTG-011.
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-005 — No cross-space "my meetings" / agenda / calendar range API; upcoming & past listing requires N+1 per-space calls and excludes invitees
- Service/module: meeting-service `services/conference.rs:369-398` (`list_for_space`), `services/meeting_space.rs:616-668` (`list_for_actor`), `routes/v2.rs` (no `/me/conferences`, no `/conferences` list).
- Expected: Personal calendar: day/week/month range across all spaces the user owns/is member of/is invited to/registered for; upcoming list; past/history; status filter; pagination (spec F-02 "next occurrence… pagination", F-04 "upcoming/past sessions").
- Actual: Only `GET /api/v2/meeting-spaces` (unpaginated, one canonical conference per space) and `GET /api/v2/meeting-spaces/{id}/conferences?from&to` (per space, member-only via `actor_space_role`, `conference.rs:378-381`; no status filter, no pagination, filter on `scheduled_start_at` only so unscheduled rows drop out of ranges). Invitation-token holders, webinar registrants and organization members who are not space members see nothing. `/dashboard/schedule` simply redirects to `/dashboard/meetings` (`app/dashboard/schedule/page.tsx`). Legacy v1 list is host-only, ordered by `created_at`, no date range (`handlers/meetings.rs:211-247`).
- Evidence: `grep -n "route(" services/meeting-service/src/routes/v2.rs | grep -i "conferences\b\|me/"`.
- API/Swagger refs: `v2_list_conferences` (query from/to), `v2_list_meeting_spaces` (no params).
- Affected frontend workflow: Dashboard, Meetings list, any calendar view, mobile list.
- Owner class: BACKEND
- Classification: MISSING
- Severity: P1
- Business impact: No calendar/agenda product possible without O(spaces) requests; invitees cannot find their meetings.
- Recommended remedy: New endpoint justified (existing per-space endpoint cannot express "all my spaces" efficiently): `GET /api/v2/me/conferences?from&to&status&cursor&limit` returning `ConferenceResponse` + minimal space summary (title, code, timezone), joining `conference_sessions` × (owned ∪ active member spaces ∪ optionally registrations). Reuse `list_for_space` filter logic and `views::conference`. Index `idx_conference_space_start` already supports it. Optionally add `status` filter to the existing per-space list. No migration needed.
- Dependencies: MTG-008 (horizon), MTG-010.
- Complexity: MEDIUM
- Regression risk: LOW (additive)
- Confidence: CONFIRMED

### MTG-006 — No invitations by email, no schedule-change notifications, no reminders
- Service/module: `services/invitations.rs` (no email), `services/email.rs:93-110` (`send_notification_email` — zero callers: `grep -rn send_notification_email services` → definition only), `workers/conference_scheduler.rs:12-13`, `services/platform/events.rs:118-127`, `handlers/v2/realtime.rs:402-414`.
- Expected: Invite by email (spec F-06 "invite by email"); invitees notified on reschedule/cancel; reminders before start.
- Actual: Creating an invitation returns a bearer token shown once; UI states "Creating an invitation does not send an email" (`InvitationsPanel.tsx:30`). Reschedule/cancel write outbox events `conference.rescheduled` / `conference.cancelled` (`conference.rs:749,812`) that only reach currently connected realtime subscribers (nobody is connected to an unstarted conference) and are not mapped to developer platform events (only created/started/ended/participant/recording/artifact). No reminder worker exists (`workers/mod.rs:25-32`).
- Evidence: commands above; `grep -rn "reminder" services/meeting-service/src` → only the scheduler comment.
- API/Swagger refs: none exist.
- Affected frontend workflow: Invite → reschedule → attendee awareness (Journey C steps 5–6).
- Owner class: BACKEND (+ PRODUCT_DECISION on channels)
- Classification: MISSING
- Severity: P1
- Business impact: Attendees miss rescheduled/cancelled meetings; invitations must be shared manually.
- Recommended remedy: Reuse the existing email queue (`queues/redis_email.rs`, `services/email.rs`) and the transactional outbox: add an outbox consumer for `conference.rescheduled|cancelled|created` that emails active members + email-bound invitations (attach ICS from MTG-007); add a reminder pass to `ConferenceScheduler::tick` using `idx_conference_scheduler_start` with a `reminder_sent_at` column (small additive migration). Map rescheduled/cancelled to platform events for developers.
- Dependencies: MTG-007, email delivery infra.
- Complexity: MEDIUM
- Regression risk: LOW–MEDIUM (outbox consumer idempotency)
- Confidence: CONFIRMED

### MTG-007 — No ICS export/import and no external calendar integration
- Service/module: whole repo.
- Expected: Calendar invitations (.ics), export; external calendars optional per spec ("External calendar sync — Optional authorized provider linking", PDF ~line 860; rollout plan `docs/phase2-certification/12-rollout-plan.md:141`).
- Actual: No `text/calendar`/`VCALENDAR` producer in any service source; Google appears only as OAuth login.
- Evidence: `grep -rln "text/calendar\|VCALENDAR\|icalendar" services apps --include=*.rs --include=*.ts --include=*.tsx` → none (only a stale `.next` build artefact).
- API/Swagger refs: none.
- Affected frontend workflow: "Add to calendar", email invitations.
- Owner class: BACKEND / PRODUCT_DECISION
- Classification: MISSING (ICS) ; DEFERRED_BY_OWNER (Google/Outlook sync)
- Severity: P2 (ICS); P3 (external sync)
- Business impact: Meetings don't land in users' calendars.
- Recommended remedy: New read-only endpoints are justified (no existing representation): `GET /api/v2/conferences/{id}/calendar.ics` (single VEVENT, member/admitted auth) and `GET /api/v2/meeting-spaces/{id}/calendar.ics` (VEVENT with RRULE + EXDATE/RECURRENCE-ID from exceptions — data already stored). Personal feed later. Defer provider sync.
- Dependencies: MTG-005
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-008 — Recurring occurrences exist only within a 30-day horizon; range queries beyond it return nothing
- Service/module: `services/recurrence.rs:125-262` (materialize from `now` to `now + occurrence_horizon_days`), `config/features.rs:133,158` (30 d), `conference.rs:369-398` (list reads rows only).
- Expected: Month/quarter calendar views and "next occurrence preview" (spec F-05) show all series occurrences.
- Actual: `GET …/conferences?from=<+45d>` returns no recurring occurrences; future exceptions are visible only via `/recurrence/exceptions`. There is no preview/expansion endpoint. Recurrence rules refuse BYHOUR/BYMINUTE (`recurrence.rs:71-78`) — fine, but documented nowhere in UI.
- Evidence: `sed -n 125,150p services/meeting-service/src/services/recurrence.rs`; unit `materialization_window_is_bounded` (`recurrence.rs:895-912`).
- API/Swagger refs: `v2_list_conferences`, `v2_get_recurrence`.
- Affected frontend workflow: Calendar month view, recurring series preview.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Long-range planning appears empty.
- Recommended remedy: Reuse existing `occurrences_between()` + exceptions to add virtual (non-persisted, `id=null`, `occurrenceKey` set) items when `to > materialized_until`, either in the existing list endpoint (flag `expand=true`) or a new `GET /meeting-spaces/{id}/recurrence/occurrences?from&to` (preview). No migration.
- Dependencies: –
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-009 — Reschedule silently clears the end time and skips the create-time validations
- Service/module: `services/conference.rs:768-823`.
- Expected: Reschedule keeps duration when only start is moved; enforces the same rules as create (end within 24 h; future start); optimistic concurrency like other v2 writes.
- Actual: `am.scheduled_end_at = Set(req.scheduled_end_at)` (:793) — omitting `scheduledEndAt` (allowed: `Option`, `dto/v2.rs:390-394`) sets the end to NULL, which disables auto-end (`conference_scheduler.rs:73` filters `ScheduledEndAt.lt(now)`) and the late-start window (`check_start_window` :886). No 24 h cap (create has it, :308), no future-start check (create has none either — see MTG-010), no `version` field (last write wins). Only `CREATED` can be rescheduled; a `PREPARING`/`WAITING` (after "Prepare") occurrence cannot be moved (:786). Web always sends both (`scheduledWindow`, `lib/meeting-scheduling.ts:17-21`) so web is not affected; SDK/other clients are.
- Evidence: `sed -n 768,800p services/meeting-service/src/services/conference.rs`.
- API/Swagger refs: `RescheduleConferenceRequest` (`scheduledEndAt` optional).
- Affected frontend workflow: Reschedule (Journey C step 6) for SDK/mobile clients.
- Owner class: BACKEND
- Classification: IMPLEMENTED_UNVERIFIED (defect path untested — `api_v2_e2e.rs:237` sends both)
- Severity: P2
- Business impact: Rescheduled meetings never auto-end; data inconsistency between create and reschedule.
- Recommended remedy: Preserve duration when end omitted (`new_end = old_end.map(|e| new_start + (e - old_start))`), add 24 h cap and `scheduledStartAt > now` checks, optional `version`. Allow reschedule from PREPARING/WAITING by releasing the room or document the restriction.
- Dependencies: –
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-010 — Past start times accepted, stale occurrences never expire, and the "canonical conference" can point at a stale occurrence
- Service/module: `services/conference.rs:297-351`; `services/meeting_space.rs:581-613` and duplicate `services/compat.rs:146-172`; `workers/conference_scheduler.rs`.
- Expected: Scheduling in the past rejected (legacy v1 rejects `start <= now+1min`, `handlers/meetings.rs:329`); unstarted occurrences whose window passed become "missed/expired"; resolve/pre-join/dashboard choose the next real upcoming occurrence.
- Actual: `create_occurrence` validates only end>start and ≤24 h. No worker transitions CREATED rows after their window (scheduler only materializes and auto-ends LIVE, `conference_scheduler.rs:57-66`; index `idx_conference_scheduler_start` (`m20261003_000001…:138`) is unused). `canonical_conference` loads all conferences ordered by `scheduled_start_at ASC` and returns the first unstarted one whose `scheduled_end_at` is NULL **or** in the future (`meeting_space.rs:600-608`). A start-only occurrence created last week (or an unscheduled one) remains "upcoming" forever and wins over tomorrow's occurrence; `check_start_window` lets it start any time after its start (no end → no late cutoff). This drives `GET /resolve/{code}` (pre-join on web and mobile) and `latestConference` on cards. Logic is duplicated verbatim in two services.
- Evidence: `sed -n 581,613p services/meeting-service/src/services/meeting_space.rs`; `sed -n 146,172p services/meeting-service/src/services/compat.rs`.
- API/Swagger refs: `v2_resolve_identifier`, `MeetingSpaceResponse.latestConference`.
- Affected frontend workflow: Joining by link/code (pre-join picks wrong occurrence), dashboard card status.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Attendees land on an old occurrence; hosts see "upcoming" meetings that never happened.
- Recommended remedy: Reject past `scheduledStartAt` on create (allow small skew); treat NULL end as `start + default duration` in canonical selection; add scheduler pass to mark unstarted occurrences past `end + lateStartGrace` (or `start + N h` when no end) as CANCELLED with `termination_reason=EXPIRED` (needs CHECK-constraint migration for the new reason) or a derived `MISSED` view flag (no migration). Extract one shared `canonical_conference` and query only candidate rows (LIMIT) instead of loading all.
- Dependencies: –
- Complexity: MEDIUM
- Regression risk: MEDIUM (resolution behaviour change; covered by e2e02)
- Confidence: CONFIRMED

### MTG-011 — Summary-view inconsistency: space summary leaks the schedule, conference summary hides it (breaks guest countdown; exposes healthcare appointment times)
- Service/module: `handlers/v2/views.rs:20-70` (space), `:84-108` (conference); web `components/room/pre-join-screen.tsx:163-204`.
- Expected: One consistent rule for who may see schedule data; pre-join can show "starts in…" for invited guests; HEALTHCARE/HIGH_SECURITY spaces reveal nothing before authorization (spec: "Handle healthcare/private meeting title as restricted until authorized").
- Actual: `latest_conference` is projected unconditionally (`views.rs:68`) including `scheduledStartAt/EndAt/startedAt/endedAt` for anyone resolving the code (unauthenticated `GET /meeting-spaces/{code}`, `security((), …)`), even when the title is hidden for HEALTHCARE/HIGH_SECURITY (`views.rs:22-26`). Conversely `GET /conferences/{id}` and `GET /resolve/{id}`'s `conference` hide `scheduledStartAt` for non-authorized callers (`views.rs:96`). Web pre-join reads `resolveData.conference.scheduledStartAt` (`pre-join-screen.tsx:192-201`), so guests/non-members never see the countdown and simply get `409 CONFERENCE_NOT_LIVE "This conference has not started yet"` (`admission.rs:234-242`).
- Evidence: code lines above; no test asserts space-summary key set (`grep -rn latestConference services/meeting-service/tests` → only the communications facade test).
- API/Swagger refs: `MeetingSpaceResponse`, `ConferenceResponse` (`view` enum).
- Affected frontend workflow: Guest pre-join; healthcare appointment privacy.
- Owner class: SHARED_CONTRACT
- Classification: CONTRACT_MISMATCH
- Severity: P2 (privacy aspect could be raised to P1 by the owner for healthcare tenants)
- Business impact: Confusing guest UX; appointment timing disclosed to anyone with a code.
- Recommended remedy: Decide policy: either expose `scheduledStartAt/EndAt` in conference SUMMARY for non-restricted profiles (reuse existing field; pre-join works unchanged) and strip schedule from `latestConference` for HEALTHCARE/HIGH_SECURITY summary, or hide both. Add a contract test on summary keys.
- Dependencies: PRODUCT_DECISION
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-012 — Timezone semantics are split and the UI ignores the meeting timezone for one-off occurrences
- Service/module: `dto/v2.rs:383-394` (no timezone on conference requests), `conference_sessions` (no TZ column), web `components/conferences/ConferencesPanel.tsx:19-37,54`, `RecurrencePanel.tsx:55`; legacy `utils/datetime.rs:24-34`.
- Expected: Spec J-02: "Create conference occurrence… schedule in space timezone"; F-05 acceptance: "All dates display in selected meeting timezone and participants' local time with labels".
- Actual: One-off occurrences are UTC instants entered via `datetime-local` interpreted in the **browser** timezone ("Dates below use {browser TZ}", `ConferencesPanel.tsx:19,35`) and displayed with `toLocaleString()` only; the space `timezone` affects only recurrence (`RecurrencePanel.tsx:15,39`). Recurrence exceptions are entered in browser TZ (:55) while the series is in space TZ. Space `timezone` is FULL-view only (`views.rs:45`), so participant-role members cannot label times. Legacy v1 converts with `.earliest()` (ambiguous DST → earliest; gap → error) and does **not persist** the timezone (no column), so v1 clients cannot re-display the user's chosen zone.
- Evidence: lines above.
- API/Swagger refs: `CreateConferenceRequest`, `MeetingSpaceResponse.timezone`.
- Affected frontend workflow: Schedule, reschedule, display.
- Owner class: SHARED_CONTRACT (mostly FRONTEND)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Host in one zone scheduling a meeting "in Africa/Lagos" gets browser-local times; attendees see no meeting-zone label.
- Recommended remedy: Frontend: convert `datetime-local` using space timezone (e.g., Temporal/`date-fns-tz`) and display both zones. Backend (optional): accept `{startLocal, timezone}` on create/reschedule as an alternative to UTC instants, and expose space `timezone` in summary view (non-sensitive). No migration needed.
- Dependencies: –
- Complexity: LOW–MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-013 — Joining at the scheduled time: no auto-start and no host start in pre-join
- Service/module: `services/admission.rs:628-630` (join requires LIVE), `services/conference.rs:497-549,873-893` (start window), `workers/conference_scheduler.rs:12` (no auto-start by design); web `pre-join-screen.tsx:582-617`.
- Expected: At start time attendees can join (or wait in a lobby until host starts); host can start from the join screen within the early-start window.
- Actual: Backend enforces a start window (default 15 min early, 60 min after scheduled end; `dto/v2.rs:63-75`) and requires a host `POST /start`; attendees joining a CREATED conference get `409 CONFERENCE_NOT_LIVE`. Web pre-join shows a disabled "Meeting Not Started" until `scheduledStartAt` (even for the host, although early start is allowed) and has no Start action; host must go to dashboard → ConferencesPanel "Start" (`ConferencesPanel.tsx:61`) then "Join occurrence". After the countdown, attendees' Join fails until the host starts; no lobby/"waiting for host" state for unstarted conferences.
- Evidence: lines above; `api_v2_e2e.rs:143-155` (start refused outside window).
- API/Swagger refs: `v2_start_conference`, `v2_join_conference`.
- Affected frontend workflow: Journey C step 7, J-02 step 13.
- Owner class: FRONTEND (+ PRODUCT_DECISION on auto-start)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Attendees hit errors at meeting time; host friction.
- Recommended remedy: Reuse existing endpoints: pre-join shows "Start meeting" when `conferencePermission(space,"start")` and within `schedulePolicy.earlyStartMinutes`, calling `/start` then `/join`; for others show "Waiting for host" and poll `GET /conferences/{id}` (or realtime). Optional product decision: auto-start per space policy flag in `schedule_policy` (no migration; JSONB).
- Dependencies: MTG-011 (guests need the schedule to show waiting state)
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-014 — Series modification only by replacement; exceptions orphaned and rescheduled occurrences survive "end series"
- Service/module: `services/recurrence.rs:551-620` (`set`), `:264-311` (`retire_future_occurrences`), `:785-815` (`end`).
- Expected: Edit series (time/duration/rule) while keeping per-occurrence exceptions; "this and following"; ending a series removes all its future occurrences (or clearly asks).
- Actual: PUT creates a new rule and ends the old one; future non-exception occurrences are deleted/cancelled but exception rows (`is_exception=true`, e.g. a rescheduled occurrence) of the old rule are kept as live CREATED conferences, and the exception records stay keyed to the old rule (new rule doesn't inherit them; `list_exceptions` shows only the active rule's). The new rule may then generate an occurrence on the same day → duplicates. `end` likewise keeps rescheduled exceptions alive (documented in UI: "exception occurrences are preserved", `RecurrencePanel.tsx:21`). No "this and following" split. No version on PUT.
- Evidence: `sed -n 264,311p` and `sed -n 572,580p services/meeting-service/src/services/recurrence.rs`.
- API/Swagger refs: `v2_set_recurrence`, `v2_end_recurrence`.
- Affected frontend workflow: Modify recurrence series.
- Owner class: BACKEND / PRODUCT_DECISION
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Ghost/duplicate occurrences after editing a series.
- Recommended remedy: Add `PATCH /meeting-spaces/{id}/recurrence` (version-checked) that re-anchors the same rule (reuse `follow_space_timezone`-style re-anchor with `include_exceptions=true`) instead of replacement; on replace, cancel the old rule's future exception occurrences too (or migrate exceptions whose local date still matches). Add `until`-split for "this and following". No schema change.
- Dependencies: –
- Complexity: MEDIUM
- Regression risk: MEDIUM (covered by rx01–rx03)
- Confidence: CONFIRMED

### MTG-015 — Recurrence UI requires raw RRULE text and exact ISO timestamps; no occurrence preview
- Service/module: web `components/meeting-spaces/RecurrencePanel.tsx:38-55`, `lib/meeting-scheduling.ts:9-26`.
- Expected: Spec F-05: "Timezone-aware date/time picker, next occurrence preview, RFC 5545-compatible recurrence" (a builder).
- Actual: User types `FREQ=WEEKLY;BYDAY=MO…`; to add an exception user must type "the exact original timestamp with a timezone offset" (`RecurrencePanel.tsx:52`). No preview of generated dates (backend has no preview endpoint; MTG-008). Mobile has no recurrence at all.
- Evidence: file lines above.
- API/Swagger refs: `SetRecurrenceRequest`, `RecurrenceExceptionRequest`.
- Affected frontend workflow: Create recurring meeting, modify single occurrence.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: Recurring meetings effectively unusable for non-technical users.
- Recommended remedy: Recurrence builder (frequency/days/count/until) generating RRULE; pick exceptions from the occurrence list (each conference has `originalStartAt`, so use per-row cancel/reschedule that already exist). Reuse MTG-008 expansion for preview.
- Dependencies: MTG-008
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-016 — Organization meetings: backend supports org-scoped spaces, web cannot create them and nobody can list them
- Service/module: backend `services/meeting_space.rs:153,171,452,968` (organizationId validation/verification), `services/organizations.rs` (membership only); web `components/meeting-spaces/CreateMeetingSpaceContent.tsx:52-59,121-131`, `components/dashboard/OrganizationsContent.tsx`.
- Expected: Journey B last step — an org admin schedules an organization meeting visible to org members (spec F-03 "optional organization").
- Actual: Create form never sends `organizationId` and offers only PUBLIC/PUBLIC_PASSCODE/AUTHENTICATED/INVITE_ONLY (no ORGANIZATION_ONLY); `grep -rn organizationId apps/web/src/components` → only OrganizationsContent member management. Backend has no org-scoped space/conference listing (no `OrganizationId.eq` filter on spaces), and `list_for_actor` only returns owned/member spaces, so org members don't see org meetings unless added individually.
- Evidence: commands above.
- API/Swagger refs: `CreateMeetingSpaceRequest.organizationId`, `AccessPolicy.ORGANIZATION_ONLY`.
- Affected frontend workflow: Journey B final step.
- Owner class: FRONTEND + BACKEND
- Classification: FRONTEND_ONLY_GAP (create) / MISSING (org listing/calendar)
- Severity: P2
- Business impact: Organizations cannot run org-wide scheduled meetings from the product.
- Recommended remedy: Web: add organization selector (from `GET /api/v2/me/organizations`) + ORGANIZATION_ONLY option — reuse existing endpoint. Backend: new `GET /api/v2/organizations/{id}/conferences?from&to` (org members only) or include org spaces in MTG-005's `/me/conferences`.
- Dependencies: MTG-005
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-017 — Legacy v1 write path is a dead end: new v1 meetings are never mirrored to v2, so current clients cannot join them
- Service/module: `handlers/meetings.rs:303-390` (create), `services/compat.rs:94-143` (resolve), `migration/src/phase2_backfill.rs` (one-off/admin backfill), `handlers/admin.rs:314-330`.
- Expected: Either v1 writes are retired (flag on) or v1-created meetings stay usable.
- Actual: v1 create inserts only into `meetings`; no automatic backfill (`grep -rn backfill src/main.rs src/workers src/handlers/meetings.rs` → none; only `POST /api/v2/admin/compat/backfill`). Web and mobile pre-join resolve via v2 (`compat.resolve` → `by_legacy_id` → none → `MEETING_NOT_FOUND`). Legacy updates on already-backfilled meetings don't update the v2 conference (shadow compare only logs, `handlers/meetings.rs:433-437`). v1 does not persist timezone. Admin meeting list shows only legacy `meetings` (`handlers/admin.rs:324`) — v2 spaces/conferences invisible to admins. Web itself no longer uses v1 for create/schedule; the only v1 calls left in web are in the legacy branch of `live-room.tsx` (MTG-019). With the flag ON, nothing in web scheduling breaks; mobile breaks entirely (MTG-001).
- Evidence: as cited; `tests/legacy_retirement.rs` verifies the 410 behaviour.
- API/Swagger refs: `/api/v1/meetings*`, `v2_resolve_identifier`, `v2_run_backfill`.
- Affected frontend workflow: Mobile scheduling; old integrations using `/api/v1/api/meetings`.
- Owner class: BACKEND / PRODUCT_DECISION
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Meetings created through v1 (mobile, third-party Api-Key integrations) cannot be joined via the current UI.
- Recommended remedy: Turn `FEATURE_LEGACY_MEETING_WRITE_DISABLED=true` once mobile is migrated (MTG-001); until then either run backfill-on-write (call `phase2_backfill::run(txn, Some(id))` inside v1 create/update) or keep v1 join (`POST /api/v1/meetings/join/{code}`) reachable from clients. Add v2 listing to the admin console.
- Dependencies: MTG-001
- Complexity: LOW (flag) / MEDIUM (backfill-on-write)
- Regression risk: MEDIUM
- Confidence: CONFIRMED

### MTG-018 — Space list is unpaginated and does N+1 full-table-per-space conference loads
- Service/module: `services/meeting_space.rs:554-668`.
- Expected: Bounded, paginated list (spec F-02 "pagination").
- Actual: For each space: `viewer_role` query + `canonical_conference` that loads **all** conferences of the space (`.all()` with no limit, :585-589). A daily recurring space accumulates hundreds of rows; the list endpoint grows O(spaces × conferences). Web paginates client-side (`MeetingsContent.tsx:54-58`); mobile polls every 5 s (legacy, `use-meetings.ts:11`).
- Evidence: code lines above.
- API/Swagger refs: `v2_list_meeting_spaces` (no params).
- Affected frontend workflow: Dashboard, meetings list.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Latency/DB load growth for active users.
- Recommended remedy: Single query with `DISTINCT ON (meeting_space_id)` / lateral join for canonical conference; add `cursor/limit`. Reuse `idx_conference_space_start`. No migration.
- Dependencies: MTG-010
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-019 — Web live room still carries a legacy v1 branch (chat/media writes) that fails under the retirement flag
- Service/module: web `components/room/live-room.tsx:99-101,163-170`, `components/room/footer-controls.tsx:64-78`, `components/room/ChatProvider.tsx:24,63`.
- Expected: Web uses v2 only.
- Actual: When `joinData.conference_id` is absent (stale stored join data; pre-join always sets it, `pre-join-screen.tsx:264`) the room uses `usePublicMeeting` (v1 GET), legacy chat `POST /api/v1/meetings/{id}/chat` and media `PATCH /api/v1/participants/{id}/audio|video` → 410 when the flag is on. Hooks `useCreateMeeting/useUpdateMeeting/useDeleteMeeting/useEndMeeting/useWaitingRoom/useMeetings` are dead (no component imports).
- Evidence: `grep -rln "\buseCreateMeeting\b" apps/web/src/app apps/web/src/components` → none.
- API/Swagger refs: v1 chat/participants.
- Affected frontend workflow: Edge case only.
- Owner class: FRONTEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P3
- Business impact: Low; maintenance noise.
- Recommended remedy: Remove the legacy branch and dead hooks/`meeting.service.ts` once v1 is retired.
- Dependencies: MTG-017
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-020 — SDK gaps: Dart SDK has no typed meeting models; TS SDK marks optional bodies as required
- Service/module: `sdks/dart/lib/src/generated/` (only `operations.dart` + `contracts.dart`, 29 lines), `sdks/typescript/src/generated/operations.ts:312,333,334`.
- Expected: Typed `ConferenceResponse.scheduledStartAt` etc. in both SDKs; body optionality matches server (`V2OptionalJson`).
- Actual: Dart exposes operations only (untyped maps). TS lists `"body": "REQUIRED"` for `v2CreateConference`, `v2CreateInstantMeeting`, `v2ArchiveMeetingSpace` although handlers accept no body (`V2OptionalJson`, `conferences.rs:44`, `spaces.rs` instant/archive). TS models do include all schedule fields (`models.ts:4582-4640, 4906`).
- Evidence: `wc -l sdks/dart/lib/src/generated/contracts.dart`; grep above.
- API/Swagger refs: OpenAPI `requestBody.required` for those ops.
- Affected frontend workflow: SDK consumers / mobile (if moved to SDK).
- Owner class: SHARED_CONTRACT
- Classification: CONTRACT_MISMATCH
- Severity: P3
- Business impact: Minor friction for SDK users.
- Recommended remedy: Mark request bodies optional in utoipa (`request_body(…, required = false)` style) and regenerate; add Dart model generation.
- Dependencies: –
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### MTG-021 — Minor scheduling UX/contract nits
- Service/module: web `ConferencesPanel.tsx:16` (`to` uses `T23:59:59` against an exclusive bound → drops 23:59:59.xxx), `CreateMeetingSpaceContent.tsx:39` copy "Schedule conference sessions separately from conferences" (confusing), no way to edit `schedulePolicy` (early start / auto-end) in web; create space page has no date/time step although spec F-03 wizard includes "scheduling" (user must create space, then open it and "Schedule occurrence").
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P3
- Business impact: Discoverability: users looking for "choose date/time" during create don't find it.
- Recommended remedy: Add optional "first occurrence" step to the create wizard that calls the existing `POST …/conferences` after space creation (reuse endpoints); fix copy; expose schedule policy.
- Dependencies: MTG-012
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

## Journeys

### Journey C — Create Meeting Space → choose date/time → schedule → calendar → invite → reschedule → join → end → history (web)
| # | Step | Operation / handler | Status |
|---|---|---|---|
| 1 | Create Meeting Space | POST /api/v2/meeting-spaces → `handlers/v2/spaces.rs:create_meeting_space` → `meeting_space.rs:441`; web `CreateMeetingSpaceContent.tsx:46-59` | PASS backend (e2e01, real DB); web IMPLEMENTED_UNVERIFIED |
| 2 | Choose date/time | Not part of create; web space page `ConferencesPanel.tsx` "Schedule occurrence" (browser TZ, not space TZ) | PARTIAL (MTG-012, MTG-021) |
| 3 | Schedule | POST /api/v2/meeting-spaces/{id}/conferences → `conferences.rs:create_conference` → `conference.rs:297` | PASS backend (e2e03, m_e2e_01) |
| 4 | Calendar | No calendar API/UI; `/dashboard/schedule` redirects; list cards show `createdAt` | FAIL (MTG-004, MTG-005, MTG-007) |
| 5 | Invite | POST /meeting-spaces/{id}/invitations (link token; no email) / POST …/members | PARTIAL (links PASS e2e12; email MISSING MTG-006) |
| 6 | Reschedule | POST /api/v2/conferences/{id}/reschedule → `conference.rs:768`; no attendee notification | PARTIAL (PASS e2e04_05 for API; notifications MISSING; end-wipe MTG-009) |
| 7 | Join | Host: dashboard Start (POST /conferences/{id}/start, window-checked) then pre-join POST /conferences/{id}/join → `admission.rs:552`; attendees blocked until started | PARTIAL (backend PASS m_e2e_01 with LiveKit mock; pre-join UX MTG-013) |
| 8 | End | POST /api/v2/conferences/{id}/end → `conference.rs:625` | PASS backend (e2e18, m_e2e_01) |
| 9 | History | GET /meeting-spaces/{id}/conferences (all statuses), GET /conferences/{id}/attendance, /recordings, /artifacts; web ConferenceWorkspace (attendance not shown) | PARTIAL (per-space only, MTG-005) |
Overall: **PARTIAL** — backend core (create/schedule/reschedule/cancel/start/join/end) is exercised against real PostgreSQL in CI (`tests/api_v2_e2e.rs`, `tests/system_e2e.rs`) with mock LiveKit; calendar, notifications and list display fail; no browser E2E exists for this journey.

### Journey D — Instant meeting: create → join details → invite → admit → connect media → end
| # | Step | Operation / handler | Status (web) | Status (mobile) |
|---|---|---|---|---|
| 1 | Create | POST /api/v2/instant-meetings → `spaces.rs:create_instant_meeting` → `meeting_space.rs:471` (LIVE immediately) | PASS backend; web `DashboardContent.tsx` | FAIL — mobile uses v1 `/meetings/instant` and misreads response (MTG-001) |
| 2 | Join details | Response `{meetingSpace{meetingCode}, conference}`; web routes to `/{meetingCode}` → GET /resolve/{code} | PASS | FAIL |
| 3 | Invite | Copy link `/{meetingCode}` (space card) or POST …/invitations | PASS (link) | n/a |
| 4 | Admit | GET /conferences/{id}/waiting-room, POST …/waiting-room/admit|deny (`handlers/v2/participants.rs`) | PASS backend (e2e07_08, e2e09); web ParticipantsPanel | FAIL — legacy v1 waiting endpoints with space id (MTG-002) |
| 5 | Connect media | Join response `rtc{url,credential}` → LiveKit | IMPLEMENTED_UNVERIFIED (CI mock; real LiveKit tests `#[ignore]`) | v2 join used, media likely connects; controls broken |
| 6 | End | POST /api/v2/conferences/{id}/end | PASS backend | FAIL — `POST /api/v1/meetings//end` (MTG-002) |
Overall: **PARTIAL** (web: backend-verified except real media; mobile: FAIL).

### Journey B — last step: schedule an organization meeting
| Step | Operation / handler | Status |
|---|---|---|
| Create org-scoped space | POST /api/v2/meeting-spaces with `organizationId` + `accessPolicy: ORGANIZATION_ONLY` (`meeting_space.rs:153,452` verify membership) | Backend PASS for access (`tests/organization_domain_access.rs`); web cannot send these fields → FAIL in UI |
| Schedule occurrence | POST …/conferences | PASS backend |
| Org members discover it | No org listing; members not auto-added | FAIL (MTG-016) |
Overall: **FAIL** from the product UI; backend capability exists.

## Counts

Features (37 matrix rows, classified by primary/backend status; UI caveats are in the row and findings):
- COMPLETE_VERIFIED: 20 (backend API exercised against real PostgreSQL in CI `backend.yml`; LiveKit mocked) — instant v2, select date/time, duration, schedule/save, retrieve schedule (backend), reschedule, cancel, recurring, single-occurrence exception, invitation links, join window, timezone/DST for recurrence, admission, host controls, end, auto-end, resolve, participant tokens, chat, recordings
- PARTIALLY_IMPLEMENTED: 6 — set timezone, modify series, list upcoming, list past, recurring expansion, legacy v1 scheduling
- IMPLEMENTED_UNVERIFIED: 2 — meeting history UI, real LiveKit media
- MISSING: 6 — schedule-change notifications, reminders, personal calendar, organization calendar, ICS export/invites, stale-occurrence expiry
- CONTRACT_MISMATCH: 1 — mobile instant/scheduling (plus MTG-002/003/011/020 as findings)
- FRONTEND_ONLY_GAP: 1 — organization meeting creation (plus MTG-004/015/021 as findings)
- DEFERRED_BY_OWNER: 1 — external calendar sync (spec: optional)

Findings by severity (21 total):
- P0: 1 (MTG-001)
- P1: 5 (MTG-002, MTG-003, MTG-004, MTG-005, MTG-006)
- P2: 12 (MTG-007, MTG-008, MTG-009, MTG-010, MTG-011, MTG-012, MTG-013, MTG-014, MTG-015, MTG-016, MTG-017, MTG-018)
- P3: 3 (MTG-019, MTG-020, MTG-021)

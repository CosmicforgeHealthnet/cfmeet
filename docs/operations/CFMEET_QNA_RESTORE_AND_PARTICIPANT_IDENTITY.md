# CF Meet — Q&A restore and LiveKit participant identity (Session 2)

Status date: 2026-11-12. Repository `CosmicforgeHealthnet/cfmeet`, branch `main`.

## 1. Executive summary

| Item | Status |
|---|---|
| Q&A restore (`POST /api/v2/conferences/{id}/questions/{questionId}/restore`) | IMPLEMENTED, TESTED (CI) |
| Realtime event `qna.question_restored` | IMPLEMENTED, TESTED (CI) |
| Canonical `participantId` in LiveKit credentials | IMPLEMENTED, TESTED (CI, mock SFU with real token verification) |
| Swagger / contracts / SDKs / frontend reference | UPDATED, all contract checks pass |
| Frontend integration contract | WRITTEN (`docs/frontend/qna-restore-and-participant-identity.md`) |
| CI toolchain break (Rust 1.100 vs `redis` 0.25.4) | FIXED (`redis` 0.25.5) |
| Production acceptance tooling | WRITTEN (`cfmeet-meeting-acceptance.yml`) |
| Integrated on `main` | YES — released SHA `9684346` (§13) |
| Production release | DONE — coordinated release run 43301863683, all 13 services, verify PASS (§14) |
| Production acceptance | PASS — 22/22 live checks, run 43307759596 (§15) |
| Live certification | NOT_CERTIFIED only for "source matches main" after PR #65 reached `main` later (§16) |

## 2. Root causes

* **Q&A restore** — the question state machine (`services/meeting-service/src/services/collaboration/questions.rs`)
  only allowed transitions *into* `DISMISSED`; there was no operation, transition or event to
  leave it, so a mis-click on "Dismiss" was permanent.
* **Participant identity** — LiveKit credentials carried only the media identity
  (`ps_<session uuid>`, one per *session*, changes on reconnect) and the display name. The canonical
  conference participant id used by every collaboration and moderation API was not in the token,
  so clients matched tiles to participants by display name, which collides.
* **CI break found on the way** — Rust 1.100.0 (2026-11-09) stopped the never-type fallback that
  `redis` 0.25.4 relied on (`!: FromRedisValue` in `script.rs`). Every Meeting Service build (CI and
  the `rust:1-bookworm` image build) failed, on `main` and on PR #65 alike.

## 3. Changed files

| Area | Files |
|---|---|
| Q&A | `src/services/collaboration/questions.rs`, `src/services/meeting_events.rs`, `src/handlers/v2/engagement.rs`, `src/routes/v2.rs`, `src/swagger/mod.rs` |
| LiveKit identity | `src/services/rtc_provider.rs`, `src/services/admission.rs`, `src/services/collaboration/media.rs` |
| Tests | `tests/qna_restore_and_identity.rs` (new, 11 tests), unit tests in `questions.rs` / `rtc_provider.rs`, `tests/api_contract_matrix.rs`, `tests/openapi_contract.rs` |
| Contracts (generated) | `packages/contracts/openapi/meeting-service.json`, `packages/contracts/bundled/public/{meetings,aggregate}/v1.openapi.json`, `infra/api-gateway/{public-routes,route-manifest,sdk-discovery}.json`, `sdks/typescript/src/generated/*`, `sdks/dart/lib/src/generated/operations.dart`, `docs/frontend/cfmeet-frontend-endpoint-reference.md` |
| Docs | `docs/frontend/qna-restore-and-participant-identity.md` (new), `docs/frontend/cfmeet-frontend-handover.md` (pointer), `services/meeting-service/docs/PHASE2_MEETING_DOMAIN.md`, this report |
| Build fix | `services/meeting-service/Cargo.lock` (`redis` 0.25.4 → 0.25.5, version + checksum only) |
| Production acceptance | `.github/workflows/cfmeet-meeting-acceptance.yml`, `infra/calling/production/meeting_acceptance.py`, `meeting_acceptance_client.py`, `test_meeting_acceptance.py` |

All paths under `services/meeting-service/` unless rooted otherwise.

## 4. Q&A restore endpoint

`POST /api/v2/conferences/{id}/questions/{questionId}/restore` — operation `v2_restore_question`,
tag **Q&A (v2)**, security `bearer_auth` or `participant_token`, no body, response
`QuestionActionResponse { question: QuestionView, changed: bool }`. Also routed by the public gateway
catalogue as `PUBLIC_CUSTOMER` (same class as dismiss).

| HTTP | Code | When |
|---|---|---|
| 200 | — | restored (`changed: true`) or already OPEN/ANSWERED (`changed: false`) |
| 401 | `UNAUTHORIZED` | no / invalid credential |
| 403 | `PERMISSION_DENIED` | caller lacks `qna.dismiss` |
| 404 | `QUESTION_NOT_FOUND` | unknown id or a question of another conference |
| 409 | `CONFERENCE_STATE_CONFLICT` | question ARCHIVED, or conference ended/cancelled |

## 5. Authorization and state transitions

Same boundary as dismiss/archive: `Collab::load` (conference access, tenant and meeting scope),
`Permission::QnaDismiss`, `require_configurable` (not ended/cancelled), question looked up by
`(id, conference_id)` under `SELECT … FOR UPDATE` inside one transaction that also locks the conference.

| From | Condition | To | Event / audit |
|---|---|---|---|
| DISMISSED (or legacy HIDDEN) | `answered_at` or `answer_text` set | ANSWERED | `qna.question_restored`, `QNA_RESTORE` |
| DISMISSED | no answer | OPEN | `qna.question_restored`, `QNA_RESTORE` |
| OPEN / ANSWERED | — | unchanged, `changed: false` | none |
| ARCHIVED | — | 409 | none |

Existing transitions are unchanged; ARCHIVED stays terminal. Answering a DISMISSED question is still
refused (the new `DISMISSED → ANSWERED` edge is reachable only through restore). Preserved: body,
internal author, anonymity, answer + `answeredAt`, vote rows and count, audit history. The pin cleared
by the dismissal is not reinstated. `version` +1 and `updated_at` on every real change. The audit
record's metadata is `{from, to}` only.

Concurrency: the row lock serializes concurrent moderators, dismiss-vs-restore and answer/archive
races; a second restore sees OPEN/ANSWERED and returns `changed: false` with no event, so no duplicate
transition or event is possible.

## 6. Realtime event schema

`qna.question_restored` (the existing family is `qna.question_<verb>`; the brief's spelling
`qna.question.restored` was not used, for consistency with `…_dismissed`, `…_answered` etc.).
Written to the transactional outbox in the restore transaction, so it commits atomically with the
state change and receives the gap-free conference `sequence`. Payload = the standard question
projection: `questionId, conferenceId, body, anonymous, participantId|null, displayName|null,
status, answer, pinned, upvoteCount, version`. Anonymous questions always have
`participantId = displayName = null`. Clients reconcile by `version` (duplicates from replay are
ignored).

## 7. LiveKit metadata contract

Every credential the Meeting Service mints for a participant session (`POST …/join`,
`POST …/media-credential`) now contains:

* metadata `{"participantId":"<conference participant uuid>"}`
* attribute `participantId = <same uuid>`

Unchanged: identity `ps_<session uuid simple>`, name (display name), TTL, room, grants and media
permissions. `canUpdateOwnMetadata` stays `false` (token and live permission updates), so a client
cannot rewrite or forge either field; tokens are signed by the server. The value is read from the
server's own `participant_sessions.conference_participant_id` row, never from the request. Metadata
holds no account id, email or role. Legacy `/api/v1` credentials are unchanged (no v2 participant
exists there).

## 8. Canonical participant id mapping

| Identifier | Where | Use |
|---|---|---|
| Account user id | auth token | never in RTC |
| Conference participant id | `conference_participants.id`; `participantId` everywhere in `/api/v2`; LiveKit attribute/metadata | **the key** for tiles, hands, spotlight, moderation, Q&A |
| Participant session id | `participant_sessions.id` | per device/session |
| LiveKit media identity | `ps_<session>` | LiveKit plumbing |
| Display name | `name` | label only |

Reconnect (`previousSessionId`), leave → rejoin and credential refresh keep the participant id; a new
participant never inherits another's id. Recording/egress participants have no `participantId` and
are not participants.

## 9. Frontend integration requirements

See `docs/frontend/qna-restore-and-participant-identity.md` (endpoint, errors, state handling,
event, reading `participantId` in livekit-client and the Flutter SDK, missing/stale metadata,
reconnect, removal of display-name matching). The frontend handover's "In meeting" row links it.

## 10. Swagger / API changes

New operation `v2_restore_question` in `packages/contracts/openapi/meeting-service.json` (Q&A (v2)
tag, not `default`), identical to `cargo run --bin export_openapi` (CI "OpenAPI contract export is
current" passed). Public catalogue, bundled public documents, SDKs (TypeScript, Dart) and the
frontend endpoint reference regenerated with the repository scripts; `public_api.py --check`,
`bundle_openapi.py --check`, `sdks/generate.py --check`, `frontend_reference.py --check`,
`check_swagger_labels.py`, `check_gateway_upstreams.py`, `operation_test_coverage.py` all pass.
No internal endpoint or secret was added to a public contract.

## 11. Unit and integration test results

Pre-merge CI on the exact commits (temporary tags, since deleted), Meeting Service run
[43283665931](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43283665931):

* `fmt / check / clippy` — PASS on Rust 1.100.0 (after the `redis` fix).
* `build / migration smoke / OpenAPI` — PASS (committed contract equals the export).
* `tests/qna_restore_and_identity.rs` — **11/11 PASS**: `qna_restore_unanswered_to_open`,
  `qna_restore_answered_keeps_answer`, `qna_restore_anonymous_stays_anonymous`,
  `qna_restore_requires_qna_dismiss`, `qna_restore_invalid_targets`, `qna_restore_is_idempotent`,
  `qna_restore_concurrent_moderators`, `identity_same_display_name_is_distinguishable`,
  `identity_stable_across_refresh_reconnect_and_rejoin`,
  `identity_drives_hand_spotlight_mute_and_removal`, `identity_service_participants_and_forgery`.
* Whole suite: only the two operation-count registries failed (206 → 207 operations); fixed in
  `88ab2c6`.
* Unit tests: Q&A transitions / restore target / privacy-safe restored payload; LiveKit token claims
  (metadata, attribute, name, no metadata update grant).
* Acceptance tooling: 7 offline tests (`test_meeting_acceptance.py`).

Coverage of the brief's lists: Q&A 1–17 and LiveKit 1–15 are covered by the tests above, except
"existing LiveKit video/audio functionality" and "media authorization", which are covered by the
unchanged existing suites (`collab_e2e`, `credential_replay`, `livekit_*`) that run in the same job.

## 12. CI results

Every push workflow of the released SHA `9684346` (and, for path-filtered ones, the latest `main` run)
was green, which the release gate verified: Meeting service, Contracts, Frontend, API Gateway,
Calling, Messaging, Numbering, Privacy Relay, SMS Gateway, Telephony, Platform Access, Developer
Platform, Communications Hub, Service boundaries, Dependency security, Backup drill, Part VI
certification, Unified Messaging, Enterprise calling and Communications e2e.

Two more runner drifts were fixed on `main` on the way (neither is a code defect):
* `ubuntu-latest` = Ubuntu 26.04 breaks Playwright → three jobs pinned to `ubuntu-24.04` (`7b103cd`,
  lines identical to PR #65).
* Runner `pg_restore` 17 emits `SET transaction_timeout`, rejected by the PostgreSQL 16 CI server →
  PR #65's `restore.sh` fix cherry-picked (`9684346`, `-x`).
* The web app's generated contract types were regenerated for the new operation (`00f87ed`).

## 13. Main commit SHA

Released: `9684346e10f17b1f4b293c4c386c73cda644c2a1`. Commits (fast-forwards from `f72bc37`):
`4756863` features, `51d406f` redis fix, `98bc637` acceptance tooling, `e15b647` handover pointer,
`88ab2c6` test registries, `7b103cd` Playwright pins, `00f87ed` web contract types, `9684346`
restore.sh cherry-pick. Afterwards PR #66 (`staging` → `main`, carrying PR #65) was merged by the
owner (`1e9364c`); the acceptance-workflow fix `c9c9c04` was merged with it as `5c59355`
(no conflicts, no force-push).
No branch or pull request was created; the pre-merge CI used two temporary tags
(`session2-verify-*`) that were deleted afterwards.

## 14. Production deployment

[Coordinated release 43301863683](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43301863683)
(`operation=release`, `release_sha=9684346…`), the only production release in flight (a read-only
`inspect`, 43291592091, ran before it). Gate PASS; all 13 images built for the SHA and transferred
with revision checks; backup of 16 databases with restore tests of Meeting, Organizations and
Platform Access (`/opt/cfmeet-core/backups/43301863683-1/`); released in order platform-access,
organizations, messaging, calling, communications, numbering, privacy-relay, sms-gateway, telephony,
meeting-service (all seven migration histories applied first), developer-platform (+ worker),
api-gateway; scan gateway acceptance PASS; verify PASS. Rollback stays available through
`operation=rollback` (journal entry of this release). Meeting Service image
`sha256:91ecd236…c61186`.

## 15. Production acceptance evidence

[Meetings acceptance 43307759596](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43307759596)
against https://api.cfmeet.com and the production LiveKit, with the existing controlled acceptance
account (no registration, no email), own space `5d264bc2-…` / conference `41c2754b-…`:
**verdict PASS, 22/22**.

* identity: each credential carries its join's participantId; two participants named
  "Acceptance Alex" get distinct ids; identity `ps_<session>` and name unchanged; no account data in
  metadata.
* livekit (real WebRTC clients): the host sees both same-name guests with their own ids; a guest sees
  the other guest and the host with the right ids.
* hands / spotlight: the raised hand and the spotlight target the tile the token names.
* reconnect: new session, same participantId; the host maps the new media identity to the same
  participant.
* Q&A: attendee restore 403; DISMISSED → OPEN with body, votes and version+1; repeat restore no-op;
  restored question visible to attendees with their vote; answered → ANSWERED with its answer;
  anonymous stays anonymous; archived → 409.
* realtime: exactly one `qna.question_restored` per real restore with the right status/version;
  anonymous event carries no author.
* cleanup: conference ended, space archived.

(The first dispatch, 43307357573, failed in its offline-test step before reaching the VPS; fixed in
`c9c9c04`.)

## 16. Regression results

* Release regression (in run 43301863683): Meetings health, Messaging/Calling/Platform TLS readiness,
  both LiveKit keys PASS; verify: CORS from the web origin, internal routes not public, migration
  histories present, safe defaults — PASS.
* [Live certification 43308028461](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43308028461)
  (read-only, all 13 services, on head `5c59355`): container, image, readiness, database, migrations,
  authentication, service auth, service-to-service and public checks pass; verdict **NOT_CERTIFIED**
  only because `source_matches_main` fails for meeting-service, developer-platform and
  developer-platform-worker — `main` gained PR #65's code after the release, which production does not
  run yet. It clears with the next coordinated release of the `main` head.

## 17. Remaining issues

* **PR #65 is on `main` but not in production.** A coordinated release of the current `main` head is
  needed to deploy it (and to make `certify` CERTIFIED again). It was left to the PR #65 owner/session
  to avoid a competing deployment of work this session does not own.
* The frontend must adopt the contract (participantId mapping, restore action, event name
  `qna.question_restored`); clients holding credentials minted before the release get the metadata on
  their next refresh/reconnect.

## 18. Final verdict

**Both features are complete, integrated on `main`, released to production and accepted live**
(22/22). All 13 services ran healthy at release verify and in certification; the only open item is
deploying PR #65's newer code from `main`, which belongs to that workstream.

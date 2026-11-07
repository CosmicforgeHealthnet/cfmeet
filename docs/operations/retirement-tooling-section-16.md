# Consolidation and retirement — Blueprint v1.0 Section 16 (remaining findings)

Closes the open Section 16 findings left after the lead's first pass (PLT-001, INV-001,
PLT-007, PLT-013/MTG-020 Dart, DATA-014; see the master register). Branch
`blueprint/s16-retirement-tooling`. **Nothing here is deployed** and no production
configuration changes: every behaviour change is either off by default, additive, or a
documentation / header change that existing clients ignore.

Preference order applied: REUSE → EXTEND → INTEGRATE → CREATE. No new service, no new
table, no new migration, no new webhook or credential system. PRIVATE_E2EE wording and
guarantees are untouched (a test asserts the served Messaging description keeps it).

## 1. Decisions and dispositions

| Finding | Disposition | What was done | What is left |
|---|---|---|---|
| PLT-003 (P1) + DATA-007 | `DEPRECATED_WITH_MIGRATION` | Legacy meeting webhooks are **not** delivered through the Developer Platform: DP meeting events exist only for environment-owned spaces, and personal users have no environment, so delivery would need an invented ownership model. Instead every `/api/v1/webhooks*` answer says nothing is delivered: `Deprecation` / `Sunset` / `Link rel="deprecation"` headers (errors included), `delivery: NOT_DELIVERED` per item, a `notice` per wrapper, `deprecated: true` in OpenAPI. [Migration guide](../api/legacy-meeting-webhooks-deprecation.md) to Developer Platform webhooks. | web panel wording (frontend), DP activation (PLT-005, owner), zero-use window then removal (D-9) |
| MTG-017 + DATA-008 | `DEPRECATED_WITH_MIGRATION` | `FEATURE_LEGACY_MEETING_CREATE_MODE` (default `mirror`): v1 creates (`POST /api/v1/meetings`, `/instant`, `/api/v1/api/meetings`) are mirrored into v2 at once by the existing `phase2_backfill` (single-meeting mode); legacy update / end refresh the mirror; `refuse` answers `410 LEGACY_WRITE_DISABLED` pointing to `/api/v2`. All three creates carry `Deprecation` / `Sunset` and are `deprecated: true`. Retirement plan for the dual model in §3. | mobile v1 → v2 (MTG-001, frontend); owner switch to `refuse`, then `FEATURE_LEGACY_MEETING_WRITE_DISABLED`; table freeze/drop (D-9) |
| DATA-001 | `PARTIAL: explicit UTC interpretation in code done; the TIMESTAMP→TIMESTAMPTZ conversion is a documented, operator-run maintenance step paired with a model change (not built, not auto-applied)` | Runtime pins every DB session to `TimeZone=UTC` and refuses to start otherwise; `legacy_as_utc` helper; API-key timestamps now serialized RFC 3339 with `Z` (were offset-less, read as local time by browsers); read-only pre-check script with size/lock estimates (§4). | the paired model change + conversion window (§4) |
| SEC-019 | `ENGINEERING_COMPLETE` | Meeting artefact storage signs only through the shared `cf-object-storage` (`S3ObjectStore`); its own SigV4 code is deleted. The shared crate gained `etag` on `ObjectMeta`/`StoredObject` and `S3ObjectStore::get_streaming` (bounded streaming read, for MD5+SHA-256 of large recordings). Client reads are capped at the shared 900 s; `ARTIFACT_SIGNED_URL_TTL_SECS` clamp 30–3600 → 30–900 (default 300 unchanged); the post-meeting transcription provider fetch keeps 3600 s through an explicit `provider_read_url`. Tests: adapter URLs equal the shared signer's; caps; key safety; live MinIO test extended. | — |
| PLT-006 | `ENGINEERING_COMPLETE` | Meeting-service `/swagger-ui/` peer documents come only from the generated public bundles (`packages/contracts/bundled/public/<domain>`); the hand-kept filter lists are gone; a test proves each served peer equals the gateway catalogue's `PUBLIC_CUSTOMER` set (Contact Vault private-contact and RTC diagnostics no longer published; compliance ops the gateway publishes are no longer hidden). Calling's Swagger UI: off by default (`CALLING_DOCS_ENABLED=false`), HTTP Basic token (`CALLING_DOCS_ACCESS_TOKEN[_FILE]`, ≥ 32 chars, SHA-256 + constant-time compare; without a token only in `local`/`test`), pinned swagger-ui 5.17.14 with SRI, hash-only CSP, no try-it-out, `persistAuthorization: false`, `no-store`, `nosniff`, no framing; serves the public Calling and Privacy Relay bundles. The no-op Developer Platform tag patch is removed (test: every peer-bundle operation already carries tag/summary/description/operationId). | — |
| MTG-020 (TS part) | `ENGINEERING_COMPLETE` | The eight `V2OptionalJson` operations are annotated `request_body = Option<T>` → `requestBody.required: false`; `sdks/generate.py` already honours it (`body?:`); SDKs regenerated. Guard test `optional_json_bodies_are_not_required` counts `V2OptionalJson` handlers. | — |
| DATA-011 | `PARTIAL: target readiness rule implemented in meeting-service for every history (base included); history consolidation deferred to a maintenance window (§5)` | `migration::pending_migrations::<M>` (read-only, unknown versions fail closed — same rule as calling/messaging/developer-platform), `migration_readiness`, `base_ready`; the five `*_ready` checks use it; `main.rs` now also checks the base history. | consolidation window (§5) |
| INV-002 | `PARTIAL: meeting-service and messaging delegated-mirror routes covered; 48 internal/operator operations in 9 other contracts remain, frozen in a baseline` | `scripts/contracts/operation_test_coverage.py` (per-contract untested operations, path-literal heuristic, frozen `BASELINE`, fails on regressions, 19 mutation tests) in Service boundaries. New tests: `services/meeting-service/tests/legacy_auth_keys_webhooks.rs` (16 legacy auth / API-key / webhook / admin routes + `GET /`), `services/messaging/tests/delegated_mirror.rs` (all 96 mirror operations: 401 / 403 / delegated-context refusal without side effects, routed to a handler). Untested: 146 → 48 of 1,128. | the 48 (numbering admin/ports 20, developer-platform operator/webhook-migration 13, telephony 5, communications internal 4, platform-access 3, three singles) |
| FE-015 | `FRONTEND_HANDOFF` | SDK freshness gate already existed (`sdk-typescript.yml`, `sdk-dart.yml`: `sdks/generate.py --check`) and now also runs in `contracts.yml`. New report-only `scripts/contracts/web_contract_parity_report.py` (never fails CI; job summary): apps/web imports 0 SDK/generated types, 23 handwritten type modules, 267 calls (222 with generated types available). | client adoption (frontend) |
| FE-018 | `NOT_APPLICABLE_WITH_EVIDENCE` (documented decision) | [Decision record](../architecture/first-party-direct-ingress-decision.md): first-party apps ingress directly by design; the gateway is the third-party `PLATFORM_CREDENTIAL` front door. The EXCLUDED list (105 operations by class) is generated from `infra/api-gateway/public-routes.json` by `scripts/contracts/first_party_ingress.py --check` (contracts.yml), so it cannot drift. Not `DEPRECATED_WITH_MIGRATION`: nothing moves; only the `LEGACY_DIRECT_INGRESS` subset retires, under its own records. | — |
| CAL-013 | `FRONTEND_HANDOFF` (deploying Privacy Relay: `EXTERNAL: owner deploy decision`) | Backend routing is already correct: Privacy Relay serves the 5 `/v1/privacy-relay/contacts*` routes (Contact Vault token in `X-CF-Contact-Authorization`, minted by Calling's `contact-access-grants`); the gateway classifies them EXCLUDED `CONTACT_VAULT_PRIVATE`; the 17 contexts/sessions/grants routes go to `cfmeet.calling`; `check_gateway_upstreams.py` passes. [Handoff](../api/privacy-relay-contacts-upstream-handoff.md) names the wrong web proxy rules. No contract change. | web proxy fix + hide the contacts page (frontend); Privacy Relay deployment (owner) |

## 2. Flags and configuration (defaults keep today's behaviour unless stated)

| Service | Variable | Default | Effect |
|---|---|---|---|
| meeting-service | `FEATURE_LEGACY_MEETING_CREATE_MODE` | `mirror` | `mirror`: legacy create as before plus an immediate, best-effort v2 mirror (failures logged/counted, repaired by the lazy resolver); `refuse`: `410 LEGACY_WRITE_DISABLED`. Unknown values fall back to `mirror` with a warning. `FEATURE_LEGACY_MEETING_WRITE_DISABLED=true` still wins (every legacy write 410). Counters `legacy_creates_mirrored`, `legacy_creates_refused`, `legacy_mirror_failures` (`/internal/metrics`). |
| meeting-service | `ARTIFACT_SIGNED_URL_TTL_SECS` | 300 | clamp tightened 30–3600 → 30–900 (shared signer cap). Only matters if an operator configured more than 900. |
| meeting-service | DB session time zone | pinned `UTC` | `TimeZone=UTC` is appended to the connection options; start-up refuses a non-UTC session. sqlx already starts sessions in UTC, so this only bites a `DATABASE_URL` that overrode it. |
| meeting-service | start-up readiness | — | the base history (`seaql_migrations`) must now satisfy the target rule too; every release flow already runs `migration up` before the server. |
| calling | `CALLING_DOCS_ENABLED` | `false` | Swagger UI and docs JSON return 404 unless enabled. |
| calling | `CALLING_DOCS_ACCESS_TOKEN` / `_FILE` | unset | required (≥ 32 chars) outside `local`/`test` when docs are enabled; otherwise the docs stay unmounted and an error is logged (the API starts). |

New behaviour visible to clients (all additive): `Deprecation` / `Sunset` / `Link` headers on
the legacy webhook routes and the three legacy creates; `delivery` and `notice` fields on
legacy webhook bodies; API-key timestamps end in `Z`; eight v2 bodies documented optional
(the server always accepted an empty body).

## 3. Retirement plan for the dual legacy / v2 meeting model (DATA-008 / MTG-017)

| Step | Gate | Owner |
|---|---|---|
| 0 (this branch) | v1 creates mirrored at once (`mirror`), deprecated, headers on | — |
| 1 | Mobile moves scheduling and join to `/api/v2` (MTG-001); web removes the legacy branch of `live-room.tsx` (MTG-019) | frontend |
| 2 | Measure: access logs for `POST /api/v1/meetings*`, `/api/v1/api/*` (the `Deprecation` header marks them) and `legacy_creates_mirrored`; zero for the agreed window (D-9 per-endpoint record) | release / owner |
| 3 | `FEATURE_LEGACY_MEETING_CREATE_MODE=refuse` on staging, then production | owner |
| 4 | `FEATURE_LEGACY_MEETING_WRITE_DISABLED=true` (every legacy write 410; reads keep working) | owner |
| 5 | Final `POST /api/v2/admin/compat/backfill` + shadow comparison with zero diffs; revoke INSERT/UPDATE on legacy tables from the runtime role | release / owner |
| 6 | Remove legacy write routes, then (after an owner-approved backup and export) drop legacy tables by an additive-then-drop pair | backend, later window |

Mirrored conferences stay legacy-managed (v2 join answers `409 CONFERENCE_STATE_CONFLICT`,
by design since Phase 2): a v1-created meeting is joined through the legacy join routes until
step 4. Making v1 meetings v2-managed would mean dual-writing two lifecycle authorities, which
this plan avoids.

## 4. DATA-001 — legacy naive timestamps

**Done (code).** Legacy columns hold UTC by construction (sqlx sessions are UTC; the runtime now
pins and verifies it). `utils::datetime::legacy_as_utc` is the one interpretation; every legacy
response already formatted `…Z` except the API-key DTOs, which now serialize `DateTime<Utc>`.
The two raw `TIMESTAMP` columns `meeting_recurrence_rules.dtstart_local` and
`meeting_recurrence_exceptions.original_start_local` are intentional local wall-clock values
paired with an IANA zone and stay as they are.

**Conversion (not run, not auto-applied).** `services/meeting-service/scripts/maintenance/data-001-timestamptz-precheck.sql`
is read-only (`BEGIN READ ONLY … ROLLBACK`): it prints the session/server zones, every naive
column with table size and row estimate, the column defaults, a future-value plausibility check
and the `ALTER TABLE … ALTER COLUMN … TYPE timestamptz USING … AT TIME ZONE 'UTC'` statements
(one per table) for review.

* **Cost.** `ALTER COLUMN … TYPE timestamptz` rewrites the table and its indexes under an
  `ACCESS EXCLUSIVE` lock (reads and writes blocked). The legacy tables are small (`users`,
  `meetings`, `participants`, `session_logs`, `chat_messages`, `webhooks`, `api_keys`,
  `audio_video_devices`, `oauth_states`): rewrite speed is roughly 50–150 MB/s on the VPS
  class disk, so a table under 100 MB rewrites in seconds; plan by the pre-check's
  `table_total_bytes`. `session_logs` and `participants` grow with usage and are the ones to
  measure.
* **Why not now.** The models read these columns as `NaiveDateTime`; sqlx refuses to decode
  `TIMESTAMPTZ` into it, so the conversion must ship in the same release as a model change
  (`DateTime` → `DateTimeUtc` in the eight legacy models and their handlers) and cannot be
  rolled back by image alone (the down path is the reverse `ALTER … TYPE timestamp USING …
  AT TIME ZONE 'UTC'`, another rewrite).
* **Procedure (future window).** Build the model change on a branch → run the pre-check on a
  restored copy and time the statements → pre-migration backup → stop meeting-service →
  apply the statements with `SET lock_timeout = '5s'` per table → start the new image →
  verify `/api/v1/meetings` and admin reads. As a separate history migrator it would follow
  the service's separate-history pattern (`m20261110_…` or later) with full wiring.

## 5. DATA-011 — migration histories

The target rule is implemented in meeting-service (`migration::pending_migrations`, read-only,
fail closed on unknown versions). For each separate history it equals the previous exact-set
check, so rollback safety is unchanged; the base history is now checked as well. The history
tables stay as they are: `seaql_migrations`, `account_communications_migrations`,
`account_communications_facade_migrations`, `account_security_migrations`,
`meetings_calendar_migrations`, `profile_notifications_migrations` (plus any added by other
Section work).

**Consolidation (later maintenance window, not run).** After every running image (meeting-service,
calling, messaging, communications) uses the target rule: copy the rows of each additive history
into the base history in one transaction (`INSERT INTO seaql_migrations (version, applied_at)
SELECT version, applied_at FROM <history> ON CONFLICT DO NOTHING`), move the corresponding
migrations into the base migrator in the same order, keep the old tables read-only for one
release, then drop them; remove the extra migrator binaries from the Dockerfile, release tooling,
certification stacks and `infra/deployment/backup/catalog.conf`. Metadata only; no schema
change. Until then new additive changes keep the separate-history pattern.

## 6. Contracts and generated artefacts

* `cfmeet.meeting-service.http-api` 1.2.0 → **1.3.0** and `cfmeet.meeting-service.public-api`
  1.2.0 → **1.3.0** (deprecations, `delivery`/`notice` fields, optional v2 bodies, Webhooks tag
  text). Regenerated: `bundle_openapi.py`, `public_api.py`, `sdks/generate.py` (TypeScript and
  Dart), all checked with `--check`. v2 operation count unchanged (188).
* No calling/privacy-relay contract change (calling's Swagger UI serves the existing public
  bundles).

## 7. Release and rollback

* **Release.** Meeting-service image only (no migration, no new secret): deploy as usual. Calling
  image: no action needed (docs stay off). Optionally set `FEATURE_LEGACY_MEETING_CREATE_MODE`
  explicitly to `mirror` in the environment to record the decision.
* **Rollback.** Redeploy the previous image; nothing to undo in the database (mirrored rows are
  the same rows the lazy resolver would create). The Developer Platform and gateway are not
  involved.
* **Checks after release.** `curl -I` a legacy webhook route shows `Deprecation`; the counters
  above appear at `/internal/metrics`; artefact download URLs show `X-Amz-Expires` ≤ 900.
* **Served Swagger changes.** Meeting-service `/api-docs/messaging.json` is now the public
  Messaging bundle: 96 operations (was 80; the 16 compliance operations the gateway publishes
  are no longer hidden), 71 native bindings (unchanged). The production acceptance script
  `infra/calling/production/messaging_acceptance.py` expects 96; a unit test pins both numbers.
  `/api-docs/communications.json` and the Calling enterprise paths in `/api-docs/calling.json`
  are unchanged in substance (the latter's internal paths were already absent).

## 8. Owner steps

1. Decide the sunset date (the headers carry 2027-05-01 as the earliest planned removal) and
   whether to announce it to known integrators.
2. After the frontend releases (MTG-001, PLT-003 panel, CAL-013 proxy), measure zero use and
   approve `refuse` → `FEATURE_LEGACY_MEETING_WRITE_DISABLED` (§3).
3. Decide on Privacy Relay deployment (CAL-013) and the Developer Platform / gateway activation
   (PLT-005) that the webhook migration depends on.
4. Schedule the DATA-001 conversion window and the DATA-011 consolidation window.
5. If Calling docs are wanted on staging: set `CALLING_DOCS_ENABLED=true` and a token file.

## 9. Still open

* Frontend: webhook panel wording, mobile v2 migration, SDK adoption (FE-015), Contact Vault
  proxy (CAL-013), review two `OPERATOR_ADMIN` calls made from the browser
  (`POST /api/v2/admin/organizations`, `PATCH …/{id}`).
* Backend later windows: DATA-001 conversion + model change; DATA-011 consolidation; legacy
  table export/drop (DATA-003 plaintext secrets with it); the 48 untested internal/operator
  operations (INV-002).
* External / owner: PLT-005 activation, Privacy Relay deployment, sunset dates.

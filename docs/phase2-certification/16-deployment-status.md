# 16 — Deployment status: staging, production migration, pre-cutover (2026-10-06)

## Status

**PHASE 2 BACKEND — COMPLETE**

Production-ready backend — `main`, backend code commit
`fd757bc056ab01ca660847a13d343ac55d89b095` (later commits on `main` are documentation only):

| | |
|---|---|
| Swagger / OpenAPI | 1.1.0 |
| Operations | 187 (49 v1 · 136 v2 · 2 unversioned: `/`, `/health`) |
| Migrations | 22 |
| Staging deployment | PASS |
| Production migration | PASS |
| Production pre-cutover certification | PASS |

**Public `api.cfmeet.com` is temporarily still routed to the legacy backend until the
frontend compatibility deployment is confirmed.** This is a deployment / cutover dependency,
NOT unfinished backend implementation. The Phase 2 backend is migrated, running and certified
in production next to the legacy backend; the only remaining step is the Nginx upstream
switch described under [Cutover](#cutover).

All times are UTC. Everything below was executed on 2026-10-06 and is evidence, not a plan.

## Cutover dependency

The Phase 2 backend takes the identity of a v1 join from the bearer token only
(`PHASE2_MEETING_DOMAIN.md` → *Legacy v1 join identity*): a join that carries `user_id`
without `Authorization` is answered `401`, and a guest that re-joins without its guest token is
a new participant. The web client deployed at `www.cfmeet.com` on 2026-10-06 still sends
`POST /api/v1/meetings/join/{code}` without the `Authorization` header (confirmed in the live
bundle and replayed against staging: `401` for a signed-in user). The fix is the web client
change in commit `e4886f4` (`apps/web/src/services/meeting.service.ts#joinMeeting` uses the
authenticated client).

Public traffic stays on the legacy backend until a web client containing that fix is live.
The legacy backend accepts the request shape the live client sends, and access tokens issued
by either backend are accepted by the other, so sessions survive the cutover and a rollback.

## Where the authoritative Phase 2 Swagger is

| Source | State |
|---|---|
| Checked-in contract `packages/contracts/openapi/meeting-service.json` | 1.1.0 · 187 · 49 v1 · 136 v2 (CI fails if it differs from the code) |
| Production-ready backend `127.0.0.1:18081` (`/swagger-ui/`, `/api-docs/openapi.json`) | 1.1.0 · 187 · 136 v2, server `https://api.cfmeet.com` |
| Staging `https://api.staging.cfmeet.com/swagger-ui/` | 1.1.0 · 187 · 136 v2, server `https://api.staging.cfmeet.com` |
| Public `https://api.cfmeet.com/swagger-ui/` | **legacy** 1.0.0 · 51 · 0 v2 until the cutover |

The public Swagger is deliberately not proxied to the new backend on its own: "Try it out"
would advertise endpoints that `api.cfmeet.com` does not route yet. It becomes 1.1.0
automatically at the cutover. The advertised server comes from `SWAGGER_SERVER_URL`
(default `https://api.cfmeet.com`).

## Staging

Isolated stack on the production host under `/opt/cfmeet-staging`; nothing is shared with
production except the LiveKit Cloud project.

| Item | Value |
|---|---|
| Source / image | `staging` `5c37070` → `cfmeet-meeting-service:staging-5c37070` (same service tree as `fd757bc`) |
| Backend | `cfmeet-staging-backend`, `127.0.0.1:18080`, public through `https://api.staging.cfmeet.com` (own Nginx site + certificate) |
| PostgreSQL / Redis | `cfmeet-staging-db` (15), `cfmeet-staging-redis` (7, AOF) on `cfmeet-staging-net`, no host ports |
| Data | restored copy of the production pre-Phase-2 dump: 10 users · 12 meetings · 37 participants · 14 migrations |
| Migration | 14 → 22; legacy rows unchanged (checksums) |
| Backfill | 12 / 12 meetings → meeting spaces, 12 legacy conferences, 37 / 37 participants, 0 mapping failures |
| Flags | as production (below) |

Functional validation against the public staging URL — **35 / 35 PASS**: v1 (login, refresh,
profile, public read of a migrated meeting, create / join / chat / end), v2 meeting spaces,
membership, conference lifecycle with provider room create / terminate, admission, waiting
room (admit / deny), roles and permissions, media policy, locks, spotlight, media credential,
chat and moderation, reactions, hand raise, polls, Q&A, breakouts, removal / ban with stream
revocation, attendance, realtime WebSocket with cursor replay, internal operations endpoints.

Rollback compatibility — **15 / 15 PASS**: the legacy image run against the *migrated* staging
database (login, reads, create / join / chat / end / delete, the join request shape the live
web client sends); tokens issued by one backend are accepted by the other.

Not executed in staging, by constraint (items of 11 that remain open):

- load, churn and soak — the host also runs production;
- real media and webhook-driven flows — staging shares the production LiveKit Cloud project
  and receives no provider webhooks (control plane only: rooms, credentials, permissions);
- recording / Egress / object storage — not provisioned; flags off (see 08 for the CI
  certification);
- staging rollback drill and 24 h observation.

## Production

| Step | Result |
|---|---|
| Baseline before any change | legacy `rtc_backend` `05836e3`; Swagger 1.0.0 · 51 · 0 v2; 14 migrations; 10 users · 12 meetings · 37 participants · 0 chat · 0 webhooks · 3 API keys |
| Rollback package | `/root/cfmeet-backups/pre-phase2-production-20261006-221945` — custom-format dump (verified with `pg_restore -l`), globals, env, compose, Nginx site, container / network inspect, version, migration list, row counts, checksums |
| Dump immediately before migration | `/root/cfmeet-backups/pre-phase2-production-migration-20261006-223142` |
| Source / image | `/opt/cfmeet-production/source` at `fd757bc` → `cfmeet-meeting-service:prod-fd757bc`; binaries byte-identical to the staging-validated image |
| Migration (22:31) | 14 → 22, exit 0; users / meetings / participants checksums identical before and after |
| Backfill | 12 / 12 meetings mapped, 37 / 37 participants mapped, 0 mapping failures, 0 unmapped ids |
| Phase 2 backend | `cfmeet-production-backend`, bound to `127.0.0.1:18081` only, on the production PostgreSQL / Redis network, env `/opt/cfmeet-production/.env` |
| Legacy backend | `cosmicforge-backend` untouched: still serving `api.cfmeet.com` on port 8080, never restarted |

Production flags: `MEETING_V2`, `WAITING_ROOM_V2`, `MEETING_V2_SHADOW_READ`,
`CONFERENCE_REALTIME`, `WEBINAR_MODE`, `COLLABORATION_V2`, `QUALITY_TELEMETRY`,
`RTC_PROVIDER_CONTROL_V2` **on**; `LEGACY_MEETING_WRITE_DISABLED`, `HEALTHCARE_PROFILE`,
`RECORDING_V2`, `TRANSCRIPTION_V2`, `INTELLIGENCE_V2` **off**.

### Pre-cutover certification (against `127.0.0.1:18081`, 18 / 18 PASS)

| Check | Result |
|---|---|
| health | 200 |
| Swagger | 1.1.0 · 187 operations · 136 v2 |
| migrations | 22 |
| DB connectivity | PASS |
| Redis connectivity | PASS (realtime fan-out delivered) |
| v1 compatibility | PASS — login, wrong password, missing token, profile, refresh, meetings list, public read identical to the legacy backend, tokens valid on both backends |
| v2 basic flows | PASS — meeting space create / read / list / update / resolve (incl. a backfilled legacy code), roster, permissions, chat, hand raise, attendance |
| WebSocket | PASS — upgrade, `FULL` / `SELF_ONLY` scopes, live waiting-room event, admit, cursor replay |
| internal auth | PASS — `/internal/*` 404 without / with a wrong token, 200 with `INTERNAL_API_TOKEN` |
| LiveKit control | PASS — 1 room confirmed, 2 credentials issued, 1 room terminated, 0 provider errors, 0 drift events, 0 incidents |

v1 write flows were not repeated in production (validated in staging with the identical
binary). Test artefacts left in production, all inert: one deactivated account
`cfmeet-prod-smoke-…@example.invalid` (inserted directly, no e-mail sent), one `ARCHIVED`
meeting space, one `ENDED` conference with its provider room terminated. Audit rows are
append-only, so they are not deleted.

## Security hardening

PostgreSQL (5432), Redis (6379) and the legacy backend port (8080) were published on all
interfaces and reachable from the internet. They are now closed to the internet by three
`DOCKER-USER` rules on the public interface (re-applied at boot and on Docker restart by
`cfmeet-docker-user-firewall.service`); loopback (Nginx → backend) and container-to-container
traffic are unaffected. Verified from outside the host: 5432 / 6379 / 8080 / 18080 / 18081
closed; 22 / 80 / 443 open. The new backends bind to `127.0.0.1` only.

## Rollback readiness

- Before the cutover there is nothing to roll back for users: public traffic never left the
  legacy backend.
- After the cutover: restore `proxy_pass http://localhost:8080` in the `api.cfmeet.com` site,
  `nginx -t`, graceful reload. The legacy backend keeps running, works on the migrated schema
  (the Phase 2 migrations do not alter any legacy table) and accepts the same tokens.
- Database restore is a last resort only; the packages above are kept. The legacy image is
  also kept as `cfmeet-rollback:pre-phase2-20261006`.

## Cutover

Precondition: the web client with the join fix is live.

1. Re-check health / Swagger on `127.0.0.1:18081` and that no legacy meeting is unmapped
   (meetings created on the legacy backend after the backfill are mapped lazily on first
   access).
2. In `/etc/nginx/sites-available/api.cfmeet.com` change the upstream to
   `http://127.0.0.1:18081`; `nginx -t`; graceful reload. No TLS or domain change.
3. Validate publicly: health, Swagger 1.1.0 · 187 · 136, v1 join from the web client,
   WebSocket handshake, logs. Roll back (above) on any failure.
4. Keep `cosmicforge-backend`, its image, `/home/chidex/rtc_backend` and the backups until
   legacy retirement is decided separately.

## Open items (operations, not backend implementation)

- Point the LiveKit project webhook at `https://api.cfmeet.com/internal/rtc/livekit/webhook`
  after the cutover; until then session state follows the reconciler.
- Give staging its own LiveKit project: with a shared project each reconciler reports the
  other environment's `cf-` rooms as unmapped (reported, never deleted).
- Both backends run the legacy auto-start / auto-end / e-mail workers while they coexist
  (one consumer per e-mail job, idempotent meeting transitions); stop the legacy container
  once rollback is no longer needed.
- Staging items listed above (load, real media, recording, 24 h observation) and the staged
  rollout / legacy-write retirement of 12 remain separate decisions.

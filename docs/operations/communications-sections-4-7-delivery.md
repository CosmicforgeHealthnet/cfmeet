# Sections 4–7 delivery record — 2026-11-04

## Release status

**IMPLEMENTED; substantially TESTED; production rollout BLOCKED. This is not a
completion or production certification.** No Section 4–7 production cutover has
been performed. The existing production deployment has been preserved.

GitHub refused to start the final VPS acceptance run
[41784595296](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41784595296).
Its check annotation says: “The job was not started because recent account payments
have failed or your spending limit needs to be increased. Please check the
'Billing & plans' section in your settings”. This is GitHub Actions billing, not
an additional deployment approval or a Vercel permission request.

The same restriction prevents rerunning the two final regression corrections.
Do not substitute previous-image acceptance receipts for the final image SHA, or
promote this milestone to production while those checks remain unresolved.

## Repository and branches

| Reference | SHA / state |
| --- | --- |
| Initial main and reconciled staging | `8d7d613d59424d7f340f70090f2355cfd9caba0e` |
| Latest product implementation and built images | `9ad4ccb6c447b2823e39afbe50835324f92910ac` |
| Latest implementation/test correction commit | `d78788cc37f00410b1f1eedaa99341de99ad3063` |
| Main at this record | Still the initial SHA; final promotion awaits validation |
| Staging at this record | Latest correction commit; this report is a subsequent documentation commit |

Only `main` and `staging` remain. Staging initially fast-forwarded to main; history
was not rewritten. Original refs and all history are preserved in the verified
local bundle `C:/Users/favou/.codex/branch-archives/cfmeet-before-communications-20261103.bundle`.
The final main/staging synchronization is pending safe release, not complete.
User attachments, the local branch-cleanup script and untracked phase documents
have been left untouched.

## IMPLEMENTED — services and ownership

`services/communications/` is a bounded Rust/Axum service with typed models,
configuration, authenticated internal routing, health/readiness, shutdown,
redacted logs/audit, metrics and its own database migrations. It owns personal
profiles, public identifiers, invitations, relationships, preferences, privacy,
reports and durable orchestration events. It stores no message plaintext,
authentication credentials, MLS private keys, recordings or media.

The Account service remains the public authenticated ingress and owns native
accounts and their existing Platform Access mappings. Its BFF forwards verified
identity through signed service authentication instead of forwarding the user's
JWT or accepting client-selected principals. Existing email/password and Google
registration do not acquire a phone requirement.

Platform Access remains authoritative for principal and service scope. The Hub
uses resolve/read permissions, not principal creation permissions. Calling and
Messaging remain canonical owners of calls, signaling, conversations, messages,
memberships, device identities and local block lists. Meetings admission remains
independent. All owner integrations use APIs/events, not runtime cross-database
joins.

The change also registers the service in root builds, Docker images, database
provisioning, service-boundary/operation guards, CI, contracts, Gateway discovery
and Swagger, and generated TypeScript/Dart SDKs. No frontend, mobile application,
Vercel settings or `frontend.yml` changes are part of this milestone.

## IMPLEMENTED — persistence and migrations

Hub migration `m20261103_000001_foundation` uses a separate
`communications_seaql_migrations` history and creates:

- `communication_profiles`, `communication_scope_authorities`,
  `communication_public_ids`, `communication_privacy`.
- `communication_invites`, `communication_invite_claims`.
- `communication_connections`, `communication_contact_requests`,
  `communication_contact_aliases`, `communication_block_bridge`.
- `communication_idempotency`, `communication_rate_windows`,
  `communication_inbox_dedupe`, `communication_service_nonces`.
- `communication_outbox`, `communication_audit`, `communication_profile_reports`.

Constraints protect unique account/principal bindings, globally unique public
IDs, canonical relationship pairs and idempotency/event keys. Transactional row
locks serialize concurrent acceptance, public-ID changes, invitation creation
and privacy revocation. Outbox leases, retries, bounded replay, inbox deduplication
and retention keep delivery durable without relying on Redis for authority.

Account, Calling and Messaging each have additive owner-specific Communications
migrations with separate histories and migration executables. They add lifecycle,
event and revocation projections without rebuilding their existing schemas or
moving customer data. The Account migration adds the suspended account state.
Runtime and migrator database roles remain separate; each service has its own
database and credentials. No OTP or phone ownership tables were added.

## IMPLEMENTED — identity, IDs and invitations

Profiles are created lazily and concurrently safely, with resumable backfill.
Activation is server controlled and idempotent without phone verification.
ACCOUNT_CREATED, ACTIVE, SUSPENDED and DEACTIVATED states are supported. Current
owner account state/version and Platform Access mapping/status are checked;
logout and credential cutoffs prevent old sessions from reviving authorization.

Public IDs use `CF-` plus a cryptographically random 128-bit base32 token, with a
database uniqueness constraint. Rotation requires owner authorization, recent
authentication, version/idempotency checks and throttling; old IDs become invalid
and outstanding invitations are revoked. Principal identity, sessions and
conversation ownership do not migrate.

QR payloads are verified-domain `/connect/<opaque-token>` URLs. Tokens contain
256 random bits and are persisted only as protected digests. Purpose, bounded
expiry/uses, ownership, revocation, replay, rate limits and privacy are enforced.
Listings/replays do not reveal reusable token material. Resolution never grants
chat/call permission automatically. Public discovery returns only permitted
public profile/relationship/action fields, never internal principal IDs, email,
phone numbers or session credentials.

## IMPLEMENTED — public authenticated contracts

All 29 public operations use `/api/v2/communications`, existing Account
authentication, versioned schemas and stable errors. Contracts include operation
IDs, capability tags, examples, negative authorization examples, pagination and
idempotency/version requirements.

| Family | Operations |
| --- | --- |
| Identity | GET `/me`, `/activation`, `/me/capabilities`, `/me/public-id`, `/me/qr`; POST `/me/public-id/rotate` |
| Privacy | GET and PUT `/me/privacy` |
| Discovery | POST `/discovery/public-id/resolve`, `/discovery/invites/resolve` |
| Invitations | POST and GET `/invites`; DELETE `/invites/{inviteId}` |
| Contacts | GET `/contacts`; GET, PATCH and DELETE `/contacts/{contactId}`; GET `/contacts/{contactId}/common-groups` |
| Requests | POST `/contact-requests`; GET `/contact-requests/incoming`, `/contact-requests/outgoing`; POST `/contact-requests/{requestId}/accept`, `/contact-requests/{requestId}/decline`; DELETE `/contact-requests/{requestId}` |
| Public profiles | GET `/profiles/{cfMeetId}`; POST `/profiles/{cfMeetId}/reports` |

Public Swagger has a distinct Communications family at
`/api-docs/communications.json`. Internal operations remain excluded. The public
catalogue has 528 operations; Communications has six documented capability
sections. Existing Calling, Messaging and Meetings public contracts are retained.
SDK domain accessors and generated contracts expose the new family without
changing application UI code.

## IMPLEMENTED — private integration and authorization

Signed private Hub routes under `/internal/v1/communications` include `/dispatch`,
`/policy/check`, `/principals/resolve`, `/account-events`, `/block-events`,
`/backfill`, `/replay` and `/metrics`. `/health` and database-backed `/ready` support
the private deployment. Account exposes owner-authoritative status/backfill;
Calling and Messaging expose scoped owner block/capability/event interfaces;
Messaging supplies mutual-group membership and actual security readiness.

Service permissions are narrowly scoped through the existing service-auth
registry. Signed requests carry delegated tenant/application/environment scope,
durable replay protection and idempotency. No internal credentials or
administrative APIs are exposed to public clients.

Connections support NONE, REQUEST_PENDING, CONNECTED, BLOCKED and REVOKED.
Requests enforce no self-connections, duplicate prevention, sender/receiver
authorization, cooldowns, privacy and concurrency-safe acceptance. Aliases,
favorites and notifications are per user. Common groups are filtered by both
users' authorized Messaging memberships. Blocking/unblocking does not silently
restore a removed relationship. Reports are authenticated, bounded and auditable.

Privacy covers discovery, invitation/request permissions, direct calls/chats,
profile visibility, presence and blocks. There is no permissive authorization
cache. Calling checks policy at authoritative creation (before idempotent replay),
acceptance, session/media admission and relevant control/transfer paths.
Messaging checks authoritative direct creation/reuse, message and MLS control
sends, invitations, group personal activity and presence. Existing domain blocks
remain enforced. A pending or declined request cannot grant direct initiation.
Policy dependency failure rejects new governed personal communications.

Enterprise exceptions use canonical enterprise call context and explicit scoped
authorization. Other permitted service scopes and legacy Messaging invitations
remain compatible. Meetings neither require a personal connection nor acquire a
Hub dependency for admission. Accepted contacts authorize creation/reuse of the
existing canonical DIRECT conversation; no competing conversation store exists.

## TESTED — recorded evidence

| Check | Actual result |
| --- | --- |
| [Hub foundation and consumers, 41700634351](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634351) | All five jobs passed at product SHA `9ad4ccb`: Hub four unit + 17 PostgreSQL/security/concurrency tests; shared policy test; 35 service-auth tests and supporting suites; all three consumer checks; real-service journey |
| Same run, real-service journey | One multi-scenario journey passed against five real binaries, PostgreSQL, Redis and LiveKit: phone-free native identities, stable mappings, migration/backfill, policy, privacy, direct Messaging reuse/send, voice/video call authorization, lifecycle/logout, cross-scope denial and fail-closed Hub outage while Meetings stays available |
| [Private VPS acceptance, 41698936564](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41698936564) | 13 scenario groups passed with image source `ad9f31383fc7ceda5273ca155d82ef7a22ccc600`; workflow source `1ee9bc641f8ac46379663822b635e12dd7e3e5f7` |
| Same private VPS acceptance, real media | Two isolated native accounts: actual incoming ringing, accept, endpoint-bound credentials, voice and video, bidirectional PCM audio tones and video frames through existing LiveKit/RTC gateway, authorized termination and fixture cleanup |
| [Messaging regression, 41700634418](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634418) | All six jobs passed at `9ad4ccb`, including real PostgreSQL/Redis/MinIO/ClamAV/Platform Access tests and client crypto runtime |
| [Meetings regression, 41693645101](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41693645101) | Passed; earlier milestone revision, not a claim that every later test correction was rerun |
| [Part VI certification, 41700634519](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634519) | Lint/unit and TypeScript/Dart real-stack SDK parity passed. Journeys failed the load profile criteria described below; not an overall pass |
| [Calling regression, 41698319135](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41698319135) | Build, migration, lint/boundaries and Docker passed. PostgreSQL suite failed one contract test described below; not an overall pass |
| [Core images, 41700635144](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700635144) | All six immutable core images built for `9ad4ccb`; build success is not deployment success |
| [Security, 41700634256](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634256) and [boundaries, 41700634217](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634217) | Passed for `9ad4ccb` |
| Local SDK | 141 TypeScript tests passed, two skipped; generation/type/build checks previously passed |
| Local final safety checks | 17 deployment/runtime-preservation tests passed; Calling formatting, public catalogue, Swagger metadata and SDK-generation checks passed; service-boundary guard passed |

These are separate executions/revisions, not one invented aggregate test total.
Tests using pre-existing Google-provider account records validate mapping and
activation compatibility; a live Google OAuth provider login was not performed.
Actual media used controlled synthetic PCM/video, not browser microphone/camera
permission automation. MANAGED message content was tested only in the isolated
TEST scope. Production MLS activation remains the next milestone; capabilities
must reflect missing device/key-transparency readiness truthfully.

### Final regression corrections awaiting execution

Calling's expired-offer contract test expected NO_ANSWER after one worker sweep.
The failed accept can still hold its transaction lock during asynchronous
rollback, while expiry intentionally uses SKIP LOCKED. Commit `d78788c` retries
bounded sweeps and still asserts the exact terminal state; it changes no runtime
deadline or authorization behavior. PostgreSQL rerun remains pending.

Part VI's unoptimized debug stack saturated the shared runner: all 2,160 mixed
requests returned 200, but measured p95/p99 were 29,376/30,304 ms, exceeding the
existing 10/20 s bounds. Warm authentication p95 was 2,878 ms versus 2,840 ms cold,
also failing its existing comparison. All next-request revocation, quota-race,
outage and denial criteria in that profile passed. Commit `d78788c` enables basic
optimization for the measured debug service binaries, keeping assertions and all
latency/security criteria unchanged. It is a pending correction, not evidence
that the performance issue has been resolved. Rerun and investigate any remaining
failure before release.

## DEPLOYED — precise limits

The earlier `ad9f313` images were deployed only to private, isolated VPS candidate
containers and feature-owned test databases. No host ports were published and no
customer database was copied. Evidence is stored privately under
`/opt/cfmeet-core/communications/staging-evidence.json`.

The final `9ad4ccb` image staging attempt did not start because GitHub blocked the
runner. Therefore no final-image VPS acceptance, production migration, main
promotion, production feature enablement or production image digest can be
reported for Sections 4–7. Registry image digests are captured by the updated
workflow before SSH Docker transfer and matched to loaded image IDs; the older
successful staging receipt predates that improvement and contains image IDs,
not registry digest evidence. This distinction is intentional.

Existing production Calling, Enterprise, capture/encryption and service flags
have not been changed by this blocked rollout. Their previous acceptance records
are not a fresh production certification. Current flags, public endpoints,
Swagger, authenticated native journeys, Enterprise regressions and loaded image
digests still require the production verification operation after cutover.

## Rollout and recovery already implemented

The existing GitHub-controlled VPS workflow now supports Communications inspect,
stage, release, verify and rollback operations. It transfers exact immutable
images using existing SSH access, verifies source revision/image identity and
retains registry manifest evidence. No new required reviewer or paid provider
was introduced.

Private candidates and repeated additive migrations precede cutover. Runtime
cloning preserves environment, mounts, secrets, loopback ingress, networks and
static IP configuration. Live calls drain before replacing Calling. Original
container configurations and images are retained for coordinated rollback;
additive database history is retained. Missing existing signing/digest keys are
recovered from protected same-host copies, not silently regenerated. The Account
deployment helper also preserves actual runtime configuration instead of
recreating it from stale environment files. See `communications-runtime-recovery.md`.

## BLOCKED — remaining release work

1. Resolve GitHub account/organization Actions billing or spending limits.
2. Rerun corrected Calling and Part VI checks; resolve any remaining failures.
   Complete final relevant CI without bypassing failed security criteria.
3. Build final immutable images and rerun private VPS acceptance for that exact
   source/image set. Earlier receipts cannot approve newer images.
4. Fast-forward main and synchronize staging after validation; deploy through the
   existing production workflow. Retain rollback and migrations evidence.
5. Verify authenticated and unauthorized public routes, private endpoint denial,
   Communications Swagger, actual capabilities, real native media, Enterprise
   compatibility and existing Messaging/Meetings; record final flags, workflow
   results, image IDs and registry digests.

The production release remains incomplete until those results are recorded.

## DEFERRED and next milestone

Phone OTP, telecom verification providers, phone ownership/matching, external
telecom carriers, off-server backups, frontend integration, production Messaging
activation and unified chat/call timeline orchestration are **DEFERRED / outside
this milestone**, not silently completed or failed. Same-host key recovery is
not off-server disaster recovery; backups remain an explicit owner-postponed
requirement.

The next Messaging activation/timeline milestone has not begun. Its foundation
is the implemented identity, invitation, contact and privacy contracts plus the
owner-enforced relationship policy. It becomes production-consumable only after
this milestone's final validation and deployment have passed.

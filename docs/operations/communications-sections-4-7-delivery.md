# Sections 4–7 delivery record — 2026-11-04

## Release status

**IMPLEMENTED, TESTED and DEPLOYED — Sections 4–7 backend foundation complete.**
All 24 application validation/build workflows passed. The coordinated production
[release 41795985590](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41795985590)
passed all 15 acceptance groups, followed by independent
[verification 41798927157](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41798927157)
with 14 passing groups. Production Enterprise real-media acceptance passed 20
checks, recording passed 12 checks, and voicemail passed seven checks. No failed
security result was accepted as a release pass.

The authenticated public Communications APIs are live at `https://api.cfmeet.com`.
No frontend, mobile or Vercel deployment was accessed or modified. The next
Messaging activation/unified timeline milestone has not begun.

GitHub initially refused to start VPS acceptance run
[41784595296](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41784595296).
Its check annotation says: “The job was not started because recent account payments
have failed or your spending limit needs to be increased. Please check the
'Billing & plans' section in your settings”. This is GitHub Actions billing, not
an additional deployment approval or a Vercel permission request. The owner
resolved the restriction and jobs resumed on November 4.

The corrected Calling suite has since passed all 720 tests. The final immutable
image set passed private VPS acceptance in run
[41788262379](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41788262379),
including actual bidirectional voice and video. The initial attempt had an RTC
connection timeout; the unchanged rerun passed every scenario. The corrected
load journey also passed, followed by all main-branch validation/build checks.

## Repository and branches

| Reference | SHA / state |
| --- | --- |
| Initial main | `8d7d613d59424d7f340f70090f2355cfd9caba0e` |
| Initial local staging (47 commits behind main) | `99db555aa5bad063cb8bbf45e7701a7b6eee257a` |
| Initial remote staging (120 commits behind main) | `fb08e3a73c0f90ef2aef4112b24a3f254779f453` |
| Reconciled baseline | `8d7d613d59424d7f340f70090f2355cfd9caba0e` |
| Final accepted immutable image source | `787bb9a58bd34add53f74a2bbf52aaa141b317d8` |
| Application validation revision | `90cfaeb400a60b3e881626460e3bc5e5ee7fda3e` |
| Final operational verification source | `0c05a4cefbbc9cb5b06c0c37b2f74e0ecca97fe3` |
| Main and staging | Synchronized at the operational source, followed by this report-only commit; exact report commit is included in the final handoff |

Only `main` and `staging` remain. Staging initially fast-forwarded to main; history
was not rewritten. Original refs and all history are preserved in the verified
local bundle `C:/Users/favou/.codex/branch-archives/cfmeet-before-communications-20261103.bundle`.
Main was fast-forwarded after security, Calling and final-image VPS acceptance;
both repository branches now match. The changes after the image source affect
deployment tooling only; the workflow verifies all service/shared compiled inputs
against the immutable image source before release.
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

Principal file groups are:

- New Hub: `services/communications/src/`, `migration/`, `tests/`, Rust manifests,
  lockfile and service Dockerfile.
- Shared policy/authentication: `packages/communications-policy/` and
  `packages/service-auth/src/{identity,permissions}.rs`.
- Account: `services/meeting-service/src/services/communications.rs`, existing
  authentication/actor/routes/state/Swagger integration and owner migration.
- Calling: `services/calling/src/communications.rs`, authoritative call create,
  ringing/control/transfer/media and signaling authorization, owner migration,
  error contract and contract-regression tests.
- Messaging: `services/messaging/src/communications.rs`, conversations,
  membership, message/MLS sends, presence/realtime/session authorization and
  owner migration.
- Contracts/SDKs: `packages/contracts/communications/`, owner-private contracts,
  bundled/public contracts, `infra/api-gateway/` catalogues, Gateway docs routing,
  existing contract scripts and generated/manual SDK domain accessors.
- Real-service tests: `certification/src/personal_stack.rs`,
  `certification/tests/personal_communications.rs` and existing CI workflows.
- VPS/deployment: `infra/calling/production/communications_{release,acceptance}.py`,
  their safety tests, existing controlled API acceptance/credential recovery,
  `infra/deployment/backend_runtime.py`, existing production/Account/image
  workflows, Makefile, database provisioning and ownership/operations registries.
- Delivery/recovery documentation: this report,
  `communications-sections-4-7-baseline.md` and
  `communications-runtime-recovery.md`.

The [complete implementation diff and tracked file inventory](https://github.com/CosmicforgeHealthnet/cfmeet/compare/8d7d613d59424d7f340f70090f2355cfd9caba0e...0c05a4cefbbc9cb5b06c0c37b2f74e0ecca97fe3)
is pinned to the implementation/deployment revision; final report updates are
documentation changes after that revision.

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
| Blocking | GET `/blocks`; PUT and DELETE `/profiles/{cfMeetId}/block` |

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

Final main revision `90cfaeb` passed **all 24 validation/build workflows**:
[Hub](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789567862),
[Calling](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789567930),
[Messaging](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789567922),
[Meetings](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789568020),
[Part VI / SDK parity](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789567803),
[contracts/service-auth](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789567894),
[Enterprise](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789567897),
[Calling RTC](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789568017),
[boundaries/secrets/database roles](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789567895),
[security](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789568015),
and all remaining existing backend, SDK and image workflows. No validation/build
workflow failed or was bypassed. The Account-only deployment was separately
superseded as explained below; its passed pre-deploy tests are retained.

| Check | Actual result |
| --- | --- |
| [Hub foundation and consumers, 41700634351](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634351) | All five jobs passed at product SHA `9ad4ccb`: Hub four unit + 17 PostgreSQL/security/concurrency tests; shared policy test; 35 service-auth tests and supporting suites; all three consumer checks; real-service journey |
| Same run, real-service journey | One multi-scenario journey passed against five real binaries, PostgreSQL, Redis and LiveKit: phone-free native identities, stable mappings, migration/backfill, policy, privacy, direct Messaging reuse/send, voice/video call authorization, lifecycle/logout, cross-scope denial and fail-closed Hub outage while Meetings stays available |
| [Private VPS acceptance, 41698936564](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41698936564) | 13 scenario groups passed with image source `ad9f31383fc7ceda5273ca155d82ef7a22ccc600`; workflow source `1ee9bc641f8ac46379663822b635e12dd7e3e5f7` |
| Same private VPS acceptance, real media | Two isolated native accounts: actual incoming ringing, accept, endpoint-bound credentials, voice and video, bidirectional PCM audio tones and video frames through existing LiveKit/RTC gateway, authorized termination and fixture cleanup |
| [Messaging regression, 41700634418](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634418) | All six jobs passed at `9ad4ccb`, including real PostgreSQL/Redis/MinIO/ClamAV/Platform Access tests and client crypto runtime |
| [Meetings regression, 41693645101](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41693645101) | Passed; earlier milestone revision, not a claim that every later test correction was rerun |
| [Part VI certification rerun, 41785134564](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41785134564) | All three jobs passed: lint/unit/boundaries, real-service journeys/load/resilience and TypeScript/Dart real-stack SDK parity |
| [Calling regression rerun, 41785134335](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41785134335) | All four jobs passed: build/migration, lint/boundaries, Docker and PostgreSQL. Database test execution: 720 passed, zero failed, zero ignored |
| [Final core images, 41786520697](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41786520697) | All six immutable core images built for `787bb9a`; build success alone is not deployment success |
| [Final-image VPS acceptance, 41788262379](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41788262379) | All 13 scenario groups passed on rerun for exact image source `787bb9a`; loaded image IDs and registry digests recorded |
| [Security rerun, 41785134193](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41785134193) | All 28 jobs passed, including new Hub/policy RustSec coverage |
| [Latest staging boundaries, 41788183383](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41788183383) | Guard/mutation/operations checks, repository secret scan and per-service database-role checks passed for deployment source `90cfaeb` |
| [Security, 41700634256](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634256) and [boundaries, 41700634217](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41700634217) | Passed for `9ad4ccb` |
| Local SDK | 141 TypeScript tests passed, two skipped; generation/type/build checks previously passed |
| Local final safety checks | 21 deployment/runtime-preservation/fixture-lifetime tests passed; Calling formatting, public catalogue, Swagger metadata and SDK-generation checks passed; service-boundary guard passed |

These are separate executions/revisions, not one invented aggregate test total.
Tests using pre-existing Google-provider account records validate mapping and
activation compatibility; a live Google OAuth provider login was not performed.
Actual media used controlled synthetic PCM/video, not browser microphone/camera
permission automation. MANAGED message content was tested only in the isolated
TEST scope. Production MLS activation remains the next milestone; capabilities
must reflect missing device/key-transparency readiness truthfully.

### Regression corrections

Calling's expired-offer contract test expected NO_ANSWER after one worker sweep.
The failed accept can still hold its transaction lock during asynchronous
rollback, while expiry intentionally uses SKIP LOCKED. Commit `d78788c` retries
bounded sweeps and still asserts the exact terminal state; it changes no runtime
deadline or authorization behavior. The full PostgreSQL rerun passed.

Part VI's unoptimized debug stack saturated the shared runner: all 2,160 mixed
requests returned 200, but measured p95/p99 were 29,376/30,304 ms, exceeding the
existing 10/20 s bounds. Warm authentication p95 was 2,878 ms versus 2,840 ms cold,
also failing its existing comparison. All next-request revocation, quota-race,
outage and denial criteria in that profile passed. Commit `d78788c` enables basic
optimization for the measured debug service binaries, keeping assertions and all
latency/security criteria unchanged. The rerun passed: 2,160 mixed requests at
72 requests/second, zero errors, p95 9.0 ms and p99 12.2 ms. Warm authentication
p95 was 167.7 ms versus 347.3 ms cold; next-request revocation remained 401.
These are CI assertion-enabled process-stack measurements, not a VPS capacity
or production load certification.

## DEPLOYED — production evidence

The earlier `ad9f313` images were deployed only to private, isolated VPS candidate
containers and feature-owned test databases. No host ports were published and no
customer database was copied. Evidence is stored privately under
`/opt/cfmeet-core/communications/staging-evidence.json`.

The final image source `787bb9a` subsequently passed private VPS acceptance.
Registry image digests were captured before SSH Docker transfer and matched to
loaded image IDs. Both production release and independent verification confirmed
the following running image source and digests:

| Image | Registry manifest digest |
| --- | --- |
| Platform Access | `sha256:5df2e7212c08e8ce0e593c47c9c1efe9051734789441cc8bb1972b9731a3ba29` |
| Account / meeting-service | `sha256:57cd7745a4c513ccd47df7e44d3b78f1a39d86a700cc645f2083c1f035425df7` |
| Calling | `sha256:46ac5c739b9fde0c67e4f531fc3724c2ad98b3401d4c3d4ed12f0e79a10da46d` |
| Messaging | `sha256:0124976ae113b139b47a2cd7e5af61488c698ce7020c49668b2ec38bcef3cc6b` |
| Communications | `sha256:df34e048a613b0818543d48b90163351141f0448fbacf9bfcb230b79af6f88ef` |

The owner-specific additive migrations and the Hub production schema were applied
successfully. All five services are running on the accepted images. Private
staging used isolated databases; production acceptance used only the existing
authorized fixture accounts and test tenants. No customer records or canonical
identities were replaced.

| Production workflow | Actual result |
| --- | --- |
| [Coordinated release, 41795985590](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41795985590) | PASS: 15 foundation/compatibility groups; exact image IDs/digests; retained rollback |
| Same run, Enterprise acceptance | PASS: 20 checks including real voice/video, ringing/decline/cancel/missed history, reconnection, group media, extensions, ring groups, queues, IVR keypad routing, internal transfer, hold/resume and tenant/scope denial |
| [Independent verification, 41798927157](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41798927157) | PASS: 14 groups, including repeated actual bidirectional voice/video through public ingress, canonical CONNECTED sessions, live block enforcement, truthful capabilities and protected identity throttling |
| [Recording, 41799294360](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41799294360) | PASS: 12 checks; real encrypted media playback, required consent/notice, nonparticipant/cross-user/tenant denial, audit, retention, access revocation, physical object deletion and retention worker |
| [Voicemail, 41799339221](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41799339221) | PASS: seven checks; real audio lifecycle, owner/tenant isolation, read/delete access revocation, retention deadline, physical object deletion and retention worker |

Final public ingress checks returned health 200; unauthenticated Communications,
Calling, Messaging identity and Meetings 401; private Account and Hub policy
routes 404. Public Communications Swagger returned 200 with all 29 operations
and no internal paths. Authenticated native accounts exercised those APIs through
the actual public Account/Calling ingress; direct calls without an accepted
relationship were denied, accepted calls carried real media, and blocking revoked
permission. Existing Messaging identity/directory/relay and Meetings compatibility
checks passed. Messaging readiness remains truthful: new fixture accounts without
the required private-messaging device/security material are not falsely advertised
as security-ready.

| Runtime capability / setting | Verified production state |
| --- | --- |
| Communications Hub and Account API adapter | Enabled; own PostgreSQL, scoped service auth, private internal endpoints |
| Personal Calling/Messaging policy client | Configured for the authoritative native personal scope; fail closed |
| `CALLING_NEW_CALLS_ENABLED` / `CALLING_APP_CALLING_ENABLED` | `true` / `true` |
| `ENTERPRISE_CALLING_ENABLED` | `true`; internal features tested without a carrier |
| `VOICEMAIL_ENABLED` / `CALL_RECORDING_ENABLED` | `true` / `true`; functional/security acceptance passed |
| `CALLING_MEDIA_PROVIDER` / `CALLING_CAPTURE_PROVIDER` | `livekit` / `livekit-egress` |
| `CALLING_ARTIFACT_STORAGE_SERVER_SIDE_ENCRYPTION` | `AES256`; existing self-hosted MinIO key configuration retained; encrypted recording playback passed |
| Public outbound / public inbound / callback relay / synthetic interconnect | All `false` |
| Phone verification | `DEFERRED`, not required for activation; no fake verification |

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

The separate [automatic Account deployment, 41789567945](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41789567945)
was deliberately superseded before its VPS job started. Its pre-deploy tests
passed. The coordinated Communications rollout includes Account, so running
both would rebuild and restart the same service twice. Independent main CI
continues unchanged; all validation/build workflows must pass before the
coordinated release is dispatched. The canceled deployment is not reported as
successful CI or a production release.

The public ingress explicitly denies the Account owner-internal Communications
routes. The private TLS proxy retains an internal 443 path to the actual host
ingress with upstream certificate verification; no new host port is published.
Production native-media acceptance uses the same ordinary outbound Docker bridge
as the existing successful production media probe, and calls the actual public
Account/Calling APIs and unchanged issued media URL. It therefore reaches the
advertised public ICE addresses without the core network's isolation or Docker
DNS alias intercepting the client route. Private Hub authentication is checked
separately on the core network, and its public counterpart must return 404.
Paused-run fixture credentials renew with finite expiry
and the same least-privilege scopes, preserving their existing principals.

The first production attempt stopped before replacing containers because the
helper did not handle null Docker aliases and its early rollback error obscured
the original failure. Inspection confirmed all original image IDs unchanged;
additive migrations were retained. The null-alias case now has a regression test,
early failures no longer require a nonexistent rollback record, and diagnostic
stack locations are redacted to file/function/line metadata.

The next attempt passed native identity, private ingress, privacy, requests and
service readiness, but its isolated-network RTC client timed out. Automatic
rollback restored all original services and both LiveKit key checks passed.
The production probe network was corrected rather than accepting that failed
media result. Deployment safety tests now total 21, all passed.

Post-release verification exposed two test issues: its ten-second canonical-state
wait was shorter than the existing fifteen-second media reconciliation lease,
and repeated rotation correctly hit the production public-ID rate limit. The
test now allows the existing lease/sweep to complete while still requiring
CONNECTED state and actual received frames. Repeated verification asserts that
429 preserves identity; initial release acceptance still requires successful
rotation. Unused-invitation invalidation is covered by the PostgreSQL rotation
tests; the independent production rerun exercised throttling instead of another
rotation. No production authorization, rate limit or feature flag was weakened.
Independent verification subsequently passed in full.

## BLOCKED

No remaining technical blocker for the Sections 4–7 backend release. Failed
attempts and their corrections are recorded above; they are not counted as
successful tests. Production implementation and functional/security acceptance
are complete. Off-server disaster recovery remains an explicit deferred owner
requirement, and production Messaging activation remains outside this milestone.

## Post-release repository synchronization — November 4, 16:00 UTC

Both final milestone branch checks passed:
[main 41800907493](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41800907493)
and [staging 41800907862](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41800907862),
including ownership/mutation guards, secret scanning and database-role isolation.

During the subsequent pause, another author advanced main to
`f7269415a8ddf35e2f2d38b59d45a1de664b983e` with later Calling documentation/CORS
and frontend work. That history was preserved and local/main/staging were
fast-forwarded together. No frontend code was edited, inspected or manually
tested as part of this continuation; its later failed checks are outside this
backend milestone.

Backend-only cleanup commit `b8c3de34b6e53c7aad7074592a979edda5e28e85` fixes
the new Calling formatting failure and scopes the security test's tracing
dispatcher to every asynchronous request poll. The test still requires the
rejection log and safe key identifier, and still checks that tokens/signatures
never reach logs, responses or headers. No security assertion was removed.
`cargo fmt --check` and `cargo check --locked --test internal_auth` passed locally.

Fresh [Calling CI 41865047261](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41865047261)
and the other main backend checks are queued during the official
[GitHub Actions/Pages incident](https://www.githubstatus.com/incidents/xr3myzj7ms51).
Their new PostgreSQL/security execution is **PENDING**, not reported as passed.
Duplicate staging suites were superseded by the identical-commit main validation;
repository guard checks remain enabled for both branches.

These later repository changes have **not** replaced the accepted production
image set. Production remains on source `787bb9a`, with the completed functional
and security receipts above. The external CI outage blocks verification of the
later repository-only cleanup, not the already delivered Sections 4–7 APIs.
The final report-only commit and synchronized branch SHA are supplied in the
handoff.

## DEFERRED and next milestone

Phone OTP, telecom verification providers, phone ownership/matching, external
telecom carriers, off-server backups, frontend integration, production Messaging
activation and unified chat/call timeline orchestration are **DEFERRED / outside
this milestone**, not silently completed or failed. Same-host key recovery is
not off-server disaster recovery; backups remain an explicit owner-postponed
requirement.

The next Messaging activation/timeline milestone has not begun. Its foundation
is the implemented identity, invitation, contact and privacy contracts plus the
owner-enforced relationship policy. These documented authenticated backend APIs
are now production-consumable. The next milestone must address actual private
Messaging device/security readiness and orchestration rather than duplicating
identity, contacts, Calling or conversation storage.

# Communications Sections 4–7 implementation inventory

Initial audit, 3 November 2026. Initial main: `8d7d613d59424d7f340f70090f2355cfd9caba0e`.
Initial local staging: `99db555aa5bad063cb8bbf45e7701a7b6eee257a`;
initial remote staging: `fb08e3a73c0f90ef2aef4112b24a3f254779f453`.
Both staging tips were ancestors of main (47 and 120 commits behind respectively).

## Reusable implementation

| Area | Existing owner and integration |
| --- | --- |
| Accounts | Meeting Service owns email/password and Google accounts, JWTs, Redis-backed session revocation, account status and registered-user actors. Registration does not require a phone. |
| Principals | Platform Access is authoritative. Existing `cfmeet-user:<account UUID>` ensure operation and Account-owned `communication_principal_links` preserve principal IDs. |
| Service authentication | `packages/service-auth`: Ed25519 issuer/audience/environment-bound tokens, delegated scope, receiver-owned trust and narrow permission registry. |
| Messaging | Canonical DIRECT conversations, membership, device public keys, encryption readiness, blocks, transactional outbox, authenticated delegated APIs. Reuse all of these. |
| Calling | Canonical call lifecycle, endpoints, signaling, media grants, block enforcement and separately authorized Enterprise routes. Personal policy belongs before authoritative initiation/admission. |
| Meetings | Existing account authentication and conference admission remain independent of personal connection state. |
| Contracts | Contract manifest, bundled OpenAPI, distinct public Swagger families, generated SDK contracts and service-boundary/secret guards. |
| Data/deployment | Per-service PostgreSQL roles and forward migrations; GitHub-controlled private Contabo topology, immutable images, preserved runtime snapshots and rollback. |

## New implementation and integrations

Create `services/communications` for profiles, opaque public IDs/invitations, contacts,
requests, privacy, reports, audit and durable events. Add a shared typed policy client,
not a competing identity or authentication authority. Account-authenticated ingress is
`/api/v2/communications/**`; internal service routes are signed and private.

Calling and Messaging must check personal policy themselves, including direct API
access. Apply this to the configured first-party personal scope. Preserve explicit
Enterprise, meeting-owned and externally scoped service use cases, and existing local
blocks. Check current account and domain privacy through owner APIs, never another
service's database. No optimistic allow on dependency failure.

Use additive migrations; preserve accounts, principal links, devices, conversations,
call history and meetings. Activation requires no phone verification. Phone OTP,
phone ownership and phone address-book discovery are deferred. Messaging activation
and unified timeline orchestration remain the next milestone.

## Branch reconciliation

Staging was fast-forwarded to main without rewriting history. There are no open PRs.
Historical merged branches were retired. Four unmerged historical heads contained
superseded Swagger tooling, a browser diagnostic and already-incorporated security
work (44 security files have identical blobs on main; remaining differences are later
main integrations or regenerated locks). Their older workflows would remove newer
contract/security coverage, and their browser changes are outside this milestone.
They were archived, not applied over current production code.

A verified, self-contained Git bundle preserves every original ref and complete
history at `C:/Users/favou/.codex/branch-archives/cfmeet-before-communications-20261103.bundle`.
Only `main` and `staging` remain as repository branches; both initially reconcile to
`8d7d613d59424d7f340f70090f2355cfd9caba0e`. No new feature branch is introduced.

## Validation environment

Rust 1.92 is available locally. Docker/PostgreSQL are not installed in this Windows
workspace; database/migration and real-service tests will run against isolated
PostgreSQL in GitHub Actions, followed by the existing VPS deployment workflow.
Local compilation and pure unit tests do not certify production behavior.

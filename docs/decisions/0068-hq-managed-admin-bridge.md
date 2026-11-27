# ADR 0068 — CF Meet is managed from HQ through one admin bridge

**Status:** Accepted (owner review of the design pending; phase A0)
**Date:** 2026-11-19
**Design:** [docs/admin/cfmeet-admin-bridge.md](../admin/cfmeet-admin-bridge.md)
**Counterpart:** `centralize-admin/docs/product-admin/cfmeet-admin-design.md` (HQ's `services/cfmeet-admin`)
**Extends:** [ADR 0004](0004-internal-service-authentication.md) (service auth), [ADR 0032](0032-platform-access-shared-identity-authority.md), [ADR 0065](0065-app-review-directory-in-meeting-apps.md) (reviewer lists retired)

## Context

CF Meet administration is spread over a Meeting Service admin login, server command-line
tools (`messaging-admin`, Calling `platform_admin`, Platform Access operator tooling), SQL
for switches held in databases, GitHub workflow buttons for activations and environment
lists of named reviewers (`DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS`,
`DEVELOPER_PLATFORM_APP_REVIEWERS`). The CosmicForge central admin (HQ) already manages
Karryam through a product bridge and a governed operation pipeline with two-person
approvals. The owner decided CF Meet is managed the same way.

## Decisions

1. **One doorway.** A new service, `services/admin-bridge` (identity `cfmeet.admin-bridge`),
   is the only way HQ reaches CF Meet. HQ never calls another CF Meet service. The bridge
   owns no business data, never reads end-to-end encrypted content and never touches
   Docker, servers or secrets.
2. **HQ interface v1.** Private mTLS port (TLS 1.3, enrolled client certificate checked on
   every request), HMAC v2 request signature, an Ed25519 staff assertion bound to the
   exact request (`request_digest`), single-use nonce and `jti`, per staff member and
   session revocation, and a pinned, CF Meet-signed access policy (role → permission →
   purpose, four scopes). The exact rules are generated into
   `services/admin-bridge/docs/contract/signing-profile.md`.
3. **The contract is generated, pinned and frozen.** The capability catalog in the bridge
   crate is the single source of truth for the runtime route table and every contract
   file HQ pins (OpenAPI, capability matrix, schemas, access-policy ceiling, datasets,
   event adapters, test vectors, `SHA256SUMS`). CI refuses drift and breaking changes
   against `main`; a published schema is immutable. Artifact state never authorizes
   anything at runtime.
4. **Two people for R3 / R4.** Approvals live in HQ. The staff assertion carries the
   approval bound to the capability called; the bridge checks it and uses it once, and
   the owning service records operator and approver separately and refuses
   approver = operator in its own decision log.
5. **Owners record the person.** The bridge calls owners with its own `cf-service+jwt`
   (never the HQ token) plus `X-CF-Operator` / `X-CF-Approver` / `X-CF-Approval-Ref` /
   `X-CF-Intent-Digest`, which owners accept only from `cfmeet.admin-bridge`
   (`cf_service_auth::operator`). New operator permissions are grantable only to the
   bridge; grants ship under the release activation `activate-admin-bridge`.
6. **Off by default, one family at a time.** `ADMIN_BRIDGE_ENABLED`,
   `ADMIN_BRIDGE_WRITES_ENABLED` and one switch per module start off. HQ staging talks to
   CF Meet production read-only first; each command family is enabled only after
   certification.

## Consequences

* Phases A0–A9 (design §15) each ship as a separate PR, switched off, with an
  acceptance check, paired with HQ phases B0–B9.
* The reviewer name lists, routine use of server tools and workflow buttons for
  activations are retired once the matching HQ families are certified (HQ stage S7).
* Every owner gains `/internal/v1/operator/*` endpoints with audit and tests (design §7).

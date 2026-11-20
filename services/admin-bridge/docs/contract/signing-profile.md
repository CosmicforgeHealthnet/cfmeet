# CF Meet Admin Bridge signing profile (HQ interface v1, contract 1.8.0)

Generated from `services/admin-bridge/src` by `admin-bridge-contract generate`; do not edit.
Design: `docs/admin/cfmeet-admin-bridge.md` §3. Test vectors: `fixtures/`.

## Transport

- Private network only, dedicated port, TLS passed straight through. Never reachable through `api.cfmeet.com`.
- TLS 1.3 only, mutual TLS. The client certificate must carry exactly one URI SAN equal to the enrolled HQ workload identity, and its SHA-256 fingerprint must match a live enrolled row for `cosmicforge-hq-cfmeet-admin`. Checked on every request.
- Path at most 4096 bytes; no percent-encoded bytes and no `.` / `..` segments in the path. Body at most 32 KiB, JSON only.

## HMAC v2

Headers: `X-HQ-Key-ID`, `X-Service-Name: cosmicforge-hq-cfmeet-admin`, `X-Service-Timestamp` (unix seconds), `X-Service-Nonce` (32 lowercase hex), `X-Service-Signature-Version: 2`, `X-Service-Signature` (64 lowercase hex).

```
signature = hex(HMAC-SHA256(key, "2" LF "cosmicforge-hq-cfmeet-admin" LF METHOD LF request-URI LF timestamp LF nonce LF hex(sha256(body))))
```

`request-URI` is the path and query exactly as sent. Clock skew at most ±30 s. Each HMAC key has its own expiry. The nonce is single-use (see Replay).

## Staff assertion

`Authorization: Bearer <compact JWS>`. Protected header exactly `{"alg":"EdDSA","typ":"JWT","kid":<enrolled kid>}` (no other member). Ed25519.

| Claim | Rule |
|---|---|
| `iss`, `aud` | equal the enrolled issuer and audience |
| `product` | `"cfmeet"` |
| `sub` | HQ staff id, 1–128 of `[A-Za-z0-9._:@-]` |
| `roles[]` | bridge roles (below); none is preferred by position |
| `permissions[]` | must contain the capability's permission |
| `market_scopes[]`, `organization_scopes[]`, `zone_scopes[]`, `tenant_scopes[]` | all four always present; the request's values must be members |
| `session_id`, `auth_time`, `acr` | revocation is checked per `sub` and per `session_id`; R2+ needs `now − auth_time ≤ 300`; R3/R4 need an enrolled strong `acr` |
| `jti` | UUID, single-use |
| `iat`, `nbf`, `exp` | `exp − iat ≤ 60`; `nbf ≤ now < exp` (±30 s skew) |
| `request_digest` | `hex(sha256(METHOD LF request-URI LF "cfmeet" LF hex(sha256(body))))` |
| `approval` | required on every TWO_PERSON capability (below) |

### Approval claim

`approval = {approval_id (uuid), capability, requester_ref, approver_ref, intent_digest (64 lowercase hex), decided_at, expires_at (RFC 3339 UTC)}`.
Refused when: missing on a TWO_PERSON capability; `capability` is not the route called; `requester_ref ≠ sub`; `approver_ref = sub`; `expires_at ≤ now`; `expires_at − decided_at > 43200 s`; `approval_id` used before (stored). The bridge forwards `X-CF-Operator: hq:<sub>`, `X-CF-Approver: hq:<approver_ref>`, `X-CF-Approval-Ref` and `X-CF-Intent-Digest`; the owning service repeats the approver ≠ operator check.

## Replay

The HMAC nonce and the assertion `jti` are stored together in one transaction and kept 11 minutes. A repeat of either is `409 REPLAY`.

## Order of checks

1 request shape → 2 client certificate → 3 HMAC v2 → 4 staff assertion → 5 request digest → 6 revocation → 7 replay → 8 capability lookup (method + path; unknown is 404) → 9 authorization (pinned signed access policy; permission, role and purpose; all four scopes; MFA / acr by risk; approval) → 10 rate limit → 11 audit row written (failure: 503, no data, nothing done) → owner call.

## Query shapes

Exact ordered lists; a missing required, extra, repeated or out-of-order parameter is `400 QUERY_INVALID`.

| Shape | Parameters (in order) | Required |
|---|---|---|
| `SCOPED` | market, organization, zone, tenant, purpose | market, organization, zone, tenant, purpose |
| `SCOPED_PAGE` | market, organization, zone, tenant, purpose, limit, cursor | market, organization, zone, tenant, purpose |
| `SCOPED_WINDOW` | market, organization, zone, tenant, purpose, limit, cursor, start, end | market, organization, zone, tenant, purpose, start, end |
| `SCOPED_SEARCH` | market, organization, zone, tenant, purpose, limit, cursor, q | market, organization, zone, tenant, purpose, q |

- `organization` is always `cfmeet`; `zone` is `production` or `sandbox`; `market` and `tenant` match `^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$`.
- `limit` 1–100; `cursor` matches `^[A-Za-z0-9._~:@+=-]{1,512}$`; `start` / `end` are RFC 3339 UTC, `end > start`, window at most 31 days, `end` not in the future by more than 60 s.
- `q` matches `^[\p{L}\p{N} ._@:-]{1,128}$`.
- Path parameters: `format: uuid`, or a pattern with no `/`.

## Purposes

- `support-review`
- `verification-review`
- `abuse-review`
- `billing-review`
- `pricing-change`
- `ops-incident`
- `release-request`
- `privacy-request`
- `admin-ui-capabilities`
- `metrics-read`
- `event-sync`

## Roles

Identical to HQ's product-grant roles (identity role map), plus the HQ worker's feed identity:

- `cfmeet.viewer`
- `cfmeet.support`
- `cfmeet.trust_safety`
- `cfmeet.verification.reviewer`
- `cfmeet.partner_ops`
- `cfmeet.finance`
- `cfmeet.pricing.editor`
- `cfmeet.pricing.approver`
- `cfmeet.platform_ops`
- `cfmeet.security_admin`
- `cfmeet.release_manager`
- `cfmeet.source_sync`

The most each role can be granted is `access-policy.v1.json`.

## Commands

Every command needs `Idempotency-Key` (UUID). HQ never retries; the bridge sends a command to its owner at most once. A timeout or broken owner answer is `502 UNKNOWN_OUTCOME`, settled only by reading back. The one read that uses `POST` is `pricing.charges.simulate`.

## Envelope

`{contract_version:"1.0", product:"cfmeet", correlation_id (uuid), as_of, freshness, result, error}`. On error `result` is absent. Missing data is `null` or `UNAVAILABLE`, never zero.

## Error codes

| Code | HTTP | Meaning |
|---|---|---|
| `BRIDGE_DISABLED` | 503 | ADMIN_BRIDGE_ENABLED is off |
| `BRIDGE_UNAVAILABLE` | 503 | the bridge's own database is unavailable; nothing was done |
| `REQUEST_SHAPE_INVALID` | 400 | path too long, encoded or dot segments, or a malformed request line |
| `BODY_TOO_LARGE` | 413 | body over 32 KiB |
| `CLIENT_CERTIFICATE_REFUSED` | 401 | no client certificate, wrong URI name, or fingerprint not enrolled and live |
| `HQ_KEY_UNKNOWN` | 401 | X-HQ-Key-ID is not an enrolled, live HMAC key |
| `SIGNATURE_INVALID` | 401 | HMAC v2 headers missing or signature mismatch |
| `TIMESTAMP_SKEW` | 401 | X-Service-Timestamp more than 30 s from bridge time |
| `STAFF_ASSERTION_INVALID` | 401 | staff assertion malformed, wrong header, unknown kid, bad signature, wrong iss / aud / product, or lifetime over 60 s |
| `STAFF_ASSERTION_EXPIRED` | 401 | staff assertion outside nbf / exp |
| `REQUEST_DIGEST_MISMATCH` | 401 | request_digest does not match this request |
| `REVOKED` | 401 | staff member or session revoked |
| `REPLAY` | 409 | nonce or jti already used |
| `CAPABILITY_NOT_FOUND` | 404 | no capability for this method and path |
| `QUERY_INVALID` | 400 | query parameters not exactly the capability's shape, in order, with valid values |
| `POLICY_UNAVAILABLE` | 503 | no verified access policy at the pinned revision |
| `PURPOSE_NOT_ALLOWED` | 403 | purpose not allowed for this capability or role |
| `PERMISSION_DENIED` | 403 | permission missing from the assertion or not granted to any asserted role by the access policy |
| `SCOPE_DENIED` | 403 | market, organization, zone or tenant outside the assertion scopes or the enrolled scope |
| `RECENT_MFA_REQUIRED` | 403 | R2+ needs auth_time within 300 s |
| `STRONG_AUTH_REQUIRED` | 403 | R3 / R4 need a strong acr |
| `APPROVAL_REQUIRED` | 403 | TWO_PERSON capability without an approval claim |
| `APPROVAL_INVALID` | 403 | approval for another capability, requester is not sub, approver is sub, or lifetime over 43200 s |
| `APPROVAL_EXPIRED` | 403 | approval expires_at has passed |
| `APPROVAL_REUSED` | 409 | approval_id already used |
| `CAPABILITY_DISABLED` | 503 | capability not served in this build, its module switch is off, or writes are off |
| `IDEMPOTENCY_KEY_REQUIRED` | 400 | command without a valid Idempotency-Key |
| `IDEMPOTENCY_CONFLICT` | 409 | Idempotency-Key reused with a different request |
| `RATE_LIMITED` | 429 | per staff member, permission and scope limit reached |
| `AUDIT_UNAVAILABLE` | 503 | the audit row could not be written; nothing was done and no data is returned |
| `DATASET_NOT_FOUND` | 404 | unknown dataset id |
| `RESOURCE_NOT_FOUND` | 404 | the owning service has no such resource |
| `OWNER_UNAVAILABLE` | 503 | the owning service did not answer a read |
| `OWNER_REFUSED` | 422 | the owning service refused the command (FAILED) |
| `UNKNOWN_OUTCOME` | 502 | a command was sent and no clear answer came back; settle by reading back, never resend |
| `INTERNAL` | 500 | unexpected bridge failure |

## Command outcomes

For a command, these codes guarantee the owning service was never called (nothing was done): every 4xx code, and these 5xx codes, which the bridge raises before any owner call:

- `BRIDGE_DISABLED`
- `BRIDGE_UNAVAILABLE`
- `POLICY_UNAVAILABLE`
- `CAPABILITY_DISABLED`
- `AUDIT_UNAVAILABLE`

Any other 5xx after a command was sent (`UNKNOWN_OUTCOME`, `INTERNAL`, `OWNER_UNAVAILABLE`) is an unknown outcome: settle it by reading back, never resend.

## Served in this contract version

- `bridge.capabilities.read`
- `bridge.health.read`
- `bridge.datasets.read`
- `hq.projections.status`
- `accounts.users.search`
- `accounts.users.read`
- `accounts.sessions.list`
- `accounts.devices.list`
- `accounts.mfa.read`
- `accounts.security-history.list`
- `meetings.spaces.search`
- `meetings.meetings.search`
- `meetings.meetings.read`
- `meetings.live.list`
- `meetings.participants.list`
- `meetings.audit.list`
- `communications.profiles.read`
- `communications.reports.list`
- `communications.holds.list`
- `communications.audit.list`
- `communications.metrics.read`
- `organizations.organizations.search`
- `organizations.organizations.read`
- `organizations.bindings.list`
- `organizations.audit.list`
- `organizations.global-audit.list`
- `organizations.members.list`
- `organizations.teams.list`
- `organizations.policies.read`
- `partners.workspaces.search`
- `partners.workspaces.read`
- `partners.workspace-activity.list`
- `partners.projects.list`
- `partners.environments.list`
- `partners.members.list`
- `partners.invitations.list`
- `partners.keys.list`
- `partners.service-accounts.list`
- `partners.websites.list`
- `partners.push-keys.list`
- `partners.users.list`
- `partners.webhooks.list`
- `partners.webhook-deliveries.list`
- `identity.tenants.list`
- `identity.applications.list`
- `identity.environments.list`
- `identity.service-accounts.list`
- `identity.scope-grants.list`
- `identity.keys.list`
- `identity.records.list`
- `identity.oauth-apps.list`
- `identity.app-grants.list`
- `identity.partner-users.list`
- `identity.audit.list`
- `apps.apps.search`
- `apps.apps.read`
- `apps.versions.list`
- `apps.installs.list`
- `notifications.deliveries.list`
- `notifications.failed-deliveries.list`
- `notifications.suppressions.list`
- `notifications.routing.read`
- `notifications.providers.read`

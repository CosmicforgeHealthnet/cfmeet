# ADR 0057 — Account sessions, typed tokens and an always-on revocation epoch

**Status:** Accepted (Blueprint v1.0 Section 2; staging only — production needs the release prerequisites in [account-security-section-2.md](../operations/account-security-section-2.md))
**Date:** 2026-11-06
**Resolves:** ACC-001, ACC-002 (+SEC-003), ACC-003, ACC-004 (+SEC-005, FE-011 backend half), ACC-005 (backend half), ACC-006, ACC-007, ACC-014, ACC-015 (+SEC-008, partial), ACC-017, ACC-018/SEC-001, DATA-002/ACC-023 (lookups), SEC-004 (existence), SEC-007 (session tokens), SEC-013/DATA-015

## Context

Meeting-service is the human account authority every other service trusts (ADR 0056 §3). The audit found that its tokens were indistinguishable (a 7-day refresh token worked as a bearer), that logout and password reset/change did not end tokens on v1 routes or the Developer Platform session bridge, that the only revocation epoch lived in the Communications integration and was switched off without it, that Google sign-in could take over a pending password account, that there was no session model and no security audit.

## Decisions

1. **Typed tokens.** Every token carries `typ` (`access` or `refresh`). Access tokens are refused by `/refresh`; refresh tokens are refused by the v1 middleware, the v2 actor, logout and the session bridge. Tokens issued before this change carry no `typ`; they are classified exactly by lifetime (access tokens lived 15 minutes, refresh tokens 7 days), so no compatibility window or forced logout is needed.
2. **Account sessions.** Every password or Google sign-in opens a row in `account_sessions`; tokens carry the session id (`sid`) and refresh tokens a refresh id (`rti`). A session lasts at most 30 days. Refresh rotates the refresh id; presenting a superseded refresh token is treated as theft and ends the whole session (`REFRESH_REUSE`). A revoked session ends its access tokens immediately, checked in PostgreSQL on every request, so revocation no longer depends on Redis for session tokens (SEC-007).
3. **Always-on account epoch.** `account_security_state.credentials_not_before` is advanced by a database trigger on every password-hash change and on a change to `inactive`/`suspended` — whoever performs the write — and the same trigger closes every open session. It is enforced by every verifier, with or without the Communications integration; the Communications epoch is still enforced in addition when configured. One function, `account_security::authorize_user_token`, makes the decision for the v1 middleware, the v2 actor, `/refresh` and the session bridge.
4. **Logout semantics.** `POST /api/v1/auth/logout` ends the current session only. `POST /api/v2/me/sessions/revoke-all` signs out everywhere (and keeps the existing Communications/Hub and Messaging fan-out). A pre-session token cannot be tied to one session, so its logout signs the account out everywhere — never less than before.
5. **Legacy refresh tokens** are accepted once: they open a session with their original `auth_time` (so the 30-day absolute limit counts from the original sign-in) and are then revoked.
6. **Google identity.** The verified Google subject is stored (`account_security_state.google_subject`, unique). The ID token is validated for issuer, audience, authorized party, expiry, nonce and `email_verified` (it is received directly from Google's token endpoint over TLS with the client secret, OIDC Core §3.1.3.7). A bound subject always wins. An ACTIVE account with the same email is linked; a PENDING password account is claimed only after its unverified password is discarded (which also ends that password's sessions); an account bound to another subject is refused. State is single-use and consumed atomically; the flow is bound to the starting browser with an HttpOnly cookie; Google usernames are sanitized and de-duplicated.
7. **No tokens in URLs (staged).** The callback can deliver a 60-second single-use `exchange_code` redeemed at `POST /api/v1/auth/oauth/google/exchange`. The legacy query-string delivery stays the default (`GOOGLE_OAUTH_TOKEN_DELIVERY`) only until the live web client adopts the exchange; switching is a configuration change, not a release.
8. **Uniform answers and limits.** Forgot-password and resend-verification answer the same whether or not an account exists; verify-email, reset-password and verify-reset-token answer unknown accounts like wrong codes; login takes the same time for unknown accounts. Per-address limits apply when the trusted proxy header is configured (`AUTH_CLIENT_IP_HEADER`); per-account limits always apply. Registration still answers 409 for a taken email (documented residual: an enumeration oracle bounded by the registration rate limit).
9. **Email identity.** Lookups are case-insensitive (`LOWER(email)` index) and new registrations store a lowercased address. A unique case-insensitive constraint is a later contract step after a duplicate review of existing rows.
10. **Signing material.** Outside `APP_ENV` development values, startup refuses a missing, example or shorter-than-32-byte `JWT_SECRET`.
11. **Audit.** `account_security_events` is append-only (update, delete and truncate refused by triggers), carries no email, token, password or address, and is readable by platform administrators at `GET /api/v2/admin/audit-events`.
12. **Rollback safety.** All schema lives in a separate history (`account_security_migrations`, binary `account-security-migration`); no existing table or column changes, so the previous image keeps running after the migration.

## Consequences

* One extra primary-key lookup per authenticated request (the session row) and one for the epoch.
* Password change signs the current session out too (the trigger cannot tell sessions apart); the client signs in again.
* Single-session logout no longer advances the Communications epoch; per-device fan-out to Hub devices belongs to Section 14.
* The legacy guest JWT path is unchanged except that a guest refresh now requires the guest participant to exist.

## Amendment A1 — Devices belong to sign-ins; revocation reaches every owner (Blueprint v1.0 Section 14)

**Status:** Accepted for `staging` (Section 14; resolves ACC-008, ACC-010; implements the device half of DATA-005). Not deployed. Release order and steps: [communications-hub-section-14.md](../operations/communications-hub-section-14.md).

### Relation between account sessions and devices

The **account session** (Decision 2) is the root credential of one sign-in on one device. Every other credential a device holds is subordinate to a session or to the account:

| Credential | Owner | Bound to | Ends when |
|---|---|---|---|
| CF Meet access / refresh tokens | Account | account session | the session ends, or the account epoch advances |
| MANAGED session device (ADR 0049 A1) | Messaging | account session (`accountSessionRef`) | the session ends; renewal re-checks the session and epoch |
| Hub device reference (`communication_devices`) linking a Messaging device, its push registration and/or a Calling endpoint | Hub (index), owners (credentials) | the account session that linked it (`account_session_ref`, new) | the session ends; or the device is revoked |
| Messaging client sessions of the principal, Calling signaling credentials | Messaging, Calling | the principal | the account epoch advances (all of them) |

The binding is the same opaque, domain-separated reference ADR 0049 A1 uses (`SHA-256("cfmeet-account-session:v1|environment|session")`); Account computes it, receivers can match it and never reverse it. It is immutable once recorded.

### Decisions

1. **Session → devices.** Ending one account session (`POST /api/v1/auth/logout` with a session token, `POST /api/v2/me/sessions/{id}/revoke`) ends its session devices at once (unchanged) and queues `ACCOUNT_SESSION_ENDED` in Account's durable outbox. The Hub revokes every ACTIVE device bound to that session at its owners (Messaging device incl. push registration and client sessions; Calling endpoint incl. live invitations and signaling) and marks it `REVOKED` with `revocationReason: ACCOUNT_SESSION_ENDED`. An owner failure answers 503 and the outbox retries the same event; owner calls are idempotent.
2. **Device → session.** `POST /api/v2/communications/devices/link` binds the device to the sign-in making the request. `POST /api/v2/communications/devices/{id}/revoke` of a bound device signs that sign-in out: the Hub returns the bound reference only to the Account relay (which sent its own reference), and Account ends the matching open session (`USER_REVOKED`, security event `SESSION_REVOKED` with `trigger: DEVICE_REVOKED`) with its session devices and other bound devices. The reference is consumed by Account and never relayed to clients. A retry of an already-revoked device hands the reference back, so a partial failure completes.
3. **Account-wide ends reach every owner, with or without a Hub profile (ACC-010).** Password change and reset, self-deactivation, admin deactivation or suspension, and logout-everywhere already advance both epochs in the database (trigger) and queue the Hub event. Now:
   * the Hub applies the event even when the account never opened the Hub: it reads the principal from Account (`/internal/v1/accounts/communications/status`) and fans `communications.account.changed {principalId, credentialsNotBefore}` out to Calling and Messaging through its durable outbox (exactly once per Account event; owners keep the greatest epoch, so replays and reordering are harmless). A profile created later takes Account's epoch at activation, so no Hub-side tombstone is needed;
   * Account also queues the durable Messaging `client-sessions/revoke-all` for the web-bridge principal on every such end (it did so only on logout), with reasons `PASSWORD_CHANGED` / `ACCOUNT_DISABLED`;
   * Account's outbox worker mirrors the account's **current** status onto its Platform Access `NATIVE_CF_USER` principal (`POST /internal/v1/principals/account-status`: inactive → DEACTIVATED, suspended → SUSPENDED, otherwise ACTIVE), retried with the event. Reading the current status makes delivery order irrelevant; reactivation restores ACTIVE.
4. **Device-management surface.** No new API. Sign-ins are listed and ended with `GET /api/v2/me/sessions`, `POST /api/v2/me/sessions/{id}/revoke`, `POST /api/v2/me/sessions/revoke-all`; linked devices with `GET /api/v2/communications/devices` (`accountSession.bound`) and `POST /api/v2/communications/devices/{id}/revoke`. Either action ends what the other holds. The client links its Messaging device / Calling endpoint after registering them (handoff §11).

### Consequences

* Revoking the device you are using signs you out (by design: the "lost phone" and "this device" cases are the same operation).
* Devices linked before this amendment, or by an older Account image, are unbound (`accountSession.bound: false`); they keep the previous behaviour (revoking them does not end a sign-in). Re-linking does not bind an existing reference.
* Deploy order: Platform Access and Communications before Meeting (an older Hub refuses `ACCOUNT_SESSION_ENDED` and the session reference; an older Platform Access answers 404 to the mirror, treated as "nothing to mirror"). Hub migration `m20261108_000001_device_session_binding` (third Hub history) before the Hub image.
* Remaining window: a Calling signaling credential issued before the epoch advance stays valid until the receiving process applies the event (seconds; bounded by its 600 s lifetime), as before.

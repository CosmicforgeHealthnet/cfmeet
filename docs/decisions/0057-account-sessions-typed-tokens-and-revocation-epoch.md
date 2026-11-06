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

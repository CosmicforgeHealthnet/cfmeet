# Account security (Blueprint v1.0 Section 2) — release and operations record

**Decision record:** [ADR 0057](../decisions/0057-account-sessions-typed-tokens-and-revocation-epoch.md). **Scope:** `services/meeting-service` (account authority) plus the release tooling that runs its migrations. Backups and restore drills: [backup-and-restore.md](backup-and-restore.md).

## 1. What changed

| Area | Behaviour now | Findings |
|---|---|---|
| Token types | `typ` claim; refresh tokens never authenticate, access tokens never refresh; legacy untyped tokens classified by lifetime | ACC-001 |
| Revocation epoch | `account_security_state` + trigger on `users`; enforced on v1, v2, refresh and the Developer Platform session bridge, with or without Communications | ACC-002, ACC-003, SEC-003 |
| Sessions | One row per sign-in, 30-day absolute lifetime, rotating single-use refresh tokens with reuse detection; DB-checked on every request | ACC-007, ACC-017, SEC-007 |
| Session APIs | `GET /api/v2/me/sessions`, `POST /api/v2/me/sessions/{sessionId}/revoke`, `POST /api/v2/me/sessions/revoke-all` | ACC-007 |
| Logout | Ends the current session only (pre-session tokens: everywhere) | ACC-007 |
| Google sign-in | Subject binding, full claim validation (aud/azp/exp/nonce/email_verified), atomic single-use state, browser-binding cookie, pending-account password discarded before takeover, safe unique usernames, optional one-time exchange code | ACC-004, ACC-005, ACC-006, SEC-005 |
| Enumeration and limits | Uniform recovery answers, equal-time login, per-address limits behind a trusted proxy header | ACC-014, ACC-015, SEC-008 |
| Email identity | Case-insensitive lookups and lowercased new registrations | DATA-002, ACC-023 |
| Signing material | Startup refuses missing/example/short `JWT_SECRET` outside development | SEC-001, ACC-018 |
| Audit | Append-only `account_security_events`; `GET /api/v2/admin/audit-events` (platform administrators) | SEC-013, DATA-015 |
| Guest refresh | Requires the guest participant to still exist | SEC-004 (existence part) |

New migration history: `account_security_migrations` (`m20261106_000001_account_security`), applied by `/app/account-security-migration up`. It only adds tables, a trigger, two functions and a non-unique index; the previous image is unaffected by it.

## 2. Release prerequisites (production is NOT changed by this section)

1. **Backup first.** Encrypted pre-migration snapshot of the account database (`infra/deployment/backup/pre-migration-snapshot.sh`) and a passed restore drill.
2. **Migration order.** `migration up` → `account-security-migration up` → (`account-communications-migration up` where Communications is configured). The release tooling now does this: `infra/deployment/backend_runtime.py` and `infra/calling/production/communications_release.py` list the new binary; feature-only releases apply every additive history after the base one.
3. **`JWT_SECRET` strength.** The new image refuses to start unless `JWT_SECRET` is at least 32 bytes and not the example value (or `APP_ENV` is `dev`/`development`/`local`/`test`). Check the deployed value's length **without printing it**, for example `docker exec <container> sh -c 'printf %s "$JWT_SECRET" | wc -c'`. If it is shorter, rotate the secret first (this signs every user out once).
4. **Trusted client-address header.** Set `AUTH_CLIENT_IP_HEADER` to the header the host nginx sets from `$remote_addr` (for example `x-real-ip`) only after confirming nginx overwrites any client-supplied value. Leave it unset otherwise; per-account limits still apply.
5. **Google token delivery.** Keep `GOOGLE_OAUTH_TOKEN_DELIVERY` unset (legacy query) until the live web client (`rtc_frontend`, Vercel, owned by the frontend team) handles `exchange_code` by calling `POST /api/v1/auth/oauth/google/exchange`. Then set `GOOGLE_OAUTH_TOKEN_DELIVERY=exchange_code`. `GOOGLE_OAUTH_BROWSER_BINDING` defaults to on; the API host (`GOOGLE_REDIRECT_URL`) must be the host that served `/api/v1/auth/oauth/google`, which it is today.
6. **Client impact to announce.** Password change now ends the current session too; refresh tokens are single-use (a client that refreshes twice in parallel with the same token ends its session — the web interceptor already serializes refresh); sessions end 30 days after sign-in.

## 3. Duplicate-email review before a unique constraint

Run read-only, report counts only:

```sql
SELECT count(*) AS groups, sum(n) AS accounts
FROM (SELECT LOWER(email) AS e, count(*) AS n FROM users GROUP BY 1 HAVING count(*) > 1) d;
```

If the result is zero, a later contract migration may add `CREATE UNIQUE INDEX ... ON users (LOWER(email))`. If not, each group needs an owner decision (merge or rename) first. Until then, a login whose address matches several rows only by case uses the exact spelling, otherwise it is refused and logged.

## 4. Verification

| Check | Where | Status |
|---|---|---|
| Unit: token kinds, legacy classification, Google claim validation, username generation, secret policy, auth policy, error catalogue | `cargo test --lib` (run locally, 10 passed) | PASS (local) |
| DB integration: typed tokens on v1/v2, rotation + reuse, per-session logout, epoch on v1/v2/refresh after password change and deactivation (no Communications), revoke one/all, legacy refresh upgrade, uniform recovery answers, email normalization, Google takeover closure, audit immutability and admin read, anonymous refusal | `tests/account_security.rs` in `backend.yml` (real PostgreSQL + Redis) | CI run recorded in the master register |
| Session bridge: refresh token and post-password-change token rejected | `tests/session_bridge.rs` | CI |
| Cross-service stack starts with the new history | `certification.yml`, `communications-hub.yml` (personal stack) | CI |
| Contracts: OpenAPI export, bundles, gateway default-deny classification, SDKs | `backend.yml`, `contracts.yml`, `sdk-*.yml` | regenerated and checked locally; CI |

## 5. Not done in this section (owner or later section)

* MFA and account deletion/export (ACC-011, ACC-013): owner decision D-6.
* Device-level Hub/Calling/Messaging fan-out for a single session (ACC-008, ACC-010): Section 14.
* Web/mobile token storage and the callback page change (ACC-019, ACC-012, FE-011 client half): Section 17 handoff.
* Legacy plaintext API keys (PLT-002/DATA-003): hashed by the Sections 2–3 hardening ([hardening-sections-2-3.md](hardening-sections-2-3.md)); retirement stays Section 16.
* Deactivation re-authentication, reactivation audit, email-verification enforcement points, legacy media eviction, migration CLI guard, database roles and alert rules: [hardening-sections-2-3.md](hardening-sections-2-3.md).

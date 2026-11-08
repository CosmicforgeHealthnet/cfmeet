# Section 17 — Web account session and organization administration (handoff)

Branch `blueprint/s17-web-account`. Applies Blueprint v1.0 Sections 2, 5, 6, 8 and 17 to `apps/web`.
Every route below is from the served contract `packages/contracts/openapi/meeting-service.json`
(types generated into `apps/web/src/types/generated/meeting-service.ts`; `npm run contracts:generate`,
CI checks freshness).

## Account session (ACC-019 / SEC-021)

| Piece | Where |
|---|---|
| Access token: memory only, per tab | `src/lib/auth/session-core.ts` (`createSessionManager`) |
| Refresh token: `cfm_rt` cookie, HttpOnly, SameSite=Strict, Path=/api/session, Secure on HTTPS, ≤ token expiry / 30 days | `src/lib/auth/session-cookies.ts`, `src/app/api/session/route.ts` |
| Non-secret hint `cfm_session=persistent\|session` (script-readable, no token) | same |
| Refresh: `POST /api/session/refresh` → server calls `POST /api/v1/auth/refresh`, rotates the cookie, returns only `access_token` | `src/app/api/session/refresh/route.ts` (`ACCOUNT_API_INTERNAL_URL` overrides the upstream base) |
| Single-flight per tab, serialized across tabs (Web Locks `cfmeet-account-session-refresh`) | `session-core.ts` |
| CSRF guard on session routes: `X-CF-Session: 1` + `Sec-Fetch-Site`/`Origin` same-origin | `isSameOriginSessionRequest` |
| One-time migration of legacy `access_token`/`refresh_token` JS cookies, then erased | `src/lib/auth/session.ts` |
| 401 → one refresh-and-retry; refresh 401 → `SessionEndedError`, cache cleared, `/login?reason=session_ended` | `src/lib/auth/axios-session.ts`, `src/lib/auth/use-session-watch.ts` |
| Cross-tab sign-out (BroadcastChannel) | `session.ts` |
| Google: `?exchange_code=` redeemed once (`POST /api/v1/auth/oauth/google/exchange`), query erased before anything else | `src/hooks/useGoogleCallback.ts` |
| Static guard: no account token written to cookies / local / session storage | `tests/account-session.test.mjs` |

Sign-in flows: password login and Google exchange call the API directly from the browser (real client
IP / UA), then hand only the refresh token to `POST /api/session`. CSP is unchanged (`connect-src 'self'`
already covers `/api/session`).

## Traceability (screen / action → method / path → errors handled)

| Screen / action | Method / path | Errors surfaced |
|---|---|---|
| Sign in | `POST /api/v1/auth/login` → `POST /api/session` | API message; session route 403/400 |
| Google callback | `POST /api/v1/auth/oauth/google/exchange` → `POST /api/session` | expired/used code |
| Any request (401) | `POST /api/session/refresh` (→ `POST /api/v1/auth/refresh`) | 401 SESSION_ENDED, 429 RATE_LIMITED, 503 |
| Sign out | `POST /api/v1/auth/logout`, `DELETE /api/session` | best effort |
| Settings → Profile load / save | `GET` / `PATCH /api/v2/me/profile` | 400 VALIDATION_ERROR, 409 USERNAME_TAKEN |
| Settings → Security: change password (then forced sign-in) | `POST /api/v1/auth/change-password` | 400 (wrong current password, Google account) |
| Settings → Security: sessions list | `GET /api/v2/me/sessions` | 401 |
| Settings → Security: sign out one session | `POST /api/v2/me/sessions/{sessionId}/revoke` | 404 |
| Settings → Security: sign out everywhere | `POST /api/v2/me/sessions/revoke-all` | 401 (kept signed in on failure) |
| Settings → Danger zone: deactivate | `POST /api/v1/users/me/deactivate` `{current_password?}` | 403 REAUTHENTICATION_REQUIRED (wrong password vs. stale Google sign-in) |
| Workspace switcher | `GET /api/v2/me/workspaces`, `GET /api/v2/me/workspaces/{id}/capabilities` | 404 → back to Personal + cache dropped; 503 → Personal only |
| Organizations list | `GET /api/v2/me/workspaces` (organization items) | 503 SERVICE_UNAVAILABLE |
| Create organization | `POST /api/v2/organizations` + `Idempotency-Key` | 400, 409 SLUG_TAKEN / IDEMPOTENCY_CONFLICT, 429, 404 FEATURE_DISABLED |
| Organization overview (ORG-003) | `GET /api/v2/organizations/{id}` | 403 MEMBERSHIP_REQUIRED, 404 ORGANIZATION_NOT_FOUND |
| Provisioning status | `GET /api/v2/organizations/{id}/provisioning` (polled until settled) | 404, 503 |
| Members (ORG-006) | `GET /api/v2/organizations/{id}/members` | 403, 404 |
| Change member role / status | `PATCH /api/v2/organizations/{id}/members/{membershipId}` `{expectedVersion, role?, status?}` | 409 VERSION_CONFLICT / LAST_OWNER, 403 |
| Leave | `POST /api/v2/organizations/{id}/members/me/leave` | 409 LAST_OWNER |
| Transfer ownership | `POST /api/v2/organizations/{id}/ownership-transfer` `{toMembershipId, expectedVersion}` | 409 VERSION_CONFLICT, 403 |
| Invitations list | `GET /api/v2/organizations/{id}/invitations?status=` | 403 |
| Invite | `POST /api/v2/organizations/{id}/invitations` + `Idempotency-Key` `{email, role, sendEmail}` | 409 ORGANIZATION_SUSPENDED / IDEMPOTENCY_CONFLICT, 429 |
| Revoke invitation | `POST /api/v2/organizations/{id}/invitations/{invitationId}/revoke` | 404 INVITATION_INVALID |
| Accept / decline (`/organizations/invitations#<token>`) | `POST /api/v2/organization-invitations/accept\|decline` `{token}` | 410 INVITE_EXPIRED / INVITATION_INVALID, 403 (email mismatch) |
| Edit profile | `PATCH /api/v2/organizations/{id}` `{expectedVersion, name?, slug?, profile?}` | 409 VERSION_CONFLICT (reload) / SLUG_TAKEN / ORGANIZATION_SUSPENDED / ORGANIZATION_ARCHIVED |
| Suspend / reactivate / archive | `POST /api/v2/organizations/{id}/suspend\|reactivate\|archive` `{expectedVersion}` | 409 VERSION_CONFLICT / ORGANIZATION_ARCHIVED / ORGANIZATION_SUSPENDED |
| Audit log | `GET /api/v2/organizations/{id}/audit-events?before=&limit=` | 403 |

UI rules applied: every organization control is gated by `viewer.capabilities` from
`GET /organizations/{id}` (re-read on every visit, `staleTime: 0`; the role is displayed, never used to
grant); per-organization queries live under `["workspace", orgId, ...]` and are dropped when the
membership disappears; invitation tokens are never in a URL sent to a server or in logs (fragment,
erased on arrival; body only); create/invite idempotency keys are reused only for retries of the same
submission. Error text is mapped from stable codes in `src/lib/organizations/organization-core.ts`.

## Backend / contract gaps found (not faked in the UI)

1. **Teams and policies are not public.** The Organizations Service has `/internal/v1/organizations/{id}/teams*`
   and `/policies*` (ORG-009 write path), but Meeting Service relays neither and the served OpenAPI has no
   `/api/v2/organizations/{id}/teams` or `/policies`. The web app has no Teams/Policies screens until they are
   exposed.
2. **Member directory (ORG-006) has no identity and no pagination.** `GET /api/v2/organizations/{id}/members`
   returns `OrganizationMemberResponse[]` (ids, role, status, dates) without display name/email and without a
   cursor. The UI shows "Account 1a2b3c4d…" and pages the served array client-side.
3. **Untyped responses.** Invitation list (`{items}`), audit events (`{items, nextCursor}`), provisioning,
   accept/decline and ownership-transfer answers are `type: object` in OpenAPI. Minimal local shapes live in
   `src/services/v2/business-organizations.service.ts`; replace them with generated types once named.
4. **`createdBy` is a user id with no display name** (ORG-003): shown as "You" or a shortened account id.
5. **Refresh rate limiting / client attribution.** Refreshes reach the API from the web server, so any per-IP
   limiter on `/api/v1/auth/refresh` sees the web tier's address; session `clientLabel` stays from sign-in.
6. **`change-password` bounds**: the API documents 8–128 characters, while bcrypt reads 72 bytes; the web form
   enforces ≤ 72 bytes (ACC-025 rule in `account-core.ts`).

# CF Meet — production frontend environment handover (Calling and Developer Console)

**Date:** 2026-11-13. **Audience:** David (frontend), the owner (hosting access). **Backend:** `https://api.cfmeet.com`.
**Scope:** configuration only. No UI, layout or component change; no backend feature change.

## 1. Where the frontend runs

| Item | Finding (live, 2026-11-13) |
|---|---|
| Host | **Vercel**. `https://cfmeet.com` answers `307` → `https://www.cfmeet.com` (`server: Vercel`, region `iad1`). |
| Project / team | **Unknown to engineering.** No Vercel access exists in GitHub or this tooling: secret `VERCEL_TOKEN` absent in the `production` environment (audit run [43381900787](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43381900787), also on 2026-11-01, run 41363275249); no Vercel connector. The project owner must identify it in the Vercel dashboard (the project whose domains list `cfmeet.com` / `www.cfmeet.com`). |
| Build | Next.js (Turbopack) from `apps/web` (root directory `apps/web`). |
| `app.cfmeet.com` | not served (502); not part of production. |

## 2. Deployed configuration versus requirements

Public (`NEXT_PUBLIC_*`) values are compiled into the browser bundle, so they were read from the live bundle
and the live Content-Security-Policy (no project access needed). Server-only values cannot be read from
outside; their state is stated as such.

| Variable | Kind | Required production value | Live state | Action |
|---|---|---|---|---|
| `NEXT_PUBLIC_API_URL` | public | `https://api.cfmeet.com/api/v1` | **correct** (in bundle) | none |
| `NEXT_PUBLIC_CALLING_API_URL` | public | `https://api.cfmeet.com` | **correct** (in bundle and CSP) | none |
| `NEXT_PUBLIC_MESSAGING_URL` | public | `https://api.cfmeet.com` | **missing** (left as an unresolved `process.env` reference) | set, redeploy |
| `NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED` | public | `true` | **missing** (`/developers` shows "switched off") | set, redeploy |
| `NEXT_PUBLIC_DEVELOPER_API_BASE_URL` | public | `https://api.cfmeet.com` | **missing** | set, redeploy |
| `NEXT_PUBLIC_MESSAGING_STORAGE_URL`, `NEXT_PUBLIC_MESSAGING_KT_LOG_ID`, `NEXT_PUBLIC_MESSAGING_KT_LOG_KEY`, `NEXT_PUBLIC_MESSAGING_POLL_MS`, `NEXT_PUBLIC_MESSAGING_DEVICE_CHECK_MS` | public, optional | leave unset (defaults) | unset | none |
| `CALLING_API_URL` | server | `https://api.cfmeet.com` | not observable | set (falls back to the public value if absent) |
| `CALLING_DEV_MODE` | server | `false` (or unset) | not observable; a production build refuses `true` | set `false` |
| `CALLING_PLATFORM_CREDENTIAL` | **secret** | the frontend server credential (below) | not observable | set from the VPS secret store |
| `CALLING_TENANT_ID` | server, non-secret | `d92576b3-b0cb-4f53-921b-9cebc10b63c9` | not observable | set |
| `CALLING_APPLICATION_ID` | server, non-secret | `b89bab3f-f0c1-4444-90de-9fa73be3a40a` | not observable | set |
| `CALLING_ENVIRONMENT_ID` | server, non-secret | `8e1af752-3148-47c2-bc5f-80bd003afab6` | not observable | set |
| `CALLING_ADMIN_PRINCIPAL_IDS` | server | empty (deny by default) until the owner names Calling administrators by principal id | not observable | owner decision |
| `CALLING_PRIVACY_ADMIN_PRINCIPAL_IDS` | server | empty (Privacy Relay is excluded) | not observable | leave empty |
| `ACCOUNT_API_INTERNAL_URL`, `CSP_EXTRA_CONNECT_SRC` | server, optional | unset on Vercel | — | none |

**Calling credential scope.** The three ids are the CF Meet communication scope from the VPS file
`/opt/cfmeet-core/communication.scope.env` (exported by run 41363979475). That is the scope the Meeting Service serves
on `GET /api/v2/messaging/me`, and the frontend server refuses every Calling request when the ids differ (`server.ts`,
`CALLING_NOT_CONFIGURED`). Checking them live against an authenticated `/api/v2/messaging/me` answer is part of the
acceptance in §5, and that acceptance has not been run yet.

**Frontend server credential.** A dedicated credential already exists: `/opt/cfmeet-core/secrets/cfmeet-web-bff.credential`
on the VPS (root-only). Reuse it; no new credential was created. On 2026-11-01 it was exported once, encrypted
with RSA-OAEP-SHA256 to the public key in `.github/workflows/cfmeet-vercel-configuration-export.yml` (run 41363979475).
Only the holder of that key pair's private key can decrypt it and paste it into Vercel as `CALLING_PLATFORM_CREDENTIAL`
(Production, "Sensitive"). If that private key is gone, re-run the export with a new public key held by the person
configuring Vercel. The credential never goes into a `NEXT_PUBLIC_*` variable, a commit or a report.

## 3. Gateway routing (live, unauthenticated probes)

| Route family | Probe | Result |
|---|---|---|
| Calling | `GET /v1/calls`, `GET /v1/calling/endpoints` | `401 AUTHENTICATION_FAILED` (routed, protected) |
| Messaging realtime | `POST /v1/realtime/tickets` / `GET /v1/realtime` | `401` / `400` (routed) |
| Meeting / Messaging REST | `GET /api/v2/messaging/me` | `401 AUTHENTICATION_REQUIRED`; CORS allows both web origins |
| Developer Platform | `GET /v1/developer-platform/context` | `401 AUTHENTICATION_FAILED` (routed, protected) |
| Excluded | `/v1/numbers`, `/v1/sms/messages` | `404` (not routed) |
| Frontend Calling adapter | `https://www.cfmeet.com/api/calling/v1/calls` with a forged token | `401` (login verified before anything else) |

**Defect found and fixed in tooling: API Gateway CORS refuses `https://www.cfmeet.com`.** The gateway allowed only
`https://cfmeet.com` (`API_GATEWAY_CORS_ALLOWED_ORIGINS`), but the site is served from `www`. The preflight from `www` to
`/v1/developer-platform/context` answered `403 ORIGIN_NOT_ALLOWED`. Because the Developer Portal calls the gateway
from the browser, it would have failed even with its variables set. The Meeting Service already allows both.
Fix: `WEB_ORIGINS = (https://cfmeet.com, https://www.cfmeet.com)` and the activation `activate-web-origins`. That step
recreates only the API Gateway with both exact origins (never a wildcard), then checks that both pass and that a foreign
origin is still refused. The read-only certification now checks the `www` preflight.

## 4. Remaining blockers (external)

1. **Vercel project access.** One of these is needed:
   * the owner adds a Vercel token, scoped to the team that owns the project, as the `production` environment secret `VERCEL_TOKEN`, together with the team and project ids; or
   * the owner or David sets the variables in §6 in the Vercel dashboard (Production) and redeploys.

   Until then nobody outside the project can change or redeploy the frontend. **No deployment was made, and none is claimed.**
2. **The frontend server credential value.** It must be pasted into Vercel by the holder of the export key (§2), or re-exported.
3. **Running the gateway CORS fix in production.** The tooling change must land on `main` with green CI, and then
   `activate-web-origins` must be dispatched (`cfmeet-coordinated-release.yml`). Pushing to `main` from the
   engineering session was refused by its permission settings; the change is on branch `claude/sharp-dijkstra-qgnwtn`.
4. **End-to-end acceptance through the frontend** (two accounts: identity resolution, endpoint registration, voice and
   video, signaling, history, reconnection, participant authorization, Developer Console lists and refusals) can only
   run after items 1–3. Calling itself was certified directly against `api.cfmeet.com` by the existing communications
   acceptance. **The frontend path has not been certified.**

## 5. Acceptance to run after configuration

1. Sign in on `https://www.cfmeet.com` with two test accounts.
2. Calls page loads with no `CALLING_NOT_CONFIGURED`. A `503` naming the ids means the scope variables differ from
   `/api/v2/messaging/me`.
3. A calls B (voice, then video). The call connects, media flows, and both histories show the call. Reload during the
   call: it reconnects. A third account cannot control the call.
4. `/developers` loads and the CF Meet session is accepted. The organization and project lists load, and environments and
   credentials follow the account's permissions. A forged or expired session is refused. The browser network tab shows
   requests to `https://api.cfmeet.com/v1/developer-platform/...` only, with no `CALLING_*` value anywhere in the page.
5. Meetings and Messaging still work (join a meeting; send a message).

## 6. Final safe environment file (Vercel, Production)

```dotenv
# Public (compiled into the browser bundle) — safe values
NEXT_PUBLIC_API_URL=https://api.cfmeet.com/api/v1
NEXT_PUBLIC_CALLING_API_URL=https://api.cfmeet.com
NEXT_PUBLIC_MESSAGING_URL=https://api.cfmeet.com
NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED=true
NEXT_PUBLIC_DEVELOPER_API_BASE_URL=https://api.cfmeet.com

# Server only (never NEXT_PUBLIC_)
CALLING_API_URL=https://api.cfmeet.com
CALLING_DEV_MODE=false
CALLING_TENANT_ID=d92576b3-b0cb-4f53-921b-9cebc10b63c9
CALLING_APPLICATION_ID=b89bab3f-f0c1-4444-90de-9fa73be3a40a
CALLING_ENVIRONMENT_ID=8e1af752-3148-47c2-bc5f-80bd003afab6
CALLING_ADMIN_PRINCIPAL_IDS=
CALLING_PRIVACY_ADMIN_PRINCIPAL_IDS=

# Secret: value only in Vercel (Sensitive). Source: VPS /opt/cfmeet-core/secrets/cfmeet-web-bff.credential
CALLING_PLATFORM_CREDENTIAL=
```

`NEXT_PUBLIC_*` values take effect only after a **rebuild**: after any change, redeploy the Production deployment
(without the build cache).

**Confirmation status:** the server-only credentials are **not yet confirmed** as configured in Vercel. No one outside the
Vercel project can see them, and the authenticated check in §5 has not run.

# Unified Messaging v2 in the web app

The signed-in web client uses Meeting Service's `/api/v2/messaging` identity bridge.
It sends only the user's login bearer. The backend resolves the acting principal,
signs the delegated request, and returns Messaging's answer. Platform/service
credentials never enter this web integration.

## Configuration

Keep `NEXT_PUBLIC_API_URL=http://localhost:8080/api/v1` in `.env.local` for the existing
login/profile API. The Messaging client derives `/api/v2` on the same origin; it also
normalizes a configured `/api/v2` URL internally, but other existing app services still
expect the environment setting to be v1. No API version change is needed in `.env.local`.

`NEXT_PUBLIC_MESSAGING_URL` is the deployed Messaging realtime ingress, using HTTPS
(HTTP is allowed for loopback development). It is separate from the Meeting API.
Leaving it empty keeps HTTP polling available and shows live delivery as unavailable.
`NEXT_PUBLIC_MESSAGING_STORAGE_URL` permits the configured object-storage origin in CSP
for attachment uploads/downloads. Signed grants, rather than login or service tokens,
authorize those transfers.

Optional `NEXT_PUBLIC_MESSAGING_KT_LOG_ID` and `NEXT_PUBLIC_MESSAGING_KT_LOG_KEY` pin the
deployment's key-transparency log. Set them to the actual log ID and public verification
key expected by the existing crypto worker; these are public configuration, never
signing keys. The UI reports when no log is pinned. Device registration still requires
the existing installed/pinned crypto worker and backend E2EE configuration.

## Code layout

- `types/messaging.ts`: typed identity, directory, device, session and migration data.
- `services/v2/messaging-http.ts`: login-only transport and shared v1 token refresh.
- `services/v2/messaging.service.ts`: all listed identity/device/session endpoints plus
  the GET/POST/PUT/DELETE end-user relay.
- `services/v2/chat-migration.service.ts`: separate platform-admin status, transitions
  and parity-run endpoints; importing it performs no requests or transitions.
- `hooks/v2/useMessaging.ts`: TanStack identity/directory/device reads and revocation.
- `hooks/v2/useChatMigration.ts`: opt-in admin reads and explicit mutations, with no
  automatic mutation retries. Backend role checks, expectedVersion and approval gates
  remain authoritative. Rollback is an explicit transition to `SHADOW_VALIDATION`;
  retirement is irreversible and is never performed automatically.
- `lib/messaging/api.ts`: compatibility exports for the existing encryption engine.
- `lib/messaging/routes.ts`: the same end-user method/path/query allowlist as Meeting.

The nested version is intentional: `/api/v2/messaging/relay/v1/conversations/...`
wraps Messaging's v1 contract inside Meeting's v2 authenticated bridge. Do not rewrite
the inner `v1` to `v2`. Conversation, MLS, attachment and sealed-history operations
continue through this relay; admin/compliance/internal operations are refused.

The realtime engine obtains device-bound tokens through the same v2 service, validates
the token kind, device, session ID and expiry, and keeps tokens outside browser storage
and the query cache. The MLS worker continues to hold private keys locally.

## Devices and sessions

On Messages, **Manage messaging devices** lists the principal's devices and allows
revoking another active device with a user revocation reason. **Remove this device**
continues to revoke this browser and erase its keys/history through the crypto worker.

**End sessions on all devices** calls `POST /api/v2/messaging/client-sessions/revoke-all`
after confirmation and pauses this browser's realtime connection. It does not revoke
devices or erase encrypted history. Existing authenticated accounts can establish new
sessions; **Reconnect this browser** explicitly does so here. It is not an account-wide
login revocation or a substitute for revoking a compromised device.

Logout clears the Messaging and migration query caches along with the existing calling
cache. The backend remains responsible for durable Messaging-session revocation during
CF Meet logout.

## Checks

From `apps/web`:

```text
node --experimental-strip-types --test tests/messaging-v2.test.mjs tests/messaging-session.test.mjs tests/messaging-realtime.test.mjs tests/csp.test.mjs
npx tsc --noEmit
```

The v2 tests exercise the real transport/service modules with mocked HTTP responses,
compare every allowed route with the Rust backend route list, and verify session
revocation/reconnection without key erasure. They do not certify a live Messaging
deployment, an MLS exchange between two browsers, or execute an admin transition.

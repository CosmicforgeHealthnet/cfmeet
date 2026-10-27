# Calling integration

The Calls page is `/dashboard/calls`, linked from the desktop and mobile navigation.
It follows `types/calling`, `services/calling`, `hooks/calling`, `lib/calling`, and
`components/calling`. Styling uses the existing `globals.css` theme tokens, Tailwind
utilities, and shadcn Button, Input, Label, and Spinner components. No new CSS framework
or dependency is required by the integration.

## Configuration

Copy the Calling settings from `.env.example` into `.env.local`. Configure the
server-only `CALLING_PLATFORM_CREDENTIAL`, `CALLING_TENANT_ID`,
`CALLING_APPLICATION_ID`, and `CALLING_ENVIRONMENT_ID`. The credential needs
`calls.read`, `calls.write`, and `enterprise.calls.use` for enterprise extensions.
These must match the communication identity returned by Meeting Service's authenticated
`GET /api/v2/messaging/me`. This reuses the existing communication-principal mapping;
a meeting user ID must never be assumed to be a calling principal ID.

`CALLING_API_URL` is the server's Calling address. `NEXT_PUBLIC_CALLING_API_URL` is
the browser-reachable Calling origin, used for signaling WebSockets and
media gateway connections. Signaling HTTP uses `/api/calling-signaling`, forwarding
only the short-lived endpoint token, so it needs no cross-origin HTTP access to Calling.
The Calling ingress must accept browser origins and
WebSocket upgrades. Use HTTPS/WSS in production. Configure any additional media origin
in the existing `CSP_EXTRA_CONNECT_SRC` setting if the credential's URL uses another
origin. Calling, Platform Access, the Meeting identity bridge, and the configured media
provider must be running for live calls.

## Authentication and authorization

Meeting login tokens are sent only to the same-origin `/api/calling` adapter and the
Meeting identity endpoint. The adapter holds the Platform Access credential on the
server, verifies the communication scope, binds initiator/principal fields, forces
history to the signed-in principal, verifies device ownership, and checks call
participation and session ownership before forwarding. Call-wide controls are limited
to the initiator; other participants may leave their own sessions.

Only native client operations are allowed. Protected-contact/callback requests,
administrative enterprise configuration, Privacy Relay administration, provider
callbacks, internal routes, billing, and webhook administration are not exposed by
this end-user adapter. The request types describe those creation variants for contract
completeness; enabling them requires a separate account-level authorization policy.

Each mounted device client owns its signaling credential in memory. A stable endpoint
key, scoped to the principal, is the only calling value persisted in localStorage.
Enable this browser registers/refreshes the endpoint. The WebSocket uses the backend's
`cfmeet.signaling.v1, bearer.<credential>` subprotocol pair. It renews credentials,
reconnects with backoff, sends presence heartbeats, and recovers invitations over HTTP
after connection and periodically. History and details also refresh over HTTP.
Leaving the page closes signaling and disconnects media. Calls continue to be owned
by the backend; use End or Leave call to terminate participation explicitly.

Mutations preserve `expectedVersion` and use a new idempotency identity per user
command. A caller retrying an uncertain service request may pass the same explicit key.
TanStack mutations never retry automatically. Successes and failures invalidate calling
queries, including stale-version refusals. Media credentials stay outside the query
cache and browser storage.

## Verification

Run from `apps/web`:

```text
npx tsc --noEmit
npx eslint src/services/calling src/types/calling src/hooks/calling src/lib/calling src/components/calling src/app/api/calling src/app/dashboard/calls
node --test tests/calling-adapter.test.mjs
node --experimental-strip-types --test tests/csp.test.mjs
```

With the web dev server on `127.0.0.1:3100`, and Playwright plus Chromium (or Edge on Windows) installed,
`node tests/calling.browser.mjs` checks the mocked dashboard at desktop/mobile sizes.
It exercises empty history, the missed filter, disabled creation before registration,
and browser runtime errors. This behavior test bypasses CSP to avoid Next's development
debugging scripts conflicting with Trusted Types; production policy is checked separately
by `csp.test.mjs`. Screenshots are written into the ignored `.next` directory.
Live microphone/video and two-party calls require the
configured services and two signed-in accounts; mocks do not certify the media path.

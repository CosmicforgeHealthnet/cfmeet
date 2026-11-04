# Calling integration

The Calling workspace is mounted under `/calling` and linked from the dashboard
navigation. It follows `types/calling`, `services/calling`, `hooks/calling`,
`lib/calling`, and `components/calling`. Styling uses the existing `globals.css` theme
tokens and Tailwind utilities; no new CSS framework or dependency is required.

The browser routes are separate from the API paths:

| Route | Surface |
| --- | --- |
| `/calling` | Active calling dashboard and recent calls |
| `/calling/history` | Call history |
| `/calling/:callId` | Call details |
| `/calling/devices` | Browser and registered endpoints |
| `/calling/settings` | Caller blocking, forwarding, and voicemail settings |
| `/calling/privacy/contexts` | Communication contexts |
| `/calling/privacy/contexts/:id` | Context detail, lifecycle actions, evidence, and session creation |
| `/calling/privacy/sessions` | Privacy Relay sessions |
| `/calling/privacy/sessions/:id` | Session detail and evidence |
| `/calling/privacy/contacts` | Protected-contact creation and lookup |
| `/calling/enterprise` | Enterprise calling dashboard |
| `/calling/enterprise/extensions` | Extension directory and provisioning |
| `/calling/enterprise/departments` | Departments and membership |
| `/calling/enterprise/ring-groups` | Ring groups and members |
| `/calling/enterprise/queues` | Queues, agents, and live entries |
| `/calling/enterprise/ivr` | IVR flows, versions, and activation |
| `/calling/enterprise/business-hours` | Schedules and evaluation |
| `/calling/enterprise/forwarding` | Enterprise forwarding rules |
| `/calling/enterprise/voicemail` | Mailboxes and inbox |
| `/calling/enterprise/voicemail/mailboxes` | Mailbox provisioning and details |
| `/calling/enterprise/voicemail/messages` | Mailbox message listing and actions |
| `/calling/enterprise/recordings` | Recording policies and protected artifacts |
| `/calling/enterprise/recordings/lookup` | Lookup a protected recording by ID |
| `/calling/enterprise/sip-devices` | SIP device bindings |
| `/calling/enterprise/public-contacts` | Protected enterprise destinations |
| `/calling/enterprise/policies` | Managed and effective policies |
| `/calling/enterprise/audit` | Authorized audit records |
| `/calling/developers/webhooks` | Webhook subscriptions and deliveries |
| `/calling/usage` | Calling usage and limits |
| `/calling/support` | Safe support guidance and authorized audit links |

Enterprise resource screens link to the public API contract and accept request JSON
matching the operation schema. Mutations use fresh idempotency keys. The UI masks
credential, secret, contact-phone, and signed-media-URL fields in returned resource
data. The backend remains authoritative for tenant scope and operation permissions.
Protected contacts and recording artifacts are looked up by opaque ID because their
APIs do not provide tenant-wide listing operations. Voicemail message listing requires
a mailbox ID.

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

The same-origin adapter has a method-and-path allowlist for the public Calling and
Privacy Relay contracts. Enterprise, webhook, and Calling usage administration
additionally requires the signed-in Calling principal to appear in the server-only
`CALLING_ADMIN_PRINCIPAL_IDS`; Privacy Relay administration uses the separate
`CALLING_PRIVACY_ADMIN_PRINCIPAL_IDS`. Both are comma-separated, deny-by-default
allowlists. API credential scopes and backend resource authorization remain required;
the browser cannot supply or widen them. Block listing and creation are bound to the
signed-in principal, and releasing a block requires verifying it belongs to that
principal. Blocking accepts a principal ID or opaque protected-contact reference, not
a phone number. Internal routes, provider callbacks, and unrelated billing APIs remain
unavailable. Signaling credentials remain endpoint-scoped and are never accepted by
the platform API adapter.

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

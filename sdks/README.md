# sdks/ — CF Meet Platform SDKs

The SDKs partners use to call the CF Meet Platform (meetings, messaging, calling and the
Developer Console) from their backends, and to connect their end users' devices with the
short-lived credentials their backend obtains
([ADR 0061](../docs/decisions/0061-cf-meet-platform-partner-embedding.md)). Not published yet:
publishing waits for an owner decision (registry, publisher, licence, version policy).

| SDK | Path | Package | Status |
|---|---|---|---|
| TypeScript | [`typescript/`](typescript) | `@cfmeet/platform-sdk` 0.1.0 (private) | beta |
| Dart | [`dart/`](dart) | `cfmeet_platform_sdk` 0.1.0 (`publish_to: none`) | beta |
| React Native | [`react-native/`](react-native) | `@cfmeet/platform-sdk-react-native` 0.1.0 (private) | beta (hand-written over the TypeScript SDK) |

Both SDKs are generated from the **published** contracts listed in
[`generation.json`](generation.json) (registered in `packages/contracts/manifest.json`)
by [`generate.py`](generate.py) — one deterministic generator, standard library only:

```bash
python3 sdks/generate.py          # regenerate src/generated / lib/src/generated
python3 sdks/generate.py --check  # CI: generated files current, public contracts only, no product names
make sdks-check
```

Every partner uses the same SDKs and the same published API contracts; partners differ
only through the scopes Platform Access grants and the tenant's trust classification.
There is no partner-specific SDK or endpoint. The SDKs cover only the operations the API
Gateway routes: the CF Meet app's own first-party surfaces (for example the Communications
Hub, `FIRST_PARTY_COMMUNICATIONS_HUB`) are not part of them.

## TypeScript (`sdks/typescript`, beta)

ESM, **no runtime dependencies** (global `fetch`, `WebSocket`, `AbortController`, Web
Crypto); Node >= 22 and modern browsers. Dev dependencies: `typescript`, `@types/node`
(tests and server examples only). CI:
[`.github/workflows/sdk-typescript.yml`](../.github/workflows/sdk-typescript.yml).

```ts
import { CfMeetClient, UnknownOutcomeError } from '@cfmeet/platform-sdk';

// Server-side only: a platform credential is a machine credential.
const cfmeet = new CfMeetClient({
  baseUrl: 'https://api.cfmeet.com',
  auth: { type: 'platformCredential', credential: process.env.CFMEET_API_CREDENTIAL! },
});

const call = await cfmeet.calling.getCall({ path: { call_id: callId } }); // ApiResponse<Calling.Call>
call.data.status; call.requestId; call.quota?.remainingThisMinute;
for await (const m of cfmeet.sms.listMessages.all({ query: { limit: 100 } })) { /* every page */ }
try {
  await cfmeet.sms.sendMessage({ body: { from: { numberResourceId }, to, text }, idempotencyKey });
} catch (e) {
  if (e instanceof UnknownOutcomeError) { /* reconcile, or replay with e.idempotencyKey */ }
}
```

* **Generated types** (`src/generated/models.ts`, by `generate.py`): a namespace per
  domain (`Calling`, `Meetings`, `Messaging`, `Numbering`, `PrivacyRelay`, `Sms`,
  `DeveloperPlatform`, `Gateway`) with a type per component schema, and `Operations` —
  the typed `params` (path / query / documented headers / body / `idempotencyKey`) and
  `response` (plus the page `item`) of every REST operation. The accessors
  `client.<domain>.<operation>(params, options)` are built from `OPERATIONS` and typed by
  them (missing path parameters, bodies or a `oneOf` branch are compile errors);
  `ParamsOf<'sms', 'sendMessage'>` / `ResponseOf<…>` name them. `client.discovery()`
  reads `GET /discovery`; `client.request(spec, params)` is the untyped core.
* **Requests:** path parameters percent-encoded (`/`, `\`, `..`, control characters
  refused); only the operation's documented query parameters and headers (plus the
  gateway-forwarded `if-match`, `if-none-match`, `accept-language`, `range` through
  `options.headers`); JSON bodies. No client request id is sent (the gateway always
  generates its own): `ApiResponse` carries `data`, `status`, `requestId`
  (`x-request-id`), `correlationId`, `idempotencyKey`, `idempotentReplayed`, `quota`
  (`x-cf-quota-*`) and `attempts`.
* **Authentication:** `{ type: 'platformCredential', credential }` (Bearer `cfmk_…`),
  `'userSession'` (Bearer developer / meeting session), `'participantToken'`
  (`X-Participant-Token`), `'clientToken'` (`Authorization: CF-Client-Token …`),
  `'signalingCredential'` (Bearer `cfsig_…`); each a string or a provider function (asked
  again per request / reconnect). `{ type: 'userToken', tokenProvider }` (Bearer
  `cfut_…`, ADR 0062, device side): the token is cached, the provider is asked again ~60 s
  before the JWS `exp` (decoded, never verified, by the SDK) and after a 401
  `USER_TOKEN_EXPIRED` / `USER_TOKEN_REVOKED` / `USER_TOKEN_INVALID` /
  `AUTHENTICATION_FAILED` / `AUTHENTICATION_REQUIRED`, and the request is retried once. An operation whose auth modes exclude the configured
  type is refused before sending (override per call with `options.auth`); anonymous
  operations never carry a credential.
* **Browser safety:** a `cfmk_` credential is **refused** when `window` and `document`
  exist unless `dangerouslyAllowPlatformCredentialInBrowserLikeRuntime: true` (trusted
  server-side runtimes only) — also when it arrives disguised as another auth type or
  from a provider. `http://` only for localhost (or `allowInsecureHttp`); redirects are
  not followed; cookies are not sent. CI checks the built bundle for literal credentials
  / secrets, `process.env` reads and Node built-in imports (`npm run check:bundle`).
* **Errors:** `CfMeetApiError` (`kind`, `status`, `code`, `requestId`, `retryAfterMs`,
  `quota`, `operation`) with `CfMeetAuthenticationError` 401, `CfMeetPermissionError` 403,
  `CfMeetNotFoundError` 404, `CfMeetValidationError` 400/413/414/422/431,
  `CfMeetConflictError` 409/412, `CfMeetRateLimitedError` / `CfMeetQuotaExceededError`
  429, `CfMeetUpstreamUnavailableError` 502/503/504, `CfMeetServerError`;
  `CfMeetNetworkError`, `CfMeetTimeoutError`, `CfMeetAbortError`,
  `CfMeetConfigurationError`; **`UnknownOutcomeError`** (`reason`, the `idempotencyKey`
  sent) for a NEVER operation after a timeout, connection loss, abort, 500, 502 or 504.
* **Retries:** exponential backoff with jitter, `Retry-After` (seconds or HTTP date)
  honoured up to 30 s (a longer one is thrown at once). SAFE / IDEMPOTENT: network
  errors, timeouts, 408 / 429 / 502 / 503 / 504, IDEMPOTENT with the same
  Idempotency-Key; NO_RETRY: 429 only; NEVER (paid telecom): no automatic retry at all.
  A key is generated (`crypto.randomUUID`) for every operation that takes one unless you
  pass `idempotencyKey`. `timeoutMs` per attempt (default 30 s) and a caller
  `AbortSignal`.
* **Pagination:** `.all()` (items) / `.pages()` (responses) on every cursor-paginated
  operation follow `nextCursor`; a repeated cursor stops with an error.
* **Realtime** (`client.realtime`): `.messaging()` (ticket from `POST /v1/realtime/tickets`
  as subprotocol `cfmeet.ticket.<ticket>`), `.meetings(conferenceId)` (ticket in the
  query, as the contract requires; reconnects resume after the last event `sequence`),
  `.callingSignaling({ credential })` (subprotocol `bearer.<cfsig>`; close 4001
  reconnects with a fresh credential from a provider). A fresh ticket per (re)connect,
  backoff with jitter; stops on `close()`, a refused ticket (400 / 401 / 403 / 404) and
  terminal close codes (1000, 1008, 4000–4099, 4400–4499).
* **Webhooks:** `verifyWebhookSignature({ secret, headers, rawBody })` →
  `{ valid: true, deliveryId, eventId, timestamp }` or `{ valid: false, reason }`
  (`MISMATCH` / `STALE_TIMESTAMP` / `MALFORMED`); `constructEvent` verifies, then parses.
  Tested against every vector of
  [`testdata/webhook-signature-vectors.json`](testdata/webhook-signature-vectors.json).
* **Messaging E2EE:** no cryptography in the SDK. `loadMessagingCrypto({ module })` or
  `loadMessagingCrypto({ glue, wasm: { url | bytes }, integrity: 'sha384-…' })` wraps the
  application-provided wasm-bindgen build of
  [`packages/messaging-crypto/wasm`](../packages/messaging-crypto/wasm) (the .wasm is
  verified against its SRI — e.g. apps/web's `manifest.json` — before init) as a typed
  `MessagingCrypto` (AAD, KeyPackages, devices and MLS groups, attachments,
  key-transparency proofs, history transfer) with stable error codes. In browsers run it
  in a dedicated Worker; encrypt exported device state before storing it.

### Testing your integration (TypeScript)

`@cfmeet/platform-sdk/testing` (a separate subpath export, side-effect free, zero
dependencies; never reachable from the main entry — `npm run check:bundle` verifies it)
runs YOUR tests against the real client without a network, live CF Meet services, telecom
providers or real time passing:

```ts
import { CfMeetClient } from '@cfmeet/platform-sdk';
import { FakeResponse, createFakeClock, createFakeFetch, createFakeWebSocket } from '@cfmeet/platform-sdk/testing';

const fake = createFakeFetch();   // deterministic request ids req_fake_000001, ...
const clock = createFakeClock();  // backoff / Retry-After recorded in clock.sleeps, not slept
const client = new CfMeetClient({ baseUrl: 'https://api.cfmeet.test', auth, fetch: fake.fetch, retry: { sleep: clock.sleep } });

fake.on('sms', 'sendMessage', FakeResponse.error(502));        // names checked (compile + run time)
fake.on('calling', 'holdCall', FakeResponse.error(503)).thenReply(FakeResponse.networkError(), FakeResponse.json({ id }));
fake.onPages('sms', 'listMessages', [[m1, m2], [m3]]);          // follows the operation's pagination spec
fake.onRequest('GET', '/v1/sms/messages/{messageId}', FakeResponse.json(msg, { quota: { remainingThisMinute: 0 } }));
fake.requests[0].idempotencyKey; fake.requestsFor('sms', 'sendMessage'); fake.unmatched; // default: 404 NOT_FOUND envelope

const sockets = createFakeWebSocket();
const conn = client.realtime.messaging({ auth, WebSocket: sockets.WebSocket, reconnect: { sleep: clock.sleep } });
const s1 = await sockets.connection(1);  // s1.ticketSubprotocol, s1.queryTicket, s1.bearer, s1.sentJson()
s1.message({ type: 'message.created' }); s1.serverClose(1011);  // → reconnect with a fresh ticket
```

Replies: `FakeResponse.json / empty / bytes / error(status, { code, retryAfter, quota }) /
page / networkError({ requestSent }) / timeout() / hang()`, with `requestId`,
`correlationId`, `idempotentReplayed` and `quota` headers; a route's replies are used in
order and the last one repeats (`thenReplyWith(req => …)` computes one). Sockets:
`refuseNext(code)`, `drop()`, `autoOpen: false` + `open()`. Helpers: `fakeErrorEnvelope`,
`fakePageBody`, `fakeQuotaHeaders`. Covered by `test/testing.test.ts`.

```bash
cd sdks/typescript
npm ci
npm run generate:check && npm run typecheck && npm run build && npm test
npm run check:bundle && npm run typecheck:examples
npm run build:wasm-test-module   # optional: fresh wasm-bindgen build for the crypto adapter test
CFMEET_STACK_ENV=/path/stack.json npm test   # + the real-stack suite (SKIPPED otherwise)
```

Examples (type-checked in CI): [`examples/`](typescript/examples) — server-side SMS with
idempotency, conversation pagination, a Node `http` webhook receiver, browser usage with
a user session, partner users / tokens / server-placed calls (`server-partner-users.ts`)
and a partner device web app (`device-web.ts`).

## CF Meet Platform "Build": partner users on their own devices (ADR 0062 / 0063)

Partners put CF Meet chat, calls and meetings in their own apps for their own users, who
have no CF Meet account. The partner's **server** creates a CF Meet user per app user and
mints a short-lived **user token** (`cfut_…`); the **device** SDK exchanges it for the
credentials each product already uses (chat client token, call signaling credential,
meeting participant token + RTC credential) and keeps them fresh. The platform credential
(`cfmk_…`) never leaves the server.

| Surface | TypeScript | Dart (Flutter) | Route |
|---|---|---|---|
| Users (server) | `client.users.create / upsert / get / list / all / findByExternalId / update / delete / revokeSessions` | `client.users.*` | `/v1/users*` |
| User tokens (server) | `client.users.createToken / upsertWithToken` | same | `POST /v1/users/{id}/tokens`, `POST /v1/users/tokens` |
| Device client | `new CfMeetDeviceClient({ baseUrl, tokenProvider, storage })` | `CfMeetDeviceClient(baseUrl:, tokenProvider:, storage:)` | — |
| Chat | `device.chat.connect()` → `ChatSession` (`sendMessage`, `listMessages`, `listConversations`, `markRead`, `getUnreadCount`, `request(name, …)`, `on('message')`) | `ChatSession` (`messages` stream, same methods) | `POST /v1/messaging/user-sessions` → `POST /v1/realtime/tickets` → `/v1/realtime` |
| Calls | `device.calls.connect({ kind })` → `CallSession` (`on('invitation')`, `acknowledge`, `accept`, `decline`, `mediaCredential`, `recoverInvitations`) | `DeviceCallSession` (`invitations` stream) | `POST /v1/calling/user-sessions` → `/v1/calling/signaling` |
| Meetings | `device.meetings.join(conferenceId)` → `MeetingJoin` (`rtc`, `participantToken`, `renewMediaCredential`, `realtime`) | same | `POST /api/v2/conferences/{id}/join` |
| Push | `device.push.register({ platform: 'APNS' \| 'APNS_VOIP' \| 'FCM', handle })`, `unregister` | `PushPlatform.*` | `/v1/messaging/user-push-registrations/{installationId}` |
| Installation id | `InstallationIdStore` over `localStorage` / AsyncStorage (`getItem` / `setItem`), default in memory | `KeyValueStorage` (e.g. a `shared_preferences` adapter) | `[A-Za-z0-9._-]{8,128}` |

The users helpers and device clients send hand-written operation specs (`USER_ROUTES`,
`DEVICE_ROUTES` / `UserRoutes`, `DeviceRoutes`) through `client.request`, so they work
before the generated catalogue includes these routes; after regeneration the same
operations also appear as `client.developerPlatform.createUser` & co. Derived credentials
are renewed before expiry (`renewBeforeExpirySeconds`, default 60, at most half their
lifetime; `autoRenew` in the background) and after a 401; a renewal whose session device
ended (404) starts a new one; realtime channels reconnect with backoff, and a refused
ticket / signaling close 4001 triggers one fresh exchange. Each feature is off until its
service flag is on (see the build contract).

### Server quickstart (Node)

```ts
const cfmeet = new CfMeetClient({ baseUrl: 'https://api.cfmeet.com', auth: { type: 'platformCredential', credential: process.env.CFMEET_API_CREDENTIAL! } });

// GET /my-api/cfmeet-token, behind YOUR login: create-or-update the user and mint a token.
const { token, user } = await cfmeet.users.upsertWithToken({
  externalId: me.id, // your opaque id — never a phone number or SIP address
  displayName: me.name,
  grants: { msg: ['read', 'send'], call: ['receive'] }, // + mtg: [{ conferenceId, role: 'PARTICIPANT' }]
  ttlSeconds: 3600,
});
// Signed out everywhere / banned:
await cfmeet.users.revokeSessions(user.id);            // or update(user.id, { status: 'SUSPENDED' })
```

The key needs `principals.write` + `user_tokens.issue` (and `messages.*`, `calls.write`,
`meetings.write` for the grants it mints). Token mints are not retried automatically:
an Idempotency-Key replay returns the metadata without the token (`tokenRedacted`).

### Device quickstart (web)

```ts
const device = new CfMeetDeviceClient({
  baseUrl: 'https://api.cfmeet.com',
  tokenProvider: async () => (await (await fetch('/my-api/cfmeet-token')).json()).token,
  storage: browserLocalStorage() ?? undefined,
});
const chat = await device.chat.connect<{ type: string }>();
chat.on('message', ({ data }) => render(data));
await chat.sendMessage(conversationId, 'On my way');

const calls = await device.calls.connect({ kind: 'WEB' });
calls.on('invitation', async ({ invitation }) => {
  await calls.acknowledge(invitation.invitationId);
  const { data } = await calls.accept(invitation);           // or calls.decline(invitation)
  const media = await calls.mediaCredential(data.sessionId); // media gateway url + token
});

const meeting = await device.meetings.join(conferenceId);
// LiveKit's client SDK (your dependency): await room.connect(meeting.rtc!.url, meeting.rtc!.credential)
```

The page's origin must be an allowed website of the environment (Developer Console).
Full examples: [`typescript/examples/server-partner-users.ts`](typescript/examples/server-partner-users.ts),
[`typescript/examples/device-web.ts`](typescript/examples/device-web.ts).

### Flutter quickstart

```dart
final device = CfMeetDeviceClient(
  baseUrl: 'https://api.cfmeet.com',
  tokenProvider: () => myApi.cfmeetToken(),   // your server mints it
  storage: PrefsStorage(prefs),               // implements KeyValueStorage
);
final chat = await device.chat.connect();
chat.messages.listen(render);
await chat.sendMessage(conversationId, 'On my way');

final calls = await device.calls.connect(kind: DeviceKind.mobile);
calls.invitations.listen((inv) async {
  final accepted = await calls.accept(inv.invitationId);
  final media = await calls.mediaCredential(accepted.json['sessionId'] as String);
});
final meeting = await device.meetings.join(conferenceId);  // meeting.rtc!.url / .credential → livekit_client
await device.push.register(platform: PushPlatform.apnsVoip, handle: voipToken);
```

CallKit / ConnectionService and PushKit / FCM glue is the app's (see the React Native
README for what it must do; the same rules apply to Flutter plugins).

### React Native quickstart

See [`react-native/README.md`](react-native/README.md): `createDeviceClient({ …, storage:
AsyncStorage })`, the hooks `useCfMeetChat` / `useCfMeetCall` / `useCfMeetMeeting`, and
the `NativeCallBridge` your native module implements for push tokens and the system call
UI (`connectNativeCalls`).

### Example: KarryGo (a rider calls the customer; chat is the fallback)

1. **Sign-in** — both apps call KarryGo's `/cfmeet-token`; KarryGo's server runs
   `users.upsertWithToken({ externalId, displayName, grants: { msg: ['read','send'], call: ['receive'] } })`
   and stores the returned `user.id` (the CF Meet principal) with its own user.
2. **Devices** — each app runs `device.calls.connect({ kind: 'MOBILE' })` and registers
   its push tokens (`APNS_VOIP` / `FCM`); the customer's phone rings even when the app is
   closed.
3. **Call** — the rider taps "Call customer": the rider app sends its `calls.endpointId`
   to KarryGo; KarryGo checks the delivery is active and the caller is its rider, then
   places the call server-side: `calling.createCall({ body: { kind: 'VOICE',
   initiatorPrincipalId: rider, initiatorEndpointId, targetPrincipalIds: [customer] },
   idempotencyKey: delivery.callKey })` (paid class: never auto-retried; replay with the
   same key on an `UnknownOutcomeError`).
4. **Answer** — the customer's app gets `call.incoming` (or the VoIP push → CallKit),
   `accept`s and connects media with `mediaCredential(sessionId)`.
5. **Fallback** — no live invitation (`liveInvitationCount === 0`), declined or no
   answer: KarryGo creates the delivery chat (`messaging.createConversation`, MANAGED,
   `contextClass: 'TRANSACTION'`) and both apps use `device.chat` with push for new
   messages. When the delivery ends KarryGo stops placing calls; tokens expire within the
   hour, and `revokeSessions` ends them at once.

## Dart (`sdks/dart`, beta)

Dart 3.4+; runtime dependencies `crypto` (webhook HMAC) and `ffi` (native bridge) only.
CI: [`.github/workflows/sdk-dart.yml`](../.github/workflows/sdk-dart.yml).

```dart
import 'package:cfmeet_platform_sdk/cfmeet_platform_sdk.dart';

final client = CfMeetClient(
  baseUrl: 'https://api.cfmeet.com',
  auth: CfMeetAuth.platformCredential(Platform.environment['CFMEET_CREDENTIAL']!),
);
final sent = await client.sms.call('sendMessage', body: {...}, idempotencyKey: orderKey);
await for (final m in client.sms.paginate('listMessages', query: {'limit': 50})) { ... }
```

* **Every operation** of the generated catalogue (`lib/src/generated/operations.dart`):
  `client.operation(domain, name)`, `client.domain('sms').call(name, ...)` or the domain
  getters (`client.calling`, `.messaging`, `.meetings`, `.numbering`, `.sms`,
  `.privacyRelay`, `.developerPlatform`, `.gateway`); names are checked (with suggestions).
  `client.discovery()` reads `GET /discovery`.
* **Requests:** path parameters percent-encoded (`/`, `\`, `..`, control characters
  refused), only documented query parameters, only documented / gateway-forwarded headers
  (`if-match`, `if-none-match`, `accept-language`, `range`), JSON or `RawBody` bodies.
  `ApiResponse` carries `status`, decoded `body`, `requestId`, `correlationId`,
  `idempotentReplayed`, `quota` (`x-cf-quota-*`), the `idempotencyKey` and `attempts`.
* **Generated models** (`lib/models/<domain>.dart`, by `generate.py`): one library per
  public domain, imported with a prefix because schema names repeat across domains.
  Each object schema is a read-only view over the decoded JSON: typed getters (required,
  non-null properties are non-nullable), `toJson()` returning the original map (fields
  this SDK version does not know are kept) and `create(...)` with typed named parameters
  for request bodies; enums and other schemas are typedefs of their Dart type, unions
  stay `Object?`.

  ```dart
  import 'package:cfmeet_platform_sdk/models/meetings.dart' as meetings;

  final res = await client.meetings.call('v2MyConferences');
  final page = meetings.AgendaResponse.fromJson(res.body! as Map<String, Object?>);
  for (final item in page.items) print('${item.scheduledStartAt} ${item.meetingSpace.meetingCode}');
  ```
* **Authentication:** `CfMeetAuth.platformCredential` (Bearer `cfmk_...`, server-side
  only: refused in browser builds unless `allowPlatformCredentialInBrowser`),
  `developerSession` / `meetingSession` (Bearer), `participantToken`
  (`X-Participant-Token`), `clientToken` (`Authorization: CF-Client-Token`),
  `signalingCredential`, `anonymous`, or `CfMeetAuth.provider(mode, () async => token)`
  (asked again on every attempt). A mode the operation does not accept is refused before
  anything is sent; anonymous operations never receive the credential. `http://` base
  URLs only for loopback or with `allowInsecureHttp`.
* **Errors:** `CfMeetApiError` (`status`, `code`, `message`, `requestId`, `retryAfter`,
  `quota`) with `CfMeetAuthenticationError`, `CfMeetPermissionError`,
  `CfMeetNotFoundError`, `CfMeetValidationError`, `CfMeetConflictError`,
  `CfMeetRateLimitedError` / `CfMeetQuotaExceededError`, `CfMeetUpstreamUnavailableError`;
  `CfMeetNetworkError`, `CfMeetTimeoutError`, `CfMeetCancelledError`, `CfMeetUsageError`;
  `UnknownOutcomeError` (with the `idempotencyKey`) when a NEVER-class operation timed out,
  lost its connection after sending, or answered 500 / 502 / 504.
* **Retries** (backoff with full jitter, `Retry-After` seconds / HTTP date honored up to
  `maxRetryAfter`): SAFE and IDEMPOTENT on network errors, timeouts, 408 / 429 / 502 /
  503 / 504 (IDEMPOTENT with the SAME Idempotency-Key); NO_RETRY only on 429; NEVER (paid
  telecom, `generation.json`) never. Idempotency keys are random UUID v4
  (`Random.secure()`) when the caller gives none. Timeouts per attempt; `CancelToken`
  cancels requests, retry waits, pagination and realtime connections.
* **Realtime** (`RealtimeClient`): Messaging (`/v1/realtime`, ticket from
  `POST /v1/realtime/tickets` as subprotocol `cfmeet.ticket.<ticket>` next to
  `cfmeet.messaging.v1`), Meeting events (`/api/v2/conferences/{id}/realtime?ticket=`,
  resuming after the last `sequence`) and Calling signaling (`/v1/calling/signaling`,
  `Authorization: Bearer cfsig_...`, subprotocol `cfmeet.signaling.v1`). Reconnects with
  backoff and a fresh ticket / credential per connection; stops on explicit close, HTTP
  401 / 403 at the upgrade or ticket mint, and close codes 4001 / 4401 / 4403.
* **Webhooks:** `verifyWebhookSignature` / `constructEvent` (raw body, 300 s tolerance,
  constant-time comparison), tested against every vector of
  [`testdata/webhook-signature-vectors.json`](testdata/webhook-signature-vectors.json).
* **Messaging E2EE** (`MessagingCrypto`, native platforms): `dart:ffi` bindings of the C
  ABI of [`packages/messaging-crypto/ffi`](../packages/messaging-crypto/ffi) (the wasm
  binding's surface, function for function): devices, KeyPackages, MLS groups,
  encrypt / process with AAD binding, attachments, key-transparency proofs, history
  transfer. No cryptography in Dart; the only secret leaving native memory is the
  device state (`exportState`), to be stored encrypted under a platform key. Library:
  `MessagingCrypto.open(path: ...)` or the platform default name
  (`libcf_messaging_crypto_ffi.so` / `.dylib`, `cf_messaging_crypto_ffi.dll`).
* **Browsers:** the library compiles with dart2js (CI proves it, and that a browser
  build refuses platform credentials); pass a `transport:` / `connector:` there (no
  `dart:io`), and no `MessagingCrypto` (no `dart:ffi`).

### Testing your integration (Dart)

`package:cfmeet_platform_sdk/testing.dart` (not exported by the main library; no
`dart:io`) runs YOUR tests against the real `CfMeetClient` / `RealtimeClient` without a
network, live CF Meet services, telecom providers or real time passing:

```dart
import 'package:cfmeet_platform_sdk/testing.dart';

final fake = FakeCfMeetTransport();  // deterministic request ids req_fake_000001, ...
final clock = FakeClock();           // backoff / Retry-After recorded in clock.sleeps, not slept
final client = CfMeetClient(baseUrl: 'https://api.cfmeet.test', auth: auth,
    transport: fake, retryOptions: clock.retryOptions());

fake.onOperation('sms', 'sendMessage', const FakeResponse.error(502)); // names checked
fake.onOperation('calling', 'holdCall', const FakeResponse.error(503))
    .thenReply(const FakeResponse.networkError())
    .thenReply(const FakeResponse.json({'id': 'c1'}, idempotentReplayed: true));
fake.onPages('sms', 'listMessages', [[m1, m2], [m3]]); // the operation's pagination spec
fake.requests.first.idempotencyKey; fake.requestsFor('sms', 'sendMessage'); fake.unmatched;

final sockets = FakeRealtimeConnector();
final realtime = RealtimeClient(client,
    connector: sockets.connector, options: clock.reconnectOptions());
final conn = realtime.messaging(auth: CfMeetAuth.clientToken(token));
final s1 = await sockets.connection(1); // s1.ticketSubprotocol, queryTicket, authorization, sentJson
s1.pushJson({'type': 'message.created'});
s1.closeFromServer(1011);               // → reconnect with a fresh ticket
```

Replies: `FakeResponse(...)` / `.json` / `.bytes` / `.error(status, code:, retryAfter:,
quota:)` / `.page` / `.networkError(requestSent:)` / `.timeout()`, with `requestId`,
`correlationId`, `idempotentReplayed` and `quota` headers; unregistered calls get the 404
`NOT_FOUND` envelope; a route's replies are used in order and the last one repeats
(`thenReplyWith((req) => ...)` computes one). Sockets: `refuseNext(status:)`,
`failNext()`, `failWithError()`. `RetryOptions.sleeper` / `ReconnectOptions.sleeper` are
the injection points `FakeClock` uses (default: real timers). Covered by
`test/testing_fakes_test.dart`.

```bash
cd packages/messaging-crypto/ffi && cargo build          # the native library
cd sdks/dart && dart pub get && dart analyze --fatal-infos && dart test
CFMEET_STACK_ENV=/path/stack.json dart test test/stack_integration_test.dart  # real stack
```

`lib/src/generated/` is not dart-formatted (it is written byte-for-byte by
`generate.py`); format checks cover the hand-written files.

## Release (Part VI §21.2 `SDK_PUBLICATION_ENABLED`)

`.github/workflows/sdk-release.yml` is manual only. Every run builds and verifies the
npm tarball and the Dart archive and uploads them; publication runs only when the
`SDK_PUBLICATION_ENABLED` variable is `true`, the run asked for it, the `sdk-release`
environment approves, and the manifests are made publishable (both are private today;
the Dart package also needs a LICENSE — a licensing decision pending before any public
release). Local generation never needs registry credentials.


### Local MLS runtime diagnostics and in-flight history window

Message processing failures now expose allowlisted categories such as
`HISTORICAL_EPOCH_UNAVAILABLE`, `EPOCH_MISMATCH`, `RATCHET_STATE_UNAVAILABLE`,
`RATCHET_GENERATION_UNAVAILABLE`, `REPLAY_REJECTED`, `GROUP_ID_MISMATCH`,
`MEMBERSHIP_UNKNOWN`, `GROUP_STATE_REFUSED`, `COMMIT_REFUSED`,
`MESSAGE_AUTHENTICATION_FAILED`, `MALFORMED_MLS_MESSAGE`, `CRYPTO_STORAGE_ERROR`,
and `AAD_MISMATCH`. Generic `MESSAGE_REFUSED` remains a fallback. These are local
crypto errors, not new HTTP/OpenAPI responses. Unknown codes must remain safe
refusals; do not log raw cryptographic errors or infer device non-membership from a
lower epoch. Native/wasm processing restores state after failed validation, so
correcting an untrusted delivery envelope does not consume a receive generation.

The candidate shared core retains at most two previous MLS epochs for at most
24 hours, pruned on load/use and SDK state-export checkpoints. Consumed generation
keys remain replay-protected. Newly joining devices do not receive earlier epoch
secrets. Offline files or snapshots cannot be remotely erased while a client is
not running; age enforcement occurs before runtime use, and old captured snapshots
remain a compromise risk. History beyond that window requires authorized encrypted
record transfer/backup, not unlimited MLS secret retention. This is candidate
behavior requiring release review, not a statement about deployed mobile clients.

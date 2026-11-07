# CF Meet unified Communications — frontend and operations handoff

For frontend, mobile and operations engineers building the five areas (Chats, Calls,
Contacts, Meetings, Settings) on the existing backend. Contract version
`cfmeet.communications.http-api` **1.2.0** (Section 14 additions are marked "1.2.0"); Swagger at `/api-docs/communications.json`.
Generated SDK accessors: TypeScript `client.communications.<operation>()`, Dart
`client.communications`. Production availability is listed in
[the delivery record](../operations/communications-sections-9-23-delivery.md); a
documented operation is not proof that it is deployed.

## 1. Who owns what

| Area | Owner | You call |
|---|---|---|
| Account login, sessions | Account (meeting-service) | existing `/api/v1/auth/*`, bearer session |
| CF Meet ID, QR, contacts, requests, privacy, blocks | Communications Hub | `/api/v2/communications/*` |
| Conversations, messages, groups, reactions, pins, read cursors, attachments, search, push registration, devices/MLS | Messaging | `/api/v2/messaging/*` (see [native API map](../operations/messaging-native-api-map.md)) |
| Calls, ringing, answer/decline, signaling, media, recording, voicemail | Calling | Calling endpoints and signaling WebSocket |
| Meetings, guests, admission, meeting chat | Meetings | existing `/api/v2/*` meeting APIs |
| Facades: call launch, timeline cards, chat controls, collections, device registry, meeting links, sync | Communications Hub | `/api/v2/communications/*` (this document) |

Never send service credentials, principal IDs or `CF-Acting-Principal` from a client.
The Account session is the only credential the app holds for these APIs.

## 2. Endpoint classes

| Class | Examples | Credential |
|---|---|---|
| Consumer public REST | everything under `/api/v2/communications`, `/api/v2/messaging` | Account bearer session |
| Consumer delegated-device | Messaging client tokens (`/api/v2/messaging/client-tokens`, E2EE device), MANAGED session-device tokens (`/api/v2/messaging/session-tokens`, no E2EE device; ADR 0049 amendment A1), Calling signaling credentials | short-lived device token issued after enrollment (session-device tokens: bound to the account session, realtime only) |
| Authenticated WebSocket | `wss://api.cfmeet.com/v1/realtime` (Messaging; `POST /v1/realtime/tickets` first). Routed by the realtime ingress (`infra/calling/ingress-ci/realtime_ingress.py`), not the API Gateway catalogue (`public-routes.json` lists HTTP operations only). Calling signaling | one-use ticket / signaling credential |
| Internal service-to-service | `/internal/v1/communications/*`, owner `/internal/v1/...` | signed service tokens; never reachable publicly |
| Operator-only | Hub `admin/*`, `reconciliation/range`, `readiness` | operator console key; never in apps |
| Webhook / worker-only | owner outboxes, Calling/Messaging workers | none for clients |
| Device-local | chat lock, themes, local translation, gallery visibility, cache cleanup, screenshots, decrypted export | no server API by design |

## 3. Feature to endpoint map (Sections 9–15)

**Contacts.** Exact ID: `POST /discovery/public-id/resolve`. QR: `GET /me/qr`, then
`POST /discovery/invites/resolve`. Requests: `POST /contact-requests`, accept/decline
`POST /contact-requests/{id}/accept|decline`, cancel `DELETE /contact-requests/{id}`.
Details: `GET /contacts/{contactId}` (alias is yours; displayName is theirs). Favorites:
`GET /contacts/favorites`, `PUT|DELETE /contacts/{id}/favorite`. Lists:
`/contact-lists`, `/contact-lists/{listId}`, `/contact-lists/{listId}/members/{contactId}`.
Groups in common: `GET /contacts/{id}/common-groups` or, from a DIRECT chat,
`GET /conversations/{id}/shared-groups`. Block: `PUT|DELETE /profiles/{cfMeetId}/block`.
Report a profile: `POST /profiles/{cfMeetId}/reports`. Report a message: Messaging
`POST /v1/conversations/{id}/reports`. Rotate ID: `POST /me/public-id/rotate`.

**Chats.** List conversations, send, edit, delete, reply, react, pin, read cursors,
unread, search, typing, presence, attachments, voice notes: Messaging. Start a DIRECT
chat with a contact: Messaging `POST /v1/conversations` with `counterpartCfMeetId`;
create a group and add members with `cfMeetId`. Overlay for the list (mute,
clear-for-me, latest call card): `GET /inbox` (1.2.0: only conversations you are still a
member of; a page may hold fewer rows than `limit`, keep following `nextCursor`). Opening a chat: `GET
/conversations/{id}/context` (allowed actions, counterpart, preferences). Mute and media
auto-download: `GET|PUT /conversations/{id}/preferences`. Clear for me: `POST
/conversations/{id}/clear-for-me`, then hide messages with `sequence <=
clearedThroughSequence` on every device. Star: `POST /saved-messages`, list `GET
/saved-messages`, unstar `DELETE /saved-messages/{savedId}`. Shared media:
`GET /conversations/{id}/media`, download through Messaging grants. Disappearing
messages: Messaging conversation `retentionSeconds` (stricter only).

**Calls.** From a contact: `POST /contacts/{contactId}/calls`; from a chat:
`POST /conversations/{id}/calls`. Body `{kind: VOICE|VIDEO, clientRequestId, endpointId}`.
Reuse the same `clientRequestId` when retrying. Then use the returned `callId` with the
existing Calling signaling session for ringing, answer and media. Calls section:
`GET /calls/history?missed=true|false`, details `GET /calls/{callId}/context`
(`actions.callBack` and `callBackContactId`). Incoming ringing, answer, decline and
cancel stay with Calling.

**Timeline.** `GET /conversations/{id}/timeline` returns call and meeting cards only.
Merge them with Messaging history by `anchorSequence`, then `occurredAt`, then `entryId`.
A card's outcome comes from Calling; when `stale` is true, show the last known state
and refresh later. Recent cards across chats: `GET /activity`. 1.2.0: a member who had
no Communications profile when a meeting was shared gets the invitation card on first
read (unless the share predates the history Messaging lets them see). Chat messages for
meetings and calls are client-composed (see §11, "Meeting links and calls in chat").

**Meetings.** Instant meeting from a chat: `POST /conversations/{id}/meetings/instant`;
share one you own or host: `POST /conversations/{id}/meetings/share`; list:
`GET /conversations/{id}/meetings`. Join through the existing Meetings resolve/join
APIs with the returned `meetingCode`. Meeting chat is separate from the conversation.

**Settings and devices.** `GET /bootstrap` once at start. Notifications:
`GET|PUT /me/notification-preferences`; diagnostics
`GET /me/notification-delivery-status`. `providers` always names exactly `APNS`, `FCM`
and `WEB_PUSH`, each `CONFIGURED` or `NOT_CONFIGURED` (Messaging's own configuration);
if Messaging is unavailable or answers outside its contract the Hub returns 503
`COMMUNICATIONS_UNAVAILABLE`. It never returns a null or partial provider map. Storage:
`GET /me/storage-summary`. Capabilities (1.2.0): `GET /me/capabilities` →
`messaging.available` follows MANAGED messaging; `messaging.managed.realtime` is
`SESSION_DEVICE` (get live delivery with `POST /api/v2/messaging/session-tokens`) or
`POLLING`; `messaging.securityReady` / `privateE2ee` are E2EE readiness only. Devices: link existing Messaging device and/or Calling
endpoint with `POST /devices/link` (1.2.0: the device is bound to the sign-in making the
request, `accountSession.bound: true`); push target `PUT|DELETE /devices/{id}/push`
(the token passes through to Messaging); revoke `POST /devices/{id}/revoke` (1.2.0: also signs out the bound sign-in, see §11).
`GET /devices` (and `POST /devices/link`) first re-check every ACTIVE device with its
owner. A device or endpoint revoked in Messaging or Calling directly shows as `REVOKED`
with `revocationReason: OWNER_SERVICE` and writes a `DEVICE` change-feed entry. An
unavailable owner never changes a device's state.
Recovery after reinstall: `GET /me/recovery-state`. Privacy: `GET|PUT /me/privacy`.

## 4. Multi-device synchronization

1. `GET /bootstrap` → remember `sync.latestPosition` as `P`.
2. Poll or, on a realtime nudge, `GET /sync?after=P`. Each entry names a domain and
   resource; refetch that resource from its endpoint. Apply in position order.
   Domains and what to refetch:
   `CONTACT` (contact requests, accept/decline, removal, alias/favorite/notification
   changes, and the other side of a block or unblock: `GET /contacts`, `GET /contact-requests`),
   `BLOCK` (your own block list: `GET /blocks`), `PRIVACY` (`GET /me/privacy`),
   `PROFILE` (your CF Meet ID after rotation or an operator change: `GET /me`),
   `CONTACT_LIST`, `CONVERSATION_PREFERENCE`, `CLEAR_FOR_ME`, `SAVED_MESSAGE`,
   `NOTIFICATION_PREFERENCE`, `DEVICE`, `TIMELINE`, `MEETING_LINK`. A blocked person's
   devices receive only a `CONTACT` entry; no block state is disclosed to them.
3. `PUT /devices/{deviceId}/sync-state {acknowledgedPosition}` after applying (monotonic).
4. If `resyncRequired` is true, reload `GET /bootstrap` and continue from
   `latestPosition`. Never treat a gap as acknowledged.
Messaging message delivery uses its own durable event cursors and WebSocket acks.

## 5. Authentication and scope matrix

All public operations: Account bearer session, registered and not revoked. The Account
relay derives tenant/application/environment and principal, and signs the Hub request
(`communications-user.forward`). The Hub then uses only these private permissions:

| Hub → owner | Permission |
|---|---|
| Platform Access | `platform-identity.read`, `platform-principal.resolve` |
| Account | `communications-account.read` |
| Calling | `calling-communications.read`, `.apply`, `.launch` |
| Messaging | `messaging-communications.read`, `.apply` |
| Meetings | `meeting-communications.read`, `.create` |

Owners → Hub: `communications-policy.check`, `communications-block.apply`,
`communications-events.publish` (defined, not yet granted in production). Operator →
Hub: `communications-backfill.run`, `communications-observability.read`,
`communications-profile.suspend` (sensitive; not granted in production).

## 6. Contact and privacy authorization

| Action | Requires |
|---|---|
| Direct call / direct chat | CONNECTED, no block either way (Hub, Calling or Messaging), recipient privacy CONNECTIONS, both accounts active, no operator hold |
| Group call | caller is a member; Calling checks each participant |
| Group invitation | CONNECTED and recipient accepts requests |
| Message notification | as direct chat, plus recipient contact mute, conversation mute and account-wide switches |
| Presence | CONNECTED and recipient presence visibility |
| Favorites, lists, aliases | owner only; grant nothing |
| Meeting share | caller owns or hosts the meeting; members of the chat |

## 7. Error catalogue

Envelope: `{"schemaVersion":1,"error":{"code","message"}}`, `Cache-Control: no-store`.
Section 17 names map to the deployed codes as follows (codes are not renamed, so old
clients keep working):

| Spec name | Code returned | HTTP |
|---|---|---|
| INVALID_INPUT | `VALIDATION_ERROR` | 400 |
| AUTHENTICATION_REQUIRED | `AUTHENTICATION_REQUIRED` | 401 |
| COMMUNICATIONS_NOT_ACTIVATED | `COMMUNICATIONS_NOT_ACTIVATED` | 403 |
| ACTION_NOT_ALLOWED | `ACTION_NOT_ALLOWED` (also `PERMISSION_DENIED`) | 403 |
| RESOURCE_NOT_FOUND | `NOT_FOUND` (neutral for hidden/blocked/foreign) | 404 |
| VERSION_CONFLICT | `VERSION_CONFLICT`, `IDEMPOTENCY_CONFLICT`, `ALREADY_EXISTS`, `REQUEST_PENDING` | 409 |
| E2EE_NOT_AVAILABLE | `E2EE_NOT_AVAILABLE`, `E2EE_DEVICES_DISABLED` (relayed from Messaging) | 409 / 503 |
| INVITE_EXPIRED | `NOT_FOUND` (expired invitations are indistinguishable by design) | 404 |
| POLICY_VALIDATION_FAILED | `POLICY_VALIDATION_FAILED` (limits) | 422 |
| RATE_LIMITED | `RATE_LIMITED` + `Retry-After` | 429 |
| DEPENDENCY_NOT_CONFIGURED | `DEPENDENCY_NOT_CONFIGURED` | 503 |
| COMMUNICATIONS_TEMPORARILY_UNAVAILABLE | `COMMUNICATIONS_UNAVAILABLE` | 503 |
| PHONE_ALREADY_BOUND | not emitted (phone binding deferred) | — |

Relayed Calling codes: `TARGET_UNAVAILABLE` (409, does not reveal who blocked whom),
`ENDPOINT_NOT_ACTIVE`, `GROUP_TOO_LARGE`, `COMMUNICATIONS_POLICY_UNAVAILABLE` (503).

## 8. Backend versus device responsibilities

| Capability | Backend | Device |
|---|---|---|
| Private message content, keys, decryption, search, translation, export | stores ciphertext only | everything |
| Clear for me | watermark, hides cards, saved items, media index | hides messages ≤ watermark, deletes local copies |
| Disappearing messages | Messaging retention purge, legal holds | delete local copies; cannot stop screenshots |
| Chat lock, themes, wallpaper, gallery visibility, cache cleanup, OS shortcuts | none | everything |
| Notifications | routing metadata, preferences, retries | display, OS permission |
| Push credentials | provider config (not configured) | token registration |

## 9. Notification architecture

Messaging owns push jobs (APNs/FCM/Web Push adapters, retries, token cleanup) and
re-checks membership, blocks and Hub policy before each delayed dispatch. The Hub policy
decision applies the recipient's contact mute, account-wide switches and operator holds.
Calling owns ring offers over signaling. Payloads carry routing metadata only, never
message text, keys, OTPs, phone numbers or recording URLs. Production push providers
are **not configured**: delivery status reports `NOT_CONFIGURED`, never delivered.
Physical-device delivery has not been tested (no provider credentials, no devices).

## 10. Deployment, rollback and monitoring

- Release: the existing GitHub-controlled `communications-stage` → `communications-release`
  → `communications-verify` operations of the Calling production workflow. Migrations
  are additive; the Hub's Sections 9–23 tables use a separate migration history, so the
  previous Hub image still starts after migrating (rollback keeps data, no down
  migrations). Owner migrations are additive too.
- Rollback: `communications-rollback` restores retained containers; new tables remain
  unused by old images.
- Monitoring: Hub `/ready` (public health only on the private network), operator
  `GET /internal/v1/communications/readiness` and `admin/metrics` (outbox backlog, dead
  events, table counts), plus the existing owner metrics.

## 11. Client journeys and endpoints (Blueprint Section 14 frontend handoff: ACC-009, MSG-006, FE-020)

The backend for every journey below exists and is tested (Hub suites with simulated owners,
the real-process journeys in `certification/tests/personal_communications.rs`); the gap is
the web and mobile clients. Paths are relative to `/api/v2/communications` unless they
start with `/api`. All calls use the Account bearer session; send `Idempotency-Key` (UUID)
on every POST/PUT/PATCH/DELETE. Generated SDKs: TypeScript `client.communications.*`,
Dart `client.communications` (regenerated for 1.2.0).

| Journey | Endpoints, in order | Notes |
|---|---|---|
| App start | `GET /bootstrap`; `GET /me/capabilities`; `GET /sync?after=P` (§4) | `capabilities.messaging.managed.realtime == SESSION_DEVICE` → open realtime (row below) |
| MANAGED realtime without an E2EE device | `POST /api/v2/messaging/session-tokens` → `POST /v1/realtime/tickets` (`CF-Client-Token`) → WebSocket; renew with `{sessionDeviceId}` before expiry | ADR 0049 A1; refused for PRIVATE_E2EE conversations |
| My CF Meet ID / QR | `GET /me`, `GET /me/qr`, `POST /me/public-id/rotate` | |
| Find someone | `POST /discovery/public-id/resolve`, `POST /discovery/invites/resolve`, `POST /invites`, `GET /invites`, `DELETE /invites/{id}` | |
| Contact requests | `POST /contact-requests`, `GET /contact-requests`, `POST /contact-requests/{id}/accept\|decline`, `DELETE /contact-requests/{id}` | |
| Contacts | `GET /contacts`, `GET /contacts/{id}`, `PATCH /contacts/{id}`, favorites, lists, `GET /contacts/{id}/common-groups` | alias is yours |
| Block / report | `PUT\|DELETE /profiles/{cfMeetId}/block`, `GET /blocks`, `POST /profiles/{cfMeetId}/reports` | first-party clients block **only** here (DATA-006 decision) |
| Privacy & notifications | `GET\|PUT /me/privacy`, `GET\|PUT /me/notification-preferences`, `GET /me/notification-delivery-status` | |
| Chat list | Messaging `GET /api/v2/messaging/relay/v1/conversations?orderBy=lastActivity` + overlay `GET /inbox` | merge by `conversationId` |
| Open a chat | `GET /conversations/{id}/context`, Messaging history, `GET /conversations/{id}/timeline`; preferences, clear-for-me, saved messages, media | merge cards by `anchorSequence` |
| Call from a contact / chat | Calling endpoint registration (once per device) → `POST /devices/link {callEndpointId}` → `POST /contacts/{id}/calls` or `POST /conversations/{id}/calls {kind, clientRequestId, endpointId}` → Calling signaling | |
| Calls tab | `GET /calls/history?missed=`, `GET /calls/{callId}/context`, `GET /activity` | |
| Meeting from a chat | `POST /conversations/{id}/meetings/instant` or `/meetings/share {meetingCode}`, `GET /conversations/{id}/meetings`; join with the Meetings APIs | |
| Devices and sign-ins ("lost phone") | `GET /api/v2/me/sessions` (sign-ins) + `GET /devices` (linked devices, `accountSession.bound`); end one sign-in `POST /api/v2/me/sessions/{id}/revoke` (its bound devices end too); revoke a device `POST /devices/{id}/revoke {reason}` (its sign-in ends too; revoking the current device signs you out); everything `POST /api/v2/me/sessions/revoke-all` | ADR 0057 A1. Link each Messaging device / Calling endpoint right after registering it so it appears here |
| Push | `PUT\|DELETE /devices/{id}/push` (token passes through to Messaging) | providers report NOT_CONFIGURED until credentials exist |
| Recovery after reinstall | `GET /me/recovery-state`, then app start | |

**Meeting links and calls in chat (MSG-009 disposition).** The server never writes chat
messages on a user's behalf: for PRIVATE_E2EE conversations it cannot (no plaintext may
enter them) and for MANAGED ones it would bypass the sender's own client. The Hub's
timeline cards are the server record, visible to every current member (1.2.0 adds cards
for late-activated members). A client that also wants a chat line sends it itself as a
normal message (MANAGED: through the relay; PRIVATE_E2EE: encrypted by the device), with
the `meetingCode` as text. Cards are refreshed by polling (`stale`); a realtime nudge for
`TIMELINE` changes is not built (no Hub realtime channel exists).

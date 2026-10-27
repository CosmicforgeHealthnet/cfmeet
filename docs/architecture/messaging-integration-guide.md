# Messaging integration guide (Part V §105, §138, §165)

For an application backend and its end-user clients integrating CF Meet Messaging. The
contracts are authoritative: `packages/contracts/messaging/http/v1.openapi.json` (served
at `/openapi.json`; Swagger bundle under `packages/contracts/bundled/`),
`messaging/common`, `messaging/events` and `messaging/realtime` schemas.

## 1. Two kinds of caller

| Caller | Credential | May call |
|---|---|---|
| Application backend | Platform Access customer credential (`Authorization: Bearer …`), scopes `messages.send`, `messages.read`, `conversations.manage`, `usage.read`, compliance scopes; `CF-Acting-Principal` names the end user it acts for | Every customer operation |
| End-user device | Short-lived client token (`Authorization: CF-Client-Token …`) issued by the backend through `POST /v1/messaging/client-tokens` for one principal and device | Only the member operations whose contract lists `clientToken` in `security` (a test keeps that list and the server's allow-list identical) |

A client token always acts for its own principal (`ACTING_PRINCIPAL_MISMATCH` otherwise)
and device; revoking its session or device ends it immediately. Tenant, application and
environment never come from a request: they come from the credential.

## 2. Security profiles

`PRIVATE_E2EE` (default; MLS 1.0, the server stores ciphertext only) or `MANAGED` (server
readable; only where the application policy allows it). A conversation's profile never
changes; to move, create a successor with `predecessorConversationId` and
`profileChangeDisclosureAcknowledged` and show users the change. There is never a silent
fallback from E2EE to MANAGED or SMS (`E2EE_NOT_AVAILABLE` instead).

## 3. Client cryptography

Use `packages/messaging-crypto` (feature `client`; `web` for wasm32):

* device registration (challenge, proof of possession), KeyPackages, MLS groups
  (`device::Device`, `group::ClientGroup`), application-message AAD (`binding`);
* attachment encryption (`attachment`, XChaCha20-Poly1305 STREAM; the key travels only
  inside the MLS-protected message);
* history transfer between a user's devices (`history_transfer`) and the optional secure
  backup (`backup`, CFMB1, recovery code) — both opaque to the server;
* key transparency proofs (`transparency`) and the safety code / QR comparison
  (`verification`); surface key changes to users.

Keep KeyPackages stocked: a device below the watermark gets
`messaging.device.key-packages-low`.

## 4. Realtime protocol

`POST /v1/realtime/tickets` (client token) → WebSocket `/v1/realtime` with the ticket.
Client frames: `subscribe`, `unsubscribe`, `ack`, `typing`, `presence.set`,
`presence.query`, `auth.refresh`, `ping`. Server frames: `welcome`, `subscribed`,
`event`, `sync.required`, `subscription.revoked`, `device.event`, `presence*`,
`server.draining`, `error`, `pong`. Delivery is at least once from committed state;
resume with `afterEventSequence`. No compression is negotiated. Frames that do not parse
get an `error` frame; the connection keeps serving.

## 5. Conversations and members

* Roles: OWNER, ADMIN, MODERATOR, MEMBER, READ_ONLY (no sends), BOT_SERVICE (bot
  principals). Invite with `invite: true`; the invitee accepts or declines
  (`/invitation/accept|decline`) and lists `GET /v1/messaging/invitations`.
* DIRECT message requests (policy `requestsRequireAcceptance`): the counterpart sees the
  conversation only after accepting.
* Features: replies / threads (`GET …/messages/{id}/thread` summary), reactions, pins
  (optional `expiresAt`), unread counts, read / delivery cursors (receipts per environment,
  conversation `readReceipts` and user `shareReadReceipts`), edits with revisions
  (MANAGED), retention per conversation (`retentionSeconds`, only stricter), expiry with
  a grace period.

## 6. Attachments

Intent (`POST …/attachments`) → signed upload to object storage → `finalize` → reference
from a message. MANAGED files are scanned (fail closed) and may get derivatives; E2EE files
are client-encrypted and never scanned. Downloads use short-lived grants or the proxied,
re-authorized read.

## 7. SMS Bridge

MANAGED conversations only, where the context allows it: create a bridge with the current
disclosure version and `smsDisclosureAcknowledged: true`. Each bridged message has its own
transport state (`…/sms-transport`); in-app delivery never depends on the carrier.

## 8. Events and webhooks

Domain events (`cfmeet.messaging.event`) are recorded in a transactional outbox and
published at least once in order per aggregate; public ones are relayed to the platform
webhook system. Payloads carry ids, states and versions — never content, keys or numbers.
Deduplicate on the event id.

## 9. Errors and limits

Every error is the envelope `{ error: { code, message, reason?, requestId } }`. Rate limits
answer `RATE_LIMITED` with `Retry-After`. Usage facts (`GET /v1/messaging/usage`) are
counts only; Messaging does not price or bill.

## Not covered here

Frontend and mobile UI, provider onboarding (APNs / FCM / carriers), hosted environments.

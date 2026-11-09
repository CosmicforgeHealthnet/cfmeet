# Section 17 — Web Messaging and Communications Hub

Branch `blueprint/s17-web-comms2`. A web-client change only: no backend, contract or production
change. PRIVATE_E2EE stays gated. MANAGED conversations are always labelled "readable by CF Meet
servers" and never shown as end-to-end encrypted. Session devices (ADR 0049 A1) are realtime-only
and never count as E2EE devices.

## Findings

| Finding | Web status | What changed |
|---|---|---|
| MSG-005 | FRONTEND_CORRECTED | Reactions, replies, edit (PATCH, `expectedEditCount`), delete for everyone / hide for me, pins, typing, read receipts ("Seen by"), unread and mention badges, MANAGED search (E2EE: device-local), back-pagination, group title / roles / add by CF Meet ID / leave, direct + group creation with contacts or colleagues |
| MSG-008 | FRONTEND_CORRECTED | `managed-store.ts`: store keyed by `messageId` converging on the server's current representation (edits replace, tombstones final, stale copies ignored), event replay with `sync.required` resnapshot, polling window re-read when no live events. `outbox.ts`: durable per-account queue, one `clientMessageId` per send (idempotent, MSG-019), per-conversation order, back-off, retry/discard, cleared at sign-out. Attachments: upload retry, bounded wait while the server scan is VERIFYING, real file name/size from the attachment record, lost-response retry with the same `clientMessageId` |
| MSG-018 | FRONTEND_CORRECTED (client half) | Subscribing without `afterEventSequence` (`subscribeResume`) so the server resumes from its stored per-device ack cursor; the `subscribed` frame sets the local cursor; reconnects send the local cursor (never behind the ack). Session-device id kept per tab so the cursor survives reloads. Read cursor only written when it moves. Receipt-event coalescing on the server stays open (backend) |
| MSG-006 / ACC-009 | FRONTEND_CORRECTED | `/dashboard/contacts`: CF Meet ID + QR (`qrcode-generator`, SVG via React), rotate ID, invite links create/list/revoke, find by ID, requests incoming/outgoing accept/decline/cancel, connections (alias, favorite, notifications, remove), block/unblock/report, privacy, unified activity (Hub calls + meetings + Messaging inbox chats), call history; `/connect/[token]` invite landing; capability gating from `GET /me/capabilities`; Hub change feed (`/bootstrap` + `/sync`) refetches changed areas |
| MSG-009 (web) | FRONTEND_CORRECTED | Hub timeline cards (calls, meeting invitations) merged into the chat by `anchorSequence` → `occurredAt` → `entryId`, hidden under the clear-for-me watermark, stale cards polled; Start meeting / Share meeting from a chat; a MANAGED chat also gets a client-composed line with the meeting code |
| Workspace awareness | FRONTEND_CORRECTED (partial by backend) | Personal: new conversations pick from connected contacts; organization: only that organization's ACTIVE members of the colleague directory. Contacts screen shows organization colleagues in a separate tab, never as contacts |

## Navigation hook for the web Calling client

`callHref({ contactId } | { conversationId }, "VOICE" | "VIDEO")` in
`apps/web/src/lib/communications/comms-core.ts` → `/calling?contactId=…&kind=…` or
`/calling?conversationId=…&kind=…`. Calling owns launch (`POST /contacts/{id}/calls`,
`POST /conversations/{id}/calls`), ringing and media. Links show only when Hub capabilities
(`personalCalling.available`) or the chat context (`actions.voiceCall` / `videoCall`) allow.

## Gaps (not invented on the client)

* Conversations carry no organization field. Organization conversations live in the organization's own
  scope (ADR 0053, final closure): the organization workspace reads them through
  `/api/v2/organizations/{id}/messaging/relay/...` (web: `ca4072cc`), so the list is partitioned by which
  relay is called, not by a field.
* `GET /api/v2/messaging/directory` spans every organization of the caller (no organization filter);
  the web narrows it with `GET /api/v2/organizations/{id}/members`.
* `principals/names` only names colleagues; a DIRECT chat with a contact takes its name from the Hub
  conversation context (`counterpart`), and the list shows "CF Meet user" until opened.
* No batch reaction read: reactions are fetched for the newest 30 messages on open and on events.
* No Hub realtime channel: timeline cards and the change feed are polled.
* Contact requests carry only the CF Meet ID (no display name in `ContactRequest`).
* Message search for PRIVATE_E2EE is device-local only (by design).
* `messaging-v2.test.mjs` (not in CI) was already failing on the base branch (`@/store` cookie mock).

## Traceability (screen / action → endpoint → errors shown)

| Screen / action | Method / path (relative to `/api/v2`) or WS | Errors handled |
|---|---|---|
| Messages: list | `GET messaging/relay/v1/conversations?orderBy=lastActivity` (+ members for groups), Hub `GET communications/inbox` (muted) | 401 session, FEATURE_DISABLED, CHAT_UNAVAILABLE |
| Messages: open | `GET …/conversations/{id}/unread`, `GET …/messages?beforeSequence=`, `?afterSequence=`, `GET …/pins`, `GET …/messages/{id}/reactions`, Hub `GET communications/conversations/{id}/context`, `/timeline` | inline alert |
| Messages: older | `GET …/messages?beforeSequence=` | inline alert |
| Send (MANAGED) | `POST …/messages {clientMessageId, text, replyToMessageId?, mentionedPrincipalIds?}` via outbox | transient → queued + back-off; 4xx → "Not sent (CODE)" + Retry/Discard |
| Edit | `PATCH …/messages/{id} {text, expectedEditCount}` | 409 VERSION_CONFLICT, MESSAGE_NOT_MODIFIABLE |
| Delete / hide | `DELETE …/messages/{id}?scope=EVERYONE|ME`, then `GET …/messages/{id}` | inline alert |
| React | `PUT|DELETE …/messages/{id}/reactions/{reaction}` | inline alert |
| Pin | `PUT|DELETE …/pins/{messageId}` | inline alert |
| Search | `GET …/conversations/{id}/search?q=&limit=25` | inline alert |
| Read | `PUT …/read-cursor {sequence}` (only when it moves) | silent retry next time |
| Attach (MANAGED) | `POST …/attachments`, upload PUT, `POST …/finalize`, `GET …/attachments/{id}` while VERIFYING, `POST …/messages {attachmentIds}`; download `POST …/download-grants` | QUARANTINED/REJECTED → "rejected by the security scan" |
| Group manage | `PATCH …/conversations/{id} {expectedVersion,title}`, `PATCH …/members/{p} {role}`, `POST …/members {cfMeetId}`, `DELETE …/members/{p}` | inline alert |
| Create chat | `POST …/conversations` (DIRECT `counterpartPrincipalId` / `counterpartCfMeetId`; GROUP + `title`), `POST …/members`; `POST messaging/directory/resolve` | create-error |
| Live updates | `POST messaging/session-tokens` (or `client-tokens` with an E2EE device) → `POST /v1/realtime/tickets` → WS `subscribe` (no cursor) / `ack` / `typing` / `auth.refresh`; REST replay `GET …/events?after=` | FEATURE_DISABLED → polling; 4401 → new session device |
| Chat: meeting | Hub `POST communications/conversations/{id}/meetings/instant|share` | Hub error catalogue |
| Contacts: my ID / QR | Hub `GET me`, `GET me/qr` (Idempotency-Key), `GET me/public-id`, `POST me/public-id/rotate` | catalogue (§7) |
| Invites | Hub `POST invites`, `GET invites`, `DELETE invites/{id}` | POLICY_VALIDATION_FAILED, ACTION_NOT_ALLOWED |
| Find / connect | Hub `POST discovery/public-id/resolve`, `POST discovery/invites/resolve`, `POST contact-requests` | NOT_FOUND (neutral), REQUEST_PENDING, RATE_LIMITED |
| Requests | Hub `GET contact-requests/incoming|outgoing`, `POST …/{id}/accept|decline`, `DELETE …/{id}` | VERSION_CONFLICT |
| Connections | Hub `GET contacts`, `PATCH contacts/{id}`, `DELETE contacts/{id}`; message → `POST …/conversations` DIRECT MANAGED `counterpartCfMeetId`; call → `callHref` | catalogue |
| Block / report | Hub `PUT|DELETE profiles/{cfMeetId}/block`, `GET blocks`, `POST profiles/{cfMeetId}/reports` | catalogue |
| Privacy | Hub `GET|PUT me/privacy {expectedVersion, privacy}` | VERSION_CONFLICT |
| Activity | Hub `GET activity`, `GET calls/history?missed=`, Messaging inbox list | catalogue |
| Capabilities / sync | Hub `GET me/capabilities`, `GET bootstrap`, `GET sync?after=` (resyncRequired → reload all) | area hidden, reason shown |
| Colleagues (organization) | `GET messaging/directory`, `GET organizations/{id}/members`, `POST messaging/directory/resolve` | inline alert |

Tests: `apps/web/tests/messaging-managed-sync.test.mjs`, `messaging-outbox.test.mjs`,
`communications-core.test.mjs` (added to `.github/workflows/frontend.yml`), plus the unchanged
`messaging-realtime.test.mjs` and `messaging-session.test.mjs`.

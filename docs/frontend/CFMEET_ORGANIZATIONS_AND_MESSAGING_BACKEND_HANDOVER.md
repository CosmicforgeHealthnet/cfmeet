# CF Meet — Organizations and organization Messaging: backend handover (web and mobile)

**Audience:** web (`apps/web`) and mobile (`apps/mobile`) developers. **Scope:** backend contracts only; this
document changes no client code. **Status of the backend in production:** see
[`CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md`](../operations/CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md).
The short version: until the Organizations authority cutover completes, every first-party organization route below answers
`404 FEATURE_DISABLED`, and clients must keep treating organizations as unavailable (feature-detect, never hard-code).

Base URL: `https://api.cfmeet.com`. Every call carries the account bearer token (`Authorization: Bearer <access token>`).
Errors use the standard envelope `{ "success": false, "error": { "code", "message", "requestId" } }`; branch on `code`,
never on `message`.

---

## 1. Feature detection and organization switching

| Step | Call | Notes |
|---|---|---|
| Is first-party organization management on? | `POST /api/v2/organizations` (or any first-party route) answering `404 FEATURE_DISABLED` | Cache the answer per session. Do not show "Create organization" while disabled. |
| Organizations I belong to | `GET /api/v2/me/organizations` | `[{ organization: {id,name,slug,status,createdAt}, role, organizationRole? }]`. `organizationRole` (OWNER, ADMIN, MANAGER, MEMBER) appears once the Organizations Service is authoritative; `role` shows MANAGER as MEMBER for older clients. |
| Workspaces (personal + organizations) | `GET /api/v2/me/workspaces`, `GET /api/v2/me/workspaces/{id}/capabilities` | Use capabilities (default deny) to gate every organization UI action. |
| Switch organization | client state only | Close the organization realtime connection of the previous organization (§6), mint a session token for the new one. Personal data never moves into an organization. |

## 2. Create an organization (self-service)

`POST /api/v2/organizations` with header `Idempotency-Key: <uuid>` (reuse it on retry).

```json
{ "name": "Acme Health", "slug": "acme-health" }
```

* `201` → `BusinessOrganization` (the caller is OWNER; `provisioningState` may be `PENDING`). Then refresh
  `GET /api/v2/me/organizations`.
* `409 SLUG_TAKEN` (choose another slug), `409 IDEMPOTENCY_CONFLICT` (same key, different body), `429 RATE_LIMITED`
  (at most 5 per user per 24 h), `400 VALIDATION_ERROR`, `404 FEATURE_DISABLED`.
* Organization-scoped Messaging / Calling answer `409 PROVISIONING_PENDING` until provisioning completes:
  `GET /api/v2/organizations/{id}/provisioning` shows the state; retry with backoff.

## 3. Member directory

`GET /api/v2/organizations/{id}/members` (optional paging `?limit=1..200&cursor=…`; the next cursor is in the
`X-Next-Cursor` response header; the body stays a JSON array).

```json
{
  "id": "<membership id — use it in PATCH …/members/{id}>",
  "organizationId": "…",
  "userId": "<account id — never show it to people>",
  "role": "MEMBER",
  "organizationRole": "MANAGER",
  "status": "ACTIVE",
  "displayName": "Jane Smith",
  "username": "jane_s",
  "email": "jane@example.com",
  "version": 3,
  "createdAt": "…", "updatedAt": "…"
}
```

* **Show `displayName`** (falls back to the username server-side). `displayName`/`username` are `null` only when the
  account no longer exists: render "Former member", never a shortened account id.
* **`email` visibility (enforced by the backend):** present only when the caller is an ACTIVE OWNER or ADMIN of this
  organization, or a platform administrator. For everyone else the field is **absent** (not `null`). Render it when
  present; never infer visibility from the client-side role.
* Role / status changes: `PATCH /api/v2/organizations/{id}/members/{membershipId}` with `expectedVersion`
  (`409 VERSION_CONFLICT` → refetch; `409 LAST_OWNER`). Leave: `POST …/members/me/leave`. Ownership:
  `POST …/ownership-transfer`.

> Frontend requirement (not done by this change): `apps/web/src/components/organizations/OrganizationMembers.tsx`
> currently renders `Account <shortId>` and a "names are not shown yet" note. Replace with `displayName` (and
> `email` when present). The mobile member list needs the same change.

## 4. Invitations

### 4.1 Invite by email or CF Meet ID

`POST /api/v2/organizations/{id}/invitations` (`Idempotency-Key` recommended). Capability
`organization.members.invite` (OWNER, ADMIN, MANAGER).

```json
{ "identifierType": "EMAIL", "identifier": "jane@example.com", "role": "MEMBER", "sendEmail": true }
{ "identifierType": "CF_MEET_ID", "identifier": "<cf-meet-id>", "role": "MEMBER", "sendEmail": true }
{ "email": "jane@example.com", "role": "MEMBER" }            // original form, still accepted
```

| Answer | Meaning / client action |
|---|---|
| `201` `OrganizationInvitation` | `token` (one time, only in this answer — share as a link if email delivery is not `ACCEPTED`), `inviteeEmailHint` (`j***@example.com`, or `CF Meet ID <id>` for CF Meet ID invitations), `identifierType`, `identifier` (CF Meet ID only), `emailDelivery` (`ACCEPTED`, `DISABLED`, `FAILED` when `sendEmail` was true). |
| `404 INVITEE_NOT_FOUND` | No invitable account for this CF Meet ID. Deliberately identical for unknown, hidden, non-discoverable, blocked and inactive accounts: say "No CF Meet user can be invited with this ID". |
| `409 ALREADY_MEMBER` | The account already belongs to the organization. |
| `403 PERMISSION_DENIED` | The caller's role cannot invite (no lookup happens). |
| `400 VALIDATION_ERROR` | Both `email` and `identifier`, unknown `identifierType`, or a missing half. |
| `429 RATE_LIMITED` | Invitation emails per hour, or CF Meet ID lookups (discovery budget). |
| `404 FEATURE_DISABLED` | Organizations not authoritative yet. |

A CF Meet ID invitation never returns the invitee's email to the inviter, not even masked. Inviting the same address
again replaces the pending invitation (this is "resend": a new token).

### 4.2 Manage

* List: `GET /api/v2/organizations/{id}/invitations?status=PENDING|ACCEPTED|DECLINED|REVOKED|EXPIRED`.
* Revoke: `POST /api/v2/organizations/{id}/invitations/{invitationId}/revoke`.
* Invitations expire after 7 days (`EXPIRED`).

### 4.3 Accept or decline (recipient)

`POST /api/v2/organization-invitations/accept` or `/decline` with `{ "token": "…" }`. Only an account whose
**verified** email equals the invited address can accept (`403` otherwise, including another signed-in account).
Accepting creates the membership; refresh `GET /api/v2/me/organizations`.

> Known gap: there is no in-app list of invitations addressed to the signed-in user yet; the recipient needs the link
> (email or shared). See the completion report §19.

## 5. Organization Messaging (REST)

Base: `/api/v2/organizations/{id}/messaging/relay/` + a Messaging `v1/…` path (same allowlist and bodies as the
personal relay `/api/v2/messaging/relay/`). The caller acts as their own organization-scoped principal.

### 5.1 Conversation list with previews and unread counts

`GET …/messaging/relay/v1/conversations?orderBy=lastActivity&limit=50[&cursor=…]`

Each item is the canonical Messaging conversation plus a member `inbox` summary:

```json
{
  "conversationId": "…", "contextClass": "GROUP", "title": "…", "securityProfile": "MANAGED", "…": "…",
  "inbox": {
    "lastActivityAt": "2026-11-12T10:00:00Z",
    "lastMessage": {
      "messageId": "…", "sequence": 41,
      "senderPrincipalId": "<organization-scope principal>",
      "createdAt": "…", "lifecycle": "ACTIVE", "securityProfile": "MANAGED",
      "attachmentCount": 0,
      "previewText": "The meeting starts at 10"
    },
    "unreadCount": 3,
    "mentionCount": 0
  }
}
```

* Empty conversation: `inbox.lastMessage` is `null`, `unreadCount` is `0`, `lastActivityAt` is the creation time.
* `previewText` is `null` for deleted messages (tombstones), for PRIVATE_E2EE conversations (the server has no
  plaintext) and for attachments without text. Only messages visible to the caller count (history window, clear-for-me).
* Unread = messages from others after the caller's read cursor. Mark read with
  `PUT …/messaging/relay/v1/conversations/{conversationId}/read-cursor` (Messaging contract body).
* Next page: `nextCursor` (keep the same `orderBy`).
* Sender names: `POST /api/v2/messaging/principals/names` (organization principals); colleagues:
  `POST /api/v2/organizations/{id}/messaging/members/resolve` (`{ userIds: [...] }` → ACTIVE members only).

## 6. Organization Messaging realtime (replaces 5-second polling)

### 6.1 Connect

1. `POST /api/v2/organizations/{id}/messaging/session-tokens` with `{}` (first time) or `{ "sessionDeviceId": "…" }`
   (renewal) → `201 { token, deviceId, expiresAt, … }`. Bound to the account session (sign-out ends it); short-lived.
2. `POST /v1/realtime/tickets` with `Authorization: CF-Client-Token <token>` → `{ ticket }` (single use).
3. Open `wss://api.cfmeet.com/v1/realtime` with `Sec-WebSocket-Protocol: cfmeet.messaging.v1, cfmeet.ticket.<ticket>`.
4. Read `welcome`. If `welcome.capabilities` contains `"inbox"`, send `{ "type": "inbox.subscribe", "requestId": "i1" }`
   → `inbox.subscribed`.
5. `subscribe` the conversation(s) on screen (no cursor: the server resumes from the device's ack cursor) and `ack`
   delivered events.

The same steps work for personal Messaging with `POST /api/v2/messaging/session-tokens`. One connection serves one
scope: one per organization in use (switching organization = close and reconnect with that organization's token).

### 6.2 Frames the client must handle

| Frame | When | Client action |
|---|---|---|
| `event` (subscribed conversation) | durable event: `message.created/updated/deleted`, `member.added/removed`, `conversation.updated`, receipt cursors… | apply in `eventSequence` order; drop duplicates (`eventSequence ≤ last applied`); `ack` |
| `inbox.event` `{conversationId, eventSequence, eventType}` | something happened in a conversation of yours that this connection is **not** subscribed to — including a **new conversation** you were added to and **your own removal** (`member.removed`) | refresh that row: `GET …/relay/v1/conversations/{conversationId}` (or the list); on `member.removed` drop it. Identifiers only, never content. |
| `read-cursor.synced` `{conversationId, readSequence}` | you read on another device | recompute unread for that conversation (or refetch it) |
| `inbox.resync` | broker interruption | refetch the conversation list once |
| `sync.required` | subscription fell behind the replay limit | refetch the conversation's messages by REST, re-subscribe |
| `subscription.revoked` | you lost access to that conversation | remove it |
| `error` `SESSION_EXPIRED` / close `4401` | token expired / session, device or membership revoked | renew (step 1 with `sessionDeviceId`, then `auth.refresh` with a new ticket); if renewal answers `403`/`404` the organization access ended — leave the organization UI |
| `server.draining` | deploy | reconnect after `reconnectAfterMs` |
| close `1013` `SLOW_CONSUMER` | client too slow | reconnect; subscriptions resume from the ack cursor |

Renew **before** `expiresAt`: mint a new token with the same `sessionDeviceId`, get a ticket, send
`{ "type": "auth.refresh", "ticket": "…" }` → `auth.refreshed` (the connection continues).

### 6.3 Reconnection and missed events

* Reconnect with exponential backoff (1 s → 30 s, jitter). Re-send `inbox.subscribe`, re-`subscribe` open conversations
  without a cursor: the server replays everything after the last `ack` once, in order (no duplicates if the client drops
  `eventSequence ≤ last applied`). After any reconnect, refetch the conversation list once (inbox signals are not
  replayed).
* Removed or suspended members, suspended organizations and signed-out sessions are closed by the server (`4401`) and
  cannot renew.
* Background delivery while the app is suspended (mobile push) is a separate dependency and is **not** covered by the
  WebSocket.

## 7. Error codes to handle

`FEATURE_DISABLED`, `ORGANIZATION_NOT_FOUND`, `MEMBERSHIP_REQUIRED`, `PERMISSION_DENIED`,
`ORGANIZATION_POLICY_RESTRICTED`, `ORGANIZATION_SUSPENDED`, `ORGANIZATION_ARCHIVED`, `PROVISIONING_PENDING`,
`SLUG_TAKEN`, `VERSION_CONFLICT`, `LAST_OWNER`, `INVITATION_REQUIRED` (legacy PUT member route: send an invitation),
`ALREADY_MEMBER`, `INVITEE_NOT_FOUND`, `IDEMPOTENCY_CONFLICT`, `RATE_LIMITED`, `REVOCATION_CHECK_UNAVAILABLE`
(retry), `SERVICE_UNAVAILABLE` (retry with backoff), `VALIDATION_ERROR`.

## 8. Contract references

* Meeting Service OpenAPI: `packages/contracts/openapi/meeting-service.json` (operations `v2_create_business_organization`,
  `v2_my_organizations`, `v2_list_organization_members`, `v2_create_organization_invitation`,
  `v2_list_organization_invitations`, `v2_revoke_organization_invitation`, `v2_accept_organization_invitation`,
  `v2_decline_organization_invitation`, `v2_update_organization_membership`, `v2_organization_messaging_get/post/put/patch/delete`,
  `v2_organization_messaging_session_token`, `v2_organization_messaging_resolve_members`). Generated client types:
  `apps/web/src/types/generated/meeting-service.ts`, `apps/mobile/lib/api/generated/meeting-service.ts` (regenerated).
* Messaging REST: `packages/contracts/messaging/http/v1.openapi.json` (conversation list `inbox`).
* Realtime protocol: `packages/contracts/messaging/realtime/v1.schema.json` (1.4.0: `inbox.subscribe`, `inbox.subscribed`,
  `inbox.event`, `inbox.resync`, `welcome.capabilities`).

# Sections 9–23 — private owner interfaces used by the Communications Hub

These routes let the Communications Hub (`cfmeet.communications`) build its facades
without copying canonical state. Each owner stays authoritative. All routes are
**private**: they never appear in public Swagger, the API Gateway catalogue or the
Account relay.

## Common rules (every route below)

- `POST`, JSON body, `deny_unknown_fields`, response `{"schemaVersion":1, ...}`.
- Caller: `cfmeet.communications` with the named permission in the receiver's trust
  bundle. Other callers get 403; unknown routes 404.
- The signed request carries the Hub's delegated first-party platform context for the
  governed personal scope (`CF_COMMUNICATIONS_PERSONAL_*`) with the initiating
  principal of kind `NATIVE_CF_USER`. Any other scope returns 404 (use the owner's
  existing `own()` guard).
- `principalId` in the body **must equal** the delegated initiating principal;
  otherwise 403. The Hub never acts for a user other than the authenticated one.
- No message content, ciphertext, keys, push tokens, recording URLs or credentials
  in responses or logs.
- Errors use the owner's existing error envelope and status.

## Calling (`services/calling`)

| ID | Route | Permission |
|---|---|---|
| CL-1 | `/internal/v1/communications/calls` | `calling-communications.launch` |
| CL-2 | `/internal/v1/communications/calls/participation` | `calling-communications.read` |
| CL-3 | `/internal/v1/communications/calls/history` | `calling-communications.read` |
| CL-4 | `/internal/v1/communications/endpoints/verify` | `calling-communications.read` |
| CL-5 | `/internal/v1/communications/endpoints/revoke` | `calling-communications.apply` |

**CL-1 launch.** Body `{idempotencyKey (1–200 printable ASCII), initiatorPrincipalId,
initiatorEndpointId, targetPrincipalIds (1..max group, distinct, no self), kind:
VOICE|VIDEO}`. Runs the same domain path as customer `POST /v1/calls` for an app call:
personal policy check before idempotent replay, endpoint ownership, group limit,
ringing. Idempotency keys live in a namespace distinct from customer keys. Response
`{schemaVersion, callId, state, kind, version, createdAt, replayed}`.

**CL-2 participation.** Body `{principalId, callIds (1–50, distinct)}`. Returns
`{schemaVersion, calls: [Entry]}` containing only calls in scope where the principal is
a participant (others silently omitted). `Entry = {callId, kind, state, terminal,
version, createdAt, answeredAt?, endedAt?, durationMs?, direction:
INCOMING|OUTGOING, outcome (existing ParticipantOutcome), missed,
counterpartPrincipalIds}`.

**CL-3 history.** Body `{principalId, limit (1–100), cursor?, missed?}`. Same entries
and keyset cursor as the existing history query. `{schemaVersion, data, nextCursor}`.

**CL-4 endpoint verify.** Body `{principalId, endpointId}` →
`{schemaVersion, owned, status: ACTIVE|REVOKED|null, endpointKind|null}`.

**CL-5 endpoint revoke.** Body `{principalId, endpointId}` → canonical revoke (cancels
live offers). `{schemaVersion, revoked, alreadyRevoked}`; 404 when not owned.

## Messaging (`services/messaging`)

| ID | Route | Permission |
|---|---|---|
| MS-1 | `/internal/v1/communications/conversations/access` | `messaging-communications.read` |
| MS-2 | `/internal/v1/communications/conversations/attachments` | `messaging-communications.read` |
| MS-3 | `/internal/v1/communications/storage-summary` | `messaging-communications.read` |
| MS-4 | `/internal/v1/communications/devices/verify` | `messaging-communications.read` |
| MS-5 | `/internal/v1/communications/devices/revoke` | `messaging-communications.apply` |
| MS-6 | `/internal/v1/communications/devices/push` | `messaging-communications.apply` |
| MS-7 | `/internal/v1/communications/push-status` | `messaging-communications.read` |

**MS-1 access.** Body `{principalId, conversationId, messageId?}`. Non-member (or not
found) → `{schemaVersion, member:false}`. Active member →
`{schemaVersion, member:true, conversationId, contextClass, securityProfile,
lifecycleState, lastSequence, visibleFromSequence, retentionSeconds|null,
expiresAt|null, role, memberCount, memberPrincipalIds (ACTIVE, sorted, at most 256),
message?: {messageId, sequence, state: VISIBLE|DELETED|HIDDEN_FOR_ME|EXPIRED|NOT_FOUND,
sentAt, senderPrincipalId}}`.

**MS-2 attachments.** Body `{principalId, conversationId, limit (1–100), cursor?,
afterSequence? (integer ≥ 0, default 0)}`. Member-visible, message-attached attachments
only (respect the member's join window); attachments whose message sequence is
`<= afterSequence` are excluded (the Hub's clear-for-me watermark).
`{schemaVersion, data: [{attachmentId, messageId|null, messageSequence|null, state,
sizeBytes, mediaClass: IMAGE|VIDEO|AUDIO|DOCUMENT|OTHER|ENCRYPTED, createdAt,
uploaderPrincipalId}], nextCursor}`. PRIVATE_E2EE attachments are always `ENCRYPTED`;
never decrypt.

**MS-3 storage.** Body `{principalId}` → `{schemaVersion, attachmentCount,
attachmentBytes, backupBytes, environmentQuotaBytes|null, measuredAt}` for content the
principal uploaded.

**MS-4 device verify.** Body `{principalId, deviceId}` → `{schemaVersion, owned,
status|null, pushRegistered, pushPlatform|null}`.

**MS-5 device revoke.** Body `{principalId, deviceId, reason: USER_REQUESTED|COMPROMISED}`
→ canonical device revoke. `{schemaVersion, revoked, alreadyRevoked}`; 404 when not owned.

**MS-6 push.** Body `{principalId, deviceId, action: REGISTER|REMOVE, registration?}`
where `registration` is exactly the existing push-registration request body. Reuses
`push::register` / `push::unregister`. The token is passed through, never echoed.
`{schemaVersion, deviceId, pushRegistered, platform|null}`.

**MS-7 push status.** Body `{principalId}` → `{schemaVersion, providers: {APNS, FCM,
WEB_PUSH}: CONFIGURED|NOT_CONFIGURED, devices: [{deviceId, pushRegistered, platform|null,
lastJobState|null, lastFailureClass|null, lastAttemptAt|null}], pendingJobs, deadJobs}`.

## Meetings (`services/meeting-service`)

| ID | Route | Permission |
|---|---|---|
| MT-1 | `/internal/v1/communications/meetings/instant` | `meeting-communications.create` |
| MT-2 | `/internal/v1/communications/meetings/resolve` | `meeting-communications.read` |

**MT-1 instant.** Body `{principalId, accountReference, idempotencyKey (UUID),
title? (≤120)}`. The account/principal pair must match Account's existing
`communication_principal_links` mapping. Creates the meeting through the same service
path as `POST /api/v2/instant-meetings`, owned by that user, under existing Meetings
rules. Idempotent per (account, key). `{schemaVersion, meetingSpaceId, conferenceId,
meetingCode, createdAt, replayed}`.

**MT-2 resolve.** Body `{principalId, accountReference, meetingCode}` →
`{schemaVersion, meetingSpaceId, meetingCode, title|null, shareable, role:
OWNER|HOST|PARTICIPANT|NONE, latestConference: {id, status, scheduledStartAt|null}|null}`.
`shareable` is true only when the user owns or hosts the space. Unknown code → 404.
Guests, admission and media credentials stay unchanged and independent of the Hub.

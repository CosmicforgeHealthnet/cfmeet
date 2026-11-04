# Messaging — native authenticated API map

Generated from the Account relay and Messaging 1.14.0 contract.
Canonical SDK contract: **96 operations**; audience-filtered public Swagger:
**80 operations**; native Account transports: **71 operations**.
There are no new canonical message/conversation/device endpoints.

CF Meet consumer REST calls use the native path below with the existing Account bearer
session. Account derives the principal/scope and signs the internal request. Never send
service credentials, an operator token or `CF-Acting-Principal` from a client. The SDK's
canonical application API paths use a separately authenticated application gateway;
their presence in the catalogue does not prove public VPS routing. Native aliases use
the same Messaging request/response schemas and preserve owner status/error bodies.

Public Swagger JSON: `https://api.cfmeet.com/api-docs/messaging.json`. Each native
operation carries `x-cfmeet-native-path`, `x-cfmeet-native-auth` and
`x-cfmeet-native-principal`. Source schemas, scopes, parameters, errors, pagination,
examples and operation IDs remain in
`packages/contracts/messaging/http/v1.openapi.json` and its self-contained bundles.
An operation's presence does not certify runtime availability.

Accepted personal contacts can address the existing DIRECT creation API with
`counterpartCfMeetId` instead of `counterpartPrincipalId`. The member-add API accepts
`cfMeetId` instead of `principalId`. Supply exactly one identifier. Messaging resolves
the public ID privately under the authenticated actor/scope and the operation's policy
purpose; no principal lookup endpoint or service credentials are needed by the client.
Rotated IDs stop resolving; reload Contacts. Existing conversation IDs/history remain
unchanged. UUID addressing remains compatible and enforces the same owner policies.

## Security and device prerequisites

Production PRIVATE_E2EE enrollment is BLOCKED pending independent assessment and
accepted cryptographic decisions. Production continues rejecting the enable flag.
Disabled enrollment returns 503 `FEATURE_DISABLED` / `E2EE_DEVICES_DISABLED`; private
conversation creation returns 409 `E2EE_NOT_AVAILABLE`. Neither chooses MANAGED.
`GET /api/v2/communications/me/capabilities` must be checked; a ready HTTP service
alone does not imply device security readiness or witnessed transparency.

Authenticated, active users first resolve their own identity with
`GET /api/v2/messaging/me`, establish an accepted Communications connection, then enroll
devices in an authorized non-production environment. Challenge -> client-held Ed25519/
HPKE keys -> challenge-bound MLS registration KeyPackage -> publish one-use public
KeyPackages. No server private-key read/decrypt endpoint exists. Signed tree heads and
inclusion/consistency proofs need pinned log/witness keys from a trusted release channel.
Never treat UNWITNESSED as witnessed. Account login/logout and device/session revocation
are separate from optional encrypted history transfer; a new device gets no old epoch
keys automatically. Backend backups store opaque client-encrypted bytes only.

Use the shared crypto core's `prepare_add_devices`, `prepare_remove_devices` and
`prepare_update_keys`; submit the Commit with its old epoch, wait for matching canonical
acceptance, then `accept_pending_commit`. Publish Welcomes only after acceptance.
On confirmed rejection/supersession use `discard_pending_commit` and process the winning
Commit. On ambiguous timeout reconcile/retry the SAME clientMessageId/bytes before
discarding. Exported pending state must be encrypted under device-protected storage.
Immediate local-simulation compatibility helpers are not the network commit protocol.
Frontend/native bindings must adopt this staged lifecycle before production client approval.

For send/edit use a stable clientMessageId and the same ciphertext on retries; generate
fresh ciphertext only for a new submission. Conversation create forwards a stable
`Idempotency-Key`. Direct reuse preserves canonical IDs. `expectedVersion` and
`expectedEditCount` protect updates. Respect 429 and Retry-After; do not reset rate limits.
Messages page by afterSequence/beforeSequence; conversations use cursor/limit; replay
uses afterEventSequence. Only committed owner events are delivery evidence. A WebSocket
notice or push is not proof of durable storage; acknowledge monotonic event cursors and
recover gaps from PostgreSQL-backed history after reconnect.

PRIVATE_E2EE clients submit authenticated MLS PrivateMessage application ciphertext;
public application frames are refused with 422 `CONTENT_REJECTED` /
`APPLICATION_MUST_BE_PRIVATE`. The server checks framing/device/membership, while
clients verify AEAD, sender binding and AAD before rendering. Private edits/reactions are
encrypted application messages; MANAGED row-based edit/reaction APIs do not relabel them.
Reply/thread IDs are disclosed metadata; quoted text remains encrypted.

Attachments: encrypt on device -> upload intent (ciphertext size/digest,
application/octet-stream) -> upload with the exact signed headers -> finalize -> attach
the ID to the encrypted message -> obtain an authorized short-lived download grant ->
verify/decrypt on the recipient. Private file names, descriptors and attachment keys stay
inside MLS. Private files are NOT_APPLICABLE_ENCRYPTED for scanning, never falsely clean.
Managed files need explicit allowed/disclosed context, encryption at rest and a successful
malware scan before READY. Server thumbnails exist only for supported MANAGED images.
Native clients download from grants; the JSON relay is not a binary-content proxy.

Realtime: obtain a short-lived device-bound client token through the Account bridge,
POST `/v1/realtime/tickets` with `Authorization: CF-Client-Token <token>`, then connect to
`wss://api.cfmeet.com/v1/realtime` using the returned one-use ticket in
Sec-WebSocket-Protocol, never the URL. Browser origins must match the existing allowlist.
Typing/presence are ephemeral and privacy-filtered; durable events are replayed by cursor.
Expired/revoked tickets, sessions or devices fail closed.

Push registration uses the device owner; payloads contain routing metadata only.
APNs/FCM/Web Push delivery needs real configured providers and physical-device evidence.
Recipient contact MUTED suppresses delayed notifications. MENTIONS also suppresses
ordinary opaque pushes because the server cannot infer private mentions. Group membership,
blocks, privacy and account state are rechecked before delayed governed push dispatch.
Logout/all-session revocation disables registrations without deleting device identities.

Negative examples: no accepted connection => direct create/send refused; foreign or
revoked device => token/send denied; missing group membership => messages/attachments
not found; stale epoch => conflict; private-to-managed mutation => immutable-profile
refusal; Hub unavailable => governed sends/push fail closed. No phone verification is
required. Meetings and established permitted service contexts retain independent rules.

## Operation map

| Method | Canonical schema path | Operation ID | Actual native/public device path | Audience |
|---|---|---|---|---|
| POST | `/v1/messaging/devices/registration-challenges` | createDeviceRegistrationChallenge | `/api/v2/messaging/devices/registration-challenges` | Account session |
| POST | `/v1/messaging/devices` | registerDevice | `/api/v2/messaging/devices` | Account session |
| POST | `/v1/messaging/devices/{deviceId}/key-packages` | publishKeyPackages | `/api/v2/messaging/devices/{deviceId}/key-packages` | Account session |
| POST | `/v1/messaging/devices/{deviceId}/revoke` | revokeDevice | `/api/v2/messaging/devices/{deviceId}/revoke` | Account session |
| GET | `/v1/messaging/principals/{principalId}/devices` | listPrincipalDevices | `/api/v2/messaging/relay/v1/messaging/principals/{principalId}/devices` | Account session |
| GET | `/v1/messaging/key-transparency/proofs` | getKeyTransparencyProofs | `/api/v2/messaging/relay/v1/messaging/key-transparency/proofs` | Account session |
| POST | `/v1/conversations` | createConversation | `/api/v2/messaging/relay/v1/conversations` | Account session |
| GET | `/v1/conversations` | listConversations | `/api/v2/messaging/relay/v1/conversations` | Account session |
| GET | `/v1/conversations/{conversationId}` | getConversation | `/api/v2/messaging/relay/v1/conversations/{conversationId}` | Account session |
| PATCH | `/v1/conversations/{conversationId}` | updateConversation | `/api/v2/messaging/relay/v1/conversations/{conversationId}` | Account session |
| GET | `/v1/messaging/policy` | getMessagingPolicy | Not exposed by Account relay | Application administration; excluded from public Swagger |
| PUT | `/v1/messaging/policy` | updateMessagingPolicy | Not exposed by Account relay | Application administration; excluded from public Swagger |
| GET | `/v1/conversations/{conversationId}/members` | listMembers | `/api/v2/messaging/relay/v1/conversations/{conversationId}/members` | Account session |
| POST | `/v1/conversations/{conversationId}/members` | addMember | `/api/v2/messaging/relay/v1/conversations/{conversationId}/members` | Account session |
| DELETE | `/v1/conversations/{conversationId}/members/{principalId}` | removeMember | `/api/v2/messaging/relay/v1/conversations/{conversationId}/members/{principalId}` | Account session |
| PATCH | `/v1/conversations/{conversationId}/members/{principalId}` | changeMemberRole | `/api/v2/messaging/relay/v1/conversations/{conversationId}/members/{principalId}` | Account session |
| GET | `/v1/conversations/{conversationId}/messages` | listMessages | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages` | Account session |
| POST | `/v1/conversations/{conversationId}/messages` | sendMessage | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages` | Account session |
| GET | `/v1/conversations/{conversationId}/messages/{messageId}` | resolveMessage | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages/{messageId}` | Account session |
| PATCH | `/v1/conversations/{conversationId}/messages/{messageId}` | editManagedMessage | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages/{messageId}` | Account session |
| DELETE | `/v1/conversations/{conversationId}/messages/{messageId}` | deleteMessage | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages/{messageId}` | Account session |
| GET | `/v1/conversations/{conversationId}/messages/{messageId}/revisions` | listMessageRevisions | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages/{messageId}/revisions` | Account session |
| PUT | `/v1/conversations/{conversationId}/read-cursor` | setReadCursor | `/api/v2/messaging/relay/v1/conversations/{conversationId}/read-cursor` | Account session |
| GET | `/v1/conversations/{conversationId}/events` | replayConversationEvents | `/api/v2/messaging/relay/v1/conversations/{conversationId}/events` | Account session |
| POST | `/v1/conversations/{conversationId}/mls/commits` | submitMlsCommit | `/api/v2/messaging/relay/v1/conversations/{conversationId}/mls/commits` | Account session |
| GET | `/v1/conversations/{conversationId}/mls/state` | getMlsGroupState | `/api/v2/messaging/relay/v1/conversations/{conversationId}/mls/state` | Account session |
| POST | `/v1/conversations/{conversationId}/mls/key-package-claims` | claimKeyPackages | `/api/v2/messaging/relay/v1/conversations/{conversationId}/mls/key-package-claims` | Account session |
| GET | `/v1/messaging/devices/{deviceId}/welcomes` | listWelcomes | `/api/v2/messaging/relay/v1/messaging/devices/{deviceId}/welcomes` | Account session |
| POST | `/v1/messaging/devices/{deviceId}/welcomes/{welcomeId}/consume` | consumeWelcome | `/api/v2/messaging/relay/v1/messaging/devices/{deviceId}/welcomes/{welcomeId}/consume` | Account session |
| GET | `/v1/messaging/blocks` | listBlocks | `/api/v2/messaging/relay/v1/messaging/blocks` | Account session |
| POST | `/v1/messaging/blocks` | createBlock | `/api/v2/messaging/relay/v1/messaging/blocks` | Account session |
| DELETE | `/v1/messaging/blocks/{principalId}` | removeBlock | `/api/v2/messaging/relay/v1/messaging/blocks/{principalId}` | Account session |
| POST | `/v1/realtime/tickets` | issueRealtimeTicket | `/v1/realtime/tickets` | Device token / one-use WS ticket |
| GET | `/v1/realtime` | connectRealtime | `/v1/realtime` | Device token / one-use WS ticket |
| POST | `/v1/messaging/client-tokens` | issueClientToken | `/api/v2/messaging/client-tokens` | Account session |
| POST | `/v1/messaging/client-sessions/{sessionId}/revoke` | revokeClientSession | `/api/v2/messaging/relay/v1/messaging/client-sessions/{sessionId}/revoke` | Account session |
| GET | `/v1/messaging/presence/settings` | getPresenceSettings | `/api/v2/messaging/relay/v1/messaging/presence/settings` | Account session |
| PUT | `/v1/messaging/presence/settings` | putPresenceSettings | `/api/v2/messaging/relay/v1/messaging/presence/settings` | Account session |
| PUT | `/v1/messaging/devices/{deviceId}/push-registration` | registerPush | `/api/v2/messaging/relay/v1/messaging/devices/{deviceId}/push-registration` | Account session |
| DELETE | `/v1/messaging/devices/{deviceId}/push-registration` | unregisterPush | `/api/v2/messaging/relay/v1/messaging/devices/{deviceId}/push-registration` | Account session |
| POST | `/v1/conversations/{conversationId}/attachments` | createAttachmentIntent | `/api/v2/messaging/relay/v1/conversations/{conversationId}/attachments` | Account session |
| GET | `/v1/conversations/{conversationId}/attachments/{attachmentId}` | getAttachment | `/api/v2/messaging/relay/v1/conversations/{conversationId}/attachments/{attachmentId}` | Account session |
| POST | `/v1/conversations/{conversationId}/attachments/{attachmentId}/finalize` | finalizeAttachment | `/api/v2/messaging/relay/v1/conversations/{conversationId}/attachments/{attachmentId}/finalize` | Account session |
| POST | `/v1/conversations/{conversationId}/attachments/{attachmentId}/download-grants` | grantAttachmentDownload | `/api/v2/messaging/relay/v1/conversations/{conversationId}/attachments/{attachmentId}/download-grants` | Account session |
| GET | `/v1/conversations/{conversationId}/attachments/{attachmentId}/content` | readAttachmentContent | Not exposed by Account relay | Separate service/application context; not native personal Messaging |
| GET | `/v1/messaging/legal-holds` | listLegalHolds | Not exposed by Account relay | Application administration; excluded from public Swagger |
| POST | `/v1/messaging/legal-holds` | createLegalHold | Not exposed by Account relay | Application administration; excluded from public Swagger |
| GET | `/v1/messaging/legal-holds/{holdId}` | getLegalHold | Not exposed by Account relay | Application administration; excluded from public Swagger |
| POST | `/v1/messaging/legal-holds/{holdId}/release` | releaseLegalHold | Not exposed by Account relay | Application administration; excluded from public Swagger |
| GET | `/v1/conversations/{conversationId}/search` | searchConversation | `/api/v2/messaging/relay/v1/conversations/{conversationId}/search` | Account session |
| GET | `/v1/messaging/search` | searchMessages | `/api/v2/messaging/relay/v1/messaging/search` | Account session |
| POST | `/v1/conversations/{conversationId}/reports` | createReport | `/api/v2/messaging/relay/v1/conversations/{conversationId}/reports` | Account session |
| GET | `/v1/messaging/reports` | listReports | Not exposed by Account relay | Application administration; excluded from public Swagger |
| GET | `/v1/messaging/reports/{reportId}` | getReport | Not exposed by Account relay | Application administration; excluded from public Swagger |
| GET | `/v1/messaging/moderation/actions` | listModerationActions | Not exposed by Account relay | Application administration; excluded from public Swagger |
| POST | `/v1/messaging/moderation/actions` | applyModerationAction | Not exposed by Account relay | Application administration; excluded from public Swagger |
| POST | `/v1/messaging/exports` | requestExport | Not exposed by Account relay | Application administration; excluded from public Swagger |
| GET | `/v1/messaging/exports/{exportId}` | getExport | Not exposed by Account relay | Application administration; excluded from public Swagger |
| POST | `/v1/messaging/exports/{exportId}/download-grants` | grantExportDownload | Not exposed by Account relay | Application administration; excluded from public Swagger |
| POST | `/v1/messaging/deletion-requests` | requestDataDeletion | Not exposed by Account relay | Application administration; excluded from public Swagger |
| GET | `/v1/messaging/deletion-requests/{requestId}` | getDataDeletion | Not exposed by Account relay | Application administration; excluded from public Swagger |
| GET | `/v1/messaging/audit` | listAudit | Not exposed by Account relay | Application administration; excluded from public Swagger |
| POST | `/v1/messaging/client-sessions/revoke-all` | revokeAllClientSessions | `/api/v2/messaging/client-sessions/revoke-all` | Account session |
| GET | `/v1/messaging/sms-bridge/disclosure` | getSmsBridgeDisclosure | Not exposed by Account relay | Separate service/application context; not native personal Messaging |
| GET | `/v1/conversations/{conversationId}/sms-bridges` | listSmsBridges | Not exposed by Account relay | Separate service/application context; not native personal Messaging |
| POST | `/v1/conversations/{conversationId}/sms-bridges` | createSmsBridge | Not exposed by Account relay | Separate service/application context; not native personal Messaging |
| POST | `/v1/conversations/{conversationId}/sms-bridges/{bridgeId}/close` | closeSmsBridge | Not exposed by Account relay | Separate service/application context; not native personal Messaging |
| GET | `/v1/conversations/{conversationId}/messages/{messageId}/sms-transport` | getSmsTransport | Not exposed by Account relay | Separate service/application context; not native personal Messaging |
| GET | `/v1/conversations/{conversationId}/bans` | listBans | `/api/v2/messaging/relay/v1/conversations/{conversationId}/bans` | Account session |
| POST | `/v1/conversations/{conversationId}/bans` | banMember | `/api/v2/messaging/relay/v1/conversations/{conversationId}/bans` | Account session |
| DELETE | `/v1/conversations/{conversationId}/bans/{principalId}` | unbanMember | `/api/v2/messaging/relay/v1/conversations/{conversationId}/bans/{principalId}` | Account session |
| GET | `/v1/conversations/{conversationId}/messages/{messageId}/reactions` | listReactions | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages/{messageId}/reactions` | Account session |
| PUT | `/v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}` | addReaction | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}` | Account session |
| DELETE | `/v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}` | removeReaction | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}` | Account session |
| GET | `/v1/conversations/{conversationId}/pins` | listPins | `/api/v2/messaging/relay/v1/conversations/{conversationId}/pins` | Account session |
| PUT | `/v1/conversations/{conversationId}/pins/{messageId}` | pinMessage | `/api/v2/messaging/relay/v1/conversations/{conversationId}/pins/{messageId}` | Account session |
| DELETE | `/v1/conversations/{conversationId}/pins/{messageId}` | unpinMessage | `/api/v2/messaging/relay/v1/conversations/{conversationId}/pins/{messageId}` | Account session |
| GET | `/v1/conversations/{conversationId}/unread` | getUnreadCount | `/api/v2/messaging/relay/v1/conversations/{conversationId}/unread` | Account session |
| GET | `/v1/messaging/usage` | getMessagingUsage | Not exposed by Account relay | Separate service/application context; not native personal Messaging |
| POST | `/v1/messaging/history-transfers` | createHistoryTransfer | `/api/v2/messaging/relay/v1/messaging/history-transfers` | Account session |
| GET | `/v1/messaging/history-transfers` | listHistoryTransfers | `/api/v2/messaging/relay/v1/messaging/history-transfers` | Account session |
| GET | `/v1/messaging/history-transfers/{transferId}` | getHistoryTransfer | `/api/v2/messaging/relay/v1/messaging/history-transfers/{transferId}` | Account session |
| PUT | `/v1/messaging/history-transfers/{transferId}/sealed-history` | sealHistoryTransfer | `/api/v2/messaging/relay/v1/messaging/history-transfers/{transferId}/sealed-history` | Account session |
| GET | `/v1/messaging/history-transfers/{transferId}/sealed-history` | getSealedHistory | `/api/v2/messaging/relay/v1/messaging/history-transfers/{transferId}/sealed-history` | Account session |
| POST | `/v1/messaging/history-transfers/{transferId}/complete` | completeHistoryTransfer | `/api/v2/messaging/relay/v1/messaging/history-transfers/{transferId}/complete` | Account session |
| POST | `/v1/messaging/history-transfers/{transferId}/cancel` | cancelHistoryTransfer | `/api/v2/messaging/relay/v1/messaging/history-transfers/{transferId}/cancel` | Account session |
| POST | `/v1/conversations/{conversationId}/attachments/{attachmentId}/thumbnail-grants` | createThumbnailGrant | `/api/v2/messaging/relay/v1/conversations/{conversationId}/attachments/{attachmentId}/thumbnail-grants` | Account session |
| POST | `/v1/messaging/backups` | createBackupUpload | `/api/v2/messaging/relay/v1/messaging/backups` | Account session |
| GET | `/v1/messaging/backups/latest` | getLatestBackup | `/api/v2/messaging/relay/v1/messaging/backups/latest` | Account session |
| DELETE | `/v1/messaging/backups/{backupId}` | deleteBackup | `/api/v2/messaging/relay/v1/messaging/backups/{backupId}` | Account session |
| POST | `/v1/messaging/backups/{backupId}/finalize` | finalizeBackup | `/api/v2/messaging/relay/v1/messaging/backups/{backupId}/finalize` | Account session |
| POST | `/v1/messaging/backups/{backupId}/download-grants` | createBackupDownloadGrant | `/api/v2/messaging/relay/v1/messaging/backups/{backupId}/download-grants` | Account session |
| GET | `/v1/conversations/{conversationId}/messages/{messageId}/thread` | getThreadSummary | `/api/v2/messaging/relay/v1/conversations/{conversationId}/messages/{messageId}/thread` | Account session |
| POST | `/v1/conversations/{conversationId}/invitation/accept` | acceptInvitation | `/api/v2/messaging/relay/v1/conversations/{conversationId}/invitation/accept` | Account session |
| POST | `/v1/conversations/{conversationId}/invitation/decline` | declineInvitation | `/api/v2/messaging/relay/v1/conversations/{conversationId}/invitation/decline` | Account session |
| GET | `/v1/messaging/invitations` | listInvitations | `/api/v2/messaging/relay/v1/messaging/invitations` | Account session |

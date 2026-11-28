# ADR 0073: Communities belong to Messaging

- Status: Accepted (owner brief 2026-11-27, sections 11-14 and 17)
- Date: 2026-11-28
- Related: ADR 0069 (Status and Channels in Messaging), ADR 0066 (MLS self-join), the
  Messaging membership model (Part V Workstream E), the group invite links (S12)

## Context

The owner asked for WhatsApp-style **Communities**: a collection of related personal or
social group conversations under one structure, with an Announcements group, linked and
created groups, join requests, invitations, QR codes, roles and bans, a Communities section
in the web app and a "Communities" chat filter. Communities must stay separate from
Organizations (enterprise; never required), Groups (independent conversations, unchanged)
and Channels (broadcast). Nothing goes in the Meeting Service except relay allowlist entries.

## Decision

1. **Messaging owns communities** (`services/messaging/src/communities/`), under
   `/v1/messaging/communities`, reached by the web only through the Meeting Service relay
   (`/api/v2/messaging/relay/v1/messaging/communities/...`, pass-through ROUTES entries).
   No Organizations membership is read or required; the Organizations service is untouched.
2. **A community is a set of ordinary group conversations.** Its groups are existing
   `GROUP` conversations (messages, calls, meetings, polls, media, reactions, replies,
   mentions and pins all work unchanged). A conversation belongs to at most one community.
   Community membership is its own table; joining a community makes the person a member
   of the **Announcements** group, which the server creates with the community (a MANAGED
   group, `announcementOnly`: only its OWNER / ADMINs, i.e. the community owner and
   community admins, post). Its group photo is the community picture; its pins are the
   pinned announcements.
3. **Joining a group never exposes its past.** A community member who joins a linked group
   becomes a conversation member through the ordinary admission (`authority_source =
   'COMMUNITY'`), which opens a NEW history window (`visible_from_sequence` /
   `visible_from_event`): earlier messages, events and attachments stay invisible; a
   PRIVATE_E2EE group is joined by the person's own device with an MLS external commit
   (ADR 0066), which never yields earlier epochs' keys. Community listings show a linked
   group's title, description and photo reference only; its member list and member count
   are shown only to its members.
4. **Linking needs both authorities.** Only an OWNER / ADMIN of the group links it, and
   only a community admin (or any member when `membersCanAddGroups`) adds groups. Groups
   can be unlinked by a community admin or by the group's own admins (the Announcements
   group cannot be unlinked), reordered by community admins and set to `OPEN` (any
   community member joins) or `RESTRICTED` (a join request, decided by the group's admins
   or a community admin).
5. **Roles** (all enforced by the server): community `OWNER` (exactly one; transfers
   ownership, deletes), community `ADMIN` (settings, members, invitations, links, groups,
   requests, bans; may promote members; only the owner demotes or removes an admin), group
   admin (the conversation's own OWNER / ADMIN: manages that group only) and `MEMBER`.
   Every community resource answers `404 NOT_FOUND` to a non-member (ids are not probeable);
   a member lacking the role gets `403 PRINCIPAL_NOT_AUTHORIZED` / `NOT_COMMUNITY_ADMIN`.
6. **Privacy**: `OPEN` (any member invites and creates links; a link joins at once),
   `APPROVAL` (any member invites; joining by link, or accepting a non-admin's invitation,
   creates a join request an admin approves) and `INVITE_ONLY` (only admins invite or create
   links). Invite links are `mcl_` tokens stored as SHA-256 only, expiring, use-capped and
   revocable; the QR code is drawn by the client from the link. A ban removes the person
   from the community and its groups, refuses their re-admission to the community and to
   every linked group (also through the group's own links or invitations), closes their
   invitations and rejects their pending requests.
7. **Own migration history** `messaging_communities_seaql_migrations`
   (`m20261220_000048_communities`), additive: the previous image never reads it and boots on
   this schema; this image requires it (`pending_migrations`).
8. **Notices** (invitation, join request for admins, approval) are written to
   `messaging_community_notices` only while `MESSAGING_COMMUNITY_NOTICES=on` (default off),
   and relayed like the status notices. Their catalogue types
   (`messaging.community.invited`, `messaging.community.join_requested`,
   `messaging.community.request_approved`) must exist in the Notification Service before the
   switch is turned on (release activation step).
9. **Chat list**: the user's preference `chatListMode` (`TOGETHER`: community groups also in
   All; `SEPARATE`: only under the Communities filter) is stored per principal (synced across
   devices).

## API (`cfmeet.messaging.http-api`; acting principal required)

Scopes: reads `messages.read`, changes `messages.send`. Pages: `limit` (1-100, default 50) and
an opaque `cursor` (`nextCursor`).

| Operation | Who |
|---|---|
| `GET /v1/messaging/communities` (`limit`, `cursor`) | my communities |
| `POST /v1/messaging/communities` (optional `Idempotency-Key`) `{ name, description?, rules?, privacy?, membersCanAddGroups? }` | anyone (rate limited) |
| `GET` / `PATCH` / `DELETE /v1/messaging/communities/{communityId}` | member / admin / owner |
| `POST …/{communityId}/owner` `{ principalId }` | owner |
| `POST …/{communityId}/leave` | member (the owner transfers first) |
| `GET …/{communityId}/members` (`limit`, `cursor`) | member |
| `PUT …/{communityId}/members/{principalId}/role` `{ role: ADMIN | MEMBER }` | admin (only the owner demotes) |
| `DELETE …/{communityId}/members/{principalId}` | admin (only the owner removes an admin) |
| `GET` / `POST …/{communityId}/bans`, `DELETE …/bans/{principalId}` | admin |
| `GET` / `POST …/{communityId}/invitations` `{ principalIds }`, `DELETE …/invitations/{invitationId}` | admin; members per privacy |
| `GET /v1/messaging/communities/invitations`, `POST …/invitations/{invitationId}/accept` / `decline` | the invitee |
| `GET` / `POST …/{communityId}/invite-links`, `POST …/invite-links/{linkId}/revoke` | admin; members per privacy |
| `POST /v1/messaging/communities/invite-links/preview` / `join` `{ token, message? }` | anyone holding the token |
| `GET …/{communityId}/join-requests` (`state`, `limit`, `cursor`), `POST …/join-requests/{requestId}/approve` / `reject` | community admin; group admin for its group |
| `GET /v1/messaging/communities/join-requests`, `POST …/join-requests/{requestId}/cancel` | the requester |
| `GET …/{communityId}/groups`, `POST …/groups` (optional `Idempotency-Key`) `{ title, description?, access? }`, `POST …/groups/link` `{ conversationId, access? }`, `PUT …/groups/order` `{ conversationIds }` | member / admin (or members when `membersCanAddGroups`) |
| `PATCH` / `DELETE …/{communityId}/groups/{conversationId}`, `POST …/groups/{conversationId}/join` `{ message? }` | community or group admin / member |
| `GET` / `PUT /v1/messaging/communities/preferences` `{ chatListMode }` | self |

`Community`: `communityId, name, description, rules, privacy, membersCanAddGroups,
ownerPrincipalId, announcementsConversationId, pictureRef, memberCount, groupCount, myRole,
joinedAt, joinedConversationIds, pendingRequestCount, version, createdAt, updatedAt`.
`CommunityGroup`: `conversationId, title, description, avatarRef, securityProfile, access,
position, announcements, joined, memberCount, myRole, canManage, pendingRequest, linkedAt`.
A join (`…/invite-links/join`, `…/groups/{id}/join`) answers `201 { outcome: JOINED }`,
`202 { outcome: REQUESTED, request }` or `200 { outcome: ALREADY_MEMBER }`.

## Consequences

- Community events (Events, ADR 0071, scope `COMMUNITY`) are a follow-up once Events is on
  main: the scope is authorized by community membership.
- Group conversations keep working on their own; unlinking never touches a conversation's
  members or content.
- Abuse bounds: community creation 10 per hour, invitations 200 per hour, link joins and
  join requests 30 per hour, link previews 60 per minute per principal; at most 100 groups
  per community and 50 open invite links per community.

## Response shapes

- `GET /communities` → `{ communities: Community[], nextCursor }`; `GET …/members` →
  `{ members: [{ principalId, role, joinedAt }], nextCursor }`; `GET …/bans` →
  `{ bans: [{ principalId, bannedAt, bannedByPrincipalId }] }` (`POST …/bans { principalId }`
  answers one ban).
- `Invitation`: `invitationId, communityId, principalId, invitedByPrincipalId, state
  (PENDING | ACCEPTED | DECLINED | REVOKED | EXPIRED), createdAt, expiresAt` and, in the
  invitee's own list, `community` (a `CommunityPreview`). `POST …/invitations` →
  `{ invitations, skipped: [{ principalId, reason }] }`. Accept → `{ outcome, community?,
  request? }`.
- `InviteLink`: `inviteLinkId, communityId, maxUses, useCount, expiresAt,
  createdByPrincipalId, createdAt, revokedAt, status (ACTIVE | EXPIRED | EXHAUSTED |
  REVOKED)`, plus `token` in the creation response only. `GET` → `{ inviteLinks }`.
- `CommunityPreview`: `communityId, name, description, rules, privacy, memberCount,
  groupCount, member, pendingRequest`.
- `JoinRequest`: `requestId, communityId, communityName, principalId, conversationId (null:
  community membership), groupTitle, state (PENDING | APPROVED | REJECTED | CANCELLED),
  message, createdAt, decidedAt, decidedByPrincipalId`. Lists → `{ requests, nextCursor }`.
- `GET …/groups` and `PUT …/groups/order` → `{ groups: CommunityGroup[] }`.
- Preferences → `{ chatListMode: TOGETHER | SEPARATE }` (default `TOGETHER`).

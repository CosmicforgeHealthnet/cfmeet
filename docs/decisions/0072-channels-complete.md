# ADR 0072 — Channels, complete (management, rich posts, discovery, analytics; verification and moderation by HQ)

Status: Accepted (owner brief 2026-11-27, "Complete Status, Channels, Communities, Events and
Chat Experience", items C13–C17). Builds on ADR 0069 (Channels belong to Messaging) and ADR
0068 (HQ manages CF Meet through the admin bridge).

## Context

ADR 0069 moved Channels into Messaging at parity with the retired Meeting Service feature:
a name and description, owner-only posts (text, photo, video, audio), followers, one
reaction each and unread counts. The owner asked for WhatsApp-level channels: usernames,
private channels, admins, every post kind, polls, scheduled and pinned posts, discovery,
analytics, reports, and verification and moderation that only HQ decides.

## Decisions

1. **Everything stays in Messaging** (`services/messaging/src/social/channels/`), under the
   separate additive history `messaging_social_seaql_migrations`
   (`m20261219_000047_channels_complete`): new columns have defaults, scheduled posts live in
   their own table until published (an older image never shows one early), taken-down posts
   are deleted, so a rollback to the previous image still serves correct channels. The
   customer API is `cfmeet.messaging.http-api` 1.26.0 (31 new operations, all `EXCLUDED`
   from the public gateway, `FIRST_PARTY_STATUS_CHANNELS`); the Meeting Service gains only
   relay `ROUTES` entries.
2. **One access rule.** A channel is PUBLIC or PRIVATE. A PRIVATE channel, and any SUSPENDED
   or BANNED one, is visible only to its members (owner, admins, followers); anyone else gets
   `404` on every path (detail, posts, media grants, directory, handle lookup, follow).
   Posts of a SUSPENDED channel are readable by its owner and admins only, of a BANNED one by
   its owner only. A private channel is joined only through an invite link.
3. **Handles.** Unique per environment, 5–32 of `a-z 0-9 _` starting with a letter
   (normalized from `@Name`), reserved words (`admin`, `support`, … and anything containing
   `cfmeet`, `official`, `verified`) refused. Only the owner sets or changes
   it; a verified channel keeps its handle. Category (closed list), BCP 47 language and
   ISO 3166-1 country are optional codes; the picture is a COMPLETE channel image upload.
4. **Admins.** The owner holds every permission; an admin holds any of `canPost` (post,
   schedule, pin, edit / delete their own posts), `canEdit` (details, invite links, every
   post) and `canManageAdmins` (add / change / remove admins who are not managers, granting
   only permissions they hold). Only the owner deletes the channel, changes its handle or
   visibility, applies for verification and transfers ownership — to an existing admin; the
   old owner stays as a full admin and a pending application is closed. **A new admin must
   be a mutual, unblocked contact of the person adding them** (Communications Hub
   `contacts/eligible`), so no follower list is ever needed. Every owner / admin action is in
   the channel's own audit log (`messaging_channel_audit`), readable by owner and admins.
5. **Invite links** follow the group invite-link model: the token (`mci_` + 32 random bytes)
   is returned once, only its SHA-256 is stored, preview and join take it in the body (never
   a URL), unknown / expired / revoked / used-up tokens are the same `404`, revocation is
   final. The web puts it in a URL fragment and a QR code generated in the browser.
6. **Posts.** TEXT; IMAGE / VIDEO / AUDIO / DOCUMENT from the poster's COMPLETE channel upload
   (DOCUMENT is a new media kind for channels only: PDF, Office, OpenDocument, EPUB, zip,
   text, CSV; ≤ 32 MiB; recognised by content, executables and scripts refused; the file
   name is kept without its path); presentations VOICE (audio), GIF (a GIF image or a
   video) and STICKER (WebP / PNG ≤ 1 MiB); POLL (2–12 distinct options, single or multiple
   choice). **Link previews are client-supplied**, exactly like chat link cards
   (`lib/messaging/rich-content.ts`): the poster's device builds the card from the link; the
   server never fetches a URL (no SSRF surface) and only checks the URL is http(s) and
   appears in the text. Edits change words, caption or preview and are marked `edited`;
   polls are not editable. At most 3 pinned posts. `publishAt` (1 minute to 30 days ahead)
   keeps a post scheduled; the social sweep (every 60 s) publishes due posts, drops those
   whose author can no longer post, and leaves a suspended channel's posts waiting.
7. **Followers.** Follow / unfollow (the owner always follows), mute, unread counts,
   reactions, one vote per follower per poll (a new vote replaces theirs; votes are rows per
   option, so results are counts), per-person starred posts, forwards into the caller's
   direct chats with mutual contacts (a message with the channel's name, an excerpt and
   `cfmeet://channel/{id}/post/{postId}`), reports of a channel or post (one open report per
   person and target). **No answer ever carries a follower's identity**: there is no
   follower list, analytics are counts, and the HQ reads relay no reporter.
8. **Discovery.** PUBLIC, ACTIVE channels only: search over name, handle and description
   keywords (a `to_tsvector('simple', …)` GIN index plus prefix / substring matches),
   category / country / language filters, sort POPULAR (the denormalized `follower_count`,
   kept in the follow transaction and recounted by the sweep), NEW, VERIFIED (keyset
   cursors) and TRENDING (net follows ×5 + reactions + unique post views ÷10 over the last 7
   days — recorded events only; ranked, at most 200), suggestions (prefix, or trending
   channels the caller does not follow), handle lookup, and preview before following (a
   public channel's posts are readable without following).
9. **Analytics** for owner and admins, per UTC day over a range (default 30 days, at most
   366): follows and unfollows (`messaging_channel_follow_events`, no principal), followers at
   the end of each day (walked back from today's count; `null` before recording began —
   `statsSince`, the migration time for existing channels), views (one row per person and
   post: a person viewing again is not counted; owner and admins are never counted), unique
   viewers, reactions, poll voters, forwards, engagement rate ((reactions + poll voters) ÷
   views) and top posts. Nothing is estimated.
10. **Verification and moderation come only from HQ through the admin bridge.** An owner
    applies (`POST …/verification`, basis code; PUBLIC, ACTIVE, with a handle; one pending at
    a time; 30 days after a rejection). States PENDING → APPROVED / REJECTED; APPROVED →
    REVOKED. No customer route sets `verified_at` or `moderation_state`. HQ acts through
    nine new operator operations (`cfmeet.messaging.internal-api` 1.7.0, the ADR 0068 owner
    contract: bridge-only permission, named operator, Idempotency-Key, tenant re-check, row
    version, two-person approval, the change and its ledger row in one transaction):
    * reads `GET /internal/v1/operator/channel-verifications`, `/channel-reports`,
      `/channel-actions`, `/channels/{channelId}` — handles, codes, states, counts and times;
      never a channel's name, description or posts, a report's note or reporter;
    * commands (all R3) `messaging.channel-verification.decide` (APPROVE / REJECT),
      `messaging.channel-verification.revoke`, `messaging.channel.moderate` (ACTIVE,
      RESTRICTED: hidden from discovery and no new followers; SUSPENDED: only admins read,
      nobody posts; BANNED: only the owner reads), `messaging.channel-post.takedown`
      (deletes the post, actions its open reports), `messaging.channel-report.resolve`.
    Every action is recorded in `messaging_channel_moderation_actions` (with the HQ operator
    reference, which the owner's moderation history does not show). Reports, applications
    and actions have no foreign key to the channel: HQ keeps its history after an owner
    deletes a channel. The bridge catalog gains the matching capabilities (contract 2.1.0;
    reads `messaging.channel-verifications.list`, `messaging.channel-reports.list`,
    `messaging.channels.read`, `messaging.channel-actions.list`).
11. **Rate limits** (per person, Messaging's `messaging_rate_windows`): channel creation
    10/hour, posts 120/hour, follows 200/hour, discovery / suggestions / lookup 120/minute,
    reports 20/hour, forwards 30/hour, admin changes 60/hour, invite creation 30/hour, invite
    preview / join 30/minute.

## Activation (a separate release step; nothing here is switched on in production)

1. Deploy (the migration runs with the release). The customer features work at once for CF
   Meet clients through the relay; verification and moderation stay unavailable until HQ
   acts, so every channel is unverified and ACTIVE.
2. The admin bridge serves the new capabilities only with `ADMIN_BRIDGE_ENABLED`,
   `ADMIN_BRIDGE_MODULE_MESSAGING` and, for commands, `ADMIN_BRIDGE_WRITES_ENABLED` — all off
   by default (ADR 0068). Messaging must already grant `cfmeet.admin-bridge`
   `messaging-firstparty.administer` (it does for the A2 / A4 messaging reads and commands).
3. HQ re-pins the bridge contract at the merged commit (contract 2.1.0), adds the read
   permissions `messaging.channel-verifications.read`, `messaging.channel-reports.read`,
   `messaging.channels.read`, `messaging.channel-actions.read` and the five command
   permissions to its grant model (roles: `cfmeet.verification.reviewer` decides
   verifications; `cfmeet.trust_safety` moderates), builds the review screens, and requests
   the module switch through the normal activation request.

## Known limitations

* Channel content is stored server-side (not end-to-end encrypted), like statuses.
* No push notifications for new channel posts yet (clients poll; mute is honoured by
  clients). A notification catalogue type would be a later, separately activated step.
* Document media is recognised by content but not virus-scanned (as status media, ADR 0069
  decision 5); the existing attachment scanner could be added later.
* Trending is ranked rather than keyset-paged (at most 200 entries).

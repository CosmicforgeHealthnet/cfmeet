# ADR 0073 — Chat-list indicators, unified Starred, section menus and status replies for the author

**Status:** Accepted
**Date:** 2026-11-28
**Builds on:** [ADR 0069](0069-status-and-channels-in-messaging.md) (Status and Channels in Messaging). Events ([ADR 0071](0071-events-in-messaging.md)) and the complete Channels ([ADR 0072](0072-channels-complete.md)) ship in their own changes; this one leaves hooks for both.

## Context

The owner's brief (2026-11-27, sections 3, 4 and 18–20) asks for:

* chat rows that show drafts, mentions, voice / photo / video / document / call indicators,
  status reply and reaction indicators with a contextual preview, group activity, and
  real-time ordering without new polling, keeping the E2EE and multi-device guarantees;
* one Starred experience for personal, group, community and channel content, with search,
  filters, jump to the original, removal and cross-device sync, never bypassing access;
* a ⋮ menu per section (Updates, Chats, Communities, a channel, My status) listing only
  what the person may do;
* a list of a status's replies for its author (today a reply is only a chat message), and
  the delivery of "mentioned you" / "shared your status" notices.

Much of the chat list already existed (drafts kept per chat in the browser, the `@` badge
from the server's per-member `mentionCount`, attachment and poll icons, realtime ordering
from the inbox stream). The account already has a per-person starred store: the
Communications Hub's saved messages, checked with Messaging (MS-1b) on every read and
synced through the Hub's change feed.

## Decision

1. **Status replies are recorded for the author, in Messaging, in their own history.**
   `messaging_status_replies` (status, author, replier, conversation, message, times; no
   content) in the new `messaging_status_replies_seaql_migrations` history, which the
   previous image never reads, so a rollback still boots (nothing is appended to
   `messaging_social_seaql_migrations`). `POST …/replies` writes the record after the reply
   message is sent (idempotent per message). `GET /v1/messaging/status/{statusId}/replies`
   (`listStatusReplies`, `messages.read`) answers only the author of a live status (404 for
   everyone else) and reads each reply through the author's own membership of the chat: a
   deleted, expired, hidden or out-of-window message, a replier the author blocked, or a chat
   the author left is not listed. The record goes with its status (foreign key, deletion and
   the 24-hour sweep, which also removes any left past the status's expiry). "My status"
   counts replies (`replyCount`); viewers carry `reactedAt`
   (`messaging_status_views.reacted_at`, same history); rings carry `mentionedMe` /
   `mentionedMeUnseen`. Contract: `cfmeet.messaging.http-api` 1.28.0, common 1.40.0; excluded
   from the public gateway like every status operation; the Meeting relay gains the
   pass-through ROUTES entry only.
2. **Starred reuses the existing stores; nothing is copied.** Chat and group messages
   (communities reuse groups) are the Hub's saved messages: per account, the same on every
   device, checked with Messaging on every read. An item the Hub reports unavailable
   (deleted, expired, hidden, cleared for me, chat left) is never shown. Adding a second
   message-star store in Messaging would duplicate an endpoint and a sync path (brief §22).
   Channel posts use the channel star store of ADR 0072 once it is deployed (the screen
   takes it as a source; until then the Channels chip is absent). Messaging's MS-1b access
   check is now covered by an integration test for deleted, hidden, expired and left
   messages. The screen (`/dashboard/starred`) searches name and text, filters by Chats /
   Groups / Channels, jumps to the original (`/dashboard/messages?conversation=…&message=…`
   scrolls to it) and removes. A private chat's message shows its text only on a device that
   decrypted it; a synced chat's text is the member read of that message.
3. **Chat-list indicators come from what the client already holds.** A call is the Hub's
   latest timeline card (`/inbox` `latestActivity`, which now also says `callKind`
   VOICE / VIDEO for a CALL card); a status reply or share is recognised from the
   server-written text the row already shows; a reaction to my status comes from "My status"
   (read when Chats opens and on focus, no interval of its own); a status mention from the
   rings already loaded; a group's "You created this group" from the conversation. A newer
   call or reaction replaces the preview and counts as activity for the order; ties keep the
   engine's realtime order. No content leaves the device and no new polling is added.
   Drafts stay in the browser only: no draft store exists server-side, and sending a private
   chat's draft to a server would break its end-to-end encryption.
4. **Section menus are data.** `lib/section-menus.ts` builds each section's items from what
   the person may do (signed in, owner / follower, read receipts, a retryable upload) and
   from which sections exist on the build (`DEPLOYED`: Events and Communities off until
   their screens ship, so no menu leads to a missing page). Updates, Chats, My status and a
   channel use it; `communitiesMenu` is the hook the Communities change wires.
5. **Status notices get their catalogue types, all off.** `messaging.status.mentioned` (#46)
   and `messaging.status.shared` (#47) in the Notification Service catalogue,
   `packages/notification-client` and the templates (event contract 1.9.0, common 1.8.0,
   inbox action `OPEN_STATUS`: the bell opens `/dashboard/updates?status=<id>`, which opens
   only if the feed, the server's gate, has it). Routing category `MESSAGING_STATUS`
   (in-app, push) seeded `OFF`; #43–#45 belong to ADR 0071.

## Activation

Nothing is enabled. To deliver status notices, follow
[`docs/operations/messaging-status-notices-activation.md`](../operations/messaging-status-notices-activation.md):
first switch the Notification Service routing rows (`activate-notifications-routing`,
`messaging-status-in-app-shadow` → `messaging-status-in-app-active`, optionally
`messaging-status-push-active`), then set `MESSAGING_STATUS_NOTICES=on` on Messaging. The
rest of this change is live with the deploy: the new Messaging history is applied by
`messaging-migration up`, and this image requires it before it reports ready.

When Events (ADR 0071) is deployed, set `DEPLOYED.events` to `true`; when the channel star
store (ADR 0072) is deployed, pass it to the Starred screen and add the channel menu's
notifications, report and management entries.

## Consequences

* A reply record outlives its message only until the next list read or the status's end;
  the list re-reads every message, so it never shows a deleted one.
* The Starred screen shows only what this device can read for private chats; the item is
  still listed (and jumps to the chat) when the text is not available here.
* Reaction indicators refresh when Chats opens or regains focus, not instantly: Status has
  no realtime push yet (ADR 0069).

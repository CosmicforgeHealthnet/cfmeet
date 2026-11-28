# Messaging status notices: activation runbook

Status mention and share notices (ADR 0069) reach people through the Notification Service.
This change only adds the catalogue types, routing rows and templates. **Nothing is enabled:**
both routing rows are seeded `OFF`, and Messaging writes no notice while
`MESSAGING_STATUS_NOTICES` is unset.

## What exists

| # | Event type | Sent to | Inbox / push text | Opens |
|---|---|---|---|---|
| 46 | `messaging.status.mentioned` | a person mentioned in a status who is inside its audience | "Mentioned in a status" | the status (`OPEN_STATUS`) |
| 47 | `messaging.status.shared` | the author, when someone forwards or reshares their status | "Status shared" | the status (`OPEN_STATUS`) |

- Producer `cfmeet.messaging`, priority NORMAL, exactly one `cfmeet-user` recipient, nothing
  sealed. Subject `STATUS` (the status id). Data: `statusId` only, never status content.
- Event contract `cfmeet.notifications.event` 1.9.0, common contract 1.8.0 (inbox action
  `OPEN_STATUS`). Catalogue numbers #43–#45 and contract versions 1.8.0 / 1.7.0 belong to
  ADR 0071 (Events).
- Routing category `MESSAGING_STATUS`, channels `IN_APP` and `PUSH` (no email), preference
  category `DIRECT_MESSAGES`. The rows are created `OFF` by the Notification Service migration
  `m20261220_000001_messaging_status_notices`.
- The web bell opens `/dashboard/updates?status=<statusId>`. The status opens only while the
  reader may still see it (live, in the audience, still a mutual contact). Otherwise
  Messaging answers 404 and nothing is shown.
- Messaging re-checks every notice before it relays it. A status that ended or was deleted,
  or a mention that was removed, is `SKIPPED`. Notices expire after 24 hours.

## Before you start

1. The Notification Service image that has the migration above is deployed, and
   `notif_routing` has the two `MESSAGING_STATUS` rows in mode `OFF`.
2. Messaging already relays to the Notification Service, so `activate-messaging-publications`
   has run. That gives Messaging `CF_NOTIFICATIONS_BASE_URL`. `MESSAGING_STATUS_NOTICES=on`
   does nothing without it.
3. To use push, `activate-notifications-push` has run.

## Activation (in this order)

1. **Routing rows, in the Notification Service.** Run the coordinated release
   (`cfmeet-coordinated-release.yml`) with operation `activate-notifications-routing`. Each
   run changes one row, named in `confirm` as `<category>-<channel>-<mode>`:
   - `messaging-status-in-app-shadow`, then check the shadow evidence;
   - `messaging-status-in-app-active`;
   - optionally `messaging-status-push-active`.

   The journal records each row's previous mode, and `rollback` restores it. The service
   reads the table on every event, so no restart is needed.
2. **Producer switch, in Messaging.** Set `MESSAGING_STATUS_NOTICES=on` in the Messaging
   service environment and restart Messaging. No workflow operation sets this variable yet,
   so add it to Messaging's release environment the same way other Messaging feature
   switches are set. From then on, publishing a status with mentions, forwarding it and
   resharing it each write a notice.

The order matters. If Messaging is switched on first, notices are relayed while the rows are
still `OFF`, and the Notification Service holds or drops them instead of delivering them.

## Rollback

- Stop new notices: unset `MESSAGING_STATUS_NOTICES` (or set it to anything other than `on`)
  and restart Messaging. Notices already written expire within 24 hours.
- Stop delivery: run `activate-notifications-routing` with
  `messaging-status-in-app-off` (and `messaging-status-push-off`), or use `rollback` on the
  journal entry.
- The Notification Service migration's `down` refuses while either row is not `OFF`, or while
  an inbox item still opens a status.

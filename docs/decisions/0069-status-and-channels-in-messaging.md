# ADR 0069 — Status and Channels belong to Messaging

Status: Accepted (product owner decision 2026-11-27: the meeting app and the messaging app are
separated; Status and Channels must not live in the Meeting Service).

## Context

WhatsApp-style Updates (24-hour statuses and broadcast channels) first shipped inside the
Meeting Service (`/api/v2/social/*`, migration `m20261127_000001_social_updates`, media in
512 KiB Postgres chunks, audience = the poster's Communications Hub contacts read with the
viewer's session). That couples a messaging feature to the meeting app and keeps media bytes
in a relational database.

## Decisions

1. **Messaging owns Status and Channels.** They live in `services/messaging/src/social/`
   (statuses, audiences, media, channels, notices), in Messaging's own database, under its
   own additive migration history `messaging_social_seaql_migrations`
   (`m20261217_000045_status_channels`; a previous Messaging image never reads it, so a
   rollback still boots). The customer API is `/v1/messaging/status/**` and
   `/v1/messaging/channels/**` in `cfmeet.messaging.http-api` 1.25.0 (Messaging's existing
   style: non-conversation resources live under `/v1/messaging`). The operations are
   `EXCLUDED` from the public API Gateway (`FIRST_PARTY_STATUS_CHANNELS`): audiences are CF
   Meet personal contacts.
2. **Reached through the relay only.** The CF Meet clients call
   `/api/v2/messaging/relay/v1/messaging/status/...` and `.../channels/...`; the Meeting
   Service holds nothing but allowlist entries (`relay.rs` `ROUTES`, idempotency-key
   forwarding for status creation, reshares and channel posts) and forwards with the
   signed-in account's principal. The Meeting Service implementation, routes, sweep and tests
   are deleted. Its `social_*` / `status_*` tables are **retired but kept** (expand /
   contract, as ADR 0032 does for retired tables): the previous Meeting image still uses
   them during a rollback, and they hold only test data that expires. Dropping them is the
   contract step of a later release.
3. **Audience = mutual, unblocked contacts, decided by the Communications Hub.** Contacts
   and personal blocks belong to Communications (one symmetric `communication_connections`
   row per pair; `CONNECTED` means both accepted). Messaging reads no other database: it
   asks Communications live, with its existing `communications-policy.check` permission
   (no new trust grant), through a new internal operation
   `POST /internal/v1/communications/contacts/eligible` (Messaging only;
   `packages/communications-policy` `PolicyClient::eligible_contacts`). The answer lists
   the actor's contacts whose connection is CONNECTED, held in BOTH contact lists, both
   profiles ACTIVE, with no active block either way from any source, plus `eligibleSince`
   (the later of the connection time and the pair's last block change). A live call was
   chosen over an event-fed projection of `communications.connection.*` events: it is
   authoritative from the first request (no backfill, no cold projection), revocation is
   immediate, and no second contact store exists. Communications unreachable fails closed
   (`503`; the feed shows only the caller's own statuses).
4. **Snapshots, one gate.** Publishing resolves the chosen audience (`CONTACTS`,
   `CONTACTS_EXCEPT`, `ONLY`, `CLOSE_FRIENDS`, `CUSTOM` named list; default: the caller's
   saved preference) against the CURRENT eligible contacts into a stored snapshot (mode,
   label, principals, excluded count, `audience_as_of` = the latest `eligibleSince` among
   them). Items of one `groupId` share the first item's snapshot. Every read by someone else
   (feed, detail, rings, media grants, views, reactions, replies, forwards, reshares,
   viewers) passes one gate: live, in the snapshot, still eligible with `eligibleSince ≤
   audience_as_of` (so removing / re-adding a contact or blocking / unblocking never restores
   access to earlier statuses, and newly added contacts never see them), no Messaging block
   (a Messaging block also removes the pair from each other's snapshots for good), and for a
   reshare the same gate on the original. Anything else is `404`. Mentions are kept only
   inside the snapshot; only the mentioned person sees `mentionedMe`.
5. **Media in protected object storage.** The attachment system's shape does not fit
   (attachments belong to a conversation and its membership), so status / channel media is
   the smallest store on the same pattern and the same bucket (`state.storage`, ADR 0023 /
   0050): one object per 4 MiB part under `social/{environment}/{media}/{part}`, each
   uploaded with a signed single-object write fixed to its size and SHA-256 (resumable: the
   server lists the parts it holds; idempotent per part; parallel; cancellable), completed
   only when every part is stored and the content is recognised by its magic bytes, and read
   through 5-minute signed part reads minted only after the gate. Limits: IMAGE / AUDIO
   8 MiB, VIDEO 64 MiB (a video status shows at most 60 s). A sweep removes statuses after
   24 hours (with reshares), unfinished uploads and unused media after 24 hours, and a
   deleted status's media at once, bytes included. No scanning (unlike MANAGED attachments):
   status media is shown only to mutual contacts; adding the existing scanner is possible later.
6. **Idempotency, notices.** Status creation, reshares and channel posts take an optional
   `Idempotency-Key` stored in Messaging's existing `messaging_idempotency_keys` (24 hours;
   the acting principal is part of the digest). Mention and share notices are written to
   `messaging_status_notices` and relayed to the Notification Service only with
   `MESSAGING_STATUS_NOTICES=on`; the catalogue types `messaging.status.mentioned` /
   `messaging.status.shared` must first be added to the Notification Service catalogue and
   `packages/notification-client` and their routing activated (off by default).

## Rollout

The web app deploys minutes after a merge to main; the backend about an hour later, after
full CI, and nothing orders the two.

1. **Transition window (TRANSITIONAL web fallback, one release).** Until the backend is
   deployed, the Meeting relay refuses the new routes with `404 ENDPOINT_NOT_FOUND`
   ("not available to CF Meet clients"). The web client (`apps/web/src/services/v2/social-backend.ts`)
   then serves the same operation from the previous `/api/v2/social/*` API
   (`social.legacy.ts`; the publishing queue through `status-publisher/legacy-transport.ts`
   with its degraded capability flags) and remembers that for the session (sessionStorage).
   Only that exact answer switches: a Messaging `404 NOT_FOUND`, a `403` privacy denial or any
   other error is never retried elsewhere (`apps/web/tests/social-backend.test.mjs`). Once the
   backend is deployed the old routes answer `404 ENDPOINT_NOT_FOUND` in turn and the client
   switches back. Messaging-only reads (rings, audience, close friends, lists, mutes) answer
   empty on the old backend. The web parity gate tolerates these `/api/v2/social` calls from
   `social.legacy.ts` only. **TODO: remove `social-backend.ts`, `social.legacy.ts`,
   `legacy-transport.ts`, `switching-transport.ts`, their test and the parity-gate exception
   after #193 is deployed.**
2. **Existing channels (one-time, idempotent copy).** Same pattern as the organizations
   cutover (`export-organizations` → `organizations-import`): the source exports a read-only
   snapshot of its own tables and the destination imports it into its own database; neither
   reads the other's database. Statuses expire within 24 hours and are not copied.
   * Export (meeting-service image, `export-social-channels`, one `REPEATABLE READ, READ ONLY`
     transaction): channels, followers, posts, reactions and the posts' complete media
     (whole, base64), people as their Messaging principals from
     `communication_principal_links`; people without a principal (never used Messaging) are
     skipped and counted; a SHA-256 checksum over every record.
   * Import (`messaging-admin social import-channels --file FILE|- --environment UUID
     --operator ID`): refuses another schema, another environment or a checksum mismatch
     without writing; keeps every id; checks media like an upload (kind, type, size,
     recognised content, SHA-256) and stores it in 4 MiB parts in the protected bucket before
     recording it; every insert is `ON CONFLICT DO NOTHING`, so a repeated run reports
     everything `unchanged`; audited as `SOCIAL_LEGACY_IMPORT`; prints counts and ids only.
   * **Release steps (the normal path).** After the backend release, dispatch
     `cfmeet-coordinated-release.yml` with `social-channels-export`, then
     `social-channels-import` (`infra/calling/production/coordinated_release.py`, in the
     style of `org-cutover-export` / `org-cutover-import`). Both refuse unless every service
     runs the released images. The export runs the Meeting backend image's
     `export-social-channels` with only `DATABASE_URL` and the three communication scope ids,
     into a 0600 root file, and reports counts and the checksum only. The import streams that
     file on stdin to `messaging-admin` inside the running Messaging container, runs it a
     second time, and fails the step unless the second run imports nothing and reports every
     record `unchanged` (and the same refusals). The snapshot is overwritten and deleted
     afterwards whatever the outcome; export again to retry. Refused records stop the step
     unless the owner accepts them (`confirm: refusals-accepted`); their ids are in the
     `SOCIAL_LEGACY_IMPORT` audit entry. Both steps may be repeated later; they only add
     what is new.
   * **Manual fallback** (only if the release workflow cannot be used), on the VPS:

     ```sh
     # on the VPS, as root (export file 0600, never mounted into a container)
     umask 077
     docker run --rm --network container:cfmeet-production-backend \
       --env-file <(docker exec cfmeet-production-backend env | grep -E '^(DATABASE_URL|CFMEET_COMMUNICATION_(TENANT|APPLICATION|ENVIRONMENT)_ID)=') \
       --entrypoint /app/export-social-channels "$(docker inspect -f '{{.Config.Image}}' cfmeet-production-backend)" > social-channels.json
     docker exec -i cfmeet-messaging /app/messaging-admin social import-channels \
       --file - --environment <CFMEET_COMMUNICATION_ENVIRONMENT_ID> --operator <you> < social-channels.json
     # run the import again: every count must be under "unchanged"
     shred -u social-channels.json
     ```

     The export needs `DATABASE_URL` and `CFMEET_COMMUNICATION_{TENANT,APPLICATION,ENVIRONMENT}_ID`
     (already in the Meeting Service environment); the import runs inside the Messaging
     container (`messaging-admin` now ships in the image), which already has `DATABASE_URL`
     and `MESSAGING_ATTACHMENTS_S3_*`. Exit codes: 2 refused input (nothing written), 1 failure.

## Known limitations

* Status and channel content is stored server-side and readable by the server (text in
  plain columns, media unencrypted in the protected bucket). Nothing is labelled end-to-end
  encrypted. End-to-end encrypted statuses are a later phase.
* Status text is not sealed with the MANAGED keyring yet.
* No realtime push of status changes (clients poll the feed / rings); the Messaging
  WebSocket's per-principal events are the natural later channel.
* Channel owners' names resolve only for colleagues / contacts (the names API's rules).

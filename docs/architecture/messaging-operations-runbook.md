# Messaging operations runbook (Part V §136–§141, §165)

Scope: the Messaging service (`services/messaging`) as built in this repository. Commands
use the operator CLI `messaging-admin` (shell access to an instance and `DATABASE_URL`;
every state change names an operator with `--operator` or `CF_OPERATOR_ID` and is audited
in `messaging_operator_audit`). Nothing here deploys, migrates a hosted database or rotates
a live credential by itself; hosted procedures follow the platform change process.

## 1. What is durable where

| Data | Store | Backup | Notes |
|---|---|---|---|
| Conversations, members, messages (MANAGED sealed text, PRIVATE_E2EE ciphertext), outbox, relay, KT log, audit, usage | PostgreSQL (`messaging_*`) | `infra/deployment/backup/backup.sh --service messaging` (pg_dump custom format + SHA-256) | MANAGED search lexemes are in the database: protect backups like MANAGED content. |
| Attachments and derivatives | Object storage bucket (private) | Bucket replication / mirror by the platform (Messaging stores no object outside its bucket) | Restored rows without objects fail closed (`ATTACHMENT_NOT_READY` / `OBJECT_MISSING`). |
| Managed key provider material (`MESSAGING_MANAGED_KEYRING_FILE`) | Secret store | Secret store only — never in a database backup | Wraps the per-environment data keys (`messaging_managed_deks`). Without it MANAGED text is unrecoverable. |
| KT signing key, client-token signing key, push credentials | Secret store (`*_FILE` settings) | Secret store | |
| Typing, presence, realtime fan-out | Redis (`MESSAGING_REALTIME_REDIS_URL`) or in-process | None needed | Ephemeral by design. |
| PRIVATE_E2EE keys and plaintext | Client devices only | Client: history transfer or secure backup (`/v1/messaging/backups`) | The server can never restore them. |

## 2. Database backup and restore

1. Backup: `backup.sh --service messaging --database-url <url> --out messaging.dump`.
2. Restore into an EMPTY database: `restore.sh --database-url <url> --in messaging.dump`
   (verifies the checksum and the service's migration table).
3. Start a Messaging instance with the SAME managed key provider material and signing
   keys from the secret store. Workers resume on their own: relay rows and outbox events
   drain once (idempotent ids), the SMS worker never re-submits a transport the carrier
   already accepted (only QUEUED ones), retention / purge continue from the stored state.
4. Restore the bucket mirror before serving attachments.
5. Key transparency: `messaging-admin kt verify --checkpoint <file>` against the operator
   checkpoint taken AFTER the backup. A restored log is behind it: `KT_ROLLBACK_DETECTED`,
   device registration and other log appends fail closed until resolved (§4).

Evidence: `services/messaging/tests/disaster_recovery.rs` (both tests). No RTO / RPO is
claimed; measured durations are printed by the test.

## 3. Redis loss / realtime outage

Committed messages are never lost: delivery is from the PostgreSQL outbox. Clients
reconnect and resume from their cursors (`afterEventSequence`); typing / presence restart
empty. `MESSAGING_REALTIME_REDIS_URL` unreachable degrades to this instance's sessions.
Evidence: `tests/realtime.rs::committed_messages_survive_a_broker_outage`,
`tests/realtime_redis.rs`.

## 4. Key-transparency rollback, fork or signing-key compromise

* Take a checkpoint after every release and regularly:
  `messaging-admin kt checkpoint --out <file>` (mode 0600, kept outside the database).
* `kt verify` exits 3 on rollback or fork and raises `KT_ROLLBACK_DETECTED` (CRITICAL);
  appends refuse until `messaging-admin incidents resolve --id <id>` after the cause is
  understood (restore of the correct log, or an accepted new checkpoint).
* Signing-key compromise: provision a new key (`MESSAGING_KT_SIGNING_KEY_FILE`) and log id
  (`MESSAGING_KT_LOG_ID`); clients pin the new key through their release channel; witnesses
  (`MESSAGING_KT_WITNESSES_FILE`) must cosign the new heads before strong claims resume.

## 5. Managed key compromise or rotation (Appendix J)

The managed key provider wraps per-environment data keys; message rows record the key id
(`dek:<uuid>`). Rotation adds a new provider key and new data keys for new writes; old
rows stay readable while their wrapping key exists. Compromise: rotate, then re-seal
affected environments with a maintenance job before retiring the old key (not automated
in this repository). The key never enters a database backup or a log.

## 6. Client-token key compromise

Replace `MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE` and restart: every outstanding token
fails verification at once. Revoking individual sessions:
`POST /v1/messaging/client-sessions/{id}/revoke`; revoking a device ends its tokens and
closes its connections.

## 7. Kill switches (§134)

`messaging-admin kill-switch list|engage|release --switch NAME [--environment UUID]
--reason CODE`. Fourteen independent switches (MESSAGE_SEND, CONVERSATION_CREATE,
E2EE_ONBOARDING, E2EE_CONVERSATION_CREATE, MANAGED_MESSAGING, ATTACHMENT_UPLOAD,
SMS_BRIDGE, WEBHOOK_RELAY, REALTIME_CONNECT, SEARCH, EXPORTS, MODERATION_ACTIONS, BACKUPS,
KT_PROOFS). A switch only stops a capability; none converts one profile into another.

## 8. Dead letters (§136)

* `messaging-admin dead-letters list` — counts for `outbox`, `webhook-relay`,
  `object-deletion`.
* `messaging-admin dead-letters replay --kind <kind> [--id UUID]` — re-queues with a fresh
  retry budget (audited). Workers are idempotent, so a replay never duplicates an effect.
  Fix the cause first (e.g. a refused webhook ingress), then replay.

## 9. Incidents and signals (§141)

`messaging-admin incidents list [--all true]` / `resolve --id UUID`. Categories:
OUTBOX_DEAD_LETTER, WEBHOOK_RELAY_DEAD_LETTER, SMS_TRANSPORT_FAILED,
ATTACHMENT_DELETE_DEAD_LETTER, KT_ROLLBACK_DETECTED, SCANNER_UNAVAILABLE,
RETENTION_BACKLOG, STORAGE_ACCOUNTING_DRIFT, ABUSE_WAVE. Metrics
(`MESSAGING_METRICS_ADDRESS`, separate listener): request latency histogram
`messaging_http_request_duration_seconds{method,route,status}`, outbox / relay / purge lag
gauges, database pool, device / KeyPackage / outbox status gauges, usage and abuse
counters. Labels never carry ids or content.

## 10. KeyPackage stock

The maintenance worker closes expired KeyPackages and emits
`messaging.device.key-packages-low` once per device below
`MESSAGING_KEY_PACKAGE_LOW_WATERMARK` (default 10) until the device publishes enough.

## 11. Scanner outage

MANAGED attachments wait in VERIFYING while ClamAV (`MESSAGING_CLAMAV_ADDRESS`) is
unavailable and are REJECTED `SCAN_UNAVAILABLE` after the retry budget (never READY
unscanned); `SCANNER_UNAVAILABLE` is raised. Restore the scanner; affected uploads must be
sent again.

## 12. Partial service outages

| Dependency down | Effect |
|---|---|
| Platform Access | New authentications fail closed (503); cached verifications expire (`MESSAGING_AUTH_CACHE_TTL_SECONDS`). |
| SMS Gateway | Bridged transports retry; in-app messaging unaffected. |
| Privacy Relay / context authorities | Context-bound operations refuse; existing conversations unaffected. |
| Object storage | Attachment intents / grants refuse (503); messages unaffected. |
| Webhook ingress | Relay rows wait (or dead-letter on permanent refusal); replay after recovery. |

## Carry-forward (not covered by this repository)

Hosted staging / production procedures, provider (APNs / FCM / carrier) onboarding,
third-party KT witnesses, independent cryptographic assessment, measured RTO / RPO.

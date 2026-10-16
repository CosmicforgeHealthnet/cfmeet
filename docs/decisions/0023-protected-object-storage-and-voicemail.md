# ADR 0023: Protected object storage and voicemail on the canonical Call

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §60 (§100–§114) and the artifact storage it shares with call
  recording (§61). Implemented in `packages/object-storage` (`cf-object-storage`),
  `services/calling/src/enterprise/voicemail.rs` (mailboxes, messages, authorized
  access), `services/calling/src/calls/app/enterprise/voicemail.rs` (routing, capture,
  delivery, retention), `services/telephony/src/capture.rs` (edge capture and delivery)
  and migrations `calling 000027` / `telephony 000008`. Builds on ADR 0018–0022.

## Context

Voicemail needs (1) audio captured from the caller of ONE canonical Call — SIP desk
phones on CF Meet's own edge and application (RTC) callers — and (2) somewhere private
and durable to keep it. The Meeting Service has its own artifact storage, but importing
its code or reading its tables would couple two services. Application nodes must never be
long-term storage, and a stored URL or object key must never become the access
mechanism.

## Decision

### Storage: a neutral package, a private bucket

* `packages/object-storage` (`cf-object-storage`) is a provider-neutral library with no
  business vocabulary and no service types: `ObjectStore` = put / head / delete
  (idempotent) / signed read (≤ 900 s). `S3ObjectStore` speaks the S3 REST API with AWS
  Signature V4 (header-signed writes, query-signed reads, optional SSE per object, path
  style for MinIO); `MemoryObjectStore` is for tests (failure injection). Each service
  embeds it with its OWN bucket and credentials (`CALLING_ARTIFACT_STORAGE_*`); nothing
  is shared with the Meeting Service except this library's code.
* Without configuration Calling uses `UnconfiguredStore`: voicemail delivery fails
  closed (`ARTIFACT_STORAGE_UNAVAILABLE`, the edge retries) and media access is refused.
* Calling stores only an internal object key (`voicemail/<tenant>/<app>/<env>/<id>.wav`,
  never returned), content type, size, duration and SHA-256. No API, event, log, metric
  or audit record carries the key or a URL.

### Voicemail on one Call

* `voicemail_mailboxes` (owner: PRINCIPAL / EXTENSION / QUEUE / RING_GROUP of the scope,
  max length 5–600 s, retention 1–3650 days, versioned, audited, never deleted) and
  `voicemail_messages` (one per originating Call — `UNIQUE (call_id)`; RECORDING →
  PROCESSING → READY, FAILED, DELETING → DELETED enforced by a trigger; UNREAD / READ;
  rows are never hard-deleted).
* The router's VOICEMAIL destination (forwarding NO_ANSWER / BUSY / UNAVAILABLE / ALWAYS,
  queue overflow, schedule closed route, IVR `ROUTE_VOICEMAIL`, ring-group fallback,
  extension) creates the message on the SAME Call (context phase VOICEMAIL, routing step
  `VOICEMAIL / RECORDING`, event `VOICEMAIL_RECEIVED`). The `VOICEMAIL_ENABLED` kill
  switch refuses NEW voicemail (`NO_ROUTE / VOICEMAIL_DISABLED`); it never stops
  delivery, retention or deletion.
* **SIP caller:** Calling enqueues ANSWER, PROMPT `VOICEMAIL_GREETING` and a
  provider-neutral `CAPTURE` command (capture id, purpose, max length, beep) for the
  device leg. Telephony records at the Asterisk edge (ARI `record`, WAV) into the edge's
  TRANSIENT spool under its own name `cfm-cap-<id>`; on `RecordingFinished` (also after a
  caller hang-up) it fetches the file, computes size / duration / SHA-256 and delivers it
  to Calling (`POST /internal/v1/telephony/captures`, permission
  `telephony-capture.deliver`, contract `cfmeet.telephony.capture`). Calling accepts
  only a SIP capture it asked for on that leg, verifies integrity, uploads, and the
  message becomes READY (`VOICEMAIL_READY`). Only after Calling's 200 does Telephony
  delete the spool copy; 503 retries with backoff; refusals or exhausted attempts
  dead-letter (`CAPTURE_DELIVERY_DEAD` incident) and still delete the spool.
* **Application caller:** the neutral `CallMediaCapture` interface (no Meeting Service
  type). The deployment default is `UnavailableCapture`: an RTC caller reaching voicemail
  is refused with `VOICEMAIL_CAPTURE_UNAVAILABLE` (never a fake message). A media-server
  egress adapter can implement it and deliver through `deliver_capture`.
* Storage failure never fakes READY: the message stays PROCESSING with a bounded failure
  code and the deliverer retries; a capture that never arrives is FAILED
  (`CAPTURE_LOST`) after its deadline + 30 min; a later delivery still recovers it.
* The Call ends NO_ANSWER with the enterprise outcome VOICEMAIL when the message is
  stored (or at its recording deadline); a caller hanging up mid-message makes it
  CANCELLED with outcome VOICEMAIL, and the audio that arrives still becomes READY.

### Access

* Messages are listed / read by the mailbox owner (the application passing
  `principalId`; the principal must own the mailbox — else 404) or by a voicemail
  administrator (`enterprise.voicemail.manage` without `principalId`). Another scope's
  message is 404. Mutations (read state, delete) need `enterprise.voicemail.manage`.
* Media access is authorized FIRST, then a read-only signed URL of ONE object valid for
  300 s is minted, audited (`MEDIA_ACCESS`) and never stored.
* Deletion is asynchronous and idempotent: DELETING (audited, reason USER / ADMIN /
  RETENTION) → object removed (retried with backoff while storage is down) → DELETED
  (`VOICEMAIL_DELETED`). Retention moves READY messages past `retention_until` to
  DELETING as the system. The originating Call's history is untouched.

## Certification

* `cf-object-storage`: SigV4 test vector, presign, memory store; live S3 test against
  MinIO (CI) / any S3-compatible server.
* Calling `tests/enterprise_voicemail.rs`: administration, routing (no answer, queue
  overflow, after hours, IVR), delivery and integrity, idempotent re-delivery, storage
  failure recovery, lost / refused captures, authorization (owner / administrator /
  other principal / other tenant), audit, read state, keyset paging, deletion and
  retention, kill switch, the internal endpoint's authorization.
* Telephony `tests/enterprise_capture.rs`: CAPTURE acceptance rules, spool naming,
  delivery with integrity, retry / dead-letter / spool cleanup, lost end events.
* Real stack `tests/enterprise_sip_certification.rs` scenario 5: SIPp desk phone → real
  Asterisk → IVR key 7 → mailbox → greeting → real G.711 audio recorded at the edge →
  delivered by the real Telephony binary → stored in a real S3-compatible bucket →
  READY → authorized signed retrieval of the same WAV → read → deletion (object gone,
  edge spool empty).

## Consequences

* RTC voicemail capture requires a media capture adapter (e.g. a LiveKit egress); until
  one is deployed, RTC callers are routed past voicemail with an explicit NO_ROUTE
  reason. This is recorded as a deferred item, not simulated.
* Recording (§61) reuses `cf-object-storage`, the capture delivery path and the access
  pattern, with its own policy, consent and tables.

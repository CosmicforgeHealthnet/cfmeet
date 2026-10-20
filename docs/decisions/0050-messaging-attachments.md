# ADR 0050 — Messaging attachments: protected storage, client-side E2EE, verification, scanning, quotas

Status: Accepted (Part V Part F, §69–§80). Builds on ADR 0023 (protected object storage)
and ADR 0045–0049.

## Decisions

1. **Reuse `packages/object-storage`.** A PRIVATE S3-compatible bucket; PostgreSQL holds
   authorized metadata and a SERVER-GENERATED key `att/<environment>/<conversation>/<id>`
   only. Signed single-object uploads are bound to the exact `content-length`, SHA-256
   (`x-amz-checksum-sha256`) and content type, so the store itself rejects other bytes
   (proved against real MinIO). Unconfigured storage fails closed.
2. **State machine.** `INTENT_CREATED → VERIFYING → READY | QUARANTINED | REJECTED`, or
   `EXPIRED`, then `DELETED`; terminal states enforced by a trigger. Finalization
   re-verifies size and digest of the stored object. Only the sender's READY,
   not-yet-used attachments of the same conversation can be referenced by a message
   (linked in the message's transaction).
3. **PRIVATE_E2EE (AD-08).** The client encrypts BEFORE upload
   (`cf_messaging_crypto::attachment`: XChaCha20-Poly1305 STREAM, 64 KiB chunks, fresh
   key, chunk AAD binds a random client content id because the ciphertext digest must be
   declared before the server assigns the attachment id). The key, file name, media type
   and sizes travel only in the MLS-protected descriptor. The server stores ciphertext
   (`application/octet-stream`, no name, no metadata — a CHECK enforces it), never sends
   it to a scanner and reports `NOT_APPLICABLE_ENCRYPTED`, never "scanned".
4. **MANAGED.** Content-type security (per-kind allow-lists, magic-byte agreement, active
   content refused, prohibited and double extensions, sanitized presentation names) and
   fail-closed self-hosted ClamAV (`INSTREAM`): CLEAN → READY, INFECTED → QUARANTINED,
   unavailable → VERIFYING with bounded retries then REJECTED — never READY unscanned
   where policy requires scanning. Voice-note / video durations are declared and marked
   unverified (no media processing pipeline yet; `processing` defines its contract).
5. **Quotas** are reserved atomically at intent time (environment storage budget,
   concurrent uploads, daily volume — the latter never refunded) and settled on
   finalization / expiry / deletion.
6. **Downloads** re-authorize every time: current membership, history window, message
   ACTIVE, attachment READY. Grants are short-lived signed reads forcing
   `Content-Disposition: attachment` and the stored type (residual window stated in the
   response); the proxied read re-checks on every request and adds `nosniff`, `no-store`
   and a sandboxing CSP. A removed member gets no fresh grant.
7. **Lifecycle workers**: expired intents, abandoned uploads, rescans, a deletion queue
   that marks objects under an active legal hold `HELD` (never deleted) and dead-letters
   failing deletions visibly, orphan reconciliation of `att/`.

## Consequences

The malware scan covers MANAGED bytes only, with whatever signatures the deployment's
clamd loads (CI loads only an EICAR test signature). No claim is made about E2EE content.

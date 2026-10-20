# ADR 0054 — MANAGED media derivatives, storage quota and deferred processors

**Status:** ACCEPTED (Part V engineering policy, 2026-10-20)  
**Scope:** Unified Messaging, MANAGED attachments only.  
**Implementation reference:** staging commits `7dee2b4` through `71cc391`; baseline evidence in `docs/phases/phase-05/part-k-remediation-evidence.md`.  
**Reference specification:** CF Meet Part V Secure Unified Messaging & E2EE v2.0, §§78–80.

## Decision

1. **Account for actual object bytes.** Each READY server-generated MANAGED `THUMBNAIL` contributes its actual stored bytes to the environment's committed attachment-storage usage. The original attachment's bytes are counted separately. An `INSPECTION` result without an object uses zero object-storage bytes.
2. **Reserve and enforce atomically.** Storage quota decisions occur with the authoritative `messaging_storage_usage` row lock. The database derivative trigger commits/reverses charges transactionally as READY thumbnails appear, change or are deleted. Retried workers and historical backfill must not double-charge.
3. **Fail closed under a quota.** A derivative that pushes a previously full environment over its limit must not silently grant future uploads additional budget: subsequent reservations refuse until usage falls or the authorized environment policy raises the limit.
4. **Prevent orphaned charges.** Deleting original attachments and their derivatives (subject to legal holds) releases the correct committed bytes. Transactional deletion and job retries must retain exactly-once accounting.
5. **Never decrypt PRIVATE_E2EE media.** Server derivatives are exclusively for eligible MANAGED attachments. E2EE derivatives, if offered later, must be produced and encrypted on client devices and remain ciphertext on the server.
6. **Defer optional processing.** Part V §80 explicitly describes thumbnails, previews, transcodes, OCR and media intelligence as **future asynchronous processors**. Image thumbnails and bounded inspection are in the current engineering scope; transcoding, OCR and media intelligence are **not required** for Part V release unless separately approved and specified. They must not be represented as implemented.

## Amendment 1 — reserve before write, charges follow objects (migration 19)

Review of the first implementation found that a thumbnail was written first and charged
after, so an environment at its budget could be pushed over it, and that the object of a
crashed or failed attempt could survive uncharged. The decision is tightened:

7. **Reserve before writing.** After rendering, the worker locks the derivative row, then
   the environment's usage row (the lock order of every accounting trigger), and reserves
   the rendered size only if `committed + reserved + size ≤ budget`. Only then is the
   object written. The database trigger turns the reservation into committed bytes when
   the thumbnail becomes READY. Concurrent workers and upload intents serialize on the
   same usage row, so they cannot overshoot together.
8. **Truthful deferral.** A thumbnail that does not fit is `QUOTA_DEFERRED`
   (`errorCode: STORAGE_BUDGET`): no object is written or charged, the original attachment
   stays READY and downloadable, and the derivative is retried every 15 minutes or at once
   when the environment's attachment policy changes (an authorized administrative
   action). Quota deferral never consumes the failure-retry budget. Contract
   `cfmeet.messaging.common` 1.14.0.
9. **One object key per attempt.** Each render attempt writes
   `drv/{env}/thumbnail/{attachment}/{attempt}.png`. A retry never overwrites an object an
   earlier attempt may still reference; a crashed, superseded, failed or deferred
   attempt's object is moved to the deletion queue **with its reservation as the charge**.
10. **Charges follow the object until it is gone.** Logical deletion (user, abandoned
    upload, retention purge, erasure) moves an object's committed bytes — originals and
    derivatives alike — into its deletion request (`messaging_object_deletions.charged_bytes`).
    The bytes stay charged while the request is PENDING, HELD by a legal hold or DEAD
    (the object still exists) and are released by a trigger only when it is DONE.
11. **Reconciliation.** `quota::audit` recomputes usage from the rows it is made of
    (attachments + READY thumbnails + open deletion charges; intent and attempt
    reservations); the lifecycle worker reports drifting environments (metric
    `messaging_storage_accounting_drift_environments`); `quota::reconcile` repairs one
    environment under its lock (operator action, e.g. after a restore). Objects under
    `drv/` that no derivative and no open deletion references are queued for deletion.
12. **Existing data.** Migration 18 backfills READY thumbnails once (atomically with the
    trigger). Migration 19 changes no existing charge: objects already queued for deletion
    under the earlier rule were released then and are not charged retroactively.
    `regenerate_thumbnail` replaces a READY thumbnail with the old object still charged
    until deleted.

## Evidence and verification

- Database migrations: `services/messaging/migration/src/m20261120_000018_derivative_quota_accounting.rs`, `m20261121_000019_derivative_reservations.rs`.
- Storage-quota reservation: `services/messaging/src/attachments/quota.rs`.
- Derivative lifecycle: `services/messaging/src/attachments/derivatives.rs`.
- Integration test coverage covers thumbnail charging and release, idempotency, and rejecting uploads when accounted usage exceeds the configured budget.
- Messaging CI, security CI and unified integration CI all passed on `71cc391` (2026-10-20). This establishes the policy's **software evidence**, not a deployed production migration or a hosted-capacity certification.

## Acceptance interpretation

The **Part K media-processing software gate may be reevaluated for PASS** after final release review confirms the implemented §§78–80 scope and tests on the intended release SHA. Do not silently edit the older certification matrix (which recorded the earlier state). Do not treat this policy as authorization for production deployment, production database migrations or server access to E2EE plaintext.

Other outstanding Part K items—hosted capacity, real web/app MLS integration, external E2EE certification and owner production approval—remain independent and open.

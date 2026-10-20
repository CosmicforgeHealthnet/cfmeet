# ADR 0051 — Messaging search, reports, moderation and compliance

Status: Accepted (Part V Part G, §81–§96). Builds on ADR 0045–0050.

## Decisions

1. **Search.** PRIVATE_E2EE conversations are searched on devices only
   (`cf_messaging_crypto::local_search`, sealed local index); the server has no index of
   them and answers `LOCAL_SEARCH_ONLY` without logging or storing the query. MANAGED text
   is indexed with PostgreSQL full text (`simple`) and queried membership-aware at query
   time (ACTIVE memberships, history window, ACTIVE messages, delete-for-me), bounded and
   rate-limited. Tombstones leave the index in the tombstone's transaction.
2. **Permalinks** resolve through current membership every time (a reference is never
   authority); unknown, foreign and pre-window messages are the same `NOT_FOUND`.
3. **Reports.** Members report messages / principals with a category. For PRIVATE_E2EE
   the server holds metadata (ids, sequences, ciphertext digests, acceptance records)
   and — only if the reporter chooses — a disclosure of what the reporter's device
   decrypted, sealed at rest and always presented as `REPORTER_ATTESTED_UNVERIFIABLE`.
   MANAGED reports reference messages whose text moderators see only where policy
   (`moderation.managedEvidenceAllowed`) allows. The reporter learns only the receipt.
4. **Moderation** (scope `moderation.manage`, application administration only): remove
   message (tombstone `MODERATED`, then purge), mute / unmute, remove member, restrict /
   lift, dismiss report. Append-only records (trigger) and audit entries. No action reads
   or decrypts E2EE content.
5. **Retention and purge.** Policy retention (environment or context class) expires
   ACTIVE messages; tombstones are purged irreversibly (trigger) after the grace period,
   attachments deleted through the queue. Legal holds (conversation / principal /
   environment, administration-only, audited, never deleted) suspend purges and object
   deletion; they never create a decryption capability.
6. **Regulated contexts.** `regulated` requires an explicit
   `managedDisclosureAcknowledged` to create a MANAGED (server-readable) conversation;
   `centralArchiveRequired` allows MANAGED only — never an E2EE conversation the server
   could not archive.
7. **Exports** (scope `messaging.export`) are MANAGED-only (`E2EE_EXPORT_NOT_AVAILABLE`
   otherwise, audited), policy-gated per context, produced by a leased worker into
   protected storage, downloadable by short-lived grants, expired and deleted.
8. **Data deletion requests** (principal erasure, conversation purge) are executed by a
   leased worker: tombstone + purge + attachment deletion, with held items preserved and
   counted (`PARTIALLY_HELD`). MLS handshake records are retained (protocol state).
9. **Administrative audit API** lists the environment's append-only audit trail with
   filters and paging; details never hold content (database CHECK).

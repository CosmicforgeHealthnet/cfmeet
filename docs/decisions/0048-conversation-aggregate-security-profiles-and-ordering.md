# ADR 0048 — The Conversation aggregate: security profiles, policy snapshots, lifecycle and ordering

Status: Accepted (Part V Workstream D). Builds on ADR 0045–0047.

## Decisions

1. **One generic aggregate, eight context classes.** `DIRECT`, `GROUP`, `MEETING`,
   `TRANSACTION`, `CONSULTATION`, `SUPPORT`, `ENTERPRISE`, `DEVELOPER_DEFINED` — no
   product-specific table (repository guard `PRODUCT_DOMAIN_IN_MESSAGING`). A product's
   order / trip / consultation / case is an opaque `ContextRef` (application namespace
   `^[a-z][a-z0-9-]{0,62}$`, reference `[A-Za-z0-9._:/-]{1,256}`, version) that confers no
   membership or authority; Messaging never reads a product's database. A live context
   reference is unique per class; creating again returns the same conversation, and a
   conversation of another profile holding it is a conflict, never a merge.
2. **Authority from the credential only.** Tenant / application / environment come from
   Platform Access; requests cannot name them (`deny_unknown_fields`); every query is
   scoped by all three; another scope's conversation is `NOT_FOUND`.
3. **Persisted, structurally enforced security profile.** `PRIVATE_E2EE` rows carry
   `MLS_1_0`, the single cipher suite and a server-assigned 32-byte MLS group id (the
   client creates its MLS group with exactly that id); `MANAGED` rows carry
   `CFMEET_MANAGED_V1` and no MLS binding — a CHECK makes any other combination
   impossible. A trigger makes the profile, protocol, binding and security-policy version
   immutable; the API refuses a change with `SECURITY_PROFILE_IMMUTABLE`. Changing profile
   requires a new (successor) conversation — the successor workflow is a later workstream.
4. **Profile selection, never a fallback.** Effective policy = explicit platform defaults
   (PRIVATE_E2EE default for every context class, SUPPORT included — Appendix G;
   platform policy version 2 — MANAGED never available by default and never a default
   by itself; later-workstream features explicitly
   off / unset; legal hold eligible only where MANAGED is allowed; SMS Bridge never) merged
   with validated, versioned application overrides (`PUT /v1/messaging/policy`,
   `conversations.manage`, optimistic concurrency, audited, evented). A PRIVATE_E2EE
   conversation is created only when encryption can be established (E2EE device
   operations enabled; the creator — and for DIRECT the counterpart — has an active
   device); otherwise `E2EE_NOT_AVAILABLE` and nothing is written.
5. **Policy snapshots.** The effective policy is canonicalized (recursively sorted keys,
   independent of JSON library features), hashed (SHA-256) and stored once per
   environment and digest as an immutable snapshot that every conversation references.
   Policy changes never alter existing conversations.
6. **Lifecycle.** ACTIVE → LOCKED / CLOSED / ARCHIVED / EXPIRED / DELETED;
   LOCKED → ACTIVE / CLOSED / ARCHIVED / EXPIRED / DELETED; CLOSED → ACTIVE (when the
   snapshot allows reopening) / ARCHIVED / EXPIRED / DELETED; ARCHIVED → ACTIVE / DELETED;
   EXPIRED → ARCHIVED / DELETED; DELETED terminal and never physically deleted. EXPIRED
   is server-only (context expiry on the server clock: evaluated first in every update and
   by a `SKIP LOCKED` worker). Updates take `expectedVersion`; the row lock and the
   one-step version trigger make concurrent writers have exactly one winner.
7. **Creation** needs `conversations.create`, an acting principal (the owner) and an
   `Idempotency-Key` (claimed first in the transaction: concurrent retries wait and replay
   the stored response; a different body is `IDEMPOTENCY_KEY_REUSED`). DIRECT creation is
   deduplicated per principal pair, environment and profile by a partial unique index.
   Only the initial owner / counterpart association is stored (membership is E).
8. **Reads**: with an acting principal (`messages.read`) only the principal's own
   conversations; without one, the application's administration view
   (`conversations.manage`). Keyset pagination.
9. **Ordering foundation.** A transactional per-conversation counter in its own table
   (`allocate_sequence`; gap-free per committed transaction, never Redis-only; capacity
   certification later). Because MLS messages are encrypted before the server assigns
   sequence / id, the client authenticates only what it knows — the AAD (Appendix H.1)
   `cfmeet-messaging-aad:v2:PRIVATE_E2EE:MLS_1_0:<suite>:<tenant>:<application>:<environment>:<conversation>:<client message id>:<sender device>`
   (proven with a real two-device OpenMLS group: delivered intact; rewritten tenant,
   device or profile rejected) — and the server binds its sequence and message id to the
   SHA-256 of the exact ciphertext, the same scope and the profile / protocol / suite in a
   length-prefixed acceptance record (`AcceptanceRecord::digest`, label
   `cfmeet-messaging-acceptance:v2`). Revision 2 (18 October 2026, after the
   specification review): v1 bound only conversation / client message / device, short of
   H.1; no v1 value was ever persisted (the send path is Workstream F). A forged copy
   injected before the genuine message makes OpenMLS consume that generation's key, so
   the genuine copy fails too: a denial of service consistent with the G.2 threat model,
   never a confidentiality or integrity break.
   Storing acceptance records and the send path are Workstream F.
10. **Events.** `messaging.conversation.created`, `.lifecycle-changed`, `.updated` and
    `messaging.policy.updated` commit atomically with their state (transactional outbox).

## Open items (owner decisions / later workstreams)

The per-context defaults now follow Part V Appendix G (reviewed 18 October 2026; version 1
of the platform policy left SUPPORT without a default, which the specification does not
allow). Successor
conversations for profile changes; membership (E), send engine (F), realtime (I).

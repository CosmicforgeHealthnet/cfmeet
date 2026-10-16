# ADR 0032 — Platform Access: the shared identity authority

Status: Accepted (Part IV Workstream B). Supersedes the identity-ownership parts of
ADR 0003 / ADR 0004: Calling no longer owns tenants, applications, environments,
service accounts, API credentials, scopes or principals — it consumes them.

## Context

Part III left the platform identity hierarchy (Tenant → Application → Environment →
Service Account → API Credential, principals, scopes, first-party / external
classification) inside Calling. Part IV adds Numbering and later SMS / Messaging, which
must authenticate the same customers with the same credentials, and number
administration needs a privileged operator scope that only a first-party application may
hold. Duplicating the authority in each service — or letting Numbering read Calling's
tables — would create two sources of truth for who a caller is and would couple a new
service to Calling's database.

## Decisions

1. **One authority, its own service and database.** `services/platform-access` owns the
   identity hierarchy, credential issuance / rotation / revocation, the scope registry,
   principals, failed-authentication windows and an append-only operator audit
   (`platform_access_audit`) in its own PostgreSQL database (`platform_access_*`
   migration history). No other service reads or writes these tables; the boundary guard
   (`IDENTITY_AUTHORITY_ELSEWHERE`) refuses verifier code anywhere else.
2. **Credentials moved, not re-issued.** The Part III credential format
   (`cfmk_<sbx|test|live>_<24 hex>_<64 hex>`), the verifier construction
   (HMAC-SHA-256 under the pepper of `cfmeet.calling.api-credential.v1 ‖ tag ‖ public id ‖
   secret`, label unchanged), the keyring (current + previous pepper, versions), rotation
   overlap and revocation semantics are carried over byte-for-byte. Identifiers are
   preserved: a credential, tenant or principal keeps its UUID. The pepper moves to
   Platform Access only (`API_CREDENTIAL_PEPPER*`); Calling refuses to start with one in
   its environment.
3. **Authenticated internal contract.** Consumers call `/internal/v1` with
   `packages/service-auth` tokens and permissions from Platform Access's trust bundle:
   `platform-credential.authenticate` (Calling, Numbering),
   `platform-principal.resolve` (Calling, Numbering), `platform-principal.register`
   (Calling), `platform-identity.read` (Calling, Numbering) and
   `platform-identity.administer` (operator console only, sensitive). Every message is in
   `cfmeet.platform-access.identity`; routes in `cfmeet.platform-access.internal-api`
   (both checked against the implementation by `tests/contracts.rs`).
4. **The tenant is never taken from a request.** Authentication returns the full
   authority chain from Platform Access's rows; operations on behalf of a customer carry
   the signed delegated platform context, which Platform Access re-validates against its
   own chain (`DELEGATED_CONTEXT_INVALID` otherwise). A forged or stale tenant /
   application claim never becomes authority.
5. **Scopes.** One registry with three trust levels: STANDARD, FIRST_PARTY_ONLY and
   PRIVILEGED_OPERATOR. Part IV adds `numbers.search`, `numbers.read`, `numbers.manage`,
   `numbers.assign`, `numbers.kyc.read`, `numbers.kyc.manage`, `numbers.portability`,
   `sms.send`, `sms.read` (STANDARD) and `numbers.admin` (PRIVILEGED_OPERATOR: only an
   explicit, audited `grant-privileged` on a FIRST_PARTY application; a database CHECK
   ties it to `grant_basis = PRIVILEGED_OPERATOR`). Application scopes may be restricted
   to environment kinds.
6. **Consumers project, they do not own.** Platform Access records every committed
   identity change in a commit-ordered feed (`platform_identity_changes`, sequence taken
   under a transaction-level advisory lock, snapshots without verifier material).
   Calling keeps its `platform_*` tables as a read-only **projection** with the same ids
   (`pa_revision`-guarded upserts, cursor in `platform_identity_projection_state`), so its
   foreign keys, limits, enterprise configuration and call evidence are unchanged.
   Credential authentication is live (one internal call per request; the projection is
   refreshed from the returned chain); principal status follows the feed within
   `CALLING_IDENTITY_SYNC_INTERVAL_MS` (default 1 s) and on demand when a principal is
   not yet projected.
7. **Throttling moved with the authority.** Failed-authentication windows live in
   Platform Access (`PLATFORM_ACCESS_AUTH_FAILURE_THRESHOLD` / `_WINDOW_SECONDS`); the
   outcome tells the consumer when to answer 429 and when to raise its single guessing
   incident. The correct secret is never refused.
8. **Controlled hand-off, no dual write.** Cutover is an operator procedure with
   integrity checks at every step and no window in which two authorities issue
   credentials:
   1. deploy Platform Access (empty database, the Part III pepper);
   2. apply Calling migration 000044 (expand: projection columns, nullable verifiers);
   3. freeze identity administration on the Part III Calling;
   4. `platform_admin identity export --out F` (0600; keyed verifiers, never secrets;
      bundle digest + verifier digest + counts recorded in
      `platform_identity_handoffs`);
   5. `platform-access-admin identity import --bundle F` (one transaction, idempotent per
      digest, refuses conflicts, writes an import receipt and audit row);
   6. `platform_admin identity verify-handoff --digest D` (Calling compares Platform
      Access's receipt with its own export);
   7. deploy the Part IV Calling (authenticates through Platform Access);
   8. `platform_admin identity complete-handoff --digest D` (scrubs Calling's verifier
      material; only a verified hand-off can complete).
   Rollback before step 8: redeploy the Part III Calling. After step 8:
   `platform-access-admin identity export` → `platform_admin identity restore-verifiers`
   restores the byte-identical verifiers into credentials that have none (a verifier is
   never replaced), then migration 000044 can be rolled back (its `down` refuses while
   any credential lacks a verifier).
9. **Failure behaviour.** Platform Access unreachable → Calling refuses customer
   requests with the normalized 503 (fail closed; no cached authority). The projection
   worker retries with its interval; a principal missing from the projection triggers a
   catch-up before NOT_FOUND.

## Consequences

* Numbering (Workstream C) authenticates customers and reads identity through the same
  contract and never touches Calling.
* A Calling deployment holds no verifier secret; a stolen Calling database no longer
  contains credential verifiers after the hand-off completes.
* Every customer request costs one internal round trip to Platform Access. Accepted for
  correctness (revocation is immediate); a short-lived signed authority cache is a
  possible later optimisation and would need its own ADR.
* The old Calling identity code paths (issuance, rotation, revocation, verifier
  computation) are deleted, not disabled.
* Calling's `platform_auth_failure_windows` table is retired but kept until the contract
  step of a later release (expand / contract); it is empty and unused.

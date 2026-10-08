# ADR 0011: Contact Vault cryptography, equality, verification and resolution

- **Status:** Accepted
- **Date:** 2026-10-08
- **Scope:** CF Meet Part D §31–§33 (and the inbound caller protection of §35);
  implemented in `services/privacy-relay/src/contact_vault/` (Privacy Relay migrations
  000006–000008), the Contact Vault access token in `packages/service-auth`
  (`contact_delegation`), Calling's token minting
  (`services/calling/src/privacy_relay/contacts.rs`) and the contracts
  `cfmeet.privacy-relay.contact`, `.contact-delegation` and `.contact-resolution`.

## Context

A Communication Context (ADR 0010) can now name a party that has no application
endpoint: a private telephone number. That number must become a reference that CF Meet
can authorize, deduplicate, verify and — only for a specific authorized execution —
hand to a future Telephony service, while it never becomes ordinary business data: not
in Calling or the Meeting Service, not in contexts, sessions, calls, events, webhooks,
logs, metrics, traces or error payloads.

## Decision

### Storage boundary

The Contact Vault is a module of Privacy Relay (`src/contact_vault/`), not a new
service and not a library other services link. Only it reads or writes
`protected_contacts`, `contact_fingerprints`, `contact_access_audit`,
`contact_verification_events` and `contact_resolution_grants`, and only it uses the
phone-number parser, AES-GCM or the plaintext accessor (`expose_e164`), which
architecture tests enforce. Calling receives no vault database URL and no number: it
only mints access tokens and, for public-network calls, requests resolution grants.

### Canonical form and masked display

Input is screened (digits, `+`, spaces, `-`, `.`, `(`, `)` only; no URI schemes,
extensions or letters; at most 32 characters) and parsed by `phonenumber` (a port of
libphonenumber, with its metadata); only **valid** numbers are accepted and stored as
canonical E.164. The country is the library's region, with shared calling codes
resolved by the national number including significant leading zeros. The only display
ever returned is masked: `+<country code>` + six bullets + at most the last four
national digits (fewer for short numbers, so at least six national digits always stay
hidden). Raw and canonical values are `Zeroizing`, have redacted `Debug`, no `Display`
and no `Serialize`; every error is a fixed code without the value.

### Encryption

AES-256-GCM (`aes-gcm` crate; no hand-written cryptography) with a fresh random 96-bit
nonce per encryption (never deterministic encryption). The associated data binds the
ciphertext to its place: `cfmeet-contact-v1 ‖ tenant ‖ application ‖ environment ‖
ContactReference ‖ key version`, so a ciphertext copied to another row, scope or key
version fails authentication. Fixed-vector, tamper, wrong-AAD, wrong-key, wrong-version
and truncation tests run in CI.

### Keys outside the database

Keys are never stored in PostgreSQL, never hard-coded and have no production fallback.
A `ContactKeyProvider` (today `FileKeyring`) reads a JSON keyring file — `{
keyring_version: 1, purpose, active, keys: [{ version, key }] }` — named by
`CONTACT_VAULT_KEYRING_FILE` (encryption) and `CONTACT_FINGERPRINT_KEYRING_FILE`
(fingerprints). These are **different secrets**: the service refuses to start if a key
appears in both. With `CONTACT_VAULT_ENABLED=true` and any file missing or invalid the
process exits before serving; with the vault disabled, contact routes answer
CONTACT_VAULT_DISABLED (restrictive verification assertions still work). Operators use
`privacy-relay-admin contact-vault generate-keyring | add-key | activate-key` (files
written 0600, never printed). Images contain no keys; keyrings are mounted at runtime.

### Key rotation

Add V2 (V1 stays readable) → redeploy → activate V2 for writes → redeploy →
`privacy-relay-admin contact-vault reencrypt` (also a background worker; bounded
`SKIP LOCKED` batches) → `rotation-status` shows no V1 rows → remove V1. References,
versions and safe metadata never change; resolution keeps returning the same canonical
value (certified end to end).

### Equality without plaintext

`fingerprint = HMAC-SHA-256(fingerprint_key[v], "cfmeet-contact-v1" 0x1f tenant 0x1f
application 0x1f environment 0x1f e164)`. It is keyed (the number space is small enough
to enumerate, so no plain hash ever exists) and scoped (the same number in two scopes
yields unrelated fingerprints: no global identity, no cross-tenant existence signal).
`contact_fingerprints` stores one row per readable key version, unique per scope and
version; lookups match any version, so fingerprint rotation (add → backfill-fingerprints
→ activate → prune-fingerprints) never breaks deduplication. Creating an existing number
answers the existing ContactReference (200, audited `DEDUPLICATED`); an update to a
number another contact protects is CONTACT_ALREADY_EXISTS; disabling releases equality.
Creation is serialized per active fingerprint (transaction advisory lock).

### Delegated access

Customers never reach the vault with their platform credential. Calling authenticates
the credential and mints a short-lived (10–300 s, default 120 s) Contact Vault access
token (JWS, typ `cf-contact-access+jwt`, use `CONTACT_VAULT_ACCESS`) carrying the signed
delegated scope and only `privacy.contacts.read / write / verify` scopes the credential
itself holds, signed with a key dedicated to this purpose (never Calling's service-auth
key). Privacy Relay verifies it against its own contact trust bundle
(`CONTACT_DELEGATION_TRUST_BUNDLE_FILE`) in the `X-CF-Contact-Authorization` header; a
customer Authorization header, a service token or a foreign audience / environment is
CONTACT_DELEGATION_INVALID. A foreign or unknown ContactReference is the same 404.

### Verification trust model

States: UNVERIFIED (on creation and after every value change), PENDING, VERIFIED
(optionally until `validUntil`, at most 366 days), EXPIRED (derived at **read time**;
a worker later materializes it), INVALID and BLOCKED (never resolved, whatever earlier
events said). Only a trusted verification integration holding `privacy.contacts.verify`
asserts a state, with a bounded trust-source code and an opaque evidence reference —
never evidence JSON; ordinary writers cannot. Every change appends
`contact_verification_events`. Public-network relay authority over a contact requires an
enabled, effectively VERIFIED contact (CONTACT_NOT_VERIFIED / _VERIFICATION_EXPIRED /
_BLOCKED otherwise); a contact over the APPLICATION route is CAPABILITY_UNAVAILABLE.

### Contact Resolution Grants and Telephony-only plaintext

Calling (`protected-contact.grant-for-execution`) may request a grant for ONE call leg,
bound to the scope, contact, context, session, AUTHORIZED public-network decision and
their versions, call and call leg; at most 120 s and never past the decision's validity;
only the SHA-256 of the 256-bit grant secret is stored; a re-request revokes the earlier
grant. The grant carries no contact value. Only Telephony
(`protected-contact.resolve-for-execution`, receiver-authoritative: Privacy Relay's trust
bundle can grant it to no other service) exchanges a valid, unexpired, unconsumed grant
for the canonical E.164 — single execution, retry only by the same service with the same
execution attempt; any authority change in between denies. Calling cannot resolve.

### Inbound callers (§35)

A presented caller number goes from Telephony straight into the vault
(`protected-contact.protect-inbound`) in the scope of a recent RESOLVED inbound relay
resolution; it is deduplicated to an existing contact or encrypted as a new UNVERIFIED
one, and only the ContactReference comes back.

### Audit and fail-closed behaviour

`contact_access_audit` records every sensitive attempt — succeeded, denied or failed —
with actor kind / service / service account / credential id, ContactReference,
operation, purpose, execution references (context, session, decision, call, leg, grant),
outcome and a stable reason code; it is append-only and never contains a number. Missing
keys, a disabled vault, an unverifiable token or an unavailable database refuse the
operation; nothing falls back to plaintext. Logging tests capture every crate at TRACE
and prove no number, token, key, fingerprint or grant secret is logged.

### Backups

Privacy Relay database backups now contain encrypted Contact Vault data, fingerprints
and access audit and are sensitive; they are useless without the keyrings, which must
never be stored with them.

## Consequences

- CF Meet can name, deduplicate, verify and authorize protected contacts while no raw
  number exists outside the vault's ciphertext (certified by canary scans of both
  services' databases, responses and logs).
- Telephony, when built, gets plaintext only per authorized execution and per grant.
- Key loss makes contacts unrecoverable by design; keyrings need their own secret
  management and backup.
- Public-network execution is still not implemented (no Telephony, no carrier).

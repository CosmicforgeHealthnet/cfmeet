# Secret and key rotation runbook

Part H §74 / §75 / §39–§40 / §50. One page per secret class: where it lives, whether two
versions can overlap, the exact steps, and what is **not** seamless. Nothing here invents
automatic rotation where the architecture requires an operator; where a cutover needs a
restart, it says so.

Conventions used by every service:

* Secrets reach a process as a **mounted file** named by a `*_FILE` variable (preferred),
  or as an inline variable where the service accepts it. Inline private keys and inline
  ARI passwords are refused. No secret has a production fallback: an absent mandatory
  secret stops the process at startup (exit 2) in every runtime environment.
* Nothing secret is written to a database row, a log, a metric label, a public contract or
  a container image (`make secret-scan`; CI scans every service image's exported root
  filesystem in `dependency-security.yml`).
* Every rotation below is recorded by the operator (change ticket / operator audit where
  the CLI supports `--operator`).

| Secret | Owner | Overlap? | Runbook |
|---|---|---|---|
| API credentials (`cfmk_…`) | Calling | **Yes** (`--overlap-minutes`) | §1 |
| API credential pepper | Calling | **Yes** (one previous version) | §2 |
| Service-auth signing keys / trust bundles | all services | **Yes** (≤ 4 keys per issuer) | §3 |
| Contact-delegation key | Calling → Privacy Relay | **Yes** (same mechanism as §3) | §3 |
| Contact Vault encryption keyring | Privacy Relay | **Yes** (versioned; guarded retire) | §4 |
| Contact fingerprint keyring | Privacy Relay | **Yes** (versioned; backfill + prune) | §4 |
| Webhook secret-box key | Calling | **Yes** (current + previous, reseal) | §5 |
| Per-subscription webhook secret | customer ↔ Calling | **No** (immediate; receiver updates first) | §5 |
| Signaling signing key, media-edge key | Calling | **No** (restart; outstanding credentials invalid) | §6 |
| LiveKit API key / secret | Calling, LiveKit | **No** (coordinated restart) | §6 |
| ARI password | Telephony ↔ Asterisk | **No** (two files, reload + restart) | §7 |
| SIP trunk credentials | Asterisk edge ↔ carrier | carrier-dependent | §7 |
| Provider callback / API secrets | Telephony | **Yes** (`<ref>.previous`) | §7 |
| Object-storage credentials | Calling | **No** (restart) | §8 |
| PostgreSQL credentials | each service | database-dependent | §8 |

## 1. API credentials

```
platform_admin --operator <you> credential rotate --credential <public id> --overlap-minutes 60
```

The old credential keeps verifying until the overlap ends (`supersession_valid_until`);
`credential revoke` is immediate and idempotent. Tests: `services/calling/tests/rotation.rs`.

## 2. API credential pepper

1. Deploy `API_CREDENTIAL_PREVIOUS_PEPPER[_VERSION]` = the current values and
   `API_CREDENTIAL_PEPPER[_VERSION]` = the new pepper with a **higher** version
   (`_FILE` variants accepted). Startup refuses equal material in both slots.
2. Restart. Existing verifiers still verify under the previous version; new credentials
   use the current version.
3. Rotate every credential (§1) so no verifier remains under the previous version, then
   remove the previous slot and restart.

Not seamless: step 1 requires a restart (no hot reload). Tests:
`platform/verifier.rs::previous_pepper_still_verifies_during_rotation`.

## 3. Service-auth signing keys, trust bundles, contact-delegation key

```
cf-service-keygen generate --kid calling-prod-2026-11 --out /secure/calling-prod-2026-11.pem
cf-service-keygen public-jwk --kid calling-prod-2026-11 --key /secure/calling-prod-2026-11.pem
```

1. Add the new public JWK to every **receiver's** trust bundle (`CF_SERVICE_AUTH_TRUST_BUNDLE_FILE`)
   and restart the receivers (bundle validation refuses more than four keys per issuer).
2. Switch the signer's `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` to the new key and restart it.
3. After the maximum token lifetime plus clock skew, remove the old key from the bundles.

The contact-delegation key (`CALLING_CONTACT_DELEGATION_KEY_FILE`, trusted by Privacy
Relay's delegation trust bundle) follows the same steps; it must never be a service-token
key (startup refuses the same kid or material). Tests: `packages/service-auth/tests/policy.rs::g_key_rotation_overlap_without_downtime`.

## 4. Contact Vault keyrings (Privacy Relay)

Keyring files are JSON documents with several versions and one active version; the
service reads every listed version and encrypts with the active one. Database rows carry
only the key version. See ADR 0011 and `services/privacy-relay/README.md`.

Encryption keyring:

1. `privacy-relay-admin contact-vault add-key --purpose encryption --version ck-<new> --file <keyring>`
   (atomic replace; a `.bak-<timestamp>` copy of the previous document is kept, mode 0600).
2. Restart (or roll) the service: both versions readable, old still active.
3. `activate-key --version ck-<new>`; restart. New writes use the new version.
4. `reencrypt` until `rotation-status` shows every contact under the new version
   (bounded batches, `SKIP LOCKED`, audited `REENCRYPT`).
5. `retire-key --version ck-<old>`: refused while any contact still references the
   version; succeeds afterwards. Startup also refuses a keyring that lacks a version still
   referenced by a contact.
6. **Backups**: a database backup taken before step 4 can only be restored with the
   retired version. Keep the retired version in an offline keyring copy until every such
   backup has aged out; keyrings are never stored with database backups.

Fingerprint keyring: `add-key` → restart → `activate-key` → restart →
`backfill-fingerprints` → `prune-fingerprints --version fk-<old>` (refused for the active
version and while any enabled contact lacks an active-version fingerprint).

Not seamless: each keyring change needs a process restart. Tests:
`tests/contact_vault.rs::encryption_key_rotation_reencrypts_in_batches_without_changing_any_contact`,
`tests/contact_equality.rs::fingerprint_key_rotation_keeps_deduplication_working`.

## 5. Webhook keys (Calling)

Secret-box key (seals per-subscription signing secrets at rest):

1. Set `CALLING_WEBHOOK_SECRET_KEY_PREVIOUS` = current key, `CALLING_WEBHOOK_SECRET_KEY` =
   new key (`_FILE` variants accepted; weak keys refused). Restart. Sealed secrets open
   under either key; new seals use the current key.
2. `platform_admin --operator <you> webhooks reseal` re-seals every subscription secret
   under the current key (bounded batches, audited).
3. Remove the previous slot and restart.

Per-subscription signing secret: `POST /v1/webhooks/{id}/rotate-secret` returns the new
secret once and it takes effect immediately — the receiver must be updated **before**
rotating (no overlap by design, ADR 0028).

## 6. Signaling key, media-edge key, LiveKit credentials (Calling)

`CALLING_SIGNALING_SIGNING_KEY` and `CALLING_MEDIA_EDGE_SIGNING_KEY` are single keys.
Rotating either invalidates every outstanding signaling / media credential: clients
re-authenticate through the normal recovery path (`GET /v1/calling/history`, endpoint
re-registration). Rotate during a maintenance window: set the new value (`_FILE`
accepted), restart all Calling instances together. Startup refuses a key equal to
another secret (edge ≠ LiveKit secret ≠ signaling ≠ pepper ≠ webhook key).

`CALLING_LIVEKIT_API_KEY` / `CALLING_LIVEKIT_API_SECRET`: add the new key pair to the
LiveKit server configuration, restart LiveKit, switch Calling, restart Calling, then
remove the old pair from LiveKit. In-progress rooms survive (LiveKit keys authenticate
API calls and tokens, not media); tokens issued under the old key stop being accepted
once it is removed. The secret is never returned to clients (bounded join credentials
only) and never appears in logs (tests: `secret_leakage.rs`, CI log grep).

## 7. Telecom credentials (Telephony / Asterisk edge)

* **ARI password** lives in two places: `/run/secrets/edge/ari-users.conf` (Asterisk)
  and the file named by `TELEPHONY_ARI_PASSWORD_FILE` (Telephony). Change both, run
  `asterisk -rx "module reload res_ari.so"` (or `core reload`), restart Telephony.
  Not seamless: Telephony's ARI WebSocket reconnects; in-flight channels stay up on the
  edge and are re-attached by reconciliation.
* **SIP trunk credentials** (`/run/secrets/edge/trunks/<peer>.conf`): where the carrier
  supports two concurrent credentials, add the second `auth` section, `pjsip reload`,
  switch the carrier, remove the old section, reload. Where it does not, coordinate a
  window with the carrier (registration drops during the switch). Never commit a trunk
  file; the repository ships only `trunk.example.conf`.
* **Provider callback / API secrets** (`TELECOM_PROVIDER_SECRETS_DIR/<credential_ref>`):
  move the current secret to `<credential_ref>.previous`, write the **new** secret to
  `<credential_ref>`, restart Telephony (callbacks verify under either secret; outbound
  uses the new one), rotate at the provider, then delete `.previous` and restart.
* **SIP TLS certificate**: replace `/run/secrets/edge/sip-tls.{crt,key}`, `pjsip reload`.

## 8. Object storage and PostgreSQL

* `CALLING_ARTIFACT_STORAGE_SECRET_KEY` (`_FILE` accepted): create the new access key at
  the provider, switch Calling, restart, revoke the old key. Signed read URLs issued under
  the old key stop working at revocation (they are ≤ 15 minutes anyway).
* Database passwords: rotate with the database's own dual-password mechanism where
  available; otherwise a rolling restart of the service with the new `DATABASE_URL`.

## Verification after any rotation

1. `/health` and `/ready` of the affected service.
2. The targeted suite named in the section (CI runs all of them on every push).
3. `make secret-scan` on the repository; the CI image scan on the rebuilt image.

# Optional encrypted account-history backup — implementation proposal

Status: proposed only; security review and owner activation approval required. No backup capability is activated.

## Separate optional encrypted account-history backup proposal

Backups remain off. This proposal is not a production capability or implemented protocol.

Generate a client-controlled random recovery root with at least 256 bits of entropy; provide an explicit owner-approved recovery artifact and verify it can restore before relying on it. Derive a versioned backup wrapping key with domain separation; encrypt a random per-snapshot data key and authenticate manifest/chunks and account/snapshot/version bindings. Store only authenticated ciphertext and opaque storage references on the service. Never send the recovery root, unwrapped keys or plaintext archives to servers or telemetry. Prefer random recovery material over a low-entropy password; any password-derived option requires reviewed memory-hard KDF parameters, salt and offline attack analysis. No silent escrow or account-password fallback.

A new device signs in normally, obtains account-scoped backup ciphertext, and locally uses the recovery secret to verify/decrypt the manifest and all chunks. Device authorization is necessary but does not replace the recovery secret. Password reset changes online authentication only: it cannot decrypt an old backup. Losing all valid device copies and the recovery secret is unrecoverable; disclose this before activation. A stolen recovery secret plus backup ciphertext compromises that backup; rotate recovery material and rewrap/re-encrypt retained snapshots, revoke obsolete access, and assess already downloaded copies, which cannot be remotely erased.

Rotation: version recovery roots and wrapped snapshot keys, bind the version in authentication, checkpoint rotation and verify a new restore before retiring the old wrapping record. Retention/deletion: owner-selected finite snapshot retention, hard byte quota, encrypted tombstones and object cleanup, legal/retention policy review, documented replica/backup expiry; user deletion cannot revoke copies already downloaded. Treat manifest metadata and access logs as sensitive. Backup storage cost is approximately retained ciphertext bytes plus manifests/chunk overhead, replication and egress; deduplication must not expose plaintext-derived cross-account identifiers. Before pricing, measure actual ciphertext growth and restore bandwidth; no fabricated monetary estimate is provided. Native secure storage and recovery UX need iOS/Android implementation and physical-device tests.

Acceptance gates: independent cryptographic/protocol review; owner approval of recovery-key UX and costs; restore and tamper/replay/rollback test vectors; lost-device and compromised-key drills; snapshot completeness and retention validation; enforce disabled-by-default configuration until approval. No backup protocol is activated by PR #175.

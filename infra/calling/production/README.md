# CF Meet production Calling operations

Run `.github/workflows/cfmeet-calling-production.yml` on `main`. It uses the existing
`production` environment and its VPS SSH credentials. The frontend workflow is unrelated.
Production operations are serialized; read-only inventory can run independently.

| Operation | Effect |
| --- | --- |
| `inspect` | Verify CF Meet host identity, inspect actual Docker runtime, count LiveKit participants, report configuration key names and safe flags. |
| `release-backend` | Run production API and real-media acceptance for Enterprise, recording and voicemail independently, followed by identity/Messaging/Contacts/Meetings compatibility. Retain each passing feature; restore only a failed changed feature. No frontend access or off-server-backup gate. |
| `test-enterprise`, `test-recording`, `test-voicemail` | Repeat one feature's production acceptance through the existing workflow. Controlled accounts, tenant resources and evidence stay isolated. |
| `compatibility` | Authenticate native CF Meet accounts and verify identity, Messaging relay, private Contacts directory and Meetings APIs plus anonymous denial. |
| `encrypt-storage` | Preserve the installed MinIO image, migrate a stopped data snapshot to native SSE-S3, verify encrypted writes and recovery-key restart/decryption, promote with rollback retained, then run infrastructure verification. |
| `provision` | Back up LiveKit and runtime inspection privately; integrate existing authenticated Redis only with no active participants; preserve keys/media settings; deploy pinned private Egress; generate capture environment. |
| `harden-egress` | Replace Egress only when it has no active jobs, bound its real working directory, use the supported SDK audio-composite source, retain rollback and run verification. |
| `storage-ingress` | Add a read-only HTTPS bucket route to existing CF Meet nginx, require storage authorization, disable query-signature access logging, configure temporary-object expiry and capacity journal checks. |
| `configure-capture` | Install capture settings and mounted storage secret in a clone of the existing Calling container. Preserve feature switches, networks, mounts, startup and host configuration. Retain original for rollback. |
| `verify` | Run two synthetic WebRTC clients and real track/composite Egress against production, retrieve/decode audio, check storage permission boundaries, SSE, HTTPS, cleanup and service readiness. This is infrastructure testing, not application acceptance. |
| `activate` | Require fresh production scenario receipts bound to exact images; enable carrier-independent flags through the preserved-container transaction; recheck runtime and media; roll back on failure. |
| `rollback` | Restore the exact retained Calling container from the protected transaction journal. Keep the failed replacement for investigation. |

Private files stay under `/opt/cfmeet-core`. Docker inspections contain secrets and are
never uploaded. Only sanitized result logs are uploaded to Actions. LiveKit configuration
is updated through its existing file inode because the container bind-mounts that file.
Configuration is restored automatically if restart checks fail.

Egress uses `livekit/egress:v1.10.0`, resolved to a digest at deployment, 2 CPUs,
2 GiB RAM, bounded `/tmp`, `/home/egress/tmp` and shared memory, rotating logs, a restart policy, and a
private health check. This is a bounded audio-capture deployment, not a capacity
certification for general video/composite workloads. Its storage identity may only
write the `capture-staging/` prefix. The image's actual UID owns its private config.

Signed artifact URLs use `https://api.cfmeet.com/cfmeet-calling-artifacts/...`.
Only GET/HEAD reach MinIO; the console and administrative paths are not proxied.
The nginx upstream is the existing store's private container address. Re-run
`storage-ingress` after replacing that storage container. Normal storage deployments
preserve the existing container. The capacity timer writes five-minute checks to the
system journal and fails below 25 GiB free or above 85% disk usage; it is not an
external alert delivery integration.

The capture input requests `AES256`; it does not silently downgrade encryption if
MinIO lacks KMS. A generated env file does not prove the running backend is configured.
`configure-capture` verifies actual runtime env, mounts and unchanged flags. Application
capture remains disabled until storage security and complete application acceptance pass.

## Acceptance evidence

`calling-production-evidence.json` is **not** a certification marker. Infrastructure
provisioning explicitly writes `INFRASTRUCTURE_ONLY_NOT_CERTIFIED`. Activation requires
`PRODUCTION_CERTIFIED`, a timestamp less than 24 hours old, exact running image IDs,
every scenario in `REQUIRED`, and immutable SHA-256 references to real JSON test receipts
under `production-evidence/`. Receipts identify the scenario, assertions, start/end
timestamps, Git SHA and image IDs. Empty files and PASS labels without receipts fail.
The infrastructure probe cannot generate application certification receipts or close
Calling. No compatibility marker is created by these scripts.

The owner postponed off-server backups on 2 November 2026. Record disaster recovery as
outstanding; it does not block independently tested features. Native MinIO SSE-S3 uses
an owner-only mounted 256-bit KMS key. The same-host recovery copy is tested by restarting
a separate container against encrypted data and retrieving an encrypted probe object.
Neither this recovery test nor the retained original data constitutes off-server backup.
Do not substitute CI fixtures or put production keys/backups in Actions artifacts.

The backend acceptance driver uses real CF Meet accounts, endpoint credentials,
WebSocket signaling and separate real WebRTC client processes. Calling decides provider
admission and publishing permissions. No provider events, grants or call states are
injected. Capture tests require server-issued notice, each participant's consent,
non-silent decoded media, ownership and tenant isolation, audited playback, explicit
deletion and revocation of the original signed URL.

Retention tests first check the configured one-day deadline. A second real captured
artifact then has **only its own isolated fixture deadline** moved into the past by
the host controller. The unmodified production worker must perform the retention
transition and physical deletion. The controller validates UUIDs and the dedicated
acceptance tenant, and restricts the update to a READY artifact, exact call and full
tenant/application/environment scope. It changes no global clock, customer policy,
call state or storage object. This is accelerated expiry testing, not a claim that
24 hours elapsed. Probe failures/timeouts restore changed feature activation.

## Native key recovery

The installed MinIO release supports `MINIO_KMS_SECRET_KEY_FILE`. The running store
mounts `/opt/cfmeet-core/secrets/calling-minio-kms` read-only at
`/run/secrets/calling-minio-kms`. The recovery copy is
`/opt/cfmeet-core/key-recovery/calling-minio-kms`. Both contain the same key ID and
256-bit key, are owned by UID 10001 with mode 0400, and have private parent directories.
Only the storage container receives this key; Egress and application credentials
cannot read it. Never print its contents or place it in Git, Docker environment
values, Actions artifacts or application logs.

If the primary file is lost or damaged, stop the storage container, preserve the
damaged file privately, restore the recovery copy at the exact existing host mount
source with the same owner/mode, then restart the existing container and validate an
authorized encrypted-object read. Preserve the key ID and bytes: generating a new
key does not recover existing encrypted objects. The migration's recovery test used
a separate stopped/restarted container against the encrypted data with the recovery
copy and successfully decrypted the probe. The migration result in the private
`production-evidence/kms-<run-id>/result.json` identifies the active data directory,
retained original container/data and key paths. Rollback to pre-encryption data is
not recovery for recordings created after migration. Loss of both copies or of the
VPS remains a disaster-recovery risk while owner-postponed off-server backups are absent.

## Focused checks

`python -m unittest discover -s infra/calling/production -p 'test_*.py' -v`

The workflow runs these checks before using production SSH. They cover fail-closed
acceptance and preservation of runtime configuration; they do not certify application
calls, consent, tenant isolation, or backup restoration.

The probe image installs the pinned SDK and FFmpeg during its build, before entering
the private production network. The runtime client has no need for package-index
access. Client-facing signed HTTPS downloads are checked outside that namespace.

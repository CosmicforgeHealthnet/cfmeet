# CF Meet Calling production completion report — 2 November 2026

**NOT CLOSED — specific production requirements remain.**

Production infrastructure was changed and tested through the repository's existing
GitHub Actions production SSH credentials. This was an implementation and deployment,
not an audit-only run. Recording/voicemail activation and full Calling application
certification were not completed.

## Deployment and evidence

| Item | Result |
| --- | --- |
| Repository | `CosmicforgeHealthnet/cfmeet`, changes committed directly to `main` |
| Tested operations commit | `2f9ed3185f4214d018e379c6315f0cefa2b90086` |
| Subsequent safety/documentation commit | `2c92751` |
| Production media verification | [Run 41527002979](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41527002979) |
| Final runtime inventory | [Run 41527701127](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41527701127) |
| HTTPS storage, lifecycle and capacity deployment | [Run 41525289523](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41525289523) |
| Calling runtime replacement | [Run 41525474480](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41525474480) |
| Verification completion | 2 November 2026, approximately 22:22 CET / 21:22 UTC |
| Workflow conclusion | **Failure**, correctly caused by `encryption_at_rest`; not reported as a successful certification |

Sanitized logs are attached to Actions. Secret-bearing container snapshots, configuration
backups and detailed probe logs remain private on the VPS under
`/opt/cfmeet-core/production-evidence/`. No live credentials or production env contents
were committed or uploaded as artifacts.

## What was deployed

1. Integrated existing authenticated `cfmeet-calling-redis` into the actual LiveKit
   configuration. Checked that there were zero participants before the shared LiveKit
   restart. Preserved existing keys, certificates, media configuration, network and ports.
   Backed up the original configuration and included automatic restoration on validation
   failure. Authenticated Redis checks passed from both LiveKit and Egress network namespaces.
2. Deployed `cfmeet-calling-egress`, fixed its owner-only configuration permissions using
   the pinned image's actual UID, and enabled its supported SDK audio-composite source.
   It is healthy, private, limited to 2 CPUs and 2 GiB RAM, has bounded temporary storage
   at both `/tmp` and `/home/egress/tmp`, bounded shared memory, rotating logs and a restart
   policy. It has no published ports. This is not a load/capacity certification.
3. Created root-only `/opt/cfmeet-core/calling-capture.production.env` using real installed
   service addresses, bucket, region, staging prefix and existing service credentials.
   Configured the actual Calling container with these settings and a read-only secret
   mount. Verified its runtime environment, original mounts, networks, entrypoint and command.
4. Added a read-only HTTPS route for signed artifact downloads under
   `https://api.cfmeet.com/cfmeet-calling-artifacts/`. MinIO remains private; its console and
   administration endpoints are not exposed. Query-signature access logging is disabled
   for this route. Signed reads of a controlled object passed; anonymous reads were denied.
5. Added a one-day MinIO lifecycle rule for abandoned `capture-staging/` objects. Final
   recording retention remains Calling's responsibility; it was not replaced with a blind
   bucket-wide deletion policy. Added a five-minute systemd capacity check that reports to
   the system journal and fails below 25 GiB free or at 85% disk usage. External alert
   delivery is not configured by this change.
6. Added one version-controlled production workflow with inspection, provisioning, storage
   ingress, capture configuration, Egress hardening, verification, gated activation and
   Calling rollback operations. It uses the existing deployment credentials. Calling
   replacement clones its actual Docker configuration and retains the previous container.
   No compatibility certification marker was created.

## Running image references

| Service | Immutable reference |
| --- | --- |
| Calling | image ID `sha256:4a9fcd6a6b5a1c867565dd24a66c6931eec2a496b4e87af84a4694353115763c`; existing application build `884776b0757109d4ccb84ad25ada7c1a0f5af336` |
| Egress | `livekit/egress@sha256:f7828a5b67bba95fa89e8d466c72781972259c1cba05a510e834b4ac2bb35c34` (`v1.10.0`); image ID `sha256:57ca2e73e83d8b3b29b50a74c586e677c76a3928a434fde6d0543198d4384cf0` |
| LiveKit | existing image ID `sha256:74b3dee8f66a429be1fd3fe210eae6df7453584513ae8644d72d89efe870be2d`; existing tag remains `latest`, not upgraded |
| MinIO | existing `pgsty/minio@sha256:b6bfe7239bfc83fb90d31612d9704d86039dd714f7904b3f1ad68f211e602372` |

## Actual production verification results

The media probe used the real production LiveKit, Redis, Egress and private MinIO with
existing production service identities. It joined a uniquely named controlled room with
two WebRTC clients and generated a synthetic 440 Hz tone. It did not use CI keys or create
a parallel storage provider.

| Check | Result |
| --- | --- |
| Two production WebRTC clients and remote audio subscription | PASS |
| Track Egress captures and uploads an Ogg/Opus audio object | PASS |
| Audio-only room-composite Egress captures and uploads an object | PASS |
| Both files decode to at least three seconds of non-silent PCM audio | PASS |
| Calling storage identity can upload/read controlled objects | PASS |
| Egress identity cannot read objects | PASS |
| Egress identity cannot upload outside the staging prefix | PASS |
| Client-style, short-lived signed HTTPS download | PASS |
| Controlled staging/HTTPS test objects are deleted and absent afterward | PASS |
| Anonymous Calling, signaling and Messaging identity requests return 401 | PASS |
| Public Calling Swagger has Enterprise routes and 83 paths; no `/internal/` or `/v1/internal/` path prefixes | PASS |
| Meetings health; Messaging/Calling/Platform TLS readiness; both LiveKit keys | PASS before and after changes |
| Egress final Docker health | HEALTHY |
| LiveKit participants after controlled probe cleanup | 0 |
| MinIO `AES256` encryption request | **FAIL: `NotImplemented`** |
| CF Meet off-server backup destination configuration | **ABSENT** |
| Encrypted off-server backup and restoration test | **NOT RUN** |

These media results verify infrastructure capture. They do **not** certify the Calling
application's authorization, consent or full voicemail/recording lifecycles.

## Feature switches in the actual Calling runtime

| Setting | Effective value |
| --- | --- |
| `CALLING_NEW_CALLS_ENABLED` | `false` |
| `CALLING_APP_CALLING_ENABLED` | `true` (already enabled; master new-calls switch remains off) |
| `ENTERPRISE_CALLING_ENABLED` | `false` |
| `VOICEMAIL_ENABLED` | `false` (unset, parser default) |
| `CALL_RECORDING_ENABLED` | `false` (unset, parser default) |
| `CALLING_CAPTURE_PROVIDER` | `livekit-egress` |
| `CALLING_PUBLIC_OUTBOUND_ENABLED` | `false` |
| `CALLING_PUBLIC_INBOUND_ENABLED` | `false` |
| `CALLING_CALLBACK_RELAY_ENABLED` | `false` |
| `CALLING_SYNTHETIC_INTERCONNECT_ENABLED` | `false` (unset, parser default) |

The artifact settings request `AES256`; they do not silently downgrade encryption when
MinIO cannot satisfy it. Capture feature switches were not enabled.

## Requirements still open

- **Encryption and key recovery:** MinIO has no mounted KMS secret file in the actual
  container. Production `AES256` uploads fail. A supported encryption key arrangement and
  protected recovery/escrow must be configured, followed by encrypted write/read tests.
  Encryption at rest and secure key management are not certified.
- **Off-server backups:** No approved CF Meet off-server destination/credential configuration
  was found in its deployment configuration; GitHub's configured secrets provide production
  SSH access, not a backup destination. No encrypted off-server backup, key recovery or
  restoration was performed. An approved destination and key-management/recovery service
  were requested without asking for credentials in chat or manual configuration creation.
- **Full Calling application acceptance:** One-to-one application voice/video, invitations,
  accept/decline/cancel, termination/cleanup, reconnection, history/missed calls, extensions,
  ring groups, queues, transfers, hold/resume, IVR and tenant isolation remain **NOT RUN**
  against production application principals in this work.
- **Voicemail and recording application acceptance:** The complete unanswered-call voicemail
  flow, consent/permission rejection, canonical artifact finalization, participant-only and
  cross-user access, audit records, retention expiration and application deletion remain
  **NOT RUN**. Passing infrastructure capture does not substitute for these tests.
- **Business regression acceptance:** Meetings media through an actual user meeting and
  Messaging send/receive through controlled users were not tested. Service health, TLS,
  LiveKit credentials and anonymous authorization checks passed.

Activation requires the complete scenario matrix, fresh per-scenario JSON receipts, exact
image IDs and SHA-256 receipt hashes. Empty marker files or PASS labels without receipts
are rejected. Seven focused safety tests pass, covering incomplete/stale evidence, image
changes, receipt tampering, runtime preservation, and unsupported network/public-port cases.
Full activation itself was not executed or certified.

## Rollback and preservation

The original Calling container is retained as `cfmeet-calling-rollback-1793653676`.
Its exact ID and configuration are recorded privately in
`/opt/cfmeet-core/calling-activation-rollback.json`. Dispatch
`cfmeet-calling-production.yml` on `main` with `operation=rollback` to stop and retain the
replacement, restore the original container name, start it, and verify readiness. This is
a Calling runtime rollback; it does not remove storage or reverse LiveKit Redis integration.

The earlier Egress container is retained as
`cfmeet-calling-egress-rollback-41527002979`. Its original configuration and inspection are
in `production-evidence/egress-hardening-41527002979/`. The hardening operation automatically
restores those if its replacement startup validation fails. Reverting it later requires
no active Egress jobs and restoration of that saved configuration before restarting the
retained container through the authorized production SSH workflow.

The pre-Redis LiveKit configuration remains in the first provisioning evidence directory
as `livekit.yaml.before`. Any later manual operational rollback must first verify zero
participants, preserve the current configuration, write through the existing mounted
file inode, restart LiveKit, and validate both production keys; restore the current config
if validation fails. Existing credentials, storage data and earlier rollback containers
were preserved. No new public ports, frontend changes, `frontend.yml` changes or Vercel
deployment changes were made.

**NOT CLOSED — specific production requirements remain.**

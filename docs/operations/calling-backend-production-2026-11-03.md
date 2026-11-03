# CF Meet Calling backend production rollout — 3 November 2026

Scope: CF Meet backend, production APIs and existing Contabo infrastructure.
Frontend/Vercel access, deployment and testing are excluded from this release.

## Production state

Application Calling, internal Enterprise Calling, recording and voicemail are enabled
after production API/security and real-media acceptance. Expanded Enterprise acceptance
passed on 3 November 2026 at 06:09 CET (05:09 UTC). External telecom remains disabled.

| Runtime setting | Effective value |
| --- | --- |
| `CALLING_NEW_CALLS_ENABLED` | `true` |
| `CALLING_APP_CALLING_ENABLED` | `true` |
| `ENTERPRISE_CALLING_ENABLED` | `true` |
| `VOICEMAIL_ENABLED` | `true` |
| `CALL_RECORDING_ENABLED` | `true` |
| `CALLING_MEDIA_PROVIDER` | `livekit` |
| `CALLING_CAPTURE_PROVIDER` | `livekit-egress` |
| `CALLING_ARTIFACT_STORAGE_SERVER_SIDE_ENCRYPTION` | `AES256` |
| `CALLING_PUBLIC_INBOUND_ENABLED` | `false` |
| `CALLING_PUBLIC_OUTBOUND_ENABLED` | `false` |
| `CALLING_CALLBACK_RELAY_ENABLED` | `false` |
| `CALLING_SYNTHETIC_INTERCONNECT_ENABLED` | `false` |

## Real production acceptance

Tests use three isolated, normally verified CF Meet accounts, dedicated production
application tenants and endpoint-scoped signaling credentials. Separate real WebRTC
clients send controlled generated audio/video through Calling's production gateway.
Calls must receive playable
non-silent audio and actual video frames; no provider events, permissions or call
states are injected. Tenant and scope denials remain enforced.

| Area | Evidence/result |
| --- | --- |
| Voice/video, ringing, accept, hold/resume, camera denial, reconnection, end | Real production media passed in [run 41580265031](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41580265031). |
| Expanded app/Enterprise scenarios | PASS: incoming ringing/accept/decline, cancellation, missed-call history, three-party real audio, reconnection, hold/resume, extension routing, ring groups, queues, IVR keypad-to-extension routing, internal transfer with bidirectional media, cross-tenant isolation and read/write scope denials. [Run 41581639199](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41581639199) succeeded. |
| Recording | PASS: missing consent denied; nonparticipant denied; server notice and all-participant consent recorded; real encrypted audio capture/playback; other-user/tenant denial; access audit; deletion and original signed URL denial; real retention worker deletion. |
| Voicemail | PASS: unanswered extension routes to caller-only real audio capture; owner playback; caller/other-tenant denial; READ state; deletion and original signed URL denial; real retention worker deletion. |
| Identity, Messaging, Contacts directory, Meetings | PASS: real native login, identity projection, private directory and out-of-organization resolution denial, authenticated conversation relay and Meetings listing; anonymous requests denied. |
| Infrastructure | PASS: encryption, write-only Egress staging permissions, track/composite audio, decoded non-silent media, signed HTTPS download, cleanup and service readiness. |
| Swagger/authentication | PASS: 83 public Calling paths, Enterprise included, internal paths excluded; anonymous Calling/signaling/Enterprise/voicemail/Messaging/Meetings denied. |

Recording, voicemail and compatibility results are in
[run 41580265031](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41580265031).
That combined run failed its Enterprise scenario because abandoned test-account
endpoints kept other offers ringing after one device declined. Capture tests passed
independently and their enabled flags were retained. The driver now revokes only
abandoned `production-api-` endpoints of its dedicated test principals before testing
single-device behavior. A decline on one device must not suppress other live offers.
The subsequent focused Enterprise run passed every assertion. An intervening workflow
lost its idle SSH connection; the same existing workflow now uses SSH keepalives and
bounded progress output, and the completed rerun succeeded.

Retention tests first verify the configured one-day deadline. A second real captured
test artifact then has only its isolated fixture expiry deadline advanced into the
past. The unmodified production worker performs the audited state transition and
physical object deletion. No global clock, customer policy or call state is changed;
the test does not claim that 24 hours elapsed.

## Encryption and recovery

The installed MinIO release `RELEASE.2026-08-04T00-00-00Z` supports its native
`MINIO_KMS_SECRET_KEY_FILE` configuration. The same installed image was retained.
Native SSE-S3 now encrypts the private artifact bucket. Encrypted upload/download
and restart/decryption using the protected recovery-copy key passed in
[run 41537691425](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41537691425).
Fresh infrastructure verification passed in
[run 41580667339](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41580667339).

The primary key is `/opt/cfmeet-core/secrets/calling-minio-kms`; its same-host recovery
copy is `/opt/cfmeet-core/key-recovery/calling-minio-kms`. Both are private, mode 0400,
owned by the storage UID, and only the storage container receives the read-only key
mount. Recovery instructions are in the production operations
[README](../../infra/calling/production/README.md#native-key-recovery). No key material,
storage credentials, signed media URLs or private logs are uploaded to Actions or Git.

## Deployment and rollback

All changes are pushed to `main`. The existing
`.github/workflows/cfmeet-calling-production.yml` uses the existing production VPS
access. Production operations are serialized. Runtime container replacement preserves
networks, secrets, mounts, volumes, startup configuration and unchanged flags; the
previous exact container is retained for rollback. Each independent feature retains
its own successful activation and restores a changed activation on failure.

The working Calling application image remains
`sha256:4a9fcd6a6b5a1c867565dd24a66c6931eec2a496b4e87af84a4694353115763c`
(existing build `884776b0757109d4ccb84ad25ada7c1a0f5af336`). The installed LiveKit,
Egress, Messaging, Platform Access and Meetings application images were preserved.
This release completes production configuration, encrypted storage, activation and
API/media integration while reusing the working services.

The tested capture/infrastructure operations commit is
`75c0379fe79a19736ecf45e7451194f7f855e611`. The subsequent Enterprise fixture cleanup
commit is `e7d5fa3`. Final Enterprise/workflow operations were tested at `8d1bf3c`.
Twelve focused local safety checks pass, covering activation
evidence, runtime preservation, isolated retention targeting and rollback on unexpected
probe failure, plus refusal to stop an unrelated image or a probe with a customer-data
mount. Private test receipts include exact Git SHA, runtime image IDs and flags.

Some initial SDK media joins timed out at the probe's 20-second deadline and succeeded
with one fresh credential retry. This is recorded in the receipts; this release does
not certify media-join latency or load capacity. Voice/video and reconnection must
still receive actual media to pass.

## Outstanding

Off-server encrypted backup and restore remain **postponed by the owner**. The
same-host recovery key copy and retained rollback data do not certify disaster
recovery or protect against total VPS loss. This dependency does not disable unrelated
Calling features. No carrier or paid infrastructure provider was added.

No requested backend feature remains blocked or disabled for a failed acceptance test.
Frontend interaction and OS/browser microphone/camera prompts were excluded by the
owner's backend-only instruction; backend microphone/camera publishing permissions
were tested through real RTC clients.

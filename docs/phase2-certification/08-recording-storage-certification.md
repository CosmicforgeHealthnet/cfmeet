# 08 — Real recording and object-storage certification (Part M §8, Part N §66)

Test: `tests/livekit_certification.rs::real_livekit_part_g_recording_and_object_storage`
— the production `LiveKitEgressRecorder` and `S3ArtifactStorage` against real
`livekit/egress` v1.14.1 (container) and a real S3-compatible store (MinIO
RELEASE.2026-08-04, `pgsty/minio` community build), with a real WebRTC publisher in the room.

## Results

| Run | When | Object storage (S3) | Egress capture → READY |
|---|---|---|---|
| Parts J–L certification run (`380e693`) | 2026-10-05 06:05 UTC, same images / binaries | **PASS** | **PASS** |
| Parts M–N final re-run, attempt 1 (repo shims `docker-egress.sh` / `docker-minio.sh`) | 2026-10-05 16:45 UTC, after a container restart | **PASS** | **FAIL — environment** |
| attempt 2 (repo shims) | 16:48 UTC | PASS | FAIL — environment |
| attempt 3 (the original J–L wrapper scripts) | 16:50 UTC | PASS | FAIL — environment |
| **GitHub CI `phase2-backend-certification.yml`, job `real-provider`** (run 37384968304, commit `a178768`, 2-vCPU runner, images pinned by digest) | 2026-10-05 23:11 UTC | **PASS** | **PASS** |

**Classification: real recording + object-storage certification — PASS (re-certified in
CI).** The M / N local re-run failure (environment, below) is superseded by the CI run:
**8 / 8 real-provider tests PASS** (`test result: ok. 8 passed; 0 failed`), real Egress
recording PASS, real S3 (MinIO) PASS, recording artefact lifecycle PASS. DoD §66 closed.

### CI run evidence (real, run 37384968304)

`CERT S3 object exists + metadata` (200 000 bytes, ETag), `CERT S3 checksum (streamed SHA-256 +
MD5/ETag)`, `CERT S3 signed read (scoped, expiring, no anonymous access)`, `CERT S3 delete +
absence confirmed`, `CERT G real composite capture start (egress EG_Ai2Jw2PYxjoE)`, `CERT G
real provider callback (egress_started → RECORDING)`, `CERT G real stop (STOPPING)`, `CERT G
real media file READY (25 438 bytes, 10 094 ms, key verified in MinIO)`, `CERT G signed
artefact access + checksum`, `CERT G unauthorized access rejected`, `CERT G callback dedupe
(4 unique egress callbacks, one READY)`, `CERT G delete + absence confirmed in object
storage` — all PASS. The same job's media load (4 × 16 … 6 × 75) completed with 0 join
failures and 0 unexpected disconnects.

Two certification-infrastructure fixes were needed on the CI runner (no application
change): `livekit/egress` v1.14 reserves 4 CPUs per video room-composite request and, when
idle, offers exactly the host CPU count, so on the 2-vCPU runner every request was refused
(`ErrNotEnoughCPU`) — the certification Egress config sets
`cpu_cost.room_composite_cpu_cost: 1.5` (`egress.yaml.tmpl`; not a production sizing); and
the harness teardown (`run.sh`) could not delete root-owned MinIO files, which turned a
passing run into exit 1 — teardown is now best-effort and the exit status is the test
result.

### J–L run evidence (real)

`CERT S3 object exists + metadata` (200 000 bytes, ETag), `CERT S3 checksum (streamed SHA-256 +
MD5/ETag)`, `CERT S3 signed read (scoped, expiring, no anonymous access)`, `CERT S3 delete +
absence confirmed`, `CERT G real composite capture start (egress EG_…)`, `CERT G real provider
callback (egress_started → RECORDING)`, `CERT G real stop (STOPPING)`, `CERT G real media file
READY (25 605 bytes, 10 064 ms, key verified in MinIO)`, `CERT G signed artefact access +
checksum`, `CERT G unauthorized access rejected`, `CERT G callback dedupe (4 unique egress
callbacks, one READY)`, `CERT G delete + absence confirmed in object storage` — all PASS.

### M / N re-run — what failed and why

- All four real S3 checks passed again.
- `StartRoomCompositeEgress` was accepted by the real Egress (`CERT G real composite capture
  start: PASS`) and LiveKit sent `egress_started`; Egress's headless Chrome then failed to load
  its room-composite template: `failed to launch chrome … page load error: websocket url
  timeout reached` → `egress_failed`. `livekit-server` logged **no** recorder participant
  joining the room. The failure is before any CF Meet logic, and it reproduced with the
  original J–L wrapper, so neither the new shims nor any M / N code change is involved.
- Environment diagnosis: the sandbox was restarted between the J–L and M / N runs; after the
  restart IPv6 is unavailable (`EAFNOSUPPORT`), while Chrome itself, loopback TCP / HTTP to
  LiveKit from inside the Egress container and `localhost` resolution all work. The root cause
  inside the Egress template was not isolated within the time box.
- CF Meet behaved correctly: it never reported RECORDING or READY for the failed capture
  (`egress_failed` handling: `recording_g::g_f01..g_f04`, `g_e2e_16_provider_failure_safe`).
- Recording code changed in M / N only by `debug_assert!`s of the artefact transition table
  (no behaviour change; release builds compile them out) — the code path certified in J–L is
  otherwise identical.

**Follow-up (done):** Part G was re-run in CI (`phase2-backend-certification.yml`, job
`real-provider`, `livekit/egress` v1.14.1 and `pgsty/minio` RELEASE.2026-08-04 pinned by
digest — the official `minio/minio` repository no longer serves public community images):
PASS, see the CI run evidence above.

## §8 requirements

| Requirement | Evidence | Status |
|---|---|---|
| Consent / policy gates before capture | `recording_g` G-S*, `system_e2e::m_e2e_09` (`CONSENT_REQUIRED` until accepted), healthcare default off | PASS |
| Real provider capture | J–L real run; CI re-certification (run 37384968304) | PASS |
| Object stored in real S3-compatible storage, size + checksum verified, opaque key | J–L real + M / N real S3 checks | PASS |
| READY only after verification | J–L + CI real runs; `g_f05`, `g_f06`, `ck_artifact_ready_validated`, `inv_ready_recording_references_conference_and_storage` | PASS |
| Authorized, scoped, expiring access; unauthorized rejected | real S3 signed read + J–L `CERT G` access checks; `platform_security::l_s14_s15` | PASS |
| Callback dedupe | J–L real (4 callbacks → one READY); `g_s09`, `g_c02`, `g_e2e_15` | PASS |
| Retention / deletion with confirmed absence | real S3 delete + absence (both runs); `g_f08`, `g_f11` | PASS |
| No storage credentials / provider ids leaked | `recording_g::g_s13`, contract tests | PASS |

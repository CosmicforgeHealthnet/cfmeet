# CosmicForge shared security scanning — operations runbook

One authenticated gateway (`services/security-scan-gateway`) lets CosmicForge products scan
files with an antivirus engine **that already runs** on the shared VPS. No product starts
its own ClamAV daemon for this. API and configuration: the
[service README](../../services/security-scan-gateway/README.md).

## Architecture

```text
cfmeet-messaging ──HTTPS──▶ cfmeet-core-tls :8449 ──▶ cosmicforge-scan-gateway :8099
   (signed, single-use        (scan + /ready only;        │  verify token, registry,
    service token,             no metrics, 26 MiB cap,     │  replay, size, rate, slots,
    content SHA-256)           no request buffering)       │  engine freshness
                                                           │
                         cosmicforge-scan-backend (Docker network, internal: no egress,
                         members: the gateway and the engine only)
                                                           │
                                                           ▼
                                     admin-server-mail-clamav :3310 (alias
                                     cosmicforge-scan-engine) — EXISTING production
                                     mail scanner, owned by admin-server-mail
```

- The gateway sits on `cfmeet-core-private` (CF Meet's own network) and on
  `cosmicforge-scan-backend`. It is **not** on `admin-server_default`, so it cannot reach
  the mail stack's other containers.
- The engine keeps its own networks and configuration. The only change on it is one extra,
  reversible network attachment. Its port is not published and is not reachable from
  `cfmeet-core-private` (checked by the probe on every verify).
- CF Meet containers cannot open the engine's raw port; only the gateway can.

## Production state (2026-11-05)

Deployed by `scan-gateway-configure` [42102547990](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42102547990), independently verified by [42113919022](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42113919022): 26/26 checks, gateway peak 8 MiB, engine unchanged (949 MiB, 0 OOM). Messaging uses it in mode `gateway` since `messaging-configure` [42104592956](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42104592956). Evidence: [Section 8 delivery record](messaging-section-8-delivery.md#shared-security-scanning-and-messaging-infrastructure-activation--2026-11-05).

## Why this engine (audit `resource-scanners`, runs 42077171046 and 42077986241)

| Engine | Owner / class | Limit | Peak | Memory events | Reload | Threads | Stream | Decision |
|---|---|---|---|---|---|---|---|---|
| `admin-server-mail-clamav` | mail, production | 3072 MiB | 1846 MiB | 0 OOM, 0 max | `ConcurrentDatabaseReload no` | 8 | 25M | **Selected** |
| `admin-server-staging-mail-clamav` | mail, staging | 3072 MiB | 1846 MiB | 0 | no | 8 | 25M | Excluded: staging |
| `karrygo-media-scanner` | KarryGo, production | 1536 MiB | 1536 MiB | 52,772 `memory.max` hits, 277 MiB swap | default (doubles) | default | default | Excluded: no headroom |

The selected engine has about 1.2 GiB spare **inside its own existing limit**, so the
gateway adds no reservation to the host. `ConcurrentDatabaseReload no` means daily reloads
do not double memory (scans wait during a reload; the gateway fails closed meanwhile).
`AlertExceedsMax yes` means archive members beyond its limits are reported, not passed.

## Limits

| Limit | Value | Why |
|---|---|---|
| Gateway memory / CPU | 64 MiB, 0.5 CPU, 64 pids, low CPU shares | production peak 8 MiB, ~0.5 CPU-seconds per acceptance run |
| Engine concurrency used by all products | 2 of 8 threads | the mail owner keeps ≥ 6 |
| Per-product concurrency | `maxConcurrent` (CF Meet: 2) | per-product slot is taken first, so one product cannot hold another's |
| Per-product rate | `maxPerMinute` (CF Meet: 120) | bounds retry storms |
| File size | 25 MiB (the engine's `StreamMaxLength`) | Messaging sets `MESSAGING_MANAGED_SCAN_MAX_BYTES` to the same; larger MANAGED files are rejected `TOO_LARGE_TO_SCAN`, never delivered unscanned |
| Queue wait | 2 s, then `OVERLOADED` | slow scans never pile up gateway connections |
| Scan deadline | 60 s, then `TIMEOUT` | Messaging's own client timeout is 75 s |
| Signature age | 72 h, then `SIGNATURES_STALE` | the same rule CF Meet used for its own scanner |

## Operations (existing workflow `cfmeet-calling-production.yml`, branch `main`)

| Operation | Effect |
|---|---|
| `resource-scanners` (main or staging) | Read-only audit of every engine: owner, limits, cgroup peak and memory events, clamd keys, signature age, benign scan latency, usage sample |
| `scan-gateway-configure` with `image_sha` | Transfers the exact gateway image (revision label checked), re-checks the engine, creates/keeps the verdict key, writes trust bundle + registry from Messaging's **current** service key, attaches the engine to `cosmicforge-scan-backend`, (re)creates the gateway, adds the `:8449` route, then runs `scan-gateway-verify`. Idempotent. A previous gateway is kept as `cosmicforge-scan-gateway-rollback-<run>` and restored if anything fails |
| `scan-gateway-verify` | Live acceptance through both the private name and the TLS route, plus resource deltas (gateway, engine, LiveKit CPU seconds; host memory) and regression |
| `scan-gateway-disable` | Stops the gateway (kept), removes the `:8449` route, detaches the engine from the backend network. Messaging fails closed (MANAGED attachments stay `VERIFYING`) |
| `messaging-ingress` | Re-applies only Messaging's public ingress and probes it; prints redacted diagnostics and rolls back if the probe fails |
| `messaging-configure` | Chooses scanner mode `gateway` (gateway ready over TLS) or `quarantine` (no scanner; MANAGED attachments stay pending). Never starts an antivirus engine; the old 6.5 GiB gate is replaced by release margin + media reserve (3.5 GiB) |

Configure order for a new image: build images → `scan-gateway-configure` → coordinated
Messaging release (`communications-stage`, `communications-release`) → `messaging-configure`.
`messaging-configure` refuses `gateway` mode if the running Messaging binary did not log
that it configured the gateway scanner (an older image cannot pretend).

## Failure behaviour

| Event | Result |
|---|---|
| Engine down, restarting or reloading | Gateway `/ready` 503; scans `UNAVAILABLE`; Messaging keeps attachments `VERIFYING` and retries with backoff (30 s × 2ⁿ, budgeted); the gateway never restarts the engine |
| Engine signatures older than 72 h | `SIGNATURES_STALE`; no byte reaches the engine. Escalate to the mail owner (their freshclam). Do not raise the threshold to force scans |
| Mail stack recreated by its owner (`compose up` recreate) | The extra network attachment is lost: scans `UNAVAILABLE`. Re-run `scan-gateway-configure` (idempotent) after confirming the engine is healthy |
| Gateway crash / restart | `restart: unless-stopped`; in-flight scans fail closed and are retried by Messaging |
| Overload | `OVERLOADED` after 2 s; Messaging retries later |
| Wrong/forged/unsigned verdict, other request id, other digest | Messaging treats it as unavailable (never READY) |
| Messaging service key rotated | Re-run `scan-gateway-configure` so the trust bundle carries the new key (rotation overlap: the bundle accepts up to 4 keys) |

## Verdict key

`/opt/cfmeet-core/scan-gateway/verdict.pem` (uid 10002, 0400) with a recovery copy under
`/opt/cfmeet-core/key-recovery/scan-gateway/`. Configure never replaces it and refuses if
the two copies differ. The public set is `verdict-keys.json`, copied to Messaging as
`/run/messaging/scan-verdict-keys.json`. Rotation: add the new key to callers' sets first
(up to 4), then switch the gateway's `SCAN_GATEWAY_VERDICT_KEY_FILE`/`_ID`, then remove
the old public key.

## Onboarding another product

1. Run `resource-scanners`; confirm the engine still has headroom for the new product's
   expected concurrency (the global cap stays 2 unless the engine owner agrees otherwise).
2. Give the product's backend service its own Ed25519 service key and identity. Service
   identities are `cfmeet.<name>` today (`packages/service-auth`); a non-CF Meet product
   needs that namespace rule extended — a contract change, not a gateway change.
3. Add the identity to `security-scan.submit`'s `grantable_to` in
   `packages/service-auth/src/permissions.rs` (the registry refuses any other grant) and
   to `CLIENTS` in `infra/calling/production/scan_gateway.py` with its own product name and
   limits (≤ the engine's stream limit and the global concurrency).
4. Implement the caller with `cf-scan-gateway-contract`: compute SHA-256, sign a fresh
   token per request, accept only a verdict for which `VerdictKeys::verify` passes.
5. Separate production and staging: a registry is for exactly one runtime environment, and
   a staging token is refused by a production gateway.
6. Re-run `scan-gateway-configure`, then `scan-gateway-verify`.

## What the gateway never does

Store or log file bytes, file names or content; expose metrics or the engine publicly;
accept a product identity from a request body; reuse another product's credentials; scan
PRIVATE_E2EE ciphertext (Messaging never sends it); restart, reconfigure or update another
product's engine.

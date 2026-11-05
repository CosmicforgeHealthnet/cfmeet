# CosmicForge shared security scan gateway

One small, authenticated internal service that lets CosmicForge products scan files with
an **existing** ClamAV engine instead of each product starting its own 1–3 GiB daemon.

- Loads **no** signature database and contains no antivirus engine.
- Stores **no** file: bytes are hashed and streamed to clamd `INSTREAM` in ≤ 64 KiB
  frames as they arrive (measured ~13 MiB resident after a 25 MiB scan).
- Returns a **signed** verdict bound to the caller, its runtime environment, the request
  id and the SHA-256 and length of the exact bytes the engine received.
- Fails closed: every outcome other than a signed `CLEAN` / `INFECTED` is "no answer".

Operations, isolation, onboarding and recovery: [docs/operations/shared-security-scanning.md](../../docs/operations/shared-security-scanning.md).

## API (internal only, never public, never in consumer Swagger)

### `POST /internal/v1/scans`

| Header | Required | Meaning |
|---|---|---|
| `X-CF-Service-Authorization` | yes | single-use Ed25519 service token (`packages/service-auth`), audience `cfmeet.security-scan` |
| `X-CF-Request-Id` / `X-CF-Correlation-Id` | yes | must equal the signed claims |
| `Content-Type` | yes | `application/octet-stream` (raw bytes, no multipart) |
| `Content-Length` | yes | exact byte count; chunked uploads are refused (411) |
| `X-CF-Scan-Content-Sha256` | yes | lower-case hex SHA-256 the caller computed |

Order of checks — nothing is read from the body before 1–3 pass:

1. token verified against the gateway's trust bundle (issuer key, audience, runtime
   environment, lifetime ≤ 300 s, request/correlation binding); customer `Authorization`
   headers are refused;
2. `security-scan.submit` granted **and** the issuer registered in the client registry;
3. the token id has not been used before (replay guard);
4. size (`0` → `EMPTY_CONTENT`, above the product limit → `TOO_LARGE`), per-product
   rate budget (`RATE_LIMITED`);
5. a per-product slot, then a shared engine slot, each waited for at most
   `SCAN_GATEWAY_QUEUE_TIMEOUT_MS` (`OVERLOADED`);
6. engine reachable within the last 120 s and signatures at most
   `SCAN_GATEWAY_MAX_SIGNATURE_AGE_HOURS` old (`BACKEND_UNAVAILABLE`, `SIGNATURES_STALE`);
7. streaming under one deadline (`SCAN_TIMEOUT`); a short body is `TRUNCATED`, a long one
   `LENGTH_MISMATCH`; a digest other than declared is `DIGEST_MISMATCH` even if clean.

Errors before authorization use `{"error": {"code": ...}}`: `UNAUTHENTICATED` (401),
`FORBIDDEN` (403), `UNSUPPORTED_MEDIA_TYPE` (415), `INVALID_CONTENT_DIGEST` (400),
`LENGTH_REQUIRED` (411). Every authorized request is answered with a `ScanResponse`
(`packages/scan-gateway-contract`):

| `verdict` | HTTP | Signed | Caller action |
|---|---|---|---|
| `CLEAN` | 200 | yes | verify signature and binding, then accept |
| `INFECTED` | 200 | yes | verify, then quarantine/reject |
| `REJECTED` | 413 / 422 / 429 | no | do not retry unchanged |
| `UNAVAILABLE` / `TIMEOUT` | 503 | no | retry later with backoff; never clean |
| `ERROR` | 502 | no | retry later; never clean |

The signed message is `cosmicforge-scan-verdict/v1` and, one per line: contract version,
request id, client id, product, environment, verdict, finding, sha256, bytes, engine name,
engine version, signature database version, scanned-at. See
`cf_scan_gateway_contract::signing_message`; callers verify with `VerdictKeys::verify`.

Idempotency: a scan has no side effect, so a retry is simply a new request with a new
token. Tokens are single-use, so a retry must re-sign.

### `GET /ready`, `GET /health`

`/ready` is 200 only with a reachable engine and fresh signatures
(`{"status":"ready","engineVersion","signatureVersion","signatureAgeSeconds"}`), else 503
with `reason`. `/health` is process liveness.

### Metrics (separate private listener, `SCAN_GATEWAY_METRICS_ADDRESS`)

`cosmicforge_scan_requests_total{product,verdict,reason}`,
`cosmicforge_scan_auth_failures_total{reason}`, `cosmicforge_scan_duration_seconds{product}`,
`cosmicforge_scan_bytes_total{product}`, `cosmicforge_scan_inflight{product}`,
`cosmicforge_scan_backend_up`, `cosmicforge_scan_signature_age_seconds`,
`cosmicforge_scan_signature_version`. Labels come only from the registry and the closed
verdict/reason vocabulary — never from requests, file names or contents.

## Configuration

See `src/config.rs` for every variable. Files (all read-only mounts):

- trust bundle (`CF_SERVICE_TRUST_BUNDLE_FILE`): receiver `cfmeet.security-scan`, each
  caller's public key and `security-scan.submit`;
- client registry (`SCAN_GATEWAY_CLIENTS_FILE`): `{"version":1,"environment":...,
  "clients":[{"issuer","product","maxBytes","maxConcurrent","maxPerMinute"}]}`. Startup
  refuses a registry for another environment, a client the bundle does not grant, a
  trusted issuer without a registry entry, or limits above the engine's;
- verdict key (`SCAN_GATEWAY_VERDICT_KEY_FILE`, `_ID`): created once with
  `security-scan-gateway-keygen <kid> <file>`, which never replaces an existing key and
  prints the public key set callers trust.

## Binaries

- `security-scan-gateway`: the service.
- `security-scan-gateway-keygen`: verdict key creation / public key export.
- `security-scan-gateway-probe`: on-host acceptance (signs as a registered caller with
  that caller's own key; clean, EICAR, replay, environment, digest, size, burst, large
  file, engine unreachable from the caller network).

## Tests

`cargo test` runs the contract and security tests against a scripted engine; with
`SCAN_GATEWAY_TEST_CLAMD_ADDRESS` set it also scans clean, EICAR and a 5 MiB file through
a real clamd (CI: `.github/workflows/security-scan-gateway.yml`, official ClamAV image).

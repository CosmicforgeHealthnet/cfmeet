# Service token request binding and replay guard (SEC-012)

Internal service tokens (`packages/service-auth`, ADR 0004) can be bound to the HTTP request
they authorize, and a token presented on a mutating request is single-use. This page is the
rollout runbook. Design and code: `packages/service-auth/src/binding.rs`,
`packages/service-auth/src/replay.rs`, `ServiceTokenVerifier::verify_request` in
`packages/service-auth/src/verifier.rs`; contract:
`packages/contracts/platform/service-auth/token-claims/v1.schema.json`.

## What is bound

| Claim | Meaning |
|---|---|
| `bnd` | request binding version, `1` |
| `htm` | HTTP method, upper case (`POST`) |
| `htu` | canonical request path |

The three claims travel together or not at all. A partial group, an unknown `bnd`, a lower-case
method or a non-canonical path is a malformed token in every receiver mode.

Canonical path (`canonical_request_path`):

* path only: no scheme, host, port, query or fragment (the query string is not bound);
* starts with `/`, at most 1024 bytes;
* percent-escapes of unreserved characters (`A–Z a–z 0–9 - . _ ~`) are decoded, every other
  escape is written with upper-case hex (`%2f` → `%2F`; an encoded slash never becomes a
  separator);
* characters outside RFC 3986 `pchar` and `/` must be percent-encoded;
* no `.` / `..` segments (also after decoding) and no empty segments (`//`): such paths are
  refused, never normalized;
* the trailing slash is significant (`/a/` ≠ `/a`) and the path is case-sensitive.

The signer binds the path exactly as sent (the internal client binds the joined URL's path;
the API Gateway binds the parsed upstream URL's path). Receivers recompute it from the path they
received, before any router prefix stripping (`OriginalUri`).

## Replay guard

For `POST`, `PUT`, `PATCH` and `DELETE` the receiver records `(issuer, jti)` until the token's
`exp` + 30 s skew and refuses a second presentation (`replayed_token`). `GET` / `HEAD` /
`OPTIONS` are not replay-checked. Signers mint a fresh 128-bit random `jti` per token and the
internal client signs every call afresh (a retry is a new hop), so legitimate callers never present
a mutating token twice.

* Default store: a process-wide bounded in-memory cache (`InMemoryReplayGuard`, 100 000 entries;
  expired entries are pruned first, then the entry closest to expiry is evicted and counted).
* Shared store: `ReplayGuard` is a trait. Messaging (the receiver that already runs Redis) uses
  `RedisReplayGuard` (`services/messaging/src/config/replay_guard.rs`) when
  `MESSAGING_REALTIME_REDIS_URL` is set, so a token replayed against another replica is refused
  too (`SET cfmeet:svc-jti:<issuer>:<jti> 1 NX EX <ttl>`). If Redis is unreachable it degrades
  to the local cache and counts `messaging_service_replay_store_degraded_total`.
* The Communications Hub and Organizations keep their durable per-token nonce tables (every
  method, across instances); the security scan gateway keeps its per-client replay check.

## Environment variables

| Variable | Where | Values | Default |
|---|---|---|---|
| `SERVICE_AUTH_REQUEST_BINDING_MODE` | every receiver | `off` / `accept` / `require` | `accept` |
| `SERVICE_AUTH_REQUEST_BINDING` | every signer | `off` / `emit` | `off` |
| `SERVICE_AUTH_REPLAY_GUARD` | every receiver | `on` / `off` | `on` |
| `SERVICE_AUTH_REPLAY_CACHE_CAPACITY` | every receiver | entries | `100000` |

An invalid value stops the service at startup (the panic names the variable, never a value).

Receiver modes:

* `off`: the claims are not compared with the request (they must still be well-formed);
* `accept`: compared when present (`request_method_mismatch`, `request_path_mismatch`);
  tokens without them are accepted — every token today's signers produce is accepted;
* `require`: tokens without the claims are refused (`request_binding_missing`); a receiver
  code path that verifies without the request (`verify_headers`) is refused
  (`request_binding_unchecked`).

## Rollout (three steps, never out of order)

1. **Receivers accept** (this release). Deploy every service with the binding-aware verifier.
   Leave `SERVICE_AUTH_REQUEST_BINDING_MODE` unset (`accept`) and `SERVICE_AUTH_REQUEST_BINDING`
   unset (`off`). Nothing changes on the wire; the replay guard is active on mutating requests.
   Gate: all receivers of a signer run this release.
2. **Signers emit.** Set `SERVICE_AUTH_REQUEST_BINDING=emit` on signers, one service at a time
   (API Gateway first: it carries the most traffic). Receivers in `accept` now verify the
   claims. Watch internal authentication failures by reason (`request_method_mismatch`,
   `request_path_mismatch`) at each receiver; any non-zero count is a path rewrite or a
   caller defect — roll the signer back to `off` and investigate. Gate: all signers emit, a
   full release cycle without mismatches.
3. **Receivers require.** Set `SERVICE_AUTH_REQUEST_BINDING_MODE=require` on receivers, one at a
   time; watch `request_binding_missing` (a signer still on `off`, or an out-of-band caller such
   as an operator tool) and `request_binding_unchecked`.

Rollback at any step is the previous value of the variable; no data migration is involved.
Never set a receiver to `require` while any of its callers is on `off`.

## Verification

```bash
make service-auth-test                       # binding, canonicalization, modes, replay, contracts
cd packages/contracts/validator && cargo run -q -- ../ ../../..
cd services/security-scan-gateway && cargo test --test gateway a_request_bound_token
# End to end through the API Gateway with emission on and a requiring upstream:
cd services/api-gateway && CFMEET_SKIP_DB_TESTS=1 SERVICE_AUTH_REQUEST_BINDING=emit \
  SERVICE_AUTH_REQUEST_BINDING_MODE=require cargo test
# Shared Redis guard (two replicas, one Redis):
cd services/messaging && TEST_REDIS_URL=redis://127.0.0.1:6379/ cargo test --lib config::replay_guard
```

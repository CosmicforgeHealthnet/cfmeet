# 15 — OpenAPI, realtime protocol and compatibility (Part M §9–§10)

## Contract

- Committed contract: `packages/contracts/openapi/meeting-service.json` (OpenAPI 3,
  `info.version` 1.1.0), generated from code by `cargo run --bin export_openapi`
  (utoipa). Parts M / N add **no** routes and change **no** public schema; the contract is
  regenerated and must be byte-identical (CI step "OpenAPI contract export is current").
- Size: **187 operations**, of which **136 under `/api/v2`** (the rest are the retained
  `/api/v1` legacy surface and documented internal / auth routes).

## Runtime ↔ contract parity (certified)

| Check | Test |
|---|---|
| Committed contract equals what the code generates | `openapi_contract::committed_contract_matches_code` + CI diff step |
| Every registered v2 route is documented and vice versa (method + path) | `openapi_contract::every_v2_route_is_documented`, `j_e2e_06_route_table_and_openapi_are_identical` |
| `$ref`s resolve; enums valid; error codes typed and published | `j_e2e_07_contract_is_resolvable_and_consistent`, `j_enum_values_are_valid_and_error_codes_are_typed`, `public_error_codes_are_published` |
| No secrets / provider ids / internal surface in the public contract | `contract_never_describes_secrets_or_provider_ids`, `j_e2e_10_no_secret_or_internal_surface_in_public_contract` |
| Every documented operation is routed and answers with the standard error envelope | `api_contract_matrix::j_api_01..j_api_06`, `j_e2e_09_runtime_errors_match_the_error_schema` |
| **Every v2 operation has a success-path AND an authorization / error-path test** | runtime recording (`tests/common/route_coverage.rs`) + `scripts/certification/route_coverage.py --strict` (CI blocking) — see 02-requirements-matrix.md |

## Error envelope (stable)

```json
{ "success": false, "error": { "code": "PERMISSION_DENIED", "message": "…", "requestId": "req_…" } }
```
`RATE_LIMITED` adds `Retry-After` (≥ 1 s). Internal / database detail is never exposed;
pool acquisition timeout maps to 503 `SERVICE_UNAVAILABLE` (unit tests `error::tests::*`).
The `requestId` is server-authoritative and correlated in logs
(`j_api_03_request_id_is_server_authority_and_correlated`).

## Realtime protocol (v2)

- `POST /api/v2/conferences/{id}/realtime/ticket` → one-shot 60-second ticket (browsers
  cannot set WebSocket headers); authorization happens **before** the upgrade and is
  re-checked during the connection (every 15 s and on authority-changing events).
- `GET /api/v2/conferences/{id}/realtime?ticket=…[&cursor=N]` → WebSocket.
- `GET /api/v2/conferences/{id}/events?after=N` → REST replay.
- Frames: `subscribed {cursor, scope: FULL|SELF_ONLY, degraded, canonical}`,
  `event {event: envelope}`, `degraded {reason: REALTIME_FANOUT_UNAVAILABLE}`,
  `resync_required {cursor, canonical}`, `revoked`, `closed {reason}`, `pong {cursor}`.
- Envelope: `eventId`, `eventType`, `schemaVersion`, `conferenceId`, `meetingSpaceId`,
  `sequence` (gap-free per conference, commit order — the replay cursor), `occurredAt`,
  `correlationId`, `payload`. Provider identities / credentials are stripped at any depth.
- Replay window: 500 events (`MAX_REPLAY`). A cursor beyond the window, ahead of the
  current sequence, or into purged history → `resync_required` + refetch
  `GET /api/v2/conferences/{id}` (certified: `failure_injection_m::m_f_ws_cursor_too_old_requires_resync`,
  `system_e2e::r_e2e_websocket_replay_and_resync`).
- Backpressure: each socket tails PostgreSQL in batches of 200 and awaits its own send, so a
  slow consumer only delays itself (certified: `m_f_ws_slow_consumer_isolated_and_gap_free`;
  load: 500 clients incl. 25 slow, 0 lost deliveries — see 06).
- Delivery latency is bounded by the outbox relay poll (2 s, `main.rs`) plus the socket
  wake; measured p95 ≈ 2.0–2.4 s locally (06). Lowering it is a hardening item, not a
  protocol change.

## Compatibility

| Consumer | Position |
|---|---|
| `apps/web` (current frontend) | Uses only `/api/v1` meeting routes — unchanged and still served; legacy writes enabled by default |
| Legacy meeting links / codes | Resolve through `GET /api/v2/resolve/{identifier}` (code, lower-case code, legacy UUID) and v1 reads (migration certification: 25/25 sampled at 3 000 and 20 000 meetings) |
| Legacy v1 identity semantics | Hardened (no client `user_id` authority) without breaking the web flows (`legacy_v1_web_compat::*`) |
| v2 clients | Not yet built (frontend migration is a later, not-started phase) |

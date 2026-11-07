# Legacy meeting-service webhooks — deprecation record and migration guide

**Findings:** PLT-003 (canonical, P1), DATA-007 (merged duplicate) — Blueprint v1.0 Section 16.
**Disposition:** `DEPRECATED_WITH_MIGRATION`. The routes stay until the sunset preconditions
in §5 are met (master register decision D-9: "only after measured zero consumers,
per-endpoint record"). House style follows the
[legacy API key deprecation record](legacy-api-keys-deprecation.md).
**Status of this record:** recorded 2026-11-07 on branch `blueprint/s16-retirement-tooling`.

## 1. What was wrong

`GET/POST /api/v1/webhooks` and `PATCH/DELETE /api/v1/webhooks/{id}` store a registration
(`endpoint_url`, `event_type`, a generated `secret`) in the meeting-service `webhooks`
table. **No code anywhere reads that table to deliver an event**, and none ever did. The
web Settings → Webhooks page says "Receive real-time notifications for meeting events",
so users configured endpoints that silently never fired.

## 2. Decision: deprecate honestly, do not deliver

Two options were weighed:

| Option | Verdict |
|---|---|
| Deliver legacy registrations through the Developer Platform webhook pipeline (outbox → DP signed delivery) | **Rejected.** Meeting events reach the Developer Platform only for meeting spaces owned by a Platform Access application environment (`services/meeting-service/src/services/platform/events.rs`: "user-owned meetings never produce one"). Legacy registrations belong to personal users, who have no environment. Delivering them would need a second ownership model (user → synthetic environment), a second subscription store and a second signing secret — a new system, which Section 16 forbids. |
| Deprecate with a migration path | **Chosen.** The Developer Platform is the only customer-webhook system (signed delivery, SSRF policy, replay, sealed secrets). |

No registration is ever silently accepted again: every answer says nothing is delivered.

## 3. What changed (Section 16)

| Where | Change |
|---|---|
| Every `/api/v1/webhooks*` response, errors included | `Deprecation: @1794009600` (RFC 9745, 2026-11-07), `Sunset: Sat, 01 May 2027 00:00:00 GMT` (RFC 8594, earliest planned removal), `Link: </swagger-ui/#/Webhooks>; rel="deprecation"; type="text/html"` (`middleware/deprecation.rs`, mounted outside authentication) |
| Create / list / update bodies | each item carries `"delivery": "NOT_DELIVERED"`; each wrapper carries `"notice"`: "Deprecated: legacy meeting webhooks are stored but never delivered; no event will be sent to this endpoint. Use Developer Platform webhooks (/v1/environments/{environmentId}/webhooks) for signed meeting event delivery." |
| OpenAPI (`packages/contracts/openapi/meeting-service.json`) | the four operations are `deprecated: true`; summaries, descriptions and the Webhooks tag say nothing is delivered |
| Behaviour | otherwise unchanged (201 create, list, PATCH, DELETE, owner scoping); a frontend that ignores the new fields keeps working |

The API Gateway already relays `deprecation`, `sunset` and `link` response headers
(`services/api-gateway/src/headers.rs`); the legacy routes are `LEGACY_DIRECT_INGRESS`
(not routed by the gateway) in any case.

## 4. Migration to Developer Platform webhooks

Integrators (not personal users) move to the Developer Platform:

1. Developer Platform organization, project and SANDBOX environment — steps 1–2 of the
   [API key migration guide](legacy-api-keys-deprecation.md#3-migration-steps-for-integrators).
2. Create meetings in that environment with a `cfmk_` credential (`meetings.write`) through
   the API Gateway — only environment-owned meeting spaces produce public events.
3. Register an endpoint: `POST /v1/environments/{environmentId}/webhooks` (developer
   session) and subscribe to the event types below. The secret is sealed at rest and
   shown once; deliveries are signed.
4. Verify signatures and handle retries: deliveries
   `GET /v1/environments/{environmentId}/webhooks/{webhookId}/deliveries`, attempts
   `GET …/webhook-deliveries/{deliveryId}/attempts`, replay
   `POST …/webhook-deliveries/{deliveryId}/replay`, secret rotation
   `POST …/webhooks/{webhookId}/rotate-secret`.
5. Delete the legacy registration (`DELETE /api/v1/webhooks/{id}`).

| Legacy `event_type` | Developer Platform envelope type |
|---|---|
| `meeting_start` | `meeting-service.meeting.started` |
| `meeting_end` | `meeting-service.meeting.ended` |
| `participant_join` | `meeting-service.participant.joined` |
| `participant_leave` | `meeting-service.participant.left` |
| — | also `meeting-service.meeting.created`, `.recording.ready` / `.deleted`, `.artifact.ready` / `.deleted` |

Activation of the Developer Platform pipeline is outside this record (PLT-005, owner):
`MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION=developer-platform` (default `none`),
`DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` and delivery mode (default shadow), and the
gateway/DP deployment ([activation checklist](../operations/developer-platform-gateway-activation-checklist.md)).

Personal accounts have no webhook product. Personal real-time meeting updates are the
first-party clients' job (conference realtime channel, Hub timeline), not webhooks.

## 5. Sunset preconditions (all required before removal)

| # | Precondition | Owner | Status |
|---|---|---|---|
| 1 | Web Settings → Webhooks stops promising delivery: hide the panel or show the `notice` (handoff §6) | frontend | OPEN |
| 2 | Developer Platform webhooks reachable for integrators (PLT-005) | user / release | OPEN |
| 3 | Measured zero use of `/api/v1/webhooks*` for an agreed window (D-9 per-endpoint record; the `Deprecation` header makes callers visible in access logs) | user / release | OPEN |
| 4 | Export, then drop the `webhooks` table and its plaintext `secret` column (DATA-003) by an additive-then-drop pair after an owner-approved backup | backend (later) | NOT STARTED |

## 6. Frontend corrections (owned by the frontend team)

| File | Correction |
|---|---|
| `apps/web/src/components/dashboard/WebhooksContent.tsx` (~line 149, "Receive real-time notifications for meeting events") | remove the promise; show the response `notice` or hide the page; link integrators to the Developer Platform console when it exists (ORG-017) |
| `apps/web/src/services/webhook.service.ts`, `apps/web/src/hooks/useWebhooks.ts` | mark deprecated; surface `delivery: NOT_DELIVERED` |

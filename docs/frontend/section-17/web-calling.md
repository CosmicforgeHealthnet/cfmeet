# Section 17 — Web calling (handoff)

Branch `blueprint/s17-web-calling` (based on `blueprint/s17-web-account`). Applies Blueprint v1.0
Section 17 to the web calling surfaces on top of the Section 12 backend (ADR 0009 amendment,
`docs/operations/org-policy-calling-scope-section-12.md`). Every route below is from a served
contract: `packages/contracts/calling/http/v1.openapi.json` (through the same-origin adapter
`/api/calling/*` / `/api/calling-signaling/*`), `packages/contracts/communications/http/v1.openapi.json`
and `packages/contracts/openapi/meeting-service.json`. `tests/calling-contract.test.mjs` fails if a web
client path, method or mirror enum drifts from them.

## Findings

| Finding | Status | What changed / why not closed |
|---|---|---|
| CAL-002 | FRONTEND_CORRECTED | `src/lib/calling/call-permissions.ts` (adapter + UI): any active participant ends / holds / resumes / transfers its own place; cancel is initiator + pre-answer; a transferred call stays endable; ex-participants only replay after termination. Controls in the call details and the global dock. |
| CAL-004 | PARTIAL (push EXTERNAL_BLOCKED) | `CallingRoot` in the root layout: automatic endpoint registration (opt-out per browser), signaling + credential renewal, global incoming-call card on every signed-in page with caller identity, Web Audio ringtone, tab title, background `Notification` (after permission), accept/decline; the active call keeps media across pages. A closed browser is not woken: Web Push / APNs / FCM is CAL-003 (credentials). Recovered offers carry no caller (SHARED_CONTRACT gap, worked around via the Call view + `principals/names`). |
| CAL-005 | FRONTEND_CORRECTED | Recording authorizations / consent (own endpoint) / start / stop and abuse reports are adapter-allowlisted, participant-bound; `CallRecordingControls` (notice, consent, start/stop, reasons) and `AbuseReportDialog` (bounded categories) are mounted in call details. |
| CAL-006 | FRONTEND_CORRECTED (with gaps) | Voicemail and forwarding are self-service through the existing `/v1/enterprise/*` routes with `principalId` bound to the user (Calling's owner view), no allowlist. Gaps: no owner filter on `GET /v1/enterprise/voicemail/mailboxes` (adapter does a bounded discovery + owner probes); no end-user extension directory (EXTENSION destinations accepted but not offered); forwarding writes are audited as the shared credential, not the human (CAL-007). |
| CAL-008 / FE-010 | FRONTEND_CORRECTED | `runtime-guard.ts`: dev mode only in a non-production build without `CALLING_PLATFORM_CREDENTIAL`; otherwise 503 `CALLING_NOT_CONFIGURED`. |
| CAL-020 | FRONTEND_CORRECTED | Adapter and signaling transport share the https-only production origin guard; signaling socket wss-only in production; identity bearer only over https; signaling credential lifetime checked (≤ 3600 s) and renewed 15 s before expiry. |
| CAL-014 | FRONTEND_CORRECTED (consumption) | Group calls (multi-select, ≤ 64 targets) via `POST /v1/calls`; Hub contacts + contact launch on the Calls page; `CallLaunchButton` (contact / conversation) for the Messaging screens (owned by web-comms). Requirement: the Hub's governed Calling scope must equal the adapter's `CALLING_*` scope (deployment config) so the launched call and endpoint are visible to the adapter. |
| CAL-015 | FRONTEND_CORRECTED | `media-reconnect.ts` state machine: LiveKit's own retry surfaced, then fresh media credential + bounded backoff (1→15 s, 5 attempts, inside the 60 s participation grace), failure surfaced with manual retry. |
| CAL-022 | PARTIAL | Contract test + adapter/logic tests in CI. A two-browser E2E against staging Calling + LiveKit is EXTERNAL_BLOCKED (staging credentials / test accounts). |
| Org enterprise calling admin | FRONTEND_CORRECTED (backend flag-off) | `OrganizationCallingAdmin` / `OrganizationCallingPanel`: "Calling" tab of `/dashboard/organizations/{id}` and `/calling/organization` (workspace), only with `calling.enterprise.manage`; overview / queues (+ live entries) / extensions / recordings (audited media access) through `/api/v2/organizations/{id}/calling/{path}`; 409 `ORGANIZATION_CALLING_DISABLED` → "not enabled for this organization". |

## Traceability (screen / action → method / path → errors handled)

| Screen / action | Method / path | Errors surfaced |
|---|---|---|
| Any page: auto-register browser | `POST /api/calling/v1/calling/endpoints` → Calling `POST /v1/calling/endpoints` | 401 sign-in, 503 `CALLING_NOT_CONFIGURED` / unavailable (status "offline" + Reconnect) |
| Any page: signaling | `POST /v1/calling/signaling-credentials`; WS `GET /v1/calling/signaling` (wss); `GET /api/calling-signaling/v1/calling/invitations` | close 4401 → re-register; other closes → backoff; invalid credential → retry |
| Incoming card: caller | `GET /v1/calls/{id}` (adapter), `POST /api/v2/messaging/principals/names` | unnamed → "CF Meet user" |
| Incoming card: accept / decline | `POST /api/calling-signaling/v1/calling/invitations/{id}/accept|decline` | 409 `STALE_CALL_VERSION`, `ANSWERED_ELSEWHERE`, message shown |
| Join media / rejoin | `POST /api/calling-signaling/v1/calling/sessions/{sid}/media-credentials` (fallback `POST /v1/calls/{id}/sessions/{sid}/media-credentials`) | 401/403/404/409/410 → failed (no retry); network/408/429/5xx → backoff |
| Dock / details: end, hold, resume, cancel | `POST /v1/calls/{id}/end|hold|resume|cancel` (`expectedVersion`, Idempotency-Key) | adapter 404 if not permitted; 409 stale → refreshed state message |
| Leave | `POST /v1/calls/{id}/sessions/{sid}/remove` | message |
| Transfer | `POST /v1/calls/{id}/transfer` (transferringParticipantId injected) | message |
| Start direct / group / extension call | `POST /v1/calls` (initiator + endpoint injected; protected-contact fields 403) | 404 device, policy/limit errors shown |
| Contacts: call | `GET /api/v2/communications/contacts`; `POST /api/v2/communications/contacts/{id}/calls` | Hub envelope (`PERMISSION_DENIED`, `RATE_LIMITED`, `COMMUNICATIONS_UNAVAILABLE`, `EMAIL_VERIFICATION_REQUIRED`) shown; launched-but-not-joinable message |
| Chat: call | `POST /api/v2/communications/conversations/{id}/calls` (`CallLaunchButton`) | as above |
| Recording | `GET /v1/calls/{id}/recording-authorizations|recordings` (principalId), `POST …/recording-consent` (own endpoint), `POST …/recordings`, `POST …/recordings/{rid}/stop` | reason text per `RecordingAuthorizationReason`; errors shown |
| Report call | `POST /v1/calls/{id}/abuse-reports` (reporter injected); `GET /v1/abuse-reports` | 400 invalid category; repeat returns existing |
| Settings › Voicemail | `GET /v1/enterprise/voicemail/mailboxes` (owner-filtered), `GET …/messages?mailboxId`, `POST …/{id}/media-access`, `PUT …/{id}/read-state`, `DELETE …/{id}?expectedVersion` (all principal-bound) | 403 scope → empty, 404/409 shown, SHA-256 mismatch refused |
| Settings › Forwarding | `GET|POST /v1/enterprise/forwarding-rules`, `PATCH|DELETE …/{id}` (ownership checked) | 403 destination type, 404 foreign rule, 409 version |
| Devices | `GET /v1/calling/endpoints`, `DELETE /v1/calling/endpoints/{id}` | message |
| Organization › Calling | `GET /api/v2/organizations/{id}/calling/v1/enterprise/effective-policy|queues|queues/{id}/entries|extensions|recordings`, `…/v1/calling/limits`, `POST …/v1/enterprise/recordings/{id}/media-access` | 409 `ORGANIZATION_CALLING_DISABLED`, 409 `PROVISIONING_PENDING`, 403 `PERMISSION_DENIED` / `ORGANIZATION_POLICY_RESTRICTED`, 409 `ORGANIZATION_SUSPENDED`, 503 |

## Deployment notes (not done here)

* Voicemail / recording playback fetches the signed URL (connect-src) and plays a blob: URL; the
  object-storage origin must be in `connect-src` (`CSP_STORAGE_URL` / `CSP_EXTRA_CONNECT_SRC`) and allow
  the web origin by CORS.
* The shared credential must hold `enterprise.config.read|write` and `enterprise.voicemail.read` for the
  self-service routes; without them Calling answers 403 (shown as "no voicemail box").
* `CALLING_DEV_MODE` must be absent in every deployed environment (now enforced in code).

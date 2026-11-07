# CF Meet — Feature Gap Register

**Audit phase:** Phase 1 — audit only, no implementation · **Repository:** `CosmicforgeHealthnet/cfmeet` · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` (`staging` = `main`) · **Date:** 2026-11-06

**Related:** [executive report](cfmeet-platform-comprehensive-audit.md) · [feature gap register](cfmeet-feature-gap-register.md) · [API inventory](cfmeet-api-inventory.csv) · [implementation plan (proposal)](cfmeet-recommended-implementation-plan.md)

> **Method and evidence rules.** Every finding was established by reading the code at the baseline commit. The file and line references are to that commit. No application code, schema, contract, configuration or deployment was changed, and no production host was contacted. No test suites were run by this audit: the sandbox has no service databases, and building would write into the repository. A feature is `COMPLETE_VERIFIED` only where an automated test exercises it against a real database or real stack **in CI**. Those CI runs were read, not re-executed. The latest backend CI evidence is on `e717d018`, an ancestor of the baseline whose six later commits change only docs, infra and workflows. The lead auditor re-checked the highest-severity claims directly; see "Lead-auditor verification" below.

> **How to read this register.** §1 summarizes completeness by domain. §2 is the master list of every finding, ordered by severity, with duplicates reported by more than one auditor consolidated under one canonical ID: `→ X` means "consolidated into X", `+ …` lists the duplicates merged into a canonical finding. §3 holds every feature matrix row: missing, partial, unverified, mismatched and complete. §4 holds the full evidence for domains that have no dedicated audit document. Meetings, Organizations, Frontend and Data findings are detailed in their own documents, linked in the Detail column.


## 1. Completeness summary

| Domain | Features | COMPLETE_VERIFIED ² | IMPLEMENTED_UNVERIFIED | PARTIALLY_IMPLEMENTED | MISSING | CONTRACT_MISMATCH | FRONTEND_ONLY_GAP | EXTERNAL_DEPENDENCY | DEFERRED_BY_OWNER | NOT_REQUIRED |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Meetings, scheduling, calendar | 37 | 20 | 2 | 6 | 6 | 1 | 1 | 0 | 1 | 0 |
| Organizations | 32 | 3 | 6 | 6 | 13 | 0 | 3 | 0 | 0 | 1 |
| Accounts, contacts, devices, notifications | 35 | 0 | 13 | 8 | 5 | 0 | 7 | 1 | 1 | 0 |
| Calling, enterprise, telephony, numbering, SMS, relay | 42 | 29 | 1 | 1 | 2 | 0 | 2 | 6 | 0 | 1 |
| Messaging, Hub, realtime | 42 | 8 | 3 | 6 | 0 | 4 | 16 | 3 | 1 | 1 |
| Platform (Developer Platform, gateway, SDK, admin) | 23 | 10 | 6 | 5 | 0 | 2 | 0 | 0 | 0 | 0 |
| **Total product features** | **211** | **70** | **31** | **32** | **26** | **7** | **29** | **10** | **3** | **3** |
| *Frontend screens (separate lens, overlaps the rows above)* | *39* | *0* | *20* | *6* | *0* | *5* | *5* | *1* | *0* | *2* |
| *Security / reliability controls (separate lens)* | *30* | *1* | *12* | *13* | *1* | *0* | *0* | *3* | *0* | *0* |

² `COMPLETE_VERIFIED` here means **backend** behaviour exercised by an automated test against a real database or real stack in CI. It does **not** mean a user can complete the feature in a shipped client. 0 of 39 frontend screens are verified end to end.

**Findings after consolidation:** 149 canonical findings (182 raw, 32 merged as duplicates, 1 informational) · **P0 4 · P1 48 · P2 66 · P3 31** · owner class: BACKEND 69, FRONTEND 37, SHARED_CONTRACT 17, PRODUCT_DECISION 12, INFRASTRUCTURE 10, EXTERNAL_DEPENDENCY 4

## 2. Master findings register

| ID | Finding | Severity | Owner class | Classification | Confidence | Complexity | Consolidated | Detail |
|---|---|---|---|---|---|---|---|---|
| ACC-002 | The v1 auth middleware and the developer session bridge ignore the account revocation epoch: password reset/change, logout and deactivation do not end sessions there | P0 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW | + SEC-003 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-004 | Google sign-in silently takes over a pending email/password account (pre-hijacking); weak ID-token validation | P0 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED (code path); exploit not executed | MEDIUM | + SEC-005, ACC-005, FE-011 | [accounts](#accounts-identity-contacts-devices-notifications) |
| MTG-001 | Mobile meeting creation, scheduling, listing, editing and instant meeting are broken (wrong v1 contract, and retired by the legacy-write flag) | P0 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | MEDIUM | + FE-002, FE-022 | [meetings](cfmeet-meetings-calendar-audit.md) |
| SEC-010 | No backup coverage for meeting-service, communications and developer-platform databases; no scheduled backups in repo | P0 | INFRASTRUCTURE |  | CONFIRMED (repository); REQUIRES_VALIDATION (host-level snap | MEDIUM |  | [security-data](#security-reliability-production-readiness) |
| ACC-001 | Access and refresh tokens are indistinguishable: a refresh token works as a 7-day bearer token, and an access token can mint refresh tokens | P1 ¹ | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED (code). Not reproduced at runtime. | MEDIUM | + SEC-002 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-003 | Credential revocation is switched off whenever the Communications bridge is not configured | P1 ¹ | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED (code); REQUIRES_VALIDATION (production config) | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-005 | The OAuth callback puts the raw access and refresh tokens in the redirect URL; no PKCE/nonce; state not bound to the browser | P1 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED | MEDIUM | → ACC-004 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-006 | Google signup fails for common or long display names | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-007 | No session model: no session listing, no per-device logout, and logout is "everywhere" or "nowhere" depending on configuration | P1 | BACKEND | MISSING | CONFIRMED | HIGH |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-008 | Device revocation does not invalidate the account session; no device-management surface | P1 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | HIGH |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-009 | The Communications Hub (contacts, discovery, CF Meet ID/QR, invites, blocks, privacy, notification preferences, devices) has no frontend consumer | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | HIGH | → MSG-006 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-010 | Revocation fan-out is lost for accounts without a Hub profile; Platform Access principals never mirror account status | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED (code); REQUIRES_VALIDATION (Calling `governs()` s | MEDIUM |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-011 | No account deletion or personal data export | P1 | PRODUCT_DECISION | MISSING | CONFIRMED | HIGH | + DATA-009 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-012 | Mobile logs credentials and tokens, misroutes unverified users, and lacks Google sign-in and push | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-013 | No multi-factor authentication | P1 | PRODUCT_DECISION | MISSING | CONFIRMED | MEDIUM |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-018 | JWT secret handling: insecure default, no strength check, a single HS256 key for every token kind | P1 ¹ | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED (code); REQUIRES_VALIDATION (deployed environment) | LOW | → SEC-001 | [accounts](#accounts-identity-contacts-devices-notifications) |
| CAL-001 | Connected calls never terminate when the parties leave; abandoned calls live forever and their media room is re-created | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-002 | Web: only the call initiator can End/Hold/Resume; after a transfer nobody can end the call from the UI | P1 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-003 | No incoming-call push (APNs VoIP/PushKit, FCM, Web Push) and no mobile calling client | P1 | BACKEND | MISSING | CONFIRMED | HIGH | + MSG-011, ACC-020 | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-004 | Web incoming-call experience is page-bound and manual (no auto device registration, no global ring, no ringtone, no caller identity) | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | MEDIUM |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-005 | Recording consent/controls and abuse reporting are unreachable from the web (components unused, adapter returns 404) | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | LOW | + FE-009 | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-006 | End-user voicemail and forwarding settings require the platform-admin allowlist; admins get a cross-user Administrator view | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | LOW–MEDIUM | + FE-008 | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-007 | Web calling runs every CF Meet user through one platform credential/scope: enterprise calling is not multi-organization and admin actions lose the human actor | P1 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED (code); production env values REQUIRES_VALIDATION | HIGH | + ORG-012 | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-008 | `CALLING_DEV_MODE=true` disables authentication in the web adapter with no production guard | P1 | FRONTEND | IMPLEMENTED_UNVERIFIED | CONFIRMED (code); deployed value REQUIRES_VALIDATION | LOW | + FE-010 | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| DATA-002 | `users.email` uniqueness and lookups are case-sensitive | P1 | BACKEND |  | CONFIRMED | MEDIUM | + ACC-023 | [security-data](#security-reliability-production-readiness) |
| DATA-003 | Secrets at rest in meeting-service schema | P1 | BACKEND |  | CONFIRMED |  | → PLT-002 | [security-data](#security-reliability-production-readiness) |
| DATA-009 | Conflicting FK delete semantics and no account-deletion path | P1 | BACKEND |  | CONFIRMED |  | → ACC-011 | [security-data](#security-reliability-production-readiness) |
| DATA-013 | Migrations run by the runtime role without pre-migration snapshot (data side of SEC-011/SEC-018) | P1 | BACKEND |  | CONFIRMED |  | → SEC-011 | [security-data](#security-reliability-production-readiness) |
| DATA-015 | Missing audit-history tables in the account authority | P1 | BACKEND |  | CONFIRMED |  | → SEC-013 | [security-data](#security-reliability-production-readiness) |
| FE-001 | Web (and mobile) pre-join reads a PreJoinResponse shape the backend does not return; consent-required joins cannot succeed | P1 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-002 | Mobile v1 meeting and profile screens use response/request shapes the backend never had | P1 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | MEDIUM | → MTG-001 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-004 | Mobile live room is a non-functional shell (no media SDK, broken id mapping, v1 endpoints with v2 ids) | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | HIGH | → MTG-002 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-005 | Mobile guest join never keeps the participant credential and re-POSTs join every 3 s while waiting | P1 | FRONTEND | CONTRACT_MISMATCH | REQUIRES_VALIDATION (the duplicate-participant creation is i | LOW |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-006 | Mobile build config hard-codes a private LAN IP over cleartext HTTP, overriding env | P1 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED (that CI builds really pick this up is REQUIRES_VA | LOW |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| MSG-001 | REST event replay uses `afterEventSequence` but Messaging only accepts `after`; web realtime recovery never advances and re-subscribes in a loop | P1 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED for the mismatch and the 400. The resubscribe loop | LOW (contract) |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-002 | Meeting relay mounts GET/POST/PUT/DELETE only: every allowlisted PATCH (MANAGED edit, member role, conversation update/retention) is unreachable for CF Meet clients | P1 | BACKEND | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-004 | In production, first-party users can never get realtime: client tokens need a registered MLS device, and device registration is refused in production | P1 | PRODUCT_DECISION | PARTIALLY_IMPLEMENTED | CONFIRMED (code). Production runtime behaviour REQUIRES_VALI | MEDIUM |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-005 | Web Messages UI consumes only a fraction of the verified backend (no typing, reactions, edits, replies, pins, unread, receipts, delete, search, block/report, invitations, back-pagination) | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | HIGH |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-006 | The Communications Hub (~70 public endpoints) and the unified Chats/Calls journeys have no frontend consumer; mobile has no Unified Messaging at all | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | HIGH | + ACC-009, FE-020 | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-013 | PRIVATE_E2EE is correctly gated off in production pending independent review; key transparency is unwitnessed | P1 | EXTERNAL_DEPENDENCY | EXTERNAL_DEPENDENCY | CONFIRMED | HIGH (external |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MTG-002 | Mobile live room drives a v2 conference through legacy v1 participant/waiting/chat/media/end endpoints | P1 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | MEDIUM | + FE-004 | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-003 | Mobile v2 client targets non-existent paths | P1 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | LOW | + FE-003 | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-004 | Web meeting list and dashboard show the space creation date, not the scheduled date/time | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | LOW |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-005 | No cross-space "my meetings" / agenda / calendar range API; upcoming & past listing requires N+1 per-space calls and excludes invitees | P1 | BACKEND | MISSING | CONFIRMED | MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-006 | No invitations by email, no schedule-change notifications, no reminders | P1 | BACKEND | MISSING | CONFIRMED | MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-016 | Organization meetings: backend supports org-scoped spaces, web cannot create them and nobody can list them | P1 ¹ | FRONTEND | MISSING | CONFIRMED | MEDIUM | + ORG-010 | [meetings](cfmeet-meetings-calendar-audit.md) |
| ORG-001 | Three unreconciled "Organization" authorities and two independent membership/role systems | P1 | PRODUCT_DECISION | CONTRACT_MISMATCH | CONFIRMED | HIGH | + PLT-010, DATA-004 | [organizations](cfmeet-organizations-audit.md) |
| ORG-002 | End users cannot create a meeting organization; platform-admin-only creation has no tooling | P1 | PRODUCT_DECISION | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-003 | Creation metadata and location missing from the user-facing Organization view (root cause per field) | P1 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW (1-2) / ME |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-004 | No organization detail endpoint and no profile editing in meeting-service | P1 | BACKEND | MISSING | CONFIRMED | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-005 | Meeting-org membership is added unilaterally by raw user UUID; no invitation/acceptance or consent | P1 | BACKEND | MISSING | CONFIRMED | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-010 | Web cannot create or edit organization meetings although backend supports them | P1 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | LOW | → MTG-016 | [organizations](cfmeet-organizations-audit.md) |
| ORG-012 | No Organization ↔ Enterprise Calling relationship in the CF Meet product; enterprise admin is a deployment-wide env allowlist | P1 | SHARED_CONTRACT | MISSING | CONFIRMED | HIGH | → CAL-007 | [organizations](cfmeet-organizations-audit.md) |
| PLT-001 | The gateway always denies `POST /v1/privacy-relay/contact-access-grants`: its required scope is a prose string, not a scope | P1 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-002 | Legacy meeting-service API keys are stored and matched in plaintext; the usage counter races | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | + SEC-006, DATA-003 | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-003 | Three webhook systems; meeting-service "webhooks" are registered but never delivered, while the web UI promises real-time meeting events | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | + DATA-007 | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-004 | Two unrelated API-credential systems; the web developer page documents the legacy one with the wrong host and key format | P1 | PRODUCT_DECISION | CONTRACT_MISMATCH | CONFIRMED | MEDIUM (consol | + FE-013 | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-005 | API Gateway and Developer Platform are not deployed; SDK and public-doc base URL assumes the gateway at `api.cfmeet.com` | P1 | INFRASTRUCTURE | DEFERRED_BY_OWNER | CONFIRMED (not deployed by the repo). REQUIRES_VALIDATION (l | HIGH |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| SEC-001 | Hard-coded JWT signing-secret fallback in meeting-service (account authority) | P1 ¹ | BACKEND | IMPLEMENTED_UNVERIFIED | CONFIRMED (code); REQUIRES_VALIDATION (production env) | LOW | + ACC-018 | [security-data](#security-reliability-production-readiness) |
| SEC-002 | Access and refresh tokens are interchangeable (refresh token = 7-day bearer token) | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | → ACC-001 | [security-data](#security-reliability-production-readiness) |
| SEC-003 | Logout / password change / reset do not revoke refresh tokens on legacy routes; password change/reset revoke nothing | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED |  | → ACC-002 | [security-data](#security-reliability-production-readiness) |
| SEC-005 | Google OAuth: no `aud`/`email_verified` check, link-by-email, tokens returned in URL query string | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | → ACC-004 | [security-data](#security-reliability-production-readiness) |
| SEC-006 | Legacy API keys stored and looked up in plaintext; non-atomic quota counter | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW–MEDIUM. Re | → PLT-002 | [security-data](#security-reliability-production-readiness) |
| SEC-009 | Legacy kick does not evict media; legacy LiveKit tokens are 6 h with client-side `room_admin` for hosts | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [security-data](#security-reliability-production-readiness) |
| SEC-011 | Production migrations run without a pre-migration snapshot; rollback is container-only | P1 | INFRASTRUCTURE | PARTIALLY_IMPLEMENTED | CONFIRMED |  | + DATA-013 | [security-data](#security-reliability-production-readiness) |
| SEC-013 | No security audit trail for account, admin and API-key events in meeting-service | P1 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED |  | + DATA-015 | [security-data](#security-reliability-production-readiness) |
| SEC-016 | No alert rules / paging as code; alert thresholds still "APPROVAL REQUIRED" | P1 | INFRASTRUCTURE | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [security-data](#security-reliability-production-readiness) |
| ACC-014 | Account enumeration across every public auth endpoint | P2 | BACKEND | IMPLEMENTED_UNVERIFIED | CONFIRMED | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-015 | Weak rate limiting on auth endpoints | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW | + SEC-008 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-016 | Email verification is not enforced anywhere that matters | P2 | PRODUCT_DECISION | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-017 | Refresh rotation without reuse detection or absolute lifetime | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW (interim)  |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-019 | Web stores bearer and refresh tokens in JavaScript-readable cookies | P2 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | + SEC-021 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-020 | Notifications: preferences mostly unenforced, no client push registration, incoming-call push not implemented | P2 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | HIGH | → CAL-003 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-021 | Minimal profile: username only; no avatar, no separate display name; inconsistent validation | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-022 | User status string contract mismatch | P2 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| CAL-009 | Shared credential concentrates per-credential limits and serializes call creation platform-wide | P2 | INFRASTRUCTURE | IMPLEMENTED_UNVERIFIED | CONFIRMED (code defaults); production limit rows REQUIRES_VA | LOW (config) / |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-010 | PSTN telephony is software-complete against simulators but has no carrier, is not deployed, and is flag-disabled in production | P2 | EXTERNAL_DEPENDENCY | EXTERNAL_DEPENDENCY | CONFIRMED | HIGH |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-011 | Numbering has only the deterministic synthetic provider; no real number-provider adapter exists | P2 | EXTERNAL_DEPENDENCY | EXTERNAL_DEPENDENCY | CONFIRMED | HIGH |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-013 | Privacy Relay not deployed; web admin pages proxy `/v1/privacy-relay/contacts*` to Calling, which does not serve them | P2 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-014 | Web does not consume group calling or Communications Hub chat/contact-to-call; it runs a parallel launch path | P2 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | MEDIUM |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-015 | Web media reconnect is manual after LiveKit gives up; status not surfaced | P2 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-016 | Enterprise SIP desk phones depend on Telephony + Asterisk edge that are not deployed | P2 | INFRASTRUCTURE | COMPLETE_VERIFIED | CONFIRMED | MEDIUM |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-017 | Admin review gaps: no recording discovery, no org-wide call list, raw-JSON enterprise editors | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-022 | Web calling has never been verified end-to-end against the real backend | P2 | FRONTEND |  | CONFIRMED |  |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| DATA-001 | Legacy meeting-service tables use naive TIMESTAMP and NaiveDateTime | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [security-data](#security-reliability-production-readiness) |
| DATA-004 | Identity and organization data duplicated across services without a single documented authority map | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED |  | → ORG-001 | [security-data](#security-reliability-production-readiness) |
| DATA-005 | Four device registries | P2 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| DATA-006 | Three block stores | P2 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| DATA-007 | Legacy meeting webhooks: accepted and stored, never delivered | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED |  | → PLT-003 | [security-data](#security-reliability-production-readiness) |
| DATA-008 | Dual legacy/v2 meeting model still live | P2 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| DATA-010 | Retention gaps | P2 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| DATA-011 | Migration-history fragmentation ("one new history table per additive migration") | P2 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| DATA-016 | Missing business/identity fields on `users` | P2 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| FE-003 | Mobile v2 API module targets 35 non-existent paths and wrong DTOs | P2 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | MEDIUM | → MTG-003 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-007 | Web artefact views read fields the backend does not send | P2 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-008 | Calling "Settings" exposes forwarding, voicemail and usage tabs that the Next adapter admin-gates (403 for normal users) | P2 | PRODUCT_DECISION | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | → CAL-006 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-009 | Call recording/consent, abuse reporting and platform usage clients exist but are unreachable (adapter has no rule; components unmounted) | P2 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | MEDIUM | → CAL-005 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-010 | CALLING_DEV_MODE mock identity and mock data have no production guard in the Next server routes | P2 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW | → CAL-008 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-011 | Google OAuth hands tokens to the web app in the URL query; callback error UI is an empty placeholder | P2 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | → ACC-004 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-013 | Developers page is static documentation for the legacy v1 third-party API on a different domain; developer-platform has no UI | P2 | PRODUCT_DECISION | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW (docs) / H | → PLT-004 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-015 | No contract-parity gate: handwritten frontend types, v2 web tests not in CI, mobile has no tests | P2 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-016 | Issue #37 (Chromium web messaging journey): the history-recovery and transfer-approval UI only renders inside a collapsed panel | P2 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED (cause of the journey failure); REQUIRES_VALIDATIO | LOW | → MSG-014 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-017 | Spec information-architecture gaps in web: live room keyed by meeting code, no profile-policy editor, waiting room not a route | P2 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-020 | Communications Hub (73 operations) has no frontend consumer | P2 | PRODUCT_DECISION | FRONTEND_ONLY_GAP | CONFIRMED | HIGH | → MSG-006 | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-021 | Mobile covers only auth plus a broken legacy meeting flow; no spaces, conferences, messaging, calling or organizations | P2 | PRODUCT_DECISION | FRONTEND_ONLY_GAP | CONFIRMED | HIGH |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-022 | Mobile is the remaining v1 meeting-write client, which blocks legacy-write retirement | P2 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | → MTG-001 | [frontend](cfmeet-frontend-backend-coverage.md) |
| INV-002 | 200 of 1,066 registered operations have no automated test that references their path (incl. 13 meeting-service auth/API-key/webhook routes and 89 messaging delegated-mirror routes) | P2 | BACKEND | IMPLEMENTED_UNVERIFIED | CONFIRMED (path-literal heuristic) | MEDIUM |  | [inventory](cfmeet-api-duplication-report.md) |
| MSG-003 | Relay query allowlists drop documented parameters (`scope` on message delete, `reason` on member removal, `threadRootId`, conversation filters) | P2 | BACKEND | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-007 | No server-side inbox model: conversations have no title/avatar, last message/activity or unread, and are ordered by creation time | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-008 | Web MANAGED client: append-only sync, no tombstone/edit refresh, fragile attachments and a non-durable offline queue | P2 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | MEDIUM |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-009 | Meeting links and calls in chat exist only as Hub timeline cards (not chat messages), with no realtime and no card for members without a Hub profile | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-010 | Hub ↔ owner integration is not tested end-to-end in CI; Hub tests use simulated owners | P2 | BACKEND | IMPLEMENTED_UNVERIFIED | CONFIRMED | MEDIUM |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-011 | Offline notifications: push providers and KT witnesses are not configured in production; no client registers push | P2 | EXTERNAL_DEPENDENCY | EXTERNAL_DEPENDENCY | CONFIRMED (config claims REQUIRES_VALIDATION on the host). | MEDIUM | → CAL-003 | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-012 | Meeting chat migration cutover would break mobile in-meeting chat (v1 chat refused after cutover; dormant v2 mobile client targets a non-existent route) | P2 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | LOW–MEDIUM |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-014 | Web E2EE journey fails in CI at multi-device history recovery (known frontend issue #37) | P2 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | LOW | + FE-016 | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-017 | Administration needed to run CF Meet's own environment (policy incl. attachments enablement, moderation queue, legal holds) has no first-party surface; attachments default to disabled | P2 | PRODUCT_DECISION | FRONTEND_ONLY_GAP | REQUIRES_VALIDATION (production policy state) | MEDIUM |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MTG-007 | No ICS export/import and no external calendar integration | P2 | BACKEND | MISSING | CONFIRMED | MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-008 | Recurring occurrences exist only within a 30-day horizon; range queries beyond it return nothing | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-009 | Reschedule silently clears the end time and skips the create-time validations | P2 | BACKEND | IMPLEMENTED_UNVERIFIED | CONFIRMED | LOW |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-010 | Past start times accepted, stale occurrences never expire, and the "canonical conference" can point at a stale occurrence | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-011 | Summary-view inconsistency: space summary leaks the schedule, conference summary hides it (breaks guest countdown; exposes healthcare appointment times) | P2 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-012 | Timezone semantics are split and the UI ignores the meeting timezone for one-off occurrences | P2 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW–MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-013 | Joining at the scheduled time: no auto-start and no host start in pre-join | P2 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-014 | Series modification only by replacement; exceptions orphaned and rescheduled occurrences survive "end series" | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-015 | Recurrence UI requires raw RRULE text and exact ISO timestamps; no occurrence preview | P2 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-017 | Legacy v1 write path is a dead end: new v1 meetings are never mirrored to v2, so current clients cannot join them | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW (flag) / M |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-018 | Space list is unpaginated and does N+1 full-table-per-space conference loads | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| ORG-006 | Member list is identity-less and member visibility is inconsistent with the messaging directory | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-007 | No durable audit history for meeting-org lifecycle and membership changes | P2 | BACKEND | MISSING | CONFIRMED | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-008 | Meeting-org lifecycle incomplete: no archive/delete/close, no ownership transfer, no last-owner protection, suspended orgs invisible to members | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-009 | Organization profile policy (org settings) has storage and readers but no write API | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-011 | No organization calendar / org meeting listing; org managers have no authority over org-scoped spaces | P2 | BACKEND | MISSING | CONFIRMED | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-014 | Developer-org creation can orphan a Platform Access tenant | P2 | BACKEND | IMPLEMENTED_UNVERIFIED | REQUIRES_VALIDATION (failure mode inferred from code order;  | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-016 | Mobile app has no organization experience | P2 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | MEDIUM |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-017 | Developer organization console (self-service orgs, invitations, ownership transfer) is backend-only | P2 | FRONTEND | DEFERRED_BY_OWNER | CONFIRMED | HIGH |  | [organizations](cfmeet-organizations-audit.md) |
| PLT-006 | Three Swagger renderers publish contradictory "public" surfaces; calling's Swagger UI is unhardened | P2 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED | LOW–MEDIUM |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-007 | Contract quality gates and the private catalogue skip the Developer Platform, Gateway and Communications contracts | P2 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-008 | Credential lifecycle lacks last-used tracking and expiry warnings | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-009 | Operator/admin plane is fragmented, partly unaudited, has no UI, and audit trails are write-only | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-010 | Three organization models (meeting v2 orgs, DP organizations, PA tenants) with no linkage | P2 | PRODUCT_DECISION | PARTIALLY_IMPLEMENTED | CONFIRMED (existence). REQUIRES_VALIDATION (intended model). | HIGH | → ORG-001 | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| SEC-004 | Guest JWTs can be refreshed indefinitely without the participant still existing | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [security-data](#security-reliability-production-readiness) |
| SEC-008 | Brute-force/enumeration controls are per-email only | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW | → ACC-015 | [security-data](#security-reliability-production-readiness) |
| SEC-012 | Internal service tokens are not bound to method/path/body and replay is only blocked by 2 receivers | P2 | SHARED_CONTRACT | IMPLEMENTED_UNVERIFIED | CONFIRMED | MEDIUM |  | [security-data](#security-reliability-production-readiness) |
| SEC-014 | Meetings v2 creates lack Idempotency-Key; legacy v1 writes have check-then-act races | P2 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [security-data](#security-reliability-production-readiness) |
| SEC-015 | Migration binaries ship the full sea-orm CLI (down/fresh/reset/refresh) in production images | P2 | BACKEND |  | CONFIRMED | LOW |  | [security-data](#security-reliability-production-readiness) |
| SEC-017 | TLS automation covers only the `api.cfmeet.com` lineage | P2 | INFRASTRUCTURE | IMPLEMENTED_UNVERIFIED | CONFIRMED | LOW |  | [security-data](#security-reliability-production-readiness) |
| SEC-018 | Meeting-service runtime DB role has DDL; meeting-service excluded from least-privilege provisioning | P2 | INFRASTRUCTURE | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [security-data](#security-reliability-production-readiness) |
| SEC-020 | Tenant-isolation sweep not complete; sampled handlers are correct | P2 | BACKEND | IMPLEMENTED_UNVERIFIED | REQUIRES_VALIDATION | MEDIUM |  | [security-data](#security-reliability-production-readiness) |
| SEC-021 | Tokens stored in JS-readable cookies on web | P2 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM | → ACC-019 | [security-data](#security-reliability-production-readiness) |
| ACC-023 | Email addresses are not normalized | P3 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW | → DATA-002 | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-024 | Calling signaling WebSocket is not closed when its endpoint is revoked | P3 | BACKEND | IMPLEMENTED_UNVERIFIED | REQUIRES_VALIDATION | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-025 | Dead or unsafe auth helpers and minor hygiene | P3 | BACKEND | IMPLEMENTED_UNVERIFIED | CONFIRMED | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| ACC-026 | Deactivation needs no re-authentication and cannot be cleanly reversed | P3 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [accounts](#accounts-identity-contacts-devices-notifications) |
| CAL-012 | SMS Gateway has only a deterministic (test) provider, refused in staging/production | P3 | EXTERNAL_DEPENDENCY | EXTERNAL_DEPENDENCY | CONFIRMED | MEDIUM |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-018 | Declining on one device does not decline the call for the user; caller keeps ringing until TTL | P3 | PRODUCT_DECISION | DEFERRED_BY_OWNER | CONFIRMED | LOW — Regressi |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-019 | Attended (consultative) transfer not implemented | P3 | PRODUCT_DECISION |  | CONFIRMED |  |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-020 | Signaling adapter lacks the production HTTPS guard | P3 | FRONTEND |  | CONFIRMED |  |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| CAL-021 | Voicemail/recording artifacts are not malware-scanned (acceptable today) | P3 | PRODUCT_DECISION |  | CONFIRMED |  |  | [calling](#calling-enterprise-calling-telephony-numbering-sms-privacy-relay) |
| DATA-012 | Inconsistent type for the same concept (`credentials_not_before`) | P3 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| DATA-014 | Migration numbering/dating anomalies | P3 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| DATA-017 | Cross-service references are opaque (no FKs) — need reconciliation coverage | P3 | BACKEND |  | REQUIRES_VALIDATION |  |  | [security-data](#security-reliability-production-readiness) |
| FE-012 | Mobile placeholders: Google sign-in "coming soon", Help/Terms/Privacy rows inert, space edit TODO | P3 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | LOW |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-014 | Dead legacy v1 meeting code in the web room and hooks | P3 | FRONTEND | NOT_REQUIRED | CONFIRMED | LOW |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-018 | First-party apps bypass the API Gateway; 59 web operations are intentionally absent from the gateway manifest | P3 | INFRASTRUCTURE | NOT_REQUIRED | CONFIRMED | LOW |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| FE-019 | Mobile error decoding ignores the v2 error envelope and mislabels every 401 | P3 | FRONTEND | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [frontend](cfmeet-frontend-backend-coverage.md) |
| INV-001 | No CI gate proves gateway manifest upstream paths exist in the upstream routers (all 572 resolve today, by static extraction) | P3 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [inventory](cfmeet-api-duplication-report.md) |
| MSG-015 | Documentation drift: ADR 0053 scope and the native API map contradict the code | P3 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-016 | Hub hygiene: `block-events` ingress has no producer; inbox shows conversations the viewer has left | P3 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-018 | Realtime efficiency: the web ignores the server's per-device ack cursor, and every read/ack writes a conversation-sequenced event | P3 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED (code); performance impact REQUIRES_VALIDATION (`t | LOW (client) / |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MSG-019 | Send idempotency edge cases: rate limits charged before dedupe; replay with a different attachment set returns the original | P3 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [messaging](#messaging-communications-hub-meeting-chat-realtime) |
| MTG-019 | Web live room still carries a legacy v1 branch (chat/media writes) that fails under the retirement flag | P3 | FRONTEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-020 | SDK gaps: Dart SDK has no typed meeting models; TS SDK marks optional bodies as required | P3 | SHARED_CONTRACT | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| MTG-021 | Minor scheduling UX/contract nits | P3 | FRONTEND | FRONTEND_ONLY_GAP | CONFIRMED | LOW |  | [meetings](cfmeet-meetings-calendar-audit.md) |
| ORG-013 | Developer-org display name drifts from Platform Access tenant display name | P3 | BACKEND | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-015 | Wrong error code and existence oracle on meeting-org endpoints | P3 | BACKEND | CONTRACT_MISMATCH | CONFIRMED | LOW |  | [organizations](cfmeet-organizations-audit.md) |
| ORG-018 | Concurrency/validation edge cases in meeting-org membership upsert | P3 | BACKEND | IMPLEMENTED_UNVERIFIED | REQUIRES_VALIDATION | LOW |  | [organizations](cfmeet-organizations-audit.md) |
| PLT-011 | Gateway scope pre-check absent for 153 credential routes | P3 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-012 | Gateway rate limiting covers only platform-credential traffic; stale config comment | P3 | INFRASTRUCTURE | PARTIALLY_IMPLEMENTED | CONFIRMED | LOW |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| PLT-013 | SDKs: Dart is untyped and first-party apps don't use the SDKs | P3 | SHARED_CONTRACT | PARTIALLY_IMPLEMENTED | CONFIRMED | MEDIUM |  | [platform](#platform-developer-platform-gateway-sdks-swagger-admin) |
| SEC-007 | v2 actor honours tokens when Redis revocation check fails (fail-open by default) | P3 | BACKEND |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |
| SEC-019 | Duplicate S3 presign implementation in meeting-service | P3 | BACKEND | IMPLEMENTED_UNVERIFIED | CONFIRMED | MEDIUM |  | [security-data](#security-reliability-production-readiness) |
| SEC-022 | External dependencies (not software defects) | INFO | EXTERNAL_DEPENDENCY |  | CONFIRMED |  |  | [security-data](#security-reliability-production-readiness) |

¹ Severity reconciled by the lead auditor:
- **ACC-001** → P1: Marginal over refresh-token theft alone; the exploitable defect is revocation (ACC-002). SEC-002 also rated P1.
- **ACC-003** → P1: Conditional: applies only where CF_COMMUNICATIONS_BASE_URL is unset; Communications is deployed in production, so production impact REQUIRES_VALIDATION.
- **ACC-018** → P1: Merged with SEC-001.
- **MTG-016** → P1: Severity raised to P1 from merged duplicate.
- **SEC-001** → P1: docker-compose.prod.yml:91 requires JWT_SECRET (${JWT_SECRET:?}); the code default is a fail-open hazard, production exposure REQUIRES_VALIDATION of the deploy-backend runtime env.

## 3. Feature matrices by domain

### 3.1 Meetings, scheduling and calendar


| Feature | Intended | Backend implementation | APIs available | APIs missing | DB fields | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| Instant meeting (v2) | Space + LIVE conference in one call | `meeting_space.rs:471-540`; handler `spaces.rs` `create_instant_meeting` | POST /api/v2/instant-meetings | – | conference `kind=INSTANT`, `scheduled_start_at=now`, no end | Web `DashboardContent.tsx` → `spaces.service.ts:100` then `/{meetingCode}` | `communications_meetings.rs` (instant via hub, real DB), `api_v2_e2e`; web unit tests only | COMPLETE_VERIFIED (backend); web IMPLEMENTED_UNVERIFIED | Keep |
| Instant meeting (mobile) | Same on mobile | v1 `handlers/meetings.rs:59-180` | POST /api/v1/meetings/instant (410 when flag on) | mobile should call v2 | `meetings` | `app/(tabs)/index.tsx:58-67` reads `response.meeting.id` / `join_data` — backend returns `{success,data}` | none | CONTRACT_MISMATCH | MTG-001 |
| Select future date/time | Pick start/end | `conference.rs:297-351` | POST /api/v2/meeting-spaces/{id}/conferences | – | `scheduled_start_at/end_at` | Web `ConferencesPanel.tsx:20-37` (datetime-local, browser TZ) ; mobile legacy form broken | `api_v2_e2e.rs:124` e2e03, `system_e2e.rs:59` | COMPLETE_VERIFIED (backend) / web IMPLEMENTED_UNVERIFIED / mobile CONTRACT_MISMATCH | MTG-001, MTG-012 |
| Set timezone | Meeting timezone | space `timezone` (validated) ; occurrence has none | POST/PATCH /meeting-spaces | per-occurrence TZ | `meeting_spaces.timezone` | Web create/edit forms | e2e via `system_e2e` (Africa/Lagos) | PARTIALLY_IMPLEMENTED | MTG-012 |
| Set duration | Duration / end | end time ≤24h on create (`conference.rs:304-316`); recurrence `durationMinutes` | as above | – | end column / duration_minutes | Web start+end; mobile `duration_minutes` (not accepted by v1) | e2e03 | COMPLETE_VERIFIED (backend) | MTG-009 for reschedule gap |
| Schedule and save | Persist occurrence | as above | as above | – | yes | Web yes | e2e03 | COMPLETE_VERIFIED (backend) | – |
| Retrieve exact scheduled date/time | Round-trip | `views.rs:96-97` (FULL), `views.rs:68,75` (summary in space) | GET /conferences/{id}, GET /meeting-spaces/{id}/conferences, GET /meeting-spaces (latestConference), GET /resolve/{id} | – | yes (timestamptz µs; JS sends ms → exact) | Web list/dashboard show **`createdAt`** not schedule (`MeetingsContent.tsx:249-254`, `DashboardContent.tsx:169-172`); ConferencesPanel shows schedule in browser TZ | `system_e2e.rs:384-425` asserts stored starts across DST; `api_v2_e2e.rs:216,266` | Backend COMPLETE_VERIFIED; list display FRONTEND_ONLY_GAP; summary-view mismatch CONTRACT_MISMATCH | MTG-004, MTG-011 |
| Edit / reschedule | Move occurrence | `conference.rs:768-823` (CREATED only) | POST /api/v2/conferences/{id}/reschedule | version/optimistic concurrency | yes | Web `ConferencesPanel.tsx:63` | `api_v2_e2e.rs:237` | COMPLETE_VERIFIED with defects | MTG-009 |
| Cancel | Cancel occurrence | `conference.rs:712-766` | POST /api/v2/conferences/{id}/cancel | – | `cancelled_at/by`, `termination_reason` | Web yes | e2e03 | COMPLETE_VERIFIED | – |
| Recurring meetings | RRULE series | `recurrence.rs:47-112,551-620`; materialize horizon 30 d (`config/features.rs:158`) | PUT/GET/DELETE /meeting-spaces/{id}/recurrence | occurrence preview/expansion beyond horizon | rules table | Web `RecurrencePanel.tsx` (raw RRULE text) ; mobile none | `recurrence_exceptions.rs` rx01–rx03 (DST, re-anchor), `system_e2e.rs:384` | COMPLETE_VERIFIED (backend), PARTIALLY (UX/expansion) | MTG-008, MTG-015 |
| Modify series | Change series | Replace only (ends old rule, new rule) `recurrence.rs:551-620` | PUT …/recurrence | "this and following", series edit preserving exceptions | – | Web "Replace series" | rx02 | PARTIALLY_IMPLEMENTED | MTG-014 |
| Modify single occurrence | Exception | `recurrence.rs:623-760`; also via cancel/reschedule | PUT …/recurrence/exceptions; /conferences/{id}/cancel,/reschedule | – | exceptions table | Web ExceptionEditor (raw ISO timestamp) | rx01, rx03 | COMPLETE_VERIFIED (backend) | MTG-015 |
| Invite participants | Members / invitation links / email | `invitations.rs`, members service | POST /meeting-spaces/{id}/invitations, /members, POST /invitations/{token}/resolve | email delivery, ICS invite | `meeting_invitations`, `meeting_members` | Web InvitationsPanel ("does not send an email") | `api_v2_e2e.rs:509` e2e12, `system_e2e` m_e2e_01 | COMPLETE_VERIFIED (links) / MISSING (email) | MTG-006 |
| Schedule-change notifications | Email/push on reschedule/cancel | outbox events `conference.rescheduled/cancelled` → realtime only (`realtime.rs:402-414`); platform events don't map them (`platform/events.rs:118-127`) | none | notification fan-out | outbox_events | none | – | MISSING | MTG-006 |
| Reminders | Pre-meeting reminder job | none (`conference_scheduler.rs:12` "No auto-start… reminders are never authorization dependencies"); `send_notification_email` has no caller | none | reminder worker | none | none | – | MISSING | MTG-006 |
| List upcoming meetings | Agenda for me | per-space only + `latestConference` (single) | GET /meeting-spaces, GET /meeting-spaces/{id}/conferences?from= | GET /me/conferences (cross-space), status filter, pagination | yes | Web list shows spaces w/ createdAt; per-space panel | – | PARTIALLY_IMPLEMENTED | MTG-005 |
| List past meetings | History | per-space list with `to=` (no status filter) | same | cross-space history | yes | Per-space ConferencesPanel date filter | – | PARTIALLY_IMPLEMENTED | MTG-005 |
| Join at appropriate time | Window enforcement | start window `conference.rs:873-893` (early 15 min / late grace 60 min); join requires LIVE (`admission.rs:628-630`); no auto-start | POST /conferences/{id}/start, /join | – | schedule_policy | Pre-join countdown disables button (`pre-join-screen.tsx:582-606`), no start control in pre-join | e2e03 (window refusal), `conference.rs:995` unit | COMPLETE_VERIFIED (backend), UX PARTIAL | MTG-013 |
| Meeting history | Past conferences + attendance | list conferences; `/attendance`; artifacts | GET /conferences/{id}/attendance, /artifacts, /recordings | cross-space | yes | Web ConferenceWorkspace (participants, recordings); attendance not shown | `system_e2e` m_e2e_09 | IMPLEMENTED_UNVERIFIED (UI) | MTG-005 |
| Personal calendar (day/week/month) | Range queries across spaces | none cross-space | per-space from/to only | GET /me/conferences?from&to | – | none (`/dashboard/schedule` redirects to `/dashboard/meetings`) | – | MISSING | MTG-005 |
| Organization calendar | Org meetings | `organization_id` on space; no org listing (only membership queries `organizations.rs:117,311`) | none | GET /organizations/{id}/conferences | `meeting_spaces.organization_id` | none | `organization_domain_access.rs` (access only) | MISSING | MTG-016 |
| Recurring expansion in range | Virtual occurrences | materialized rows only within 30 d | – | expansion | – | – | `recurrence.rs:895` unit | PARTIALLY_IMPLEMENTED | MTG-008 |
| Timezone & DST | Wall-time stability | rrule with TZID (`recurrence.rs:79-91`), re-anchor on TZ change | – | – | – | Web shows browser TZ only | rx01/rx02, m_e2e_06 | COMPLETE_VERIFIED (recurrence); display PARTIAL | MTG-012 |
| ICS export/import / calendar invitations | .ics | none (`grep -rln "VCALENDAR\|text/calendar"` → no source hits) | none | GET /conferences/{id}/calendar.ics, GET /me/calendar.ics | – | none | – | MISSING | MTG-007 |
| External calendars (Google/Outlook) | Sync | none (only Google OAuth login) | none | provider link + sync | – | none | – | DEFERRED_BY_OWNER (spec "Optional") | MTG-007 |
| Admission / waiting room | Lobby + admit/deny | `admission.rs`, `handlers/v2/participants.rs` | GET /conferences/{id}/waiting-room, POST …/admit, …/deny, …/return-to-waiting | – | `conference_participants.admission_state` | Web ParticipantsPanel + pre-join polling; mobile legacy (broken) | e2e07_08, e2e09, m_e2e_01 | COMPLETE_VERIFIED (backend) | MTG-002 for mobile |
| Host controls | lock/unlock/mute/remove/ban/role | `conference.rs:580-623`, participants handlers | /lock,/unlock,/participants/{pid}/{mute,remove,ban,role} | – | – | Web | e2e13–16 | COMPLETE_VERIFIED | – |
| End meeting | End for all | `conference.rs:625-710` | POST /conferences/{id}/end | – | ended_at, termination_reason | Web ConferenceRow; mobile calls v1 `/meetings//end` | e2e18, m_e2e_01 | COMPLETE_VERIFIED (backend) / mobile CONTRACT_MISMATCH | MTG-002 |
| Auto-end | End after scheduled end + grace | `conference_scheduler.rs:68-114` | worker | – | schedule_policy | – | `failure_injection_m.rs:284` | COMPLETE_VERIFIED | MTG-009 (end wiped on reschedule disables it) |
| Stale/missed occurrence expiry | Mark never-started occurrences | none (index `idx_conference_scheduler_start` unused) | – | expiry transition | – | – | – | MISSING | MTG-010 |
| Meeting links / resolve | Code/link → space + canonical conference | `compat.rs:94-172` | GET /resolve/{identifier}, GET /meeting-spaces/{id} | – | meeting_code | Web/mobile pre-join | e2e02 | COMPLETE_VERIFIED; canonical choice defect | MTG-010 |
| Participant tokens | Guest credential | join response `participantToken` | header X-Participant-Token | – | `credential_hash` | Web `storageStore.setParticipantToken` | `credential_replay.rs`, e2e14 | COMPLETE_VERIFIED | – |
| LiveKit join | Media credential | `admission.rs` RtcOutcome; `rtc_provider.rs` | join response `rtc` | – | rtc_room_* | Web live room; mobile live room | CI uses LiveKit mock; real LiveKit tests `#[ignore]` | IMPLEMENTED_UNVERIFIED (real media) | – |
| Meeting chat linkage | Conference chat | `handlers/v2/collaboration.rs` messages | GET/POST /conferences/{id}/messages | – | – | Web v2 branch; legacy ChatProvider in fallback | collab_* tests | COMPLETE_VERIFIED (backend) | MTG-019 |
| Recordings link | Recording per conference | artifacts handlers | /conferences/{id}/recordings | – | meeting_artifacts | Web ConferenceRecordingsPanel | m_e2e_09 | COMPLETE_VERIFIED (backend, mock storage) | – |
| Organization meeting scheduling | Create org-scoped meeting | `organization_id` + `ORGANIZATION_ONLY` validated (`meeting_space.rs:153,452`) | POST /meeting-spaces (+ /conferences) | – | yes | Web create form has no org/ORGANIZATION_ONLY option | `organization_domain_access.rs` | FRONTEND_ONLY_GAP | MTG-016 |
| Legacy v1 scheduling | Back-compat | `handlers/meetings.rs:303-390` | POST/PUT/DELETE /api/v1/meetings (410 when flag on) | v2 mirroring of new legacy rows | meetings | Web: none for create; mobile: all | `legacy_retirement.rs`, `legacy_v1_*` | PARTIALLY_IMPLEMENTED | MTG-017 |


### 3.2 Organizations


Abbreviations: MS = meeting-service, DP = developer-platform, PA = platform-access. "CI DB test" as defined above.

| Feature | Intended functionality | Backend implementation (file:line) | APIs available | APIs missing | DB fields present / missing | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| Org creation (MS) | Create org; PDR A.32: platform-admin "privileged engineering tools, do not expose in public meeting frontend" | `services/organizations.rs:144-186`; admin gate `:96-102,151-152` | `POST /api/v2/admin/organizations` | self-service create (if product wants it); admin UI | name, slug, status, created_by, created_at present | **None** — `organizationsService.create` defined (`organizations.service.ts:24-30`) but never called; no `/admin` app route exists | org03 asserts non-admin → 403 (`organization_domain_access.rs:211-222`) — CI DB test | PARTIALLY_IMPLEMENTED (API only, no tooling) | Product decision D-07; at minimum ship an operator/admin tool |
| Org creation (DP) | Any signed-in human creates a workspace & becomes OWNER | `control/organizations.rs:83-149` (delegates EXTERNAL tenant to PA, then one txn: org row + OWNER member + activity) | `POST /v1/organizations` (idempotent `public.rs:401-422`) | — | created_by (actor ref), created_at present | **None** (console deferred: `developer-console-api-handoff.md:3-4`) | `control_plane.rs:62,136` real DB, **fake PA** | IMPLEMENTED_UNVERIFIED (cross-service) | Decide if this is "the" organization (ORG-001) |
| Org profile retrieval (MS) | View org details | none for single org; list only `services/organizations.rs:318-341` | `GET /api/v2/me/organizations` | `GET /api/v2/organizations/{id}`, admin list-all | — | `OrganizationsContent.tsx:107-118` (name, slug, status, role) | org01 asserts list (CI DB) | PARTIALLY_IMPLEMENTED | Add GET detail (ORG-004) |
| Org profile retrieval (DP) | | `control/organizations.rs:209-215` (+PA classification) | `GET /v1/organizations[/{id}]` | — | | none | DB test, fake PA | IMPLEMENTED_UNVERIFIED | — |
| created_at | Workspace creation timestamp | MS model `organizations.rs:17`; handler maps it `handlers/v2/organizations.rs:21` | in `OrganizationResponse.createdAt` (`dto/v2.rs:841`), Swagger schema (`openapi/meeting-service.json` OrganizationResponse required) | — | `created_at TIMESTAMPTZ NOT NULL DEFAULT now()` (`m20261004…:20`) | web type has it (`types/v2/index.ts:1519`) but **not rendered** (`OrganizationsContent.tsx:114-118`) | not asserted by any test | FRONTEND_ONLY_GAP | Render as "Created on CF Meet" (ORG-003) |
| created_by | Creator identity | MS: stored `services/organizations.rs:165`; DP: stored `control/organizations.rs:116` | **not exposed** (MS `handlers/v2/organizations.rs:15-23`; DP `ORG_VIEW` L28-32 omits it) | expose `createdBy` (+ display name) | MS `created_by UUID → users ON DELETE SET NULL` (L19); DP `created_by VARCHAR(100)` | none | none | PARTIALLY_IMPLEMENTED (storage only) | Add to DTO + resolve name (ORG-003) |
| Creation source | Self-service vs operator vs import | — | — | — | MS: **missing**; DP: `joined_via` on member only; activity event `organization.created` | none | — | MISSING | Add `creation_source` if required |
| Registered / operating location | Country/region/address | — | — | all | **missing in every org table** (MS, DP, PA). Only `devplat_billing_accounts.country` (billing) and numbering KYC evidence `ADDRESS` | none | — | MISSING | Data-model change (ORG-003) |
| Legal incorporation date | Legal entity date | — | — | all | **missing** everywhere (KYC evidence `BUSINESS_REGISTRATION` is a document type, not a field) | none | — | MISSING / PRODUCT_DECISION | Do not conflate with created_at |
| Organization type | e.g. healthcare/business | — | — | all | missing (space-level `security_profile` exists, not org) | none | — | MISSING | Product decision |
| Description | | — | — | all | missing (MS & DP org; DP project has description) | none | — | MISSING | Add column + PATCH |
| Status | ACTIVE/SUSPENDED (MS); 5-state (DP) | MS `set_status` `services/organizations.rs:221-236`; DP `close` L273-314, operator L319-391 | `PATCH /api/v2/admin/organizations/{id}`; `POST /v1/organizations/{id}/close`; `POST /operator/organizations/{id}/state` | MS archive/delete | MS CHECK (L18) | web renders status (L117) but suspended orgs are never returned (`mine` filters via verified orgs) | org02 CI DB test (suspension denies) | COMPLETE_VERIFIED (backend, CI DB; lifecycle partial) | ORG-008 |
| Owner | Show owner | MS: OWNER role memberships; DP: OWNER members | via member list (managers only) | owner field on org response | no owner column (derived) | role shown only for caller | org03 | PARTIALLY_IMPLEMENTED | Add owner summary to detail |
| Member count | | — | — | count field | derivable | none | — | MISSING | Add aggregate to detail |
| Member invitation & acceptance (MS) | Invite by email, accept | **none**; direct add `services/organizations.rs:241-298` | `PUT /api/v2/organizations/{id}/members/{userId}` (direct add by UUID) | invite/accept/decline/revoke | no invitation table | UUID text box `OrganizationsContent.tsx:156-164` | org03 (direct add) | MISSING (invitation) | ORG-005 |
| Member invitation & acceptance (DP) | | `control/members.rs:325-400,513-615` (token once, email-bound, 7-day TTL) | `POST/GET /v1/organizations/{id}/invitations`, `…/revoke`, `POST /v1/invitations/accept` | email delivery (token returned to inviter only) | `devplat_invitations` | none | `control_plane.rs:196` real DB | IMPLEMENTED_UNVERIFIED | reuse for MS? (ORG-001/005) |
| Roles & permissions (MS) | OWNER/ADMIN/MEMBER; admins manage MEMBERs only; no self-change | `services/organizations.rs:125-141,249-270` | PUT member | custom permissions | role CHECK (L29) | role select `OrganizationsContent.tsx:166-177` (client guards mirror server) | org03 asserts admin cannot mint admin / touch owner / self-promote; stranger 403 — CI DB | COMPLETE_VERIFIED (backend, CI) | — |
| Member listing (MS) | managers list members | `services/organizations.rs:300-315` | `GET /api/v2/organizations/{id}/members` | user names/emails in response | — | `OrganizationsContent.tsx:132-151` shows raw UUIDs | org03 (member → 403) | PARTIALLY_IMPLEMENTED | ORG-006 |
| Member removal | | MS: status REMOVED via PUT (`:277,281-283`); DP: `DELETE …/members/{id}` (`members.rs:210-268`, last-owner guard) | PUT (MS), DELETE (DP) | MS "leave organization" | `removed_at` | status select (web) | MS: no explicit REMOVED test found beyond org02 "Removed membership"; DP test L488 | IMPLEMENTED_UNVERIFIED (MS) | ORG-008 |
| Profile editing | rename etc. | MS: **none** (PATCH = status only, `dto/v2.rs:823-825`); DP: `control/organizations.rs:218-268` (displayName, supportEmail, optimistic version) | DP `PATCH /v1/organizations/{id}` | MS PATCH name/slug/description | — | none | DP DB test | MISSING (MS) | ORG-004 |
| Settings / org policy | org-wide profile restrictions | read in `profiles.rs:950-953`, `specialized.rs:262-265`, `prejoin.rs` | **no write API** | `PUT /api/v2/organizations/{id}/profile-policy` | `organizations.profile_policy JSONB DEFAULT '{}'` (`m20261005…:163`) | none | profiles_h tests (space-level) | PARTIALLY_IMPLEMENTED | ORG-009 |
| Ownership transfer | | MS none; DP `members.rs:269-316` | DP `POST …/ownership-transfer` | MS | — | none | DP L488 | MISSING (MS) | ORG-008 |
| Org meetings (create) | meeting with optional organization (PDR F-03) | `meeting_space.rs:305-314,452` (verified member only), ORGANIZATION_ONLY validation `:153-156`; admission `meeting_access.rs:331-341` | `POST /api/v2/meeting-spaces` (`organizationId`) | — | `meeting_spaces.organization_id` FK RESTRICT | **no org option** in `CreateMeetingSpaceContent.tsx:124-133`; Edit offers 4 policies only (`EditMeetingContent.tsx:38-40`); mobile none | org01 CI DB (create + join + non-member denied) | FRONTEND_ONLY_GAP | ORG-010 |
| Org calendar / org meeting list | see org's meetings | none — `list_for_actor` owner/member only (`meeting_space.rs:616-660`) | — | `GET /api/v2/organizations/{id}/meeting-spaces` (or filter) | — | none | — | MISSING | ORG-011 |
| Org ↔ Messaging | colleagues = org co-members | `relay.rs:670-717`; resolve/name `handlers/v2/unified_messaging.rs:272,304,335` | `GET /api/v2/messaging/directory`, `/resolve` | — | — | `MessagesContent.tsx:187` | unified_messaging tests | IMPLEMENTED_UNVERIFIED (visibility inconsistency ORG-006) | — |
| Org ↔ Calling / Enterprise | org admin configures enterprise calling for org | none for MS org; web uses fixed tenant (`lib/calling/server.ts:83-88`) + env allowlist (`route.ts:58-68`) | `/v1/enterprise/**` (tenant-scoped) | mapping MS org → tenant/env | — | `CallingEnterpriseHome.tsx`, resource pages | calling enterprise tests (tenant-scoped) | MISSING (relationship) | ORG-012 |
| Suspension | | MS `set_status`; DP close/operator | see Status | — | | none (no admin UI) | org02 CI DB | IMPLEMENTED_UNVERIFIED (UI) | — |
| Archive / deletion | | MS none (FK RESTRICT on memberships & spaces); DP CLOSED terminal + no-delete trigger (L185) | DP close | MS archive/delete | | none | DP L845 | MISSING (MS) | ORG-008 |
| Audit history | who changed what | MS: `tracing::info!` only (`services/organizations.rs:184,234,296`); DP `devplat_activity_events` append-only + `GET /v1/organizations/{id}/activity` | DP only | MS audit API | MS **no audit table** | none | DP L1370 | MISSING (MS) | ORG-007 |
| Swagger | | MS `swagger/mod.rs:195-199,465-472` (all 5 ops, schemas) | | | | | `openapi_contract.rs` | COMPLETE_VERIFIED (contract test) | — |
| SDK | | TS/Dart: MS **only** `v2MyOrganizations` (`operations.ts:383`); DP full org family (`operations.ts:239-305`) | | MS member ops intentionally EXCLUDED (`public-routes.json:2440,2447`) | | | | NOT_REQUIRED (by gateway policy) | — |
| Mobile org experience | | — | — | — | — | none (only `ORGANIZATION_ONLY` label in `space-card.tsx:44,57`) | — | FRONTEND_ONLY_GAP | ORG-016 |


### 3.3 Accounts, identity, contacts, devices, notifications


| Feature | Intended functionality | Backend implementation (file:line) | APIs available | APIs missing | DB fields present/missing | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| Registration (email/password) | Create pending account and send OTP | `handlers/auth.rs:33-127` | POST /api/v1/auth/register | – | users(email,username unique; case-sensitive) | web `services/auth.service.ts:20`; mobile `features/auth/api/auth.api.ts` | Used by certification `personal_communications.rs:69` (real stack); no direct negative tests; enumeration via 409 | IMPLEMENTED_UNVERIFIED | Normalize email, add anti-enumeration and rate limit (ACC-014/015/023) |
| Email verification | 6-digit OTP, 24 h | `auth.rs:145-187`, `services/otp.rs` | POST /verify-email, /resend-verification | – | Redis keys only | web `/verify`, mobile `verify.tsx` | No automated test; not enforced (ACC-016) | IMPLEMENTED_UNVERIFIED | Enforce for login or for comms features |
| Login | Email/password → access 15 min + refresh 7 d | `auth.rs:211-293`, `services/auth.rs:24-29` | POST /login | – | users.last_login | web, mobile | Exercised by certification; per-email limiter only; pending accounts allowed | IMPLEMENTED_UNVERIFIED | ACC-001/015/016 |
| Google signup/OAuth | OIDC code flow | `auth.rs:794-1049` | GET /oauth/google, /oauth/google/callback | No PKCE/nonce; no linking confirmation | users.auth_type; Google `sub` not stored | web `hooks/useGoogleCallback.ts`; mobile "coming soon" (`login-form.tsx:164`) | No tests | PARTIALLY_IMPLEMENTED | ACC-004/005/006 |
| Logout | Revoke session | `auth.rs:384-442`, `communications.rs:651-672`, `unified_messaging/identity.rs:232` | POST /logout | Per-device logout | account_communication_state, communication_session_revocations | web `auth.service.ts:61` (+ messaging device revoke); mobile `use-logout.ts` | Real-stack: refresh→401 after logout (`personal_communications.rs:1105-1131`); messaging sessions revoked (`unified_messaging_e2e.rs:927`). Refresh-as-bearer on v1 NOT tested | PARTIALLY_IMPLEMENTED | ACC-001/002/003/007 |
| Token refresh | Rotate pair | `auth.rs:309-363` | POST /refresh | Reuse detection | – | web `api.ts:57-104` interceptor | `legacy_v1_web_compat.rs:72` | IMPLEMENTED_UNVERIFIED | ACC-001/017 |
| Password reset | OTP by email, 1 h | `auth.rs:524-650`, `665-698` | POST /forgot-password, /verify-reset-token, /reset-password | – | Redis | web `(auth)/forgot-password/*`; mobile `forgot-password.tsx` | No tests; does not revoke v1 sessions (ACC-002) | IMPLEMENTED_UNVERIFIED | ACC-002/014 |
| Password change | Authenticated change | `auth.rs:720-766`; trigger `m20261103…:18-32` | POST /change-password | – | password_hash → trigger bumps credentials_not_before | web `SettingsContent.tsx:358-386`; mobile `change-password.tsx` | No tests | IMPLEMENTED_UNVERIFIED | ACC-002/003 |
| MFA / 2FA | TOTP/WebAuthn | none (grep returns nothing) | – | All | – | – | – | MISSING | ACC-013 |
| Profile view/edit | View/edit own profile | `handlers/users.rs` | GET/PATCH /api/v1/users/me | Display name, avatar, locale | users.username only; no avatar/display_name column | web Settings; mobile `edit-profile.tsx` | No tests | PARTIALLY_IMPLEMENTED | ACC-021 |
| Avatar upload | Image upload | none; Hub `avatar_reference: None` (`meeting communications.rs:283`) | – | Upload/serve | Hub column exists, Account column missing | – | – | MISSING | ACC-021 |
| Account deactivation | Self-deactivate | `users.rs` `deactivate_account`; trigger emits STATUS_CHANGED | POST /api/v1/users/me/deactivate | Re-auth, reactivate | users.status | web Settings | No tests | IMPLEMENTED_UNVERIFIED | ACC-011/026 |
| Account deletion / data export | Erasure + export | none for consumer accounts (messaging has tenant-admin erasure only, `messaging/src/compliance/deletion.rs`) | – | DELETE account, export | – | – | – | MISSING | ACC-011 |
| Session listing / revocation | List and revoke sessions | No session store (stateless JWT); Messaging `/messaging/client-sessions/revoke-all` only | POST /api/v2/messaging/client-sessions/revoke-all | GET sessions, DELETE session, logout-others | No sessions table | none | – | MISSING | ACC-007 |
| Admin user management | List users, change role/status | `handlers/admin.rs:151-290` | GET /api/v1/admin/users, PATCH /admin/users/:id | – | trigger propagates status | web? (not inspected deeply) | No tests seen | IMPLEMENTED_UNVERIFIED | – |
| Comms activation / CF Meet ID | Lazy profile + public ID | Hub `profiles.rs:16-130`, `api.rs:308-320` | GET /api/v2/communications/me, /activation, /me/public-id, POST /me/public-id/rotate | – | communication_profiles, communication_public_ids | **none** (web/mobile) | Real stack (`personal_communications.rs:62-136`) + Hub DB tests | FRONTEND_ONLY_GAP | ACC-009 |
| Contact discovery (public ID) | Resolve CF-ID | Hub `api.rs:338-362` (rate 120) | POST /discovery/public-id/resolve, GET /profiles/:publicId | Address-book/email/phone discovery (by design) | – | none | Real stack covers resolve/rotation | FRONTEND_ONLY_GAP | ACC-009 |
| Invitations / QR | One-time connect tokens | Hub `invites.rs`, `api.rs:322-340` | GET /me/qr, POST/GET /invites, DELETE /invites/:id, POST /discovery/invites/resolve | – | communication_invites (digest only) | none | `foundation.rs:180`, `:840` (real PG) + real stack | FRONTEND_ONLY_GAP | ACC-009 |
| Contact requests / contacts | Request/accept/decline, aliases, favorites, lists | Hub `relationships.rs`, `collections.rs`, `api.rs:396-540` | /contact-requests*, /contacts*, /contact-lists* | – | communication_connections, contact_requests, lists | none | `foundation.rs:275,360`, `unified.rs:60` | FRONTEND_ONLY_GAP | ACC-009 |
| Org colleague directory (web discovery) | Find colleagues for chat | `handlers/v2/unified_messaging.rs:264-338` | GET /api/v2/messaging/directory, POST /directory/resolve, /principals/names | – | org membership | web `MessagesContent.tsx:172` | unified messaging e2e | IMPLEMENTED_UNVERIFIED | Only path in UI; users without an org cannot find anyone |
| Blocking / unblocking | Block profile | Hub `api.rs:366-388`, `events.rs:135` | PUT/DELETE /profiles/:id/block, GET /blocks | – | communication_block_bridge | none (web `services/calling/blocking.service.ts` is enterprise Calling blocking, not personal) | `foundation.rs:407`, real stack | FRONTEND_ONLY_GAP | ACC-009 |
| Privacy settings | Discoverability, who can call or message | Hub `profiles.rs:214+` | GET/PUT /me/privacy | – | communication_privacy | none | `foundation.rs:454` | FRONTEND_ONLY_GAP | ACC-009 |
| Abuse reports | Report profile | Hub `api.rs:551-586` | POST /profiles/:id/reports | Moderator review API | communication_profile_reports | none | – | PARTIALLY_IMPLEMENTED | Add operator review surface |
| Phone verification (OTP) | SMS OTP | Hub `internal.rs:265` returns DependencyNotConfigured | (internal only) | All public | – | – | Doc `cfmeet-sections-8-23-final-closure.md:21,160` | DEFERRED_BY_OWNER | – |
| Messaging device add/auth | MLS device + client token | meeting `routes/v2.rs:429-452` → Messaging `http/devices.rs` | POST /api/v2/messaging/devices/registration-challenges, /devices, /devices/:id/key-packages, /client-tokens | Device list for own account (Messaging has `/principals/:id/devices`, via relay) | messaging_devices | web `lib/messaging/engine.ts` | Real stack `personal_communications.rs:222-260`; `messaging/tests/devices.rs` | IMPLEMENTED_UNVERIFIED (real-stack test exists; CI result not verified) | – |
| Messaging device revoke | Revoke device, its sessions and push | Messaging `devices/mod.rs:809-905` | POST /api/v2/messaging/devices/:id/revoke | – | status, key packages, sessions, push disabled | web only on logout (`signout.ts`) | `messaging/tests/devices.rs`, real stack line 568 | IMPLEMENTED_UNVERIFIED | Add device-management UI |
| Calling endpoint register/revoke | Endpoint registry | Calling `endpoints/service.rs:195-221`, `communications.rs:783-808` | /v1/calling/endpoints (web BFF `app/api/calling`) | – | call_endpoints.push_target_ref | web `services/calling/endpoints.service.ts` | `calling/tests/communications_owner_routes.rs` | IMPLEMENTED_UNVERIFIED | – |
| Unified device registry (Hub) | List/link/revoke devices across Messaging and Calling | Hub `devices.rs:40-410` | GET /devices, POST /devices/link, PUT/DELETE /devices/:id/push, POST /devices/:id/revoke, GET/PUT /devices/:id/sync-state | – | communication_devices | none; never auto-populated | `unified.rs:618,1214` (real PG, fake owners) | FRONTEND_ONLY_GAP | ACC-008/009 |
| Push registration (FCM/APNs/Web Push) | Register push token | Messaging `realtime/push.rs:124-200`, adapters `push_adapters.rs` | via Hub PUT /devices/:id/push (→ Messaging internal); Messaging native API | Calling push transport | messaging push registrations | **none**: no service worker in web; no `expo-notifications` in mobile | Adapters tested against local fakes only (header comment); live provider certification needed | EXTERNAL_DEPENDENCY (backend) + FRONTEND_ONLY_GAP | ACC-020 |
| Incoming-call push | Ring a device while the app is closed | `calling/src/signaling/push.rs` – only `NoPushTransport` (`main.rs:453`) | – | Real dispatcher | push_target_ref stored | – | – | MISSING | ACC-020 |
| Notification preferences | Per-category toggles | Hub `chat.rs:354-410` | GET/PUT /me/notification-preferences | – | 7 boolean columns | none | `foundation.rs:14` covers direct/group only | PARTIALLY_IMPLEMENTED | ACC-020 |
| In-app notification inbox | Unified notification centre | Hub `/inbox` = conversation overlay (`sync.rs:134`), `/activity` = timeline | GET /inbox, /activity, /sync, /events | Notification feed with read state | – | none | `unified.rs:907` | PARTIALLY_IMPLEMENTED | Product decision |
| Delivery failure handling / status | Retries, dead letters, truthful status | Messaging `push.rs:375-490`; Hub `devices.rs:418-440` | GET /me/notification-delivery-status | – | messaging_push_jobs | none | real stack `personal_communications.rs:1060-1080` | IMPLEMENTED_UNVERIFIED | – |
| Revocation propagation (Account → Hub → Messaging/Calling) | Logout, password change and deactivation end every credential | trigger `m20261103…:18-32`; outbox worker `main.rs:146-157`; Hub `events.rs:56-122`; Messaging `communications.rs:321`; Calling `communications.rs:356` | internal | Principal status sync to Platform Access | account_communication_state/outbox, *_communication_revocations | – | Logout covered by the real stack; password change and deactivation not covered | PARTIALLY_IMPLEMENTED | ACC-002/003/010 |
| Developer session bridge | Dev Platform verifies human session | `services/session_bridge.rs:140-210` | POST /internal/v1/user-sessions/verify | – | – | dev console | `tests/session_bridge.rs` | PARTIALLY_IMPLEMENTED | ACC-002 |


### 3.4 Calling, enterprise calling, telephony, numbering, SMS, privacy relay


Legend: "Backend" classification is for the server capability; frontend status is stated in the "Frontend consumption" column and in findings.

| Feature | Intended functionality | Backend implementation (file:line) | APIs available | APIs missing | DB fields | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| Endpoint (device) registration | A browser/app device registers to receive offers | `src/endpoints/{http,service}.rs` | POST/GET `/v1/calling/endpoints`, DELETE `/v1/calling/endpoints/:id` | — | `call_endpoints` | `useCallingDevice.ts` l.19–33 (manual "Enable this browser" button, not automatic) | `tests/app_ringing.rs` (real PG) | COMPLETE_VERIFIED | Auto-register on app load (CAL-004) |
| Signaling credential + realtime WS | Short-lived endpoint credential, WS delivery, recovery | `src/signaling/*`, routes l.(signaling::http) | POST `/v1/calling/signaling-credentials`, GET `/v1/calling/signaling` (WS), GET `/v1/calling/invitations` | — | `call_signaling_*` | `signaling.service.ts`, `useCallingDevice.ts` l.36–90 (backoff reconnect, 10 s recovery poll) | `tests/signaling.rs` (auth before upgrade, expiry, multi-instance fanout) | COMPLETE_VERIFIED | — |
| 1:1 call create (voice/video) | Caller starts app-to-app call | `src/calls/app/create.rs`; `src/calls/app/http.rs` l.53 | POST `/v1/calls` (Idempotency-Key) | — | `calls`, `call_participants`, `call_invitations`, `call_sessions` | `CallsContent.tsx` l.172–186 (directory or extension target) | `app_calls.rs`, `rtc_certification.rs::voice_call`/`video_call_and_removal` (real LiveKit); production acceptance run 41580265031 (API clients, not web) | COMPLETE_VERIFIED | — |
| Ringing fan-out | All eligible endpoints of target ring; TTL | `src/calls/app/ringing.rs` | WS signals, GET `/v1/calling/invitations` | — | `call_invitations` (status, expires_at) | Incoming card `CallsContent.tsx` l.165–168 (only on /calling pages) | `app_ringing.rs` (presence hint, endpoint cap, revoked endpoints) | COMPLETE_VERIFIED | Global incoming-call UX (CAL-004) |
| Answer arbitration across devices | Exactly one winning device per participant | `ringing.rs`; DB `call_answer_claims` unique guarantee | POST `/v1/calling/invitations/:id/accept`, POST `/v1/calls/:id/accept` | — | `call_answer_claims` | `useCallingDevice.ts` answer() | `app_races.rs::concurrent_accepts_have_exactly_one_winner`, `the_single_winner_constraint_is_the_database_guarantee` | COMPLETE_VERIFIED | — |
| Decline | Device declines its offer | `ringing.rs` | POST `/v1/calling/invitations/:id/decline`, `/v1/calls/:id/decline` | "Decline on all my devices" | invitation status | Accept/Decline buttons | `app_ringing.rs::one_device_decline_declines_only_that_offer` | COMPLETE_VERIFIED | Product decision (CAL-018) |
| Cancel | Caller cancels pre-answer | `control.rs` l.73–159 | POST `/v1/calls/:id/cancel` | — | — | Cancel button (initiator) | `app_control.rs::cancel_is_pre_active_idempotent…`, `app_races.rs::accept_racing_cancel…` | COMPLETE_VERIFIED | — |
| Missed / no-answer | Offer expiry → missed outcome in history | `workers.rs` l.50–118 (`expire_due`), l.183–230 (stalled setup) | GET `/v1/calls?principalId&missed=true`, GET `/v1/calling/history` | — | invitation `expires_at`, outcome derived | "Missed" toggle `CallsContent.tsx` | `app_ringing.rs::offers_expire_on_the_server…`, `call_history.rs::a_missed_call_survives_push_and_realtime_outage` | COMPLETE_VERIFIED | — |
| Incoming-call push (APNs VoIP / FCM / Web Push) | Wake device for incoming call | `signaling/push.rs` l.24–33 `NoPushTransport`; `main.rs` l.453 `push: Arc::new(NoPushTransport)`; payload builder `signals.rs` l.184–203 | (payload contract `packages/contracts/calling/push`) | Push dispatcher implementation, device push-token binding for calls | `pushTargetRef` on endpoints | None (web & mobile) | Communications reports `"callRinging": "…mobile push transport NOT_CONFIGURED"` (`communications/src/devices.rs` l.435) | MISSING | CAL-003 |
| LiveKit media credential | Short-lived token only for authorized session | `calls/app/media.rs` l.109–223; `media/livekit.rs`; edge gateway `media/gateway.rs` | POST `/v1/calls/:id/sessions/:sid/media-credentials`, POST `/v1/calling/sessions/:sid/media-credentials`; GET `/v1/calling/media/rtc/v1` | — | `call_sessions.media_access`, generations | `CallMedia.tsx` (LiveKitRoom) | `app_media.rs::media_credentials_are_issued_only_to_authorized_sessions`; rtc: `refreshed_privileged_token_cannot_reenter`, `superseded_generation_and_terminal_call_tokens_are_refused` | COMPLETE_VERIFIED | — |
| Hold / resume | Provider-enforced media pause | `control.rs` l.519–697 | POST `/v1/calls/:id/hold`, `/resume` | — | call state HELD | Buttons shown **only to initiator** (`CallsContent.tsx` l.228; adapter `route.ts` l.222) | `app_control.rs::hold_and_resume_are_enforced_at_the_provider`; rtc voice_call | COMPLETE_VERIFIED | CAL-002 |
| Transfer (ring target, retire transferor) | Same Call id, target rings, transferor retired on join | `calls/app/transfer.rs` l.1–60; enterprise `calls/app/enterprise/transfer.rs` | POST `/v1/calls/:id/transfer` (principal / extension / destination) | Attended (consultative) transfer | `call_transfers` | Transfer-to-extension form only (`CallsContent.tsx` l.244) | `app_control.rs::transfer_keeps_the_same_call…`, `enterprise_transfer.rs` (races), rtc `enterprise_transfer` (bidirectional media) | COMPLETE_VERIFIED (attended: MISSING, not specified in ADR 0009/0022) | CAL-019 |
| Reconnect after network loss | Session survives transient loss; same-generation rejoin | `media.rs` l.407–424, l.651–686 (record_absent), l.758+ (reconcile); `media.rs` l.934+ admission | GET `/v1/calling/media/rtc/v1` re-admission | — | `call_session_intervals` | Signaling WS auto-reconnects; **media does not**: `CallMedia.tsx` l.20 `onDisconnected={onLeave}` drops media, user must press "Join audio" | rtc `legitimate_reconnect_with_the_current_generation`; production run 41580265031 "reconnection" | COMPLETE_VERIFIED (backend); web PARTIAL | CAL-015 |
| End call | Any party hangs up; call terminates | `control.rs` l.162–268 (`end`), l.271+ `finish_ending`; `lifecycle.rs` l.889–930 | POST `/v1/calls/:id/end`, POST `/v1/calls/:id/sessions/:sid/remove` | Automatic termination when ≤1 party remains | `ending_at`, `ended_at` | End only for initiator; callee only "Leave call" (session removal) | `app_control.rs::end_reaches_ended_only_on_cleanup_evidence`; adapter test "recipient cannot control the whole call" | PARTIALLY_IMPLEMENTED | CAL-001, CAL-002 |
| Call history & missed calls | Per-principal history, outcome per viewer | `calls/app/history.rs`; `calls/http.rs` l.79–210 | GET `/v1/calls?principalId=…`, GET `/v1/calling/history` | — | derived from evidence | `CallsContent.tsx` "Recent calls" + Missed filter | `call_history.rs` (pagination, scope, ring-group answered-elsewhere, voicemail link) | COMPLETE_VERIFIED | — |
| Group calls | Up to N targets, one winner per participant | `app/http.rs` l.231 (`target_principal_ids` ≤64), `max_group_participants` | POST `/v1/calls` with `targetPrincipalIds[]` | — | — | Web sends exactly one target (`CallsContent.tsx` l.182) | `app_ringing.rs::group_calls_have_one_winning_device…`, rtc `group_call`, production "three-party real audio" | COMPLETE_VERIFIED; FRONTEND_ONLY_GAP | CAL-014 |
| Chat-to-call / contact-to-call | Launch call from conversation/contact | Hub `services/communications/src/calls.rs` l.66–130 → Calling internal `/internal/v1/communications/calls` (`calling/src/communications.rs` l.207–235) | (Hub) POST `/v2/communications/contacts/{id}/calls`, `/conversations/{id}/calls`, GET `/calls/history` | — | `communication_call_links` | **Not consumed** by web (no reference in `apps/web/src`) | Closure doc: production "Hub-launched voice and video with media" PASS (run 42191576906) | FRONTEND_ONLY_GAP | CAL-014 |
| Personal call policy (who may call me) | Communications policy `DirectCall` enforced | `calling/src/communications.rs` l.69–181; `create.rs` l.122 | (internal) | — | `calling_communication_revocations` | Implicit | `communications_owner_routes.rs`; scope only enforced if `CF_COMMUNICATIONS_*` scope equals the web credential scope | IMPLEMENTED_UNVERIFIED (for the web path) | CAL-007 |
| Blocking | User blocks a caller | `calls/app/blocks.rs` | POST/GET `/v1/blocks`, DELETE `/v1/blocks/:id` | — | `calling_block_rules` | `BlockingPanel.tsx` (adapter binds principal, verifies ownership) | `blocks.rs` | COMPLETE_VERIFIED | — |
| Abuse reports | Report a call | `calls/app/abuse_reports.rs` | POST `/v1/calls/:id/abuse-reports`, GET `/v1/abuse-reports` | — | `calling_abuse_reports` | `AbuseReportDialog.tsx` **never imported**; adapter has no rule → 404 | `abuse_reports.rs` | FRONTEND_ONLY_GAP | CAL-005 |
| Mobile calling | Calls on iOS/Android | — | (same APIs) | — | — | **None** in `apps/mobile` (grep: no calling/livekit/voip/callkit) | — | MISSING | CAL-003 |
| Departments (enterprise org structure) | Org units, members | `enterprise/departments.rs` | GET/POST `/v1/enterprise/departments`, GET/PATCH/DELETE `…/:id`, PUT `…/:id/members` | — | `enterprise_departments*` | Generic JSON admin page (`CallingResourcePage.tsx` l.130–240), admin allowlist only | `enterprise_foundation.rs` | COMPLETE_VERIFIED | CAL-007, CAL-017 |
| Extensions | Dial internal numbers | `enterprise/extensions.rs` | `/v1/enterprise/extensions[/:id]` | — | `enterprise_extensions` | Dial by extension in `CallsContent`; admin JSON page | `enterprise_extensions.rs`, rtc `enterprise_extension_call` | COMPLETE_VERIFIED | — |
| Ring groups | Simultaneous/sequential ringing | `enterprise/ring_groups.rs`; `calls/app/enterprise/ring_groups.rs` | `/v1/enterprise/ring-groups[/:id][/members]` | — | `enterprise_ring_groups*` | Admin JSON page | `enterprise_ring_groups.rs`, rtc `enterprise_ring_groups` | COMPLETE_VERIFIED | — |
| Queues | FIFO queue, agent offers, overflow, max wait | `enterprise/queues.rs`; `calls/app/enterprise/queues.rs` l.817–1100 | `/v1/enterprise/queues[/:id]`, PUT `…/members`, GET `…/entries` | Agent self-service pause/available | `enterprise_queue_*` | Admin JSON page; live entries page | `enterprise_queues.rs`, rtc `enterprise_queue` | COMPLETE_VERIFIED | CAL-001 (agent starvation) |
| IVR | Versioned flows, DTMF menus, timeouts | `enterprise/ivr.rs`; `calls/app/enterprise/ivr.rs` | `/v1/enterprise/ivr-flows[/:id][/versions[/:n]][/activation]`, POST `/v1/calls/:id/ivr/input` | — | `enterprise_ivr_*` | Keypad input in `CallsContent` l.238–242; flows via raw JSON | `enterprise_ivr.rs`; production "IVR keypad-to-extension" PASS | COMPLETE_VERIFIED | Structured IVR editor (CAL-017) |
| Business hours | Schedules + evaluation | `enterprise/hours.rs` | `/v1/enterprise/business-hours[/:id][/evaluation]` | — | `enterprise_business_hours` | Admin JSON page | `enterprise_hours_forwarding.rs` | COMPLETE_VERIFIED | — |
| Forwarding (internal targets) | Forward per rule/hours | `enterprise/forwarding.rs`; `calls/app/enterprise/forward.rs` | `/v1/enterprise/forwarding-rules[/:id]` | — | `enterprise_forwarding_rules` | `ForwardingSettings.tsx` (user settings tab) but adapter requires admin allowlist | `enterprise_hours_forwarding.rs` | COMPLETE_VERIFIED; web gated | CAL-006 |
| Forwarding to external numbers | PSTN forward | `calls/app/public/*` (`public_forward_tick`) | same | — | — | — | `public_*` suites with simulator | EXTERNAL_DEPENDENCY | CAL-010 |
| Voicemail deposit (app/RTC callers) | Unanswered → caller-only capture → mailbox | `calls/app/enterprise/voicemail.rs` (1067 l.), `capture.rs`, `packages/object-storage` | `/v1/enterprise/voicemail/mailboxes[/:id]` | — | `voicemail_mailboxes`, `voicemail_messages` (object key, sha256, duration) | — | `enterprise_voicemail.rs`; rtc `rtc_voicemail_through_egress`; production PASS (real audio, retention deletion) | COMPLETE_VERIFIED | — |
| Voicemail playback / read state | Owner listens, marks read, deletes | `enterprise/voicemail.rs`; `enterprise/http.rs` l.1789–1844 | GET `/v1/enterprise/voicemail/messages?mailboxId&principalId`, GET/DELETE `…/:id`, POST `…/media-access`, PUT `…/read-state` | — | — | `VoicemailInbox.tsx`/`VoicemailPlayer.tsx` exist but adapter (a) requires `CALLING_ADMIN_PRINCIPAL_IDS`, (b) never injects `principalId` → Administrator view | Backend tested; web path unusable for ordinary users | COMPLETE_VERIFIED (backend); FRONTEND_ONLY_GAP | CAL-006 |
| Voicemail malware scan | Scan stored audio | none (`grep -i malware|scan` in voicemail/recording → nothing) | — | — | — | — | Audio is server-generated (Egress/Asterisk), never user-uploaded | NOT_REQUIRED | CAL-021 |
| Recording: policy, consent, start/stop | Policy-driven, all-party consent, notice | `enterprise/recording.rs`; `calls/app/enterprise/recording.rs` (1583 l.) | `/v1/enterprise/recording-policies[/:id]`, GET `/v1/calls/:id/recording-authorizations`, POST `/v1/calls/:id/recording-consent`, GET/POST `/v1/calls/:id/recordings`, POST `…/recordings/:rid/stop` | — | `call_recordings*`, consent rows | `CallRecordingControls.tsx` **never imported**; adapter has no rule for these paths → 404 | `enterprise_recording.rs`; rtc `rtc_recording_through_egress`; production PASS | COMPLETE_VERIFIED (backend); FRONTEND_ONLY_GAP | CAL-005 |
| Recording access / retention / deletion | Signed ≤900 s URL, access audit, retention worker | `enterprise/recording.rs`; `retention/worker.rs`; `packages/object-storage` | GET/DELETE `/v1/enterprise/recordings/:id`, POST `…/media-access`, GET `…/access-audit` | Enterprise recordings list | `call_recording_access_audit` | Admin lookup by ID only (`[...segments]/page.tsx` "recordings/lookup") | `retention.rs`, production retention PASS | COMPLETE_VERIFIED | CAL-017 |
| SIP desk devices | Internal SIP phones via own edge | `enterprise/sip_devices.rs`; `calls/app/enterprise/sip.rs` (via Telephony command outbox) | `/v1/enterprise/sip-devices[/:id]`, internal `/internal/v1/telephony/enterprise-calls`, `/enterprise-keypad` | — | `enterprise_sip_devices` (opaque device_ref) | Admin JSON page | `enterprise_sip_certification.rs` (real Asterisk + SIPp in CI) | COMPLETE_VERIFIED (CI); not deployed | CAL-016 |
| Enterprise audit / admin review | Who changed what; queue boards | `enterprise/audit.rs`; `enterprise/mod.rs` l.204–214 | GET `/v1/enterprise/audit`, GET `/v1/enterprise/effective-policy`, GET `…/queues/:id/entries` | Org-wide call listing for admins via web | `enterprise_audit_*` | Admin pages | `enterprise_operations.rs` | COMPLETE_VERIFIED (actor fidelity gap) | CAL-007, CAL-017 |
| Usage & limits | Usage records, quotas | `usage/mod.rs`, `platform/limits.rs` | GET `/v1/calling/usage`, `/v1/calling/limits`, `/v1/usage`, `/v1/limits` | — | `usage_*`, `platform_*_limits` | `UsageDashboard.tsx` (admin allowlist) | `usage_limits.rs`, `limits.rs`, `quota_levels.rs` | COMPLETE_VERIFIED | CAL-009 |
| PSTN interconnect (app↔phone, phone↔app, phone↔phone) | Carrier calls via Telephony | `services/telephony` (Asterisk/ARI, `providers/sip_trunk.rs`, `deterministic.rs`); Calling `calls/app/public/*` | Telephony internal `/internal/v1/legs/*`, Calling `/internal/v1/telephony/*` | Contracted carrier; production deployment | Telephony DB | None (out of PDR scope) | `telephony-sip-certification` (SIPp), `public_*` suites; `real_interconnect_certification.rs` prints NOT RUN | EXTERNAL_DEPENDENCY | CAL-010 |
| Numbering (search/reserve/provision/KYC/assign/port) | DCN lifecycle | `services/numbering` (71 k LOC); providers: `deterministic.rs` only; KYC: `DeterministicKycVerifier`, `ManualKycVerifier` | `/v1/numbers/search`, `/reservations`, `/acquisitions`, `/kyc/cases`, `/assignments/*`, `/ports` | Real carrier/number-provider adapter; portability provider | own DB | None (out of PDR scope) | `numbering.yml` suites (real PG, real Platform Access), `communications_e2e` | EXTERNAL_DEPENDENCY | CAL-011 |
| SMS gateway | Outbound/inbound SMS, receipts | `services/sms-gateway`; only `providers/deterministic.rs` (refused in staging/production) | `/v1/sms/*` (6 gateway routes), provider callbacks | Real aggregator adapter | own DB | None | `sms-gateway.yml`, `communications_e2e` | EXTERNAL_DEPENDENCY | CAL-012 |
| Privacy Relay (contexts, sessions, temporary relay numbers, contact vault) | Masked communication | `services/privacy-relay` (39 k LOC); Calling ingress `privacy_relay/http.rs` | Calling: `/v1/privacy-relay/{contexts,sessions,contact-access-grants}…`; Relay service: `/v1/privacy-relay/contacts…` (cluster-internal) | Production deployment; carrier for relay numbers | own DB | Generic admin pages; contacts pages target a route Calling does not serve | `privacy-relay.yml`, `privacy_relay_e2e.rs` | EXTERNAL_DEPENDENCY (+CONTRACT_MISMATCH in web) | CAL-013 |
| Callback relay | Call-me-back via PSTN | `calls/app/public/callback.rs` | via POST `/v1/calls` (`callbackRequested`) | — | — | Adapter refuses (`route.ts` l.205–209) | `public_callback_relay.rs` (simulated) | EXTERNAL_DEPENDENCY | CAL-010 |


### 3.5 Messaging, Communications Hub, meeting chat, realtime


Legend: "BE-verified" = exercised by a Messaging integration test against real PostgreSQL (and real WebSockets/Redis/MinIO/clamd where noted) in `messaging.yml`, latest runs green (e.g. run 42182739628, main, 2026-11-06).

| Feature | Intended functionality | Backend implementation (file:line) | APIs available | APIs missing | DB fields present/missing | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| Direct chat creation | 1:1 conversation, request semantics, idempotent | `conversations/service.rs` create (~500–700); policy/profile selection 531–548 | `POST /v1/conversations` (Idempotency-Key); relay `POST /api/v2/messaging/relay/v1/conversations` | — | `messaging_conversations` (no title/avatar) | web `engine.ts:586-607` | `tests/conversations.rs`, `engagement.rs:1336` (DM requests); web CI journey creates DIRECT E2EE + MANAGED (steps pass before #37 failure) | COMPLETE_VERIFIED (BE) | — |
| Group chat creation | GROUP + add members | create + `membership/mod.rs:470-540` | `POST /v1/conversations`, `POST …/members` | group name/avatar | missing: title, description, avatar | web creates GROUP then adds members one by one (`engine.ts:595-604`) | BE-verified; web group realtime broken (MSG-001) | PARTIALLY_IMPLEMENTED | add conversation title/avatar (MSG-007); fix MSG-001 |
| Send message (MANAGED) | sealed text, clientMessageId dedupe | `messages/mod.rs:445-520`, `managed.rs:252-375` | `POST …/messages` | — | `uq_messaging_messages_client` (conversation, sender, clientMessageId) | web `engine.ts:1016-1023` | `tests/messaging.rs:445` | COMPLETE_VERIFIED (BE) | web: stable clientMessageId on retry (MSG-008) |
| Send message (PRIVATE_E2EE / MLS) | opaque MLS ciphertext, epoch check | `messages/e2ee.rs` | `POST …/messages`, `…/mls/*` | — | ciphertext, acceptance digest | web `engine.ts:1025-1050` | `tests/messaging.rs:61,145,218` real OpenMLS devices | DEFERRED_BY_OWNER / EXTERNAL_DEPENDENCY in prod (BE verified in staging/test) | see MSG-013 |
| Delivery receipts | DELIVERED on ack or sync past | `messages/mod.rs:925-970` (advance_delivery), ack `realtime/connection.rs:677-740` | WS `ack`; event `delivery.cursor.updated` | REST read of others' delivered cursors (only via events) | `messaging_received_cursors` | none (web "delivered" is a local label, `engine.ts:926,975`) | `tests/realtime.rs:836` | FRONTEND_ONLY_GAP | render receipts from events |
| Read receipts | monotonic read cursor, privacy-gated sharing | `messages/mod.rs:847-918` | `PUT …/read-cursor`; events `read.cursor.updated`, own-device `read-cursor.synced` | — | `messaging_read_cursors`, `messaging_presence_settings.share_read_receipts`, `conversations.read_receipts` | web writes cursor (`engine.ts:989-993`), never displays others' | `engagement.rs:1254`, `realtime.rs:836` | FRONTEND_ONLY_GAP | display read state |
| Typing indicators | ephemeral, TTL, coalesced | `realtime/connection.rs:742-790` | WS `typing {state: started|stopped}` → `typing` frame | — | none (by design) | transport has `typing()` (`messaging-realtime.ts`), engine/UI never call it | `realtime.rs:529` | FRONTEND_ONLY_GAP | wire UI; unusable in prod anyway (MSG-004) |
| Presence | privacy-respecting | `realtime/presence.rs`, connection 793-860 | WS `presence.set`, `presence.query`; `GET/PUT /v1/messaging/presence/settings` | — | presence settings table | none | `realtime.rs:587` | FRONTEND_ONLY_GAP | — |
| Reactions | MANAGED server reactions; E2EE reactions are messages | `engagement/http.rs:31-38` | `GET/PUT/DELETE …/messages/{id}/reactions[/{reaction}]` (relayed) | — | reactions table | none (web route allowlist `routes.ts` lacks them) | `engagement.rs:129` | FRONTEND_ONLY_GAP | — |
| Edit (MANAGED) | PATCH with revisions | `messages/managed.rs` edit; `messages/http.rs:38-45` | `PATCH …/messages/{id}`, `GET …/revisions` | **PATCH not reachable for first-party clients** (relay has no PATCH handler) | `edit_count`, `edited_at`, revisions table | none | `messaging.rs:445,991` | CONTRACT_MISMATCH | MSG-002 |
| Edit (E2EE) | new ciphertext kind=EDIT | `e2ee.rs` | `POST …/messages` kind EDIT | — | `target_message_id` | none | `messaging.rs` | FRONTEND_ONLY_GAP | — |
| Delete for everyone | sender tombstone within window | `messages/mod.rs:767-830`, tombstone 707-757 | `DELETE …/messages/{id}` | — | lifecycle, purge_after | none in web UI | `messaging.rs:445,991` | FRONTEND_ONLY_GAP | — |
| Delete for me | hidden marker | `messages/mod.rs:778-792` | `DELETE …?scope=ME` | **relay rejects `scope` query** (`relay.rs:107` query `&[]`, check 604-617) | `messaging_message_hidden` | none | BE-verified | CONTRACT_MISMATCH | MSG-003 |
| Replies / threads | replyToMessageId, thread root | `engagement/mod.rs:416+`, list filter `messages/mod.rs:611-660` | send `replyToMessageId`; `GET …/messages?threadRootId`; `GET …/messages/{id}/thread` | relay drops `threadRootId` (thread route still relayed) | `reply_to_message_id`, `thread_root_id` | none | `engagement.rs:129,831` | FRONTEND_ONLY_GAP | — |
| Pins | OWNER/ADMIN pins, expiry | `engagement/http.rs:40-41` | `GET …/pins`, `PUT/DELETE …/pins/{id}` | — | pins table | none | `engagement.rs:129,769` | FRONTEND_ONLY_GAP | — |
| Unread counts | per conversation | `engagement/mod.rs:386-410` | `GET …/unread` | aggregate/inbox unread; unread in conversation list | — | none (allowlisted, unused) | `engagement.rs:129` | PARTIALLY_IMPLEMENTED | include unread/last activity in list (MSG-007) |
| History pagination | before/afterSequence | `messages/mod.rs:596-660` | `GET …/messages?afterSequence|beforeSequence&limit≤500` | — | `sequence` | web only forward from 0 (loads whole history, no back-pagination) | `messaging.rs:527` | FRONTEND_ONLY_GAP | virtualized back-pagination |
| Sync after reconnect (event cursor) | REST replay `?after=`, WS subscribe `afterEventSequence` | `messages/mod.rs:1159-1200`; `realtime/connection.rs:447-560` | `GET …/events?after&limit`; WS `subscribe` | **web/relay use `afterEventSequence` on REST** | outbox `conversation_event_sequence`, `messaging_ack_cursors` | web `engine.ts:488-502` | `realtime.rs:288` (BE); web path untested | CONTRACT_MISMATCH | MSG-001 |
| Ordering / gap-free fanout | per-conversation sequence under row lock | `events/outbox.rs:91-131` (UPDATE…RETURNING holds lock to commit) | — | — | `messaging_conversation_sequences` | web enforces contiguous sequence (`messaging-realtime.ts:199-207`) | `realtime.rs:416` concurrent senders | COMPLETE_VERIFIED (BE) | — |
| Idempotency | conversation create key; message clientMessageId | `managed.rs:301-352`; create Idempotency-Key | — | — | unique constraint | web sends `web-<uuid>` key on create; new clientMessageId per attempt | `messaging.rs:218` | IMPLEMENTED_UNVERIFIED (client side) | MSG-008 |
| Offline queue | queue sends while offline | — (client) | — | — | — | web in-memory `pendingSends`, failed items never retried (`engine.ts:996-1006`) | web CI "offline delivery in order" step passes | PARTIALLY_IMPLEMENTED | persistent queue + retry with stable ids |
| Attachments upload/finalize | signed PUT, size/sha bound, finalize | `attachments/mod.rs:300-306` (size), 780-880 | `POST …/attachments`, `…/finalize`, `GET …/{id}` | — | attachment rows, quota | web `engine.ts:1063-1100` | `tests/attachments.rs` real MinIO + clamd | COMPLETE_VERIFIED (BE) | default policy disables attachments (MSG-017) |
| Malware scan | fail-closed, scan gateway in prod | `attachments/scanner.rs:1-60`, `scan_and_settle` 878+; `services/security-scan-gateway` | `POST /internal/v1/scans` (gateway) | — | scan_status, scan_finding | web treats non-READY finalize as failure (`engine.ts:1092`) | `attachments.rs`, `security-scan-gateway/tests/gateway.rs` | COMPLETE_VERIFIED (BE) | web retry on VERIFYING (MSG-008) |
| Download | grant re-checks membership/window/lifecycle | `attachments/mod.rs:1040-1170` | `POST …/download-grants`, `…/thumbnail-grants`, `GET …/content` (not relayed) | — | — | web `engine.ts:1117-1133` | `attachments.rs` | COMPLETE_VERIFIED (BE) | — |
| Size limits | 25 MiB default, 10 per message | `conversations/policy.rs:249-262` | policy `PUT /v1/messaging/policy` (admin, not relayed) | — | — | — | BE-verified | COMPLETE_VERIFIED (BE) | — |
| Search (MANAGED) | FTS, membership-aware | `search/mod.rs` | `GET …/search`, `GET /v1/messaging/search` (relayed) | — | `managed_search` tsvector | none (web search only filters titles locally, `MessagesContent.tsx:399`) | `compliance.rs`/`query_plans.rs` | FRONTEND_ONLY_GAP | — |
| Privacy / blocking | DIRECT block, Hub policy | `messages/mod.rs:344-351, 400-410`; `communications.rs:122-157`; Hub `policy.rs:13-40` (fresh pull of Messaging/Calling blocks) | `GET/POST/DELETE /v1/messaging/blocks` (relayed); Hub `PUT/DELETE /api/v2/communications/profiles/{id}/block` | — | blocks tables in both services | none | `messaging.rs:640`, Hub `unified.rs:1283` | FRONTEND_ONLY_GAP | — |
| Retention / disappearing | conversation retention (only tighter), purge worker | `conversations/service.rs:619, 1132, 1281-1330`; `compliance/retention.rs` | create `retentionSeconds`; PATCH (not relayed) | first-party change after creation (PATCH) | `retention_seconds`, `purge_after` | none | `engagement.rs:625` | PARTIALLY_IMPLEMENTED | MSG-002 |
| Moderation / reporting | member report; admin queue/actions | `moderation/http.rs:29-36` | `POST …/reports` (relayed); admin routes need `moderation.manage` credential | CF Meet admin surface | reports/actions tables | none | `compliance.rs`, `authorization_matrix.rs` | FRONTEND_ONLY_GAP | MSG-017 |
| E2EE (MLS) | client-side MLS, KT | `messages/e2ee.rs`, `mls/*`, `transparency/*`; gate `config/mod.rs:466-483`, create gate `service.rs:571-591` | devices, KeyPackages, MLS DS | production enablement | — | web worker engine | real OpenMLS tests; web CI journey fails at history recovery (#37) | EXTERNAL_DEPENDENCY (prod) | MSG-013 |
| Multi-device | per-device sessions, history transfer | `devices/history_transfer.rs`; realtime `per_device_connections` | `…/history-transfers/*` (relayed) | — | ack cursor per device | web `engine.ts:1137-1232` | `realtime.rs:653`, `history_transfer.rs`; web UI step fails (#37) | FRONTEND_ONLY_GAP (#37) | fix #37 |
| Meeting chat migration (ADR 0053) | shadow → MESSAGING_PRIMARY | `unified_messaging/{migration,mirror,store,scoped}.rs` | `/api/v2/admin/chat-migration[/transitions|/parity-runs]` | — | `conference_chat_message_links`; `CHAT_MIGRATED` triggers | web admin hook `useChatMigration.ts`; mobile still v1 chat | `meeting-service/tests/unified_messaging_e2e.rs` (CI job green) | DEFERRED_BY_OWNER (cutover) | MSG-012 before cutover |
| Chat-to-call | Hub launches call from conversation | Hub `calls.rs:208-233` → Calling `/internal/v1/communications/calls` | `POST /api/v2/communications/conversations/{id}/calls` | — | `communication_call_links`, timeline entries | none (web/mobile) | Hub `unified.rs:406` (simulated Calling owner) | FRONTEND_ONLY_GAP | MSG-006 |
| Meeting links in chat | instant/share meeting into conversation | Hub `meetings.rs:94-178` → Meeting `/internal/v1/communications/meetings/{instant,resolve}` | `POST …/conversations/{id}/meetings/{instant,share}`, `GET …/meetings` | chat message representation | `communication_meeting_links` | none | Hub `unified.rs:819`; `meeting-service/tests/communications_meetings.rs` | PARTIALLY_IMPLEMENTED | MSG-009 |
| Unified timeline / activity | call & meeting cards merged by anchorSequence | Hub `calls.rs:453-560` | `GET …/conversations/{id}/timeline`, `GET /activity`, `GET /inbox` | realtime nudge; message entries | `communication_timeline_entries` | none | Hub `unified.rs:406,1343,1462` | PARTIALLY_IMPLEMENTED / FRONTEND_ONLY_GAP | MSG-006, MSG-009 |
| Contact sync across services | Hub contacts + owner block pulls + account events | Hub `relationships.rs`, `policy.rs:13-40`, `events.rs:56-200` | contacts/contact-requests/favorites/lists (Hub) | block-event producers | Hub tables | none | Hub `unified.rs`, `foundation.rs` | FRONTEND_ONLY_GAP | MSG-016 |
| Communications Hub facades | Meeting → Hub dispatch → owners | Meeting `services/communications.rs:145-151` (`/*path` → dispatch); Hub `api.rs:288-556` | ~70 `/api/v2/communications/*` | — | — | none | Hub tests with simulated owners | IMPLEMENTED_UNVERIFIED (end-to-end) | MSG-010 |
| Realtime WS gateway | ticketed WS, durable + ephemeral | `realtime/http.rs:49-65`, `connection.rs` | `POST /v1/realtime/tickets`, `GET /v1/realtime` | — | client sessions, tickets | web transport `messaging-realtime.ts` | `realtime.rs` (19 tests), `realtime_redis.rs` | COMPLETE_VERIFIED (BE); unreachable for prod first-party users (MSG-004) | MSG-004 |
| Push notifications | APNs/FCM/WebPush adapters | `realtime/push*.rs` | `PUT/DELETE /v1/messaging/devices/{id}/push-registration` | provider credentials | push registrations | none | `push_adapters.rs` (stubbed endpoints) | EXTERNAL_DEPENDENCY | MSG-011 |
| In-meeting chat (web) | v2 conference messages | Meeting `/api/v2/conferences/:id/messages` (`routes/v2.rs:209-224`) | GET/POST/DELETE | — | — | web v2 rooms `useCollaboration.ts:47-58,258`; legacy rooms v1 `ChatProvider.tsx:24` | meeting tests | IMPLEMENTED_UNVERIFIED (frontend) | — |
| In-meeting chat (mobile) | v2 conference messages | — | — | mobile calls non-existent `/conferences/{id}/chat` (unmounted v2) | — | mounted panel uses v1 `/meetings/{id}/chat` (`use-chat.ts`) | — | CONTRACT_MISMATCH | MSG-012 |
| Session bridge (`session_bridge.rs`) | Developer Console session verify | `meeting-service/src/services/session_bridge.rs` | `POST /internal/v1/user-sessions/verify` | — | — | — | — | NOT_REQUIRED (not a messaging feature) | — |


### 3.6 Platform: Developer Platform, gateway, SDKs, Swagger, administration


| Feature | Intended functionality | Backend implementation (file:line) | APIs available | APIs missing | DB fields present/missing | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| Gateway route catalogue / default deny | Generated manifest; only allow-listed ops routable | `api-gateway/src/catalogue.rs:1-135`, `http/ingress.rs:200-263`, `scripts/contracts/public_api.py` | 572 routes (meetings 130, calling 124, messaging 96, communications 73, developer-platform 73, numbering 52, privacy-relay 17, sms 6, gateway 1) | — | n/a | none (SDKs only) | `catalogue.rs` unit tests; `tests/ingress.rs`; `certification/tests/journeys.rs` (real stack). Not deployed (PLT-005). | COMPLETE_VERIFIED (real-stack cert tests; not re-run here) | Fix PLT-001; deploy per PLT-005 |
| Manifest ↔ upstream route consistency | Every manifest upstream path exists | see checks table | — | none dangling | n/a | n/a | `public_api.py --check` (contract level); upstream route existence only by my script | IMPLEMENTED_UNVERIFIED (no CI check of upstream router existence) | Add a CI check that each manifest `upstreamPath` resolves in the upstream router (e.g. cert-stack 404 sweep) |
| Gateway scope pre-check | Route-level `requiredScope` vs credential scopes | `ingress.rs:259-263`, `public_api.py:234-236` | 207 of 360 credential routes carry a scope | 153 credential routes (messaging 94, numbering 52, sms 6, gateway 1) have none; upstream enforces | n/a | n/a | One route is broken (PLT-001) | CONTRACT_MISMATCH | PLT-001, PLT-011 |
| Platform request quotas | Per-env / service-account / credential minute+day limits | `api-gateway/src/quota.rs:1-25`, `ingress.rs:268-300` | `x-cf-quota-*` headers; 429 + Retry-After | No gateway throttling for session/participant/anonymous routes | Redis counters; DP durable daily counts | n/a | `certification/tests/journeys.rs:573 quota_enforcement_journey`, `chapters_10_13.rs:604` | COMPLETE_VERIFIED (credential traffic only) | PLT-012 |
| DP orgs / members / invitations / ownership | Tenant-scoped org admin with RBAC (OWNER/ADMIN/DEVELOPER/ANALYST/BILLING) | `developer-platform/src/http/public.rs:42-110`, `control/{organizations,members}.rs` | `GET/POST /v1/organizations`, `GET/PATCH /v1/organizations/{id}`, `/close`, members, invitations, `/invitations/accept`, `/ownership-transfer` | — | DP DB | **None.** The console UI is deferred (`docs/phases/phase-06/developer-console-api-handoff.md:3`) | `tests/control_plane.rs` (real PG); cert `journeys.rs:181` | COMPLETE_VERIFIED backend / FRONTEND_ONLY_GAP | Product decision D-07; build console |
| DP projects / environments / scopes / service accounts | App model under orgs | `public.rs:70-140`, `control/{projects,service_accounts,scopes}.rs` | see contract `platform-developer/http/v1.openapi.json` (73 ops) | — | DP DB plus PA authority | none | `tests/control_plane.rs`, cert `service_accounts.rs:78` | COMPLETE_VERIFIED backend / FRONTEND_ONLY_GAP | as above |
| Credentials (PA authority, DP self-service) | create, list, get, rotate with overlap, revoke, suspend/resume, scopes, expiry, limits, org policy | PA `identity/credentials.rs:1-125`; DP `control/credentials.rs:1-80` | `GET/POST …/service-accounts/{sa}/credentials`, `GET …/credentials/{id}`, `/rotate`, `/revoke`, `/suspend`, `/resume`, `PUT …/limits`, `GET/PUT …/credential-policy` | last-used, expiry warnings, rename/label update | HMAC verifier only (no plaintext); **no `last_used_at`** | none | cert `chapters_10_13.rs:604 credential_lifecycle_and_caller_quota_journey`, `journeys.rs:422 credential_revocation_journey` | COMPLETE_VERIFIED (core lifecycle); PARTIALLY_IMPLEMENTED (observability) | PLT-008 |
| Legacy meeting API keys | User API keys for `/api/v1/api/meetings*` | `meeting-service/src/handlers/api_keys.rs:66-279`, `middleware/api_key.rs:24-76` | `GET/POST /api/v1/api-keys`, `GET/DELETE /api/v1/api-keys/{id}`, `POST /api/v1/api/meetings`, `POST /api/v1/api/meetings/{id}/join` | rotate, scopes, configurable expiry/limit | `api_keys.api_key` **plaintext**; `usage_limit`, `used_count`, `expires_at`, `status` | `apps/web/src/services/api-keys.service.ts`, `ApiKeysContent.tsx`, `DevelopersContent.tsx` | no dedicated security test found | PARTIALLY_IMPLEMENTED (security defect) | PLT-002, PLT-004 |
| Legacy meeting webhooks | Per-user meeting event webhooks | `handlers/webhooks.rs:108-305` | `GET/POST /api/v1/webhooks`, `PATCH/DELETE /api/v1/webhooks/{id}` | **delivery (none exists)**, deliveries list, replay, rotate secret | `webhooks.secret` plaintext | `webhook.service.ts`, `WebhooksContent.tsx` | CRUD only | PARTIALLY_IMPLEMENTED (registers, never delivers) | PLT-003 |
| Calling customer webhooks (legacy owner) | Machine webhooks via gateway | calling `webhooks::http::routes()` (`calling/src/routes.rs:56`) | `/v1/webhooks*`, `/v1/webhook-deliveries/{id}/replay` (gateway default upstream calling) | — | calling DB | none | calling tests; cert `webhooks.rs` | IMPLEMENTED (verification belongs to calling audit) | Converge on DP |
| DP central webhooks | Env-scoped webhooks, signed delivery, SSRF policy, replay, migration from calling | `developer-platform/src/webhooks/**`, `config/webhooks.rs` | 9 console routes `/v1/environments/{env}/webhooks*` (DEVELOPER_SESSION) + 8 forwarded `/v1/webhooks*` when `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED` | — | DP DB, sealed secrets | none | `tests/webhooks.rs`, `webhook_migration.rs`; cert `webhooks.rs:542-1412` incl. SSRF and shadow | COMPLETE_VERIFIED (flags default off / shadow) | PLT-003 |
| Usage & limits | Unified usage ledger, env usage/export/warnings, limits | `developer-platform/src/ledger/{mod,views}.rs`; gateway `routing.rs:434-446` | DP `/v1/environments/{env}/usage{,/facts,/export,/warnings}`, `/limits`; calling `/v1/usage`, `/v1/limits` (default owner); `/v1/{calling,messaging,numbers}/usage` | — | DP ledger plus rollups | none | cert `usage_migration.rs:382`, `chapters_10_13.rs:994` | COMPLETE_VERIFIED backend; ownership cut-over pending (`API_GATEWAY_CENTRAL_USAGE_ENABLED=false`) | Flip central usage after DP deploy |
| Production access workflow | Request → operator decide → activate | `control/production.rs`, `http/operator.rs` | `/v1/projects/{id}/production-access{,-requests}`, operator `/internal/v1/operator/production-access-requests*` | — | DP DB | none | cert `journeys.rs:1308 production_approval_journey` | COMPLETE_VERIFIED (flag `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED=false` in deploy manifest) | — |
| Operator APIs (DP) | Org state, plans, overrides, cost centres, incidents, usage ops, webhook migration | `developer-platform/src/http/operator.rs:20-60` (31 routes) | `/internal/v1/operator/**` | list/search orgs, operator audit read | DP activity table | **no operator console exists** (`cfmeet.operator-console` identity only in tests and cert harness) | cert harness drives it as the "operator console" | IMPLEMENTED_UNVERIFIED for UI; backend COMPLETE_VERIFIED in cert | PLT-009 |
| Operator APIs (PA) | Tenants/apps/envs/service accounts/credentials/principals | `platform-access/src/http/admin.rs` (23 routes), `bin/platform-access-admin.rs` | `/internal/v1/admin/**`, admin CLI | audit read, list tenants | `platform_access_audit` (write-only) | none | `tests/admin_cli.rs` | PARTIALLY_IMPLEMENTED (mutations cert-tested; audit read MISSING) | PLT-009 |
| Meeting legacy admin | Users, meetings, logs | `meeting-service/src/handlers/admin.rs:33-507` | `GET /api/v1/admin/users`, `PATCH /api/v1/admin/users/{id}`, `GET /api/v1/admin/meetings`, `GET /api/v1/admin/logs` | audit of admin actions | users.role/status | none found in web | — | PARTIALLY_IMPLEMENTED (no audit) | PLT-009 |
| Meeting v2 organizations | Platform-admin-created orgs, members | `handlers/v2/organizations.rs:35-110` | `POST/PATCH /api/v2/admin/organizations{,/{id}}`, `GET/PUT /api/v2/organizations/{id}/members{,/{userId}}` | admin list orgs | meeting DB | `apps/web/src/services/v2/organizations.service.ts` | meeting tests | IMPLEMENTED_UNVERIFIED (here) | PLT-010 |
| Public docs (gateway) | Read-only Swagger, SRI, no try-it-out, discovery | `api-gateway/src/docs.rs` | `GET /openapi.json`, `/openapi/{domain}.json`, `/docs`, `/discovery` | — | n/a | n/a | `docs.rs:215,248` parity tests; `tests/ingress.rs:75-81` | COMPLETE_VERIFIED (not deployed) | PLT-006 |
| Public docs (meeting-service, calling) | Engineering/public Swagger | `meeting-service/src/swagger/mod.rs:764-1100`; `calling/src/public_docs.rs` | `/swagger-ui/`, `/api-docs/*.json` on each | — | n/a | n/a | meeting swagger tests (SRI count at `swagger/mod.rs:1282`) | CONTRACT_MISMATCH (inconsistent publication) | PLT-006 |
| Private engineering catalogue | Full catalogue on protected host | `scripts/swagger/build-private-docs.py:14-27`, `private-docs-vps.yml` | static files behind htpasswd | DP, gateway, communications contracts | n/a | n/a | `scripts/swagger/test_private_docs.py` | PARTIALLY_IMPLEMENTED | PLT-007 |
| SDKs (TS, Dart) | Generated from public contracts; retry classes; paid-telecom never retried | `sdks/generate.py`, `generation.json` | 572 operations in both | Dart typed models | n/a | **not consumed by apps/web or apps/mobile** | `--check` in 5 workflows; TS/Dart unit tests; cert stack integration (`certification.yml:269-273`) | COMPLETE_VERIFIED (generation and staleness); FRONTEND_ONLY_GAP (no first-party use) | PLT-014 |
| Security scan gateway | Signed ClamAV verdicts, single-use tokens | `security-scan-gateway/src/lib.rs:1-150` | `POST /internal/v1/scans`, `/health`, `/ready`, `/metrics` (separate router) | — | none (stateless) | n/a | `tests/gateway.rs` | IMPLEMENTED_UNVERIFIED (here) | — |
| Object storage | Provider-neutral private S3 with short-lived signed reads | `packages/object-storage/src/lib.rs` | library only | — | key plus integrity metadata in owners | n/a | `tests/s3_live.rs` (live S3, opt-in) | IMPLEMENTED_UNVERIFIED (here) | — |


### 3.7 Security and reliability controls


| Feature | Intended functionality | Backend implementation (file:line) | APIs available | APIs missing | DB fields present/missing | Frontend consumption | Tests & limitations | Classification | Recommended action |
|---|---|---|---|---|---|---|---|---|---|
| User login / JWT sessions (meeting-service) | Short access token, rotating refresh token, revocation on logout/password change | `services/auth.rs:7-96` (HS256, no `typ`), `middleware/auth.rs:23-107`, `handlers/auth.rs:309-364` (refresh), `:384-436` (logout) | POST /api/v1/auth/{login,refresh,logout,change-password,reset-password} | refresh-token revocation list / session list per device | `users` has no session table; revocation epoch `account_communication_state.credentials_not_before` only when Hub configured | `apps/web/src/services/auth.service.ts:44-67` (JS cookie store) | Unit tests only for parts; refresh-as-access path untested | PARTIALLY_IMPLEMENTED | SEC-002/003/004 |
| JWT signing secret | Mandatory strong secret in every env | `config/app.rs:48-49` falls back to a hard-coded placeholder | — | — | — | — | No production guard (contrast platform-access `config/mod.rs:492-505` rejects the same placeholder for its pepper) | IMPLEMENTED_UNVERIFIED (unsafe default) | SEC-001 |
| Google OAuth sign-in | Verified ID token, account link by provider subject | `handlers/auth.rs:892-1049` | GET /api/v1/auth/oauth/google, /callback | — | `users` lacks `google_sub` / provider subject | `apps/web/src/hooks/useGoogleCallback.ts:22-73` reads tokens from query string | No test of aud/email_verified | PARTIALLY_IMPLEMENTED | SEC-005, DATA-016 |
| Login / OTP rate limiting | Per-account and per-source throttles | `services/rate_limiter.rs:11-60` (5/15 min per email), `services/otp.rs:17-133` | — | per-IP / per-device throttle, CAPTCHA | Redis only | — | — | PARTIALLY_IMPLEMENTED | SEC-008 |
| Legacy third-party API keys | Hashed key, atomic quota | `handlers/api_keys.rs:27-111`, `middleware/api_key.rs:131-183` | /api/v1/api-keys CRUD, /api/v1/api/meetings | — | `api_keys.api_key` plaintext; `used_count` RMW | — | — | PARTIALLY_IMPLEMENTED | SEC-006 |
| Platform credentials (Part VI) | Gateway → Platform Access live verification, delegated chain | `api-gateway/src/identity.rs:1-60`, `meeting-service/src/middleware/platform_auth.rs:121-232`, platform-access peppered verifiers | gateway-forwarded /api/v2 + all public product APIs | — | `platform_api_credentials` verifier+pepper version | SDKs | CI: `api-gateway/tests/platform_access_e2e.rs`, Part VI certification workflow | IMPLEMENTED_UNVERIFIED (CI-certified, real DB) | Keep |
| Internal service-to-service auth | Ed25519 JWS, 1 audience, ≤300 s, pinned alg, trust bundle permissions | `packages/service-auth/src/{lib.rs:1-25,verifier.rs:295-345,claims.rs:21-47}` | `X-CF-Service-Authorization` | method/path/body binding; replay cache in most receivers | Hub: `communication_service_nonces` | — | Strong unit tests; replay only checked by communications (`auth.rs:59-68`) and scan gateway (`lib.rs:256-268`) | IMPLEMENTED_UNVERIFIED | SEC-012 |
| Tenant / org isolation (v2 meetings, devplat, messaging) | Every by-id load scoped to owner/tenant/env | `platform_auth.rs:74-119` (404 for foreign env), `developer-platform/src/control/members.rs:135-150` (`AND tenant_id`), `messaging/src/attachments/mod.rs:992-1040` (`as_member`, `visible_from_sequence`) | — | — | composite (tenant,app,env,id) keys in calling/privacy-relay | — | Sampled; no defect found in samples | IMPLEMENTED_UNVERIFIED | SEC-020 (complete sweep) |
| Legacy v1 meeting authz | Host-only mutations, participant-scoped reads | `handlers/meetings.rs:419-431` (filter HostId), `participants.rs:294-320` (meeting-scoped) | /api/v1/meetings/** | — | — | `apps/web` legacy room | — | IMPLEMENTED_UNVERIFIED | SEC-009 (kick does not evict media) |
| Contact discovery (CF-ID) | Non-enumerable ids, rate limited, privacy-gated | `communications/src/crypto.rs:6-24` (128-bit), `api.rs:341-363` (120/h/profile), `policy.rs:275-296` (discoverable / visibility / blocks → 404) | POST /discovery/public-id/resolve, GET /profiles/{id} | — | `communication_privacy.public_id_discoverable` | web/mobile | Hub CI suites | IMPLEMENTED_UNVERIFIED | Keep; SEC-008 for sybil limits |
| Account enumeration (meeting-service) | Uniform responses | `handlers/auth.rs:538` 404 "No account found", register 409 | — | — | — | — | — | PARTIALLY_IMPLEMENTED | SEC-008 |
| PRIVATE_E2EE approval gate | Production refuses device enablement until independent review | `messaging/src/config/mod.rs:466-483` | — | — | no secret columns (repo guard) | — | config unit tests `:1081-1098` | EXTERNAL_DEPENDENCY (gate COMPLETE in code) | Keep; #13 external |
| Device authorization & revocation | Terminal revocation, immutable binding | `communications/migration/sql/002_unified.sql:87-113` (terminal trigger), messaging devices | Hub devices ops, messaging device APIs | — | duplicate registries (DATA-005) | web/mobile | stage 22/22 "devices" | IMPLEMENTED_UNVERIFIED | DATA-005 |
| LiveKit grants (Calling) | Least-privilege, short TTL, resealed | `calling/src/media/livekit.rs:60-68,399-408,586-612` (30 s upstream, ≤900 s, admin 60 s, no roomAdmin for users) | — | — | — | — | CI real LiveKit cert | COMPLETE_VERIFIED (CI real LiveKit) | Keep |
| LiveKit grants (legacy meetings) | Same | `meeting-service/src/services/livekit.rs:14,46-77` (6 h, host `room_admin`) | /api/v1 join | revoke-on-kick | — | legacy room | — | PARTIALLY_IMPLEMENTED | SEC-009 |
| Object storage / presigned URLs | Short TTLs, private buckets | `packages/object-storage/src/lib.rs:78-80` (read ≤900 s, write ≤3600 s), meeting artifacts `config/operations.rs:104-105` (default 300, clamp 30–3600), voicemail/recording 300 s | grants endpoints | — | — | — | storage relay verified (closure §8) | IMPLEMENTED_UNVERIFIED | SEC-019 (duplicate signer) |
| Attachment scanning | Signed, bound verdicts, replay-safe | `security-scan-gateway/src/lib.rs:245-275` | internal | — | `messaging_attachments` scan state | — | `scan-gateway-verify` run per closure | IMPLEMENTED_UNVERIFIED (prod run claimed) | Keep |
| Customer webhooks SSRF | Block private/metadata/rebinding | `packages/webhooks/src/endpoint.rs:1-120,218-231` | devplat/calling webhook subs | — | sealed secrets | — | unit tests `:284-316` | IMPLEMENTED_UNVERIFIED | Keep |
| Legacy meeting webhooks | Deliver signed events | `handlers/webhooks.rs:92-305` CRUD only; no dispatcher anywhere | /api/v1/webhooks CRUD | delivery | `webhooks.secret` plaintext, rows never read by a worker | — | — | PARTIALLY_IMPLEMENTED (accepted, never delivered) | DATA-007 |
| Push providers (APNs/FCM/WebPush) | Real delivery | `messaging/src/realtime/push_adapters.rs:1-30` | — | — | `messaging_push_registrations` | — | fakes only; prod `NOT_CONFIGURED` | EXTERNAL_DEPENDENCY | Owner credentials |
| SMS carrier | Real outbound/inbound | `sms-gateway/src/providers/{contract,deterministic}.rs` only | sms APIs | real provider adapter | — | — | deterministic | EXTERNAL_DEPENDENCY / MISSING adapter | Carrier contract then adapter |
| Rate limiting (public APIs) | Per-credential quotas | `api-gateway/src/quota.rs` (Redis + per-instance fallback) | — | — | — | — | api-gateway CI | IMPLEMENTED_UNVERIFIED | Keep |
| Idempotency | Idempotency-Key on creates | Present in calling, messaging, numbering, sms, telephony, devplat, communications; meeting-service only polls/webinar/artifacts (`specialized.rs:549`, `polls.rs:1344`) | — | Idempotency-Key on POST /api/v2/meeting-spaces, invitations, conferences | `meeting_artifacts.idempotency_key`, `webinar_registrations.idempotency_key_digest` | — | — | PARTIALLY_IMPLEMENTED | SEC-014 |
| Audit logging | Append-only audit of security-relevant actions | platform-access `platform_access_audit`, calling `platform_operator_audit`/`enterprise_admin_audit`, messaging `messaging_operator_audit`, meeting `conference_audit_events` (immutable trigger) | — | meeting-service account/admin/API-key audit | no table for user/admin/API-key events | — | — | PARTIALLY_IMPLEMENTED | SEC-013 / DATA-015 |
| DB backup & restore | Scheduled, off-host, tested | `infra/deployment/backup/{backup,restore}.sh` (7 services), DR cert tests (calling `tests/disaster_recovery.rs`, numbering/privacy-relay/sms CI) | — | meeting-service, communications, developer-platform backups; any schedule | — | — | CI restore tests for 4 services | PARTIALLY_IMPLEMENTED (+ off-server DEFERRED_BY_OWNER) | SEC-010 |
| Release migrations & rollback | Pre-migration snapshot, separate migrator role | `infra/deployment/backend_runtime.py:51-62` runs `migration up` with the runtime container's env; no dump | — | pre-migration backup | — | — | — | PARTIALLY_IMPLEMENTED | SEC-011 / DATA-013 |
| TLS renewal | Automated renew, activate, monitor, notify | `docs/operations/tls-certificate-automation.md`, `cfmeet-certificate-monitor.yml` (daily cron 04:23) | — | coverage of non-`api.cfmeet.com` names (livekit/rtc/TURN) unverified | — | — | `infra/calling/ingress-ci/private_tls_renewal.py` | IMPLEMENTED_UNVERIFIED (prod runs claimed) | SEC-017 |
| Monitoring / alerting | Metrics + alert rules + paging | Prometheus exposition (`/internal/metrics`), runbook PromQL; alerts are a table with "APPROVAL REQUIRED" | — | alert rules as code, Alertmanager/paging | — | — | none | PARTIALLY_IMPLEMENTED | SEC-016 |
| Data retention | Per-domain retention + purge | calling/telephony/privacy-relay/messaging retention migrations; meeting artifacts `services/artifacts/retention.rs` | — | meeting-service outbox/session logs/chat/webhook events purge; devplat activity/usage purge; platform audit retention | — | — | — | PARTIALLY_IMPLEMENTED | DATA-010 |
| Account deletion / erasure | User-initiated deletion with cascade policy | only `POST /api/v1/users/me/deactivate` (`routes/users.rs`) | deactivate | delete/erase account | FK policy conflicts (DATA-009) | — | — | MISSING | DATA-009 |

---


## 4. Detailed findings: domains without a dedicated document

## Accounts, identity, contacts, devices, notifications


- **meeting-service (account authority)**
  - `src/routes/{auth,users,admin,mod,v2,api,api_keys}.rs`
  - `src/handlers/auth.rs` (whole file), `src/handlers/users.rs`, `src/handlers/admin.rs` (status/role paths)
  - `src/handlers/v2/unified_messaging.rs` (bridge, directory)
  - `src/middleware/{auth,api_key,platform_auth}.rs`
  - `src/services/{auth,otp,token_blacklist,rate_limiter,actor,session_bridge,communications}.rs`
  - `src/services/unified_messaging/identity.rs`, `src/config/app.rs`, `src/state.rs`, `src/main.rs` (Communications worker)
  - `src/dto/{auth,users}.rs`
  - `migration/src/m20260116_000001_create_users.rs`, `migration/src/m20261103_000001_communications_account_authority.rs` (DB trigger)
  - tests: `legacy_v1_web_compat.rs`, `unified_messaging_e2e.rs` (logout), `session_bridge.rs` (listing only)
- **communications (Hub)**
  - `src/api.rs` (whole dispatch table), `src/devices.rs` (whole file), `src/events.rs` (account/block events, outbox), `src/auth.rs` (account status / principal checks)
  - `src/profiles.rs` (`ensure_profile`), `src/invites.rs`, `src/internal.rs` (`phone_deferred`)
  - `src/chat.rs` (notification preferences), `src/sync.rs` (inbox), `src/calls.rs` (`call_contact`, activity), `src/policy.rs:162`
  - tests: `tests/foundation.rs`, `tests/unified.rs` (names and harness)
- **messaging**
  - `src/http/devices.rs`, `src/devices/mod.rs:800-905` (revoke), `src/realtime/push.rs`, `src/realtime/push_adapters.rs` (header), `src/communications.rs:321-323`, `src/realtime/sessions.rs:170`, `src/config/mod.rs` (token TTL)
- **calling**
  - `src/endpoints/service.rs` (revoke), `src/communications.rs` (`require_personal_signaling`, `revoke_endpoint`)
  - `src/signaling/{ws,push,credentials,mod}.rs`, `src/main.rs:453`
- **platform-access**
  - route inventory (`src/http/*`), `src/identity/principals.rs` (status). It holds machine credentials and principals only. There is no human session or device authority here.
- **certification**
  - `certification/tests/personal_communications.rs` (real five-service stack, CI job `.github/workflows/communications-hub.yml:132`)
- **web**
  - `apps/web/src/services/{auth.service,user.service,api}.ts`, `store/cookie.store.ts`, `hooks/useGoogleCallback.ts`
  - `components/dashboard/SettingsContent.tsx`, `components/auth/LoginModal.tsx`, `lib/messaging/signout.ts`, `lib/calling/server.ts`
  - `services/calling/endpoints.service.ts`, `components/messaging/MessagesContent.tsx`
- **mobile**
  - `apps/mobile/features/auth/{api,hooks,components}`, `lib/storage/secure-store.ts`, `app/(tabs)/settings.tsx`, `package.json`
- **infra**
  - `infra/api-gateway/public-routes.json` (auth routes), `infra/deployment/backend_runtime.py`
- **spec**
  - `CFMeet_Phase2_Frontend_Product_Design_Requirements_v1.0.pdf`, F-01 (converted with `pdftotext` for reading)

Commands used included `grep -rn "verify_token\|is_blacklisted\|require_fresh_credentials\|credentials_not_before" services/meeting-service/src`, `grep -rn "account_communication_outbox" …`, `grep -rln "communications" apps/web/src`, `grep -rn "pushManager\|serviceWorker" apps/web/src`, and `grep -rniE "totp|mfa|two.factor|webauthn"` across the services and apps.



### ACC-001 — Access and refresh tokens are indistinguishable: a refresh token works as a 7-day bearer token, and an access token can mint refresh tokens
- **Service/module:** meeting-service `services/auth.rs`, `middleware/auth.rs`, `services/actor.rs`, `services/session_bridge.rs`, `handlers/auth.rs`
- **Expected:**
  - Refresh tokens are accepted only by `/auth/refresh`.
  - Access tokens are rejected by `/auth/refresh`.
  - Tokens carry a type/audience (`typ`, `aud`, `jti`).
- **Actual:**
  - Both tokens come from the same `generate_token` with identical claims (`sub, role, exp, iat, auth_time`). Only `exp` differs (`services/auth.rs:33-90`).
  - `verify_token` uses `Validation::default()` with no type check (`services/auth.rs:88-96`).
  - `auth_middleware` (`middleware/auth.rs:38-47`), `resolve_actor` (`actor.rs:266`) and the developer session bridge (`session_bridge.rs:161`) accept any valid, non-blacklisted token. A refresh token therefore authenticates every `/api/v1/*` and `/api/v2/*` route for 7 days.
  - `refresh_token` accepts an access token too (`auth.rs:323`).
  - Logout blacklists only the presented access token (`auth.rs:414-417`). The refresh token is never blacklisted, so used as a bearer it survives logout on v1 (see ACC-002).
- **Evidence:**
  - `services/auth.rs:24-29`: `access_token_expiry: Duration::minutes(15)`, `refresh_token_expiry: Duration::days(7)`, same `Claims` struct, `Header::default()` (HS256).
  - `grep -rn "typ\|token_type" services/meeting-service/src/services/auth.rs` finds nothing.
  - Web stores the refresh token in a JS cookie (`store/cookie.store.ts:23`), which raises the value of stealing it.
- **API/Swagger refs:** Swagger for `/api/v1/auth/login` documents "refresh token (valid 7 days)" (`auth.rs:192-197`).
- **Affected frontend workflow:** every signed-in workflow. A stolen refresh cookie gives 7-day API access.
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P0
- **Business impact:** session hijack persists 7 days and survives logout. Token-theft impact is multiplied about 670× compared with 15-minute access tokens.
- **Recommended remedy:**
  - Add `typ` (`access`/`refresh`) plus `aud`/`iss`/`jti` claims. Reject `typ=refresh` in all middleware and bridges, and accept only `typ=refresh` in `/auth/refresh`.
  - Old tokens without `typ`: treat them as access-only and force re-login for refresh. Note this logs out existing users.
  - Better still, use opaque, DB-backed refresh tokens (see ACC-007/017). This needs a new `user_sessions` table (additive migration).
- **Dependencies:** ACC-007, ACC-017
- **Complexity:** MEDIUM
- **Regression risk:** MEDIUM (web/mobile refresh interceptors, certification tests decode JWTs)
- **Confidence:** CONFIRMED (code). Not reproduced at runtime.

### ACC-002 — The v1 auth middleware and the developer session bridge ignore the account revocation epoch: password reset/change, logout and deactivation do not end sessions there
- **Service/module:** meeting-service `middleware/auth.rs`, `services/session_bridge.rs`
- **Expected:** after a password change/reset or logout-all, every earlier access and refresh token is rejected on every surface.
- **Actual:**
  - The DB trigger sets `account_communication_state.credentials_not_before=now()` on password or status change (`migration/src/m20261103_000001_communications_account_authority.rs:18-32`). Logout sets it too (`services/communications.rs:651-672`).
  - That epoch is enforced only in `/auth/refresh` (`auth.rs:347-353`) and the v2 `resolve_actor` (`actor.rs:322-326`).
  - `auth_middleware` (all `/api/v1/users/*`, `/api/v1/auth/change-password`, `/api/v1/api-keys/*`, `/api/v1/admin/*`, legacy `/api/v1/meetings/*`) checks only the per-token Redis blacklist and user status (`middleware/auth.rs:38-90`).
  - `session_bridge::verify` does the same (`session_bridge.rs:161-200`).
  - Result: after a victim resets their password because a session was stolen, the attacker's access token (15 min) and refresh token used as a bearer (7 days, ACC-001) can still:
    - `PATCH /users/me`
    - `POST /users/me/deactivate`
    - create API keys (`routes/api_keys.rs`), which become a durable foothold
    - drive legacy meetings
    - authenticate to the Developer Platform
  - `reset_password`/`change_password` have no explicit token revocation (`auth.rs:640-650`, `759-765`).
- **Evidence:**
  - `grep -rn "require_fresh_credentials" services/meeting-service/src` → only `actor.rs:322`, `handlers/auth.rs:348`, `communications.rs:704`.
  - The real-stack test checks only that refresh returns 401 after logout (`certification/tests/personal_communications.rs:1120-1131`). It never presents the old token to a v1 route.
- **API/Swagger refs:** `/api/v1/auth/reset-password`, `/change-password`, `/logout`
- **Affected frontend workflow:** "I was hacked → reset password" recovery. Logout on shared devices.
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P0
- **Business impact:** account recovery does not evict an attacker, and stolen credentials can mint persistent API keys.
- **Recommended remedy:**
  - Call the same `require_fresh_credentials` (made unconditional, see ACC-003) from `auth_middleware` and `session_bridge::verify`. Cache it briefly if needed.
  - Optionally revoke or flag the user's API keys on password reset.
  - No schema change is needed.
- **Dependencies:** ACC-003
- **Complexity:** LOW
- **Regression risk:** LOW–MEDIUM. The current session is also revoked after a password change, so the web client must re-login or receive fresh tokens from the change-password response.
- **Confidence:** CONFIRMED

### ACC-003 — Credential revocation is switched off whenever the Communications bridge is not configured
- **Service/module:** meeting-service `services/communications.rs:651-731`
- **Expected:** the account-owned revocation epoch works no matter which optional integrations are enabled.
- **Actual:**
  - `require_fresh_credentials` returns `Ok(())` immediately when `state.communications.is_none()` (`communications.rs:709-711`).
  - `record_logout` is a no-op in the same case (`:652-654`).
  - The DB trigger still writes `credentials_not_before`, but no one reads it.
  - Without `CF_COMMUNICATIONS_BASE_URL`:
    - logout blacklists one access token, and the refresh token stays fully usable on `/auth/refresh` for 7 days
    - password reset/change and deactivation never invalidate refresh tokens (deactivation is still caught by the status check)
  - `infra/deployment/backend_runtime.py:41,59` treats `CF_COMMUNICATIONS_BASE_URL` as optional. The production value was not verified.
- **Evidence:** `communications.rs:704-731`, `communications.rs:651-654`, `state.rs:83` (`communications: None` by default).
- **API/Swagger refs:** the `/api/v1/auth/logout` doc ("Clients should also discard their stored refresh token") admits the server does not revoke it.
- **Affected frontend workflow:** logout, password reset.
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P0 (if any deployment runs without Communications; otherwise P1 as a latent misconfiguration risk)
- **Business impact:** logout and password reset are cosmetic in such deployments.
- **Recommended remedy:**
  - Move the epoch check and logout epoch bump out of the Communications module into an Account-owned `credential_epoch` service that always runs. Keep the Hub fan-out optional.
  - The table already exists, so no schema change is needed.
- **Dependencies:** none
- **Complexity:** LOW
- **Regression risk:** LOW
- **Confidence:** CONFIRMED (code); REQUIRES_VALIDATION (production configuration)

### ACC-004 — Google sign-in silently takes over a pending email/password account (pre-hijacking); weak ID-token validation
- **Service/module:** meeting-service `handlers/auth.rs:885-1049`
- **Expected:**
  - Link a Google identity to an existing account only after the user proves ownership of the existing account, or after `email_verified=true` combined with discarding any unverified local credential.
  - Verify the ID token (`aud`=client_id, `email_verified`, ideally `nonce`). Store the Google `sub` and match on it.
- **Actual:**
  - Existing user found by exact email match (`:984-988`). If the status is `PendingVerification`, it is flipped to `Active` (`:999-1004`), while `password_hash` and `auth_type=Local` are left untouched.
  - Attack: an attacker registers `victim@gmail.com` with a password of their choice (the account stays pending, and pending accounts can already log in, `:250-263`). Later the victim clicks "Continue with Google", which activates the attacker-created account. The attacker keeps password access to the now-verified account and to everything the victim does in it.
  - ID token is "basic decoding without signature verification" (`:950`). Only `iss` is checked (`:977-981`). There is no `aud` or `email_verified` check.
  - Google `sub` is parsed and discarded (`_google_sub`, `:972-975`). Identity is keyed on mutable email only.
  - Mitigation: the token comes from a direct TLS exchange with Google using the client secret, so forging the signature is not practical. The missing `email_verified` check matters for Google accounts on non-Gmail addresses.
- **Evidence:** quoted lines above. `grep -n "email_verified" services/meeting-service/src/handlers/auth.rs` finds nothing.
- **API/Swagger refs:** GET `/api/v1/auth/oauth/google/callback`
- **Affected frontend workflow:** web "Continue with Google" (`auth.service.ts:72`)
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P0
- **Business impact:** account takeover of any user who registers or links via Google.
- **Recommended remedy:**
  - If a matching local account is pending, either delete it and create a fresh Google account, or clear `password_hash` before activating. Linking an Active local account should require the password.
  - Require `email_verified==true` and `aud==client_id`, and store `google_sub`. This needs an additive column `users.google_sub UNIQUE NULL`.
  - Verify the JWS against Google JWKS, or use the `jsonwebtoken` JWKS flow.
- **Dependencies:** ACC-005
- **Complexity:** MEDIUM
- **Regression risk:** LOW
- **Confidence:** CONFIRMED (code path); exploit not executed

### ACC-005 — The OAuth callback puts the raw access and refresh tokens in the redirect URL; no PKCE/nonce; state not bound to the browser
- **Service/module:** meeting-service `auth.rs:839-882`; web `hooks/useGoogleCallback.ts`
- **Expected:**
  - Spec F-01 acceptance: "No raw tokens in URLs/logs" (spec text line 204).
  - PKCE plus a state value bound to a browser cookie.
- **Actual:**
  - Redirects to `{APP_URL}/google/callback?access_token=…&refresh_token=…&user_id=…` (`auth.rs:874-883`). The tokens land in browser history, proxy/CDN/Vercel access logs and possibly Referer headers.
  - Web reads them from `useSearchParams` and never scrubs the URL; it only `router.replace`s after storing them (`useGoogleCallback.ts:26-80`).
  - State is stored only in Redis (`auth.rs:802-807`) and is not tied to the initiating browser, which enables login CSRF (an attacker can complete a flow into a victim's browser).
  - No PKCE or nonce.
  - `access_type=offline&prompt=consent` requests a Google refresh token that is then discarded (`:811`).
  - Error redirects embed internal error text such as "Token exchange failed: {Google body}" (`:900-907`, `:862-868`).
  - Get-then-delete of the state is not atomic (`:892-904`).
- **Evidence:** lines quoted above.
- **API/Swagger refs:** Swagger says 200/LoginResponse but the handler returns 302 (contract mismatch, `auth.rs:826-832`).
- **Affected frontend workflow:** Google sign-in
- **Owner class:** SHARED_CONTRACT
- **Classification:** CONTRACT_MISMATCH
- **Severity:** P1
- **Business impact:** token leakage, and a spec acceptance criterion fails.
- **Recommended remedy:**
  - Redirect with a single-use, short-TTL code (Redis) that the web exchanges via POST, or set httpOnly cookies.
  - Add PKCE and a state cookie, drop `access_type=offline`, and use generic error codes.
- **Dependencies:** ACC-019
- **Complexity:** MEDIUM
- **Regression risk:** MEDIUM (web callback page)
- **Confidence:** CONFIRMED

### ACC-006 — Google signup fails for common or long display names
- **Service/module:** meeting-service `auth.rs:1006-1020`; `migration/src/m20260116_000001_create_users.rs:40-45`
- **Expected:** every new Google user gets a unique, valid username.
- **Actual:**
  - `username: Set(name.clone())`, where `name` is the Google display name (for example "John Smith", or "Google User" as a fallback).
  - `users.username` is `varchar(50) NOT NULL UNIQUE`. A second "John Smith", a second account without a name ("Google User"), or a name over 50 characters makes the insert fail, and the user is redirected with a DB error.
  - Google names also bypass the registration regex (`dto/auth.rs:27-31`).
- **Evidence:** above.
- **API/Swagger refs:** –
- **Affected frontend workflow:** Google signup
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P1
- **Business impact:** Google signup breaks for a growing share of users.
- **Recommended remedy:** derive a slug from the email local-part or name, add a random suffix on conflict, and truncate to 50. Store the display name separately (ACC-021).
- **Dependencies:** ACC-021
- **Complexity:** LOW
- **Regression risk:** LOW
- **Confidence:** CONFIRMED

### ACC-007 — No session model: no session listing, no per-device logout, and logout is "everywhere" or "nowhere" depending on configuration
- **Service/module:** meeting-service auth; web/mobile settings
- **Expected:** list active sessions and devices, revoke one, "sign out other sessions", and log out only the current device.
- **Actual:**
  - Sessions are stateless HS256 JWTs. There is no sessions table and no session id (`jti`).
  - With Communications configured, `logout` bumps the account-wide `credentials_not_before` (`communications.rs:656`), so logging out on one phone kills refresh on every device, including web.
  - Without Communications, logout revokes only the presented access token (ACC-003).
  - The only "revoke-all" API is Messaging-scoped (`POST /api/v2/messaging/client-sessions/revoke-all`, `routes/v2.rs:449`).
  - Web Settings has only Profile, Danger Zone and Change Password (`SettingsContent.tsx:201,270,420`). Mobile is the same.
  - `auth_time` has one-second granularity, so a login in the same second as a logout is rejected (`communications.rs:724`, `<=`).
- **Evidence:** above. No sessions table in `migration/src`.
- **API/Swagger refs:** missing GET/DELETE `/api/v1/auth/sessions`
- **Affected frontend workflow:** settings → security; Journey H
- **Owner class:** BACKEND (+FRONTEND)
- **Classification:** MISSING
- **Severity:** P1
- **Business impact:** users cannot see or kill a stolen session, and unexpected global logouts happen.
- **Recommended remedy:**
  - Add a `user_sessions` table (id, user_id, device label, client_kind, created_at, last_refresh_at, revoked_at, refresh_hash). Put `sid` in the JWT and check revocation in the middleware.
  - Add GET `/api/v1/auth/sessions`, DELETE `/api/v1/auth/sessions/:id`, POST `/api/v1/auth/sessions/revoke-others`.
  - Make logout per-session, with an explicit "logout everywhere" that keeps using the epoch.
  - Additive migration.
- **Dependencies:** ACC-001, ACC-017
- **Complexity:** HIGH
- **Regression risk:** MEDIUM
- **Confidence:** CONFIRMED

### ACC-008 — Device revocation does not invalidate the account session; no device-management surface
- **Service/module:** Hub `devices.rs:291-358`; meeting bridge; web/mobile
- **Expected (Journey H):** revoking a device ends every credential that device holds: CF Meet JWT, Messaging client token, Calling signaling credential and push token.
- **Actual:**
  - Hub `revoke_device` revokes the Messaging device (`devices.rs:316-323`; Messaging then revokes the device's client sessions, key packages and push registration, `messaging/src/devices/mod.rs:893-896`) and the Calling endpoint (`devices.rs:325-332`; Calling cancels live invitations, `calling/src/communications.rs:801-803`).
  - It explicitly does not touch the account session: `"accountSessions":"Use Account logout or revoke-all to end other sessions"` (`devices.rs:355`). The CF Meet JWT is not device-bound, so it cannot be revoked per device (ACC-007).
  - The Hub device registry is populated only by an explicit `POST /devices/link`. No client calls it (`grep -rn "devices/link" apps services/meeting-service` finds nothing), so for real users `GET /devices` is empty.
  - Web revokes its own Messaging device only during logout (`lib/messaging/signout.ts`). There is no UI to list or revoke other devices.
  - Calling: an open signaling WebSocket re-checks only principal-level revocation every 5 s (`calling/src/signaling/ws.rs:127-131`), not endpoint status (see ACC-024).
- **Evidence:** above.
- **API/Swagger refs:** Hub `/api/v2/communications/devices*`
- **Affected frontend workflow:** "lost phone" flow
- **Owner class:** SHARED_CONTRACT
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P1
- **Business impact:** a lost or stolen device keeps CF Meet account access (meetings, v1 APIs, ability to register a new Messaging device) for up to 7 days.
- **Recommended remedy:**
  - Bind the session to the device (session id ↔ Messaging deviceId/Calling endpointId) and make Hub `revoke_device` call an Account `POST /internal/v1/accounts/sessions/{sid}/revoke`.
  - Auto-link Messaging devices and Calling endpoints into the Hub registry at registration time, or drop the Hub registry in favour of owner lists.
  - Build a device-management UI.
- **Dependencies:** ACC-007, ACC-009
- **Complexity:** HIGH
- **Regression risk:** MEDIUM
- **Confidence:** CONFIRMED

### ACC-009 — The Communications Hub (contacts, discovery, CF Meet ID/QR, invites, blocks, privacy, notification preferences, devices) has no frontend consumer
- **Service/module:** apps/web, apps/mobile
- **Expected:** a personal-communications UI (contacts, add-by-ID/QR, requests, block, privacy, notification settings, devices).
- **Actual:**
  - `grep -rln "communications" apps/web/src` → only marketing copy in `DevelopersContent.tsx:183`.
  - No `/api/v2/communications` call in web or mobile. Mobile has no messaging, contacts or calling at all.
  - Web discovery for chat is limited to colleagues in verified organizations (`MessagesContent.tsx:172-189`, `handlers/v2/unified_messaging.rs:264-338`). A user with no organization cannot find anyone.
  - Only the generated SDKs reference the Hub (`sdks/typescript/src/generated/operations.ts`).
- **Evidence:** grep commands above.
- **API/Swagger refs:** about 60 Hub operations behind `/api/v2/communications/*` (`communications/src/api.rs:298-540`)
- **Affected frontend workflow:** Journey A "discover contacts → initiate calling/messaging"
- **Owner class:** FRONTEND
- **Classification:** FRONTEND_ONLY_GAP
- **Severity:** P1
- **Business impact:** a large, tested backend surface is unreachable for end users, and personal (non-org) communication is impossible from the apps.
- **Recommended remedy:** build web (then mobile) screens on the existing APIs, reusing the generated TS SDK. No new endpoints are needed.
- **Dependencies:** ACC-020 for notifications
- **Complexity:** HIGH
- **Regression risk:** LOW
- **Confidence:** CONFIRMED

### ACC-010 — Revocation fan-out is lost for accounts without a Hub profile; Platform Access principals never mirror account status
- **Service/module:** Hub `events.rs:56-122`; meeting `services/communications.rs`; platform-access `identity/principals.rs`
- **Expected:** password change, deactivation, suspension and logout revoke Messaging and Calling credentials for every account that has a principal.
- **Actual:**
  - The Hub applies an account event only if a `communication_profiles` row exists (`events.rs:86 if let Some(p) = profile`). Otherwise it returns 200 `applied:false` and the Account outbox marks the event delivered.
  - Web users use Messaging through the meeting bridge (principal created in `unified_messaging/identity.rs:122`) and Calling through the web BFF, and never touch the Hub (ACC-009). So no Hub profile exists, and `messaging_communication_revocations`/`calling_communication_revocations` are never written for them.
  - Only logout separately revokes Messaging sessions (`auth.rs:428-435`). Password change, reset, deactivation and admin suspension do not.
  - Existing Messaging client tokens (default TTL 900 s, up to 86 400 s, `messaging/src/config/mod.rs:33`) and Calling signaling credentials (600 s default, 3600 s max, `calling/src/signaling/credentials.rs:44-45`) stay valid until they expire.
  - Meeting never calls Platform Access `set_principal_status`: `grep -rn "/internal/v1/principals" services/meeting-service/src` → only `/ensure`. A deactivated user's `NATIVE_CF_USER` principal stays ACTIVE for owners that check Platform Access only.
- **Evidence:** above.
- **API/Swagger refs:** internal `/internal/v1/communications/account-events`
- **Affected frontend workflow:** password reset / account deactivation while Messaging or Calling are open
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P1
- **Business impact:** revocation windows of up to 24 h on Messaging (configurable), and inconsistent identity state across services.
- **Recommended remedy:**
  - Have Account call `revoke_sessions_for_user` (Messaging) on every trigger event, not just logout.
  - Either have the Hub persist a revocation for principals without a profile (keyed by principal/accountReference), or have Account publish directly to Messaging and Calling.
  - Sync principal status to Platform Access on deactivate/suspend.
- **Dependencies:** ACC-003
- **Complexity:** MEDIUM
- **Regression risk:** LOW
- **Confidence:** CONFIRMED (code); REQUIRES_VALIDATION (Calling `governs()` scope for web BFF credentials)

### ACC-011 — No account deletion or personal data export
- **Service/module:** meeting-service users; all owners
- **Expected:** self-service account deletion (erasure across Account, Hub, Messaging, Calling, recordings) and data export (GDPR Art. 17/20).
- **Actual:**
  - Only `POST /api/v1/users/me/deactivate` (soft, `handlers/users.rs`). No re-authentication or password confirmation.
  - Hub profiles in state `DEACTIVATED` refuse activation forever (`profiles.rs:66-70`), even if an admin reactivates the account. Reactivation is therefore half-broken.
  - Messaging erasure exists only as a tenant-admin compliance tool (`messaging/src/compliance/deletion.rs`).
- **Evidence:** `grep -rniE "delete_account|erasure|export" services/meeting-service/src/handlers` → only meeting artifact exports.
- **API/Swagger refs:** missing DELETE `/api/v1/users/me`, POST `/api/v1/users/me/export`
- **Affected frontend workflow:** Settings → Danger Zone
- **Owner class:** PRODUCT_DECISION / BACKEND
- **Classification:** MISSING
- **Severity:** P1
- **Business impact:** regulatory exposure in GDPR markets, app-store requirements (Apple requires in-app account deletion), and no recovery path.
- **Recommended remedy:**
  - Add an orchestrated deletion job: Account tombstone → Hub DEACTIVATED + purge → Messaging `PRINCIPAL_ERASURE` → Calling endpoints revoked → Platform Access principal tombstone.
  - Add an export bundle job.
  - Require re-auth for deactivation and deletion.
- **Dependencies:** ACC-010
- **Complexity:** HIGH
- **Regression risk:** MEDIUM
- **Confidence:** CONFIRMED

### ACC-012 — Mobile logs credentials and tokens, misroutes unverified users, and lacks Google sign-in and push
- **Service/module:** apps/mobile
- **Expected:** no secrets in logs; verification routing; feature parity.
- **Actual:**
  - `features/auth/hooks/use-login.ts:14-25` `console.log`s the request (including the password) and the full response (access and refresh tokens). `auth.api.ts:24-25` logs URLs.
  - `use-login.ts:37` checks `status === "pending"`, but the backend sends `"pendingverification"` (`auth.rs:286-288`), so unverified users are never sent to `/verify`. `response.user.email` does not exist in `UserInfo` (`dto/auth.rs:78-90`).
  - Google sign-in is a "coming soon" toast (`login-form.tsx:164`, `signup-form.tsx:214`).
  - No `expo-notifications` dependency (`package.json`), so no push.
- **Evidence:** above.
- **API/Swagger refs:** LoginResponse
- **Affected frontend workflow:** mobile login/verify
- **Owner class:** FRONTEND
- **Classification:** FRONTEND_ONLY_GAP
- **Severity:** P1 (log leak), P2 (rest)
- **Business impact:** credential leakage via device logs and crash reporters.
- **Recommended remedy:** remove the logs (or guard them with `__DEV__` and redact), fix the status constant, and implement the Google flow with expo-auth-session using PKCE.
- **Dependencies:** ACC-022
- **Complexity:** LOW
- **Regression risk:** LOW
- **Confidence:** CONFIRMED

### ACC-013 — No multi-factor authentication
- **Service/module:** meeting-service auth
- **Expected:** optional TOTP or WebAuthn, at least for admins and for developers who hold API keys.
- **Actual:** `grep -rniE "totp|mfa|two.factor|webauthn|passkey"` over services and apps → nothing (only an SMTP test hint and a UI OTP input component).
- **Evidence:** above.
- **API/Swagger refs:** –
- **Affected frontend workflow:** security settings
- **Owner class:** PRODUCT_DECISION
- **Classification:** MISSING
- **Severity:** P1 for admin and developer accounts (platform admin can modify any user, `handlers/admin.rs:227`); P2 otherwise
- **Business impact:** single-factor compromise of admins.
- **Recommended remedy:** TOTP with recovery codes. Columns `users.mfa_secret_enc` and `mfa_enabled` plus a step-up claim. Additive.
- **Dependencies:** ACC-007
- **Complexity:** MEDIUM
- **Regression risk:** LOW
- **Confidence:** CONFIRMED

### ACC-014 — Account enumeration across every public auth endpoint
- **Service/module:** `handlers/auth.rs`
- **Expected:** uniform responses for existing and non-existing accounts.
- **Actual:**
  - `register` → 409 "A user with this email already exists" (`:48-51`).
  - `forgot-password` → 404 "No account found…" (`:536-539`), or 400 "This account uses Google sign-in" (`:542-547`).
  - `verify-email`, `resend-verification`, `reset-password`, `verify-reset-token` → 404 "User not found".
  - `verify-reset-token` additionally reveals whether a reset is pending (`:684-693`).
  - `login` returns before bcrypt for unknown emails and Google-only accounts (`:228-241`), which is a timing oracle.
  - Swagger documents these 404s, so the frontend depends on them.
- **Evidence:** above.
- **API/Swagger refs:** all `/api/v1/auth/*`
- **Affected frontend workflow:** signup, forgot password
- **Owner class:** BACKEND
- **Classification:** IMPLEMENTED_UNVERIFIED
- **Severity:** P2
- **Business impact:** enables targeted phishing and credential stuffing.
- **Recommended remedy:** return generic 200 for forgot/resend, run a dummy bcrypt for unknown users, and keep the 409 only behind CAPTCHA/rate limit, or send an "account exists" email instead.
- **Dependencies:** ACC-015
- **Complexity:** LOW
- **Regression risk:** LOW (frontend messaging)
- **Confidence:** CONFIRMED

### ACC-015 — Weak rate limiting on auth endpoints
- **Service/module:** `services/rate_limiter.rs`, `services/otp.rs`, infra
- **Expected:** per-IP and per-account throttles, signup throttle, CAPTCHA, and no lockout DoS.
- **Actual:**
  - Login: 5 attempts per email per 15 min (`rate_limiter.rs:10-14`). No per-IP limit, so credential stuffing across many emails is unthrottled. An attacker can also lock any user out indefinitely with 5 bad attempts every 15 minutes (lockout DoS).
  - The counter is cleared on success, and is incremented before the user lookup.
  - Register: no limit at all. Every call sends a verification email, so the endpoint can be used for mail bombing or spam relay to arbitrary addresses.
  - OTP verify: 5 per 15 min per email (`otp.rs`), so about 480 guesses/day against a 6-digit code (acceptable).
  - Auth routes are `EXCLUDED`/`LEGACY_DIRECT_INGRESS` from the API gateway (`infra/api-gateway/public-routes.json:1285-1362`), and `grep -rn limit_req infra` finds nothing.
- **Evidence:** above.
- **API/Swagger refs:** `/api/v1/auth/*`
- **Affected frontend workflow:** login, signup
- **Owner class:** BACKEND / INFRASTRUCTURE
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P2
- **Business impact:** account-takeover throughput, email reputation damage, DoS.
- **Recommended remedy:** add a per-IP sliding window (Redis, with trusted X-Forwarded-For), a signup limit with CAPTCHA, a progressive delay instead of a hard lock, and edge limits at nginx or the gateway.
- **Dependencies:** –
- **Complexity:** LOW
- **Regression risk:** LOW
- **Confidence:** CONFIRMED

### ACC-016 — Email verification is not enforced anywhere that matters
- **Service/module:** `handlers/auth.rs:250-263`, `services/auth.rs:118-140`, Hub `profiles.rs:16-130`
- **Expected:** `EMAIL_VERIFICATION_REQUIRED=true` (the default, `config/app.rs:81`) blocks unverified accounts, or at least blocks communications features.
- **Actual:**
  - `login` explicitly allows `PendingVerification` users and issues full tokens.
  - `check_email_verification` is dead code (`grep -rn check_email_verification src` → definition only).
  - The flag only controls the `verified_email` attribute (`actor.rs:317`).
  - Pending accounts can activate a Hub profile (`ensure_profile` checks only `active` status), register Messaging devices and get client tokens (`verified_user` checks only revocation, `unified_messaging.rs:49-59`). The certification test does exactly this with a never-verified account (`personal_communications.rs:69-118`).
- **Evidence:** above.
- **API/Swagger refs:** Swagger `/login` says "Accounts still pending email verification can log in".
- **Affected frontend workflow:** signup → verify
- **Owner class:** PRODUCT_DECISION / BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P2
- **Business impact:** throwaway accounts can message and call real users (spam, abuse), and ACC-004 is amplified.
- **Recommended remedy:** refuse Hub activation and Messaging/Calling bridges unless `verified_email` is present. Optionally restrict pending sessions to the verify flow.
- **Dependencies:** –
- **Complexity:** LOW
- **Regression risk:** MEDIUM (certification fixtures must verify accounts)
- **Confidence:** CONFIRMED

### ACC-017 — Refresh rotation without reuse detection or absolute lifetime
- **Service/module:** `handlers/auth.rs:309-363`, `services/auth.rs:50-61`
- **Expected:** rotate on use, invalidate the old token, detect reuse (revoke the family), and cap sessions at an absolute lifetime (e.g. 30–90 days).
- **Actual:**
  - The old refresh token is not blacklisted after rotation, so any number of parallel chains can be created from one token.
  - `refreshed_tokens` preserves `auth_time`, but nothing compares it with a maximum age. Refreshing weekly keeps a session alive forever.
  - When Communications is off there is no server-side state at all (ACC-003).
- **Evidence:** above.
- **API/Swagger refs:** `/api/v1/auth/refresh`
- **Affected frontend workflow:** –
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P2
- **Business impact:** a stolen refresh token gives permanent access.
- **Recommended remedy:** DB-backed refresh tokens with a family id (with ACC-007). In the interim, blacklist the presented refresh token for its remaining TTL and reject when `now - auth_time > MAX`.
- **Dependencies:** ACC-007
- **Complexity:** LOW (interim) / HIGH (full)
- **Regression risk:** MEDIUM. The web interceptor's concurrent refreshes (`api.ts:73-85` already queues them) and the multi-tab case (spec F-01 "logout across tabs") must not break.
- **Confidence:** CONFIRMED

### ACC-018 — JWT secret handling: insecure default, no strength check, a single HS256 key for every token kind
- **Service/module:** `config/app.rs:48-49`, `state.rs:75,85`
- **Expected:** fail fast when `JWT_SECRET` is missing or weak (under 32 bytes). Separate keys or `typ` per token kind. Key rotation (`kid`).
- **Actual:**
  - `JWT_SECRET` falls back to the literal `"your-secret-key-change-this-in-production"`, which is also in `.env.example:27`. A deployment missing the variable accepts tokens forged by anyone.
  - There is no length check.
  - User access, refresh and legacy guest tokens (`role:"guest"`) share one key and one claim shape. The realtime ticket key is derived from it (`realtime.rs:337`).
  - No `kid` and no rotation path.
- **Evidence:** above.
- **API/Swagger refs:** –
- **Affected frontend workflow:** all
- **Owner class:** BACKEND / INFRASTRUCTURE
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P2 (P0 if any environment runs without `JWT_SECRET`; production value not checked)
- **Business impact:** total authentication bypass under misconfiguration.
- **Recommended remedy:** make `JWT_SECRET` required and at least 32 bytes in non-local runtimes (like `CALLING_SIGNALING_SIGNING_KEY`, `MIN_KEY_LEN=32`). Add `typ`/`kid`.
- **Dependencies:** ACC-001
- **Complexity:** LOW
- **Regression risk:** LOW
- **Confidence:** CONFIRMED (code); REQUIRES_VALIDATION (deployed environment)

### ACC-019 — Web stores bearer and refresh tokens in JavaScript-readable cookies
- **Service/module:** `apps/web/src/store/cookie.store.ts:8-35`
- **Expected:** refresh token kept in an httpOnly, Secure, SameSite cookie set by the server (or a BFF), with the access token in memory.
- **Actual:** `js-cookie` sets `access_token` and `refresh_token` with `secure: true, sameSite: "lax"` but not httpOnly (it cannot be set from JS). Any XSS exfiltrates a 7-day token (ACC-001).
- **Evidence:** above.
- **API/Swagger refs:** –
- **Affected frontend workflow:** all web
- **Owner class:** FRONTEND / SHARED_CONTRACT
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P2
- **Business impact:** XSS escalates to account takeover.
- **Recommended remedy:** route auth through a Next.js route handler that sets an httpOnly refresh cookie (the web already has BFF routes under `app/api`).
- **Dependencies:** ACC-005
- **Complexity:** MEDIUM
- **Regression risk:** MEDIUM
- **Confidence:** CONFIRMED

### ACC-020 — Notifications: preferences mostly unenforced, no client push registration, incoming-call push not implemented
- **Service/module:** Hub `chat.rs:354-410`, `policy.rs:162`; Calling `signaling/push.rs`, `main.rs:453`; web/mobile
- **Expected:** every preference category gates delivery; devices register FCM/APNs/Web Push; incoming calls ring via push.
- **Actual:**
  - Preferences hold 7 booleans. Only `direct_messages`/`group_messages` are read, by the MessageNotification policy (`policy.rs:162`).
  - `incoming_calls`, `missed_calls`, `contact_requests`, `group_invitations` and `meeting_invitations` have no consumer (`grep` shows writes and the view only).
  - Calling push uses `NoPushTransport` only. The Hub itself reports `"mobile push transport NOT_CONFIGURED"` (`devices.rs:436`).
  - Messaging has real APNs/FCM/Web Push adapters with invalid-handle disabling and retry/dead handling (`push_adapters.rs:1-30`, `push.rs:375-490`). They are certified only against local fakes.
  - Web has no service worker or `pushManager`. Mobile has no `expo-notifications`. Nothing registers a push target.
  - There is no notification centre: Hub `/inbox` is a conversation overlay, not notifications.
- **Evidence:** above.
- **API/Swagger refs:** Hub `/me/notification-preferences`, `/devices/:id/push`, `/me/notification-delivery-status`
- **Affected frontend workflow:** notifications settings, incoming call on a locked phone
- **Owner class:** SHARED_CONTRACT / EXTERNAL_DEPENDENCY
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P2 (P1 if mobile calling is in launch scope)
- **Business impact:** users miss calls and messages, and preference toggles would mislead users once exposed.
- **Recommended remedy:**
  - Wire `incoming_calls` and `missed_calls` into the Calling ring policy, and `contact_requests` into the Hub request notification.
  - Implement a Calling `PushDispatcher` that reuses Messaging's adapters (or VoIP/PushKit for iOS).
  - Add client registration (service worker + VAPID; expo-notifications).
  - Hide unenforced toggles until they are wired.
- **Dependencies:** ACC-009, provider accounts (Apple/Google)
- **Complexity:** HIGH
- **Regression risk:** LOW
- **Confidence:** CONFIRMED

### ACC-021 — Minimal profile: username only; no avatar, no separate display name; inconsistent validation
- **Service/module:** `handlers/users.rs`, `dto/users.rs:19-26`
- **Expected:** display name, avatar upload, and possibly locale and timezone.
- **Actual:**
  - PATCH accepts `username` only.
  - Length 3–50 is validated before trimming, and there is no regex (registration enforces `^[A-Za-z0-9_]+$`, `dto/auth.rs:27-31`). `"  ab "` becomes `"ab"`, and control characters are allowed.
  - Control characters make Hub activation fail with a validation error (`profiles.rs:36-41`).
  - Avatar: Account sends `avatar_reference: None` (`meeting communications.rs:283`). No upload API exists.
- **Evidence:** above.
- **API/Swagger refs:** PATCH `/api/v1/users/me`
- **Affected frontend workflow:** Settings → Profile
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P2
- **Business impact:** weak identity presentation in contacts and calls.
- **Recommended remedy:** add `display_name` and `avatar_object_key` columns (additive). Avatar upload through `packages/object-storage` signed URLs. Apply the same validator as registration.
- **Dependencies:** –
- **Complexity:** MEDIUM
- **Regression risk:** LOW
- **Confidence:** CONFIRMED

### ACC-022 — User status string contract mismatch
- **Service/module:** `handlers/auth.rs:118-123,284-288`, `users.rs`, `admin.rs:56-63`
- **Expected:** a single enum string across endpoints and docs.
- **Actual:**
  - Login, register, OAuth and `/users/me` return `format!("{:?}").to_lowercase()` = `"pendingverification"` (the `.replace('_',"_")` call is a no-op).
  - Admin endpoints and Swagger/OpenAPI use `"pending_verification"` (`packages/contracts/openapi/meeting-service.json:232,873,13211`).
  - Web hard-codes `"pendingverification"` (`LoginModal.tsx:72`). Mobile expects `"pending"`.
- **Evidence:** above.
- **API/Swagger refs:** RegisterResponse, UserInfo, UserMeResponse
- **Affected frontend workflow:** verify routing
- **Owner class:** SHARED_CONTRACT
- **Classification:** CONTRACT_MISMATCH
- **Severity:** P2
- **Business impact:** unverified users are misrouted (mobile), and SDK consumers break.
- **Recommended remedy:** return `pending_verification` everywhere (use `format_status`), and update the web and mobile constants in the same release. Optionally accept both values temporarily.
- **Dependencies:** ACC-012
- **Complexity:** LOW
- **Regression risk:** MEDIUM (web)
- **Confidence:** CONFIRMED

### ACC-023 — Email addresses are not normalized
- **Service/module:** `handlers/auth.rs` lookups, `migration …create_users.rs:46-51`
- **Expected:** emails stored lowercase (or in a `citext` unique column).
- **Actual:**
  - Uniqueness and lookups are case-sensitive (`Users::Column::Email.eq(&payload.email)`), while OTP, rate-limit and `verified_email` keys are lowercased (`otp.rs`, `rate_limiter.rs:29`, `actor.rs:333`).
  - `Alice@x.com` and `alice@x.com` can be separate accounts that share an OTP slot and a verified-email identity for invitations.
  - Google's lowercase email will not find a mixed-case local account (duplicate accounts).
  - Login with different casing fails.
- **Evidence:** above.
- **API/Swagger refs:** –
- **Affected frontend workflow:** login, signup
- **Owner class:** BACKEND
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P3
- **Business impact:** support load, and edge-case identity confusion.
- **Recommended remedy:** lowercase on write and read, and add a unique index on `lower(email)`. The migration must first detect existing case duplicates.
- **Dependencies:** –
- **Complexity:** LOW
- **Regression risk:** MEDIUM (duplicate data)
- **Confidence:** CONFIRMED

### ACC-024 — Calling signaling WebSocket is not closed when its endpoint is revoked
- **Service/module:** `calling/src/signaling/ws.rs:127-140`, `calling/src/communications.rs:183-200`
- **Expected:** revoking an endpoint closes its live signaling channel.
- **Actual:**
  - The WebSocket authenticates once at upgrade. The 5-second `policy_tick` checks only `calling_communication_revocations` (principal epoch), not `call_endpoints.status`.
  - HTTP uses re-check the authority SQL, and new ring offers go only to ACTIVE endpoints (`ringing.rs:268`). Practical exposure is therefore low: the channel stays open until the credential expires (up to 1 h) but receives few signals.
- **Evidence:** above.
- **API/Swagger refs:** GET `/v1/calling/signaling`
- **Affected frontend workflow:** device revoke
- **Owner class:** BACKEND
- **Classification:** IMPLEMENTED_UNVERIFIED
- **Severity:** P3
- **Business impact:** low
- **Recommended remedy:** include an endpoint status check in `policy_tick`, or publish an endpoint-revoked signal that closes the channel with 4401.
- **Dependencies:** –
- **Complexity:** LOW
- **Regression risk:** LOW
- **Confidence:** REQUIRES_VALIDATION

### ACC-025 — Dead or unsafe auth helpers and minor hygiene
- **Service/module:** meeting-service
- **Expected:** no latent bypass helpers.
- **Actual:**
  - `combined_auth_middleware` (`middleware/api_key.rs:82-100`) accepts any JWT without blacklist, status or epoch checks. It is currently unused (`grep` shows no route usage), but it is a trap for future use.
  - `check_email_verification` is unused.
  - bcrypt (`DEFAULT_COST`=12, `services/auth.rs:99-102`) silently truncates at 72 bytes, while the maximum password length is 128.
  - Logout errors after blacklisting if the DB write in `record_logout` fails (`auth.rs:422` uses `?`), so the client sees a failure although the token is revoked.
- **Evidence:** above.
- **API/Swagger refs:** –
- **Affected frontend workflow:** –
- **Owner class:** BACKEND
- **Classification:** IMPLEMENTED_UNVERIFIED
- **Severity:** P3
- **Business impact:** maintainability
- **Recommended remedy:** delete the dead helpers, cap passwords at 72 bytes or pre-hash, and make the logout response tolerant.
- **Dependencies:** –
- **Complexity:** LOW
- **Regression risk:** LOW
- **Confidence:** CONFIRMED

### ACC-026 — Deactivation needs no re-authentication and cannot be cleanly reversed
- **Service/module:** `handlers/users.rs` (`deactivate_account`), Hub `profiles.rs:66-70`, `events.rs:88-90`
- **Expected:** confirm the password or step up before deactivating, and keep a reversible path.
- **Actual:**
  - Any bearer token (including a refresh token, ACC-001, or a stale one, ACC-002) can deactivate the account. The web UI confirms with a dialog only.
  - Once the Hub marks the profile `DEACTIVATED`, both `ensure_profile` and the account events keep it deactivated forever, even after an admin sets the user back to active.
- **Evidence:** above.
- **API/Swagger refs:** POST `/api/v1/users/me/deactivate`
- **Affected frontend workflow:** Settings → Danger Zone
- **Owner class:** BACKEND / PRODUCT_DECISION
- **Classification:** PARTIALLY_IMPLEMENTED
- **Severity:** P3
- **Business impact:** sabotage via a stolen token, and support cannot restore communications.
- **Recommended remedy:** require `current_password` (or a recent `auth_time`). Define a reactivation event in the Hub (`StatusChanged` → ACTIVE allowed when sourceVersion is newer).
- **Dependencies:** ACC-011
- **Complexity:** LOW
- **Regression risk:** LOW
- **Confidence:** CONFIRMED


## Calling, enterprise calling, telephony, numbering, SMS, privacy relay


- **services/calling**: `src/routes.rs`, `src/main.rs` (runtime wiring, l.440–500), `src/communications.rs`, `src/calls/{http.rs,transitions.rs,service/lifecycle.rs,service/participation.rs}`, `src/calls/app/{http.rs,control.rs,media.rs,ringing.rs,transfer.rs,workers.rs,create.rs}`, `src/calls/app/enterprise/{queues.rs,sip.rs,voicemail.rs}`, `src/enterprise/{mod.rs,http.rs,sip_devices.rs}`, `src/signaling/{push.rs,signals.rs}`, `src/platform/limits.rs`, `.env.example`, `migration/src` (50 migrations), `tests/` (90 integration suites; looked at test names and harness of `app_ringing`, `app_races`, `app_control`, `app_media`, `call_history`, `signaling`, `enterprise_transfer`, `rtc_certification`).
- **services/telephony**, **services/numbering**, **services/sms-gateway**, **services/privacy-relay**: READMEs (status tables), `src/providers/*` listings, `numbering/src/providers/registry.rs`, `numbering/src/numbers/kyc/verifier.rs`, `telephony/src/config.rs`, `sms-gateway/src/config/mod.rs`, `privacy-relay/src/contact_vault/http.rs`, test listings.
- **services/communications**: `src/calls.rs` (Hub call launch), `src/api.rs` (contact/conversation → call routes), `src/devices.rs` (push status).
- **services/meeting-service**: `src/handlers/v2/unified_messaging.rs` (`/api/v2/messaging/me`, directory — identity used by Calling web adapter).
- **infra**: `docker-compose.prod.yml`, `docker-compose.yml`, `infra/deployment/README.md`, `infra/calling/production/production.py` (grep), `infra/telephony/README.md`, `infra/api-gateway/route-manifest.json` (grep).
- **CI**: `.github/workflows/{calling,calling-rtc-certification,enterprise-calling-certification,telephony-sip-certification,communications-e2e,cfmeet-calling-production}.yml`; latest run conclusions via `gh run list` (read-only).
- **docs**: `docs/operations/calling-backend-production-2026-11-03.md`, `calling-production-2026-11-02.md`, `cfmeet-sections-8-23-final-closure.md` (grep); ADR list 0003, 0006–0044 (titles; 0023 read); PDR PDF §2.1 and Appendix B.1 (scope boundaries).
- **apps/web**: `src/app/api/calling/[...path]/route.ts`, `src/app/api/calling-signaling/[...path]/route.ts`, `src/lib/calling/{server.ts,origin.mjs,resource-routes.ts}`, `src/services/calling/*.ts`, `src/hooks/calling/useCallingDevice.ts`, `src/components/calling/{CallsContent,CallMedia,CallingSettingsPage,CallingResourcePage}.tsx`, `src/app/calling/**`, `tests/calling-adapter.test.mjs`, `tests/calling.browser.mjs`.
- **apps/mobile**: grep for calling / livekit / voip / callkit — no calling code exists.

Grep commands used (representative): `grep -rn '\.route(' services/calling/src`, `grep -rn "AllParticipationEnded\|EndedEvidence::" services/calling/src`, `grep -rn "NoPushTransport" services/calling/src`, `grep -rliE 'calling|livekit|voip|callkit' apps/mobile`, `grep -n "initiatorPrincipalId !== identity" apps/web/src/app/api/calling/[...path]/route.ts`, `grep -rn "CallRecordingControls\|AbuseReportDialog" apps/web/src`.

### Production configuration facts (evidence)

- `docs/operations/calling-backend-production-2026-11-03.md` l.11–26: `CALLING_NEW_CALLS_ENABLED=true`, `CALLING_APP_CALLING_ENABLED=true`, `ENTERPRISE_CALLING_ENABLED=true`, `VOICEMAIL_ENABLED=true`, `CALL_RECORDING_ENABLED=true`, media `livekit`, capture `livekit-egress`; `CALLING_PUBLIC_INBOUND_ENABLED`, `CALLING_PUBLIC_OUTBOUND_ENABLED`, `CALLING_CALLBACK_RELAY_ENABLED`, `CALLING_SYNTHETIC_INTERCONNECT_ENABLED` = **false**.
- `.github/workflows/cfmeet-calling-production.yml` l.100: the production release ships only `platform-access meeting-service calling messaging communications` (plus security-scan-gateway). **Telephony, Numbering, SMS Gateway and Privacy Relay are not deployed to production** (`grep -ciE 'telephony|numbering|sms-gateway|privacy-relay' infra/calling/production/production.py` → 0).
- `docker-compose.prod.yml` contains only postgres/redis/livekit/meeting-service/nginx/certbot — Calling production runs from the separate `cfmeet-core` VPS flow above, not this compose file.
- Defaults in `services/calling/.env.example` l.136–175: enterprise/voicemail/recording default `false`; public PSTN flags default `false`. Telephony README: `TELEPHONY_ENABLED`, `PUBLIC_*_CALLING_ENABLED`, `CALLBACK_RELAY_ENABLED` default `false`.
- PDR v1.0 §2.1 explicitly excludes "Public PSTN/telephony calling and SMS", "Number provisioning / DCNs, Privacy Relay" from the Phase 2 frontend; Appendix B.1 lists Calling as "separate product modules".
- Latest CI (via `gh run list`, 2026-11-06): `calling.yml`, `calling-rtc-certification.yml` (real LiveKit, `CALLING_RTC_CERT_REQUIRED=1`), `enterprise-calling-certification.yml` (`ENTERPRISE_SIP_CERT_REQUIRED=1`), `communications-e2e.yml` (`CFMEET_REQUIRE_E2E=1`, `CFMEET_REQUIRE_VOICE_E2E=1`), `telephony*.yml`, `numbering.yml`, `sms-gateway.yml`, `privacy-relay.yml` → **success** on main and staging. Calling integration tests run against real PostgreSQL 16 (`calling.yml` l.105–158) with a `FakeMediaProvider`; the RTC certification uses a real LiveKit server and real WebRTC clients.



### CAL-001 — Connected calls never terminate when the parties leave; abandoned calls live forever and their media room is re-created
- Service/module: services/calling — `calls/app/control.rs`, `calls/app/media.rs`, `calls/service/lifecycle.rs`, `calls/transitions.rs`
- Expected: In a 1:1 (or group) call, when all but one participant has left/hung up — or all participants disconnected beyond a grace period — the Call moves to ENDING → ENDED, media is torn down, usage is finalized, and participants become free for new calls/queue offers.
- Actual: The only path from CONNECTED/HELD to ENDING is an explicit `END` command (`CallAction::RequestEnd`, `control.rs` l.162–268). Session removal (`remove_session`, `control.rs` l.813–879) and provider `ParticipantLeft`/`ParticipantConnectionAborted` events (`media.rs` l.407–424 → `record_absent` l.651–686) only mark the session ENDED/DISCONNECTED; nothing checks whether participation is exhausted. `EndedEvidence::AllParticipationEnded` exists (`lifecycle.rs` l.898–918) but is only a verifier for ENDING→ENDED; `grep -rn "EndedEvidence::" src` shows it is never produced by any code path except `CleanupConfirmed`. Worse, when LiveKit closes the empty room (`media_empty_timeout`) the `ResourceFinished` event on a non-terminal call triggers `schedule_reconcile_now`, and `reconcile_call` re-creates the room (`media.rs` l.431–443, l.800–815: "An active Call lost its resource: reconciliation re-ensures it"). There is no max-duration or idle reaper for app calls (`workers.rs` only expires invitations, stalled setups, transfers, IVR, voicemail, recording). Consequences: (a) callee hangs up → caller still sees "Connected" with a running timer; (b) both close their tabs → call is CONNECTED indefinitely; (c) such calls count as active in `enforce_concurrent_call_limit` (`lifecycle.rs` l.290–380, "Only non-terminal Calls count"); (d) a queue agent who is a participant of such a call is never offered queue calls again (`calls/app/enterprise/queues.rs` l.948–980: agent must not be on an `ANSWERED/CONNECTED/HELD/TRANSFERRING/ENDING` call); (e) history duration grows without bound.
- Evidence: commands `grep -rn "AllParticipationEnded\|ReconciliationTimeout" services/calling/src` (only transitions.rs/kinds.rs/lifecycle.rs verifier), `grep -rn -iE "auto.?end|last participant" services/calling/src` (nothing relevant).
- API/Swagger refs: POST `/v1/calls/{id}/end`, POST `/v1/calls/{id}/sessions/{sid}/remove`.
- Affected frontend workflow: Journey E "end", Journey G queue agent availability; `CallsContent.tsx` "Leave call"/"Disconnect this device".
- Owner class: BACKEND (with PRODUCT_DECISION on the exact rule)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1
- Business impact: Ghost calls, wrong call durations, agents silently starved in queues, concurrency limit exhaustion for the shared web credential (see CAL-009) → calling outage for everyone.
- Recommended remedy: Reuse existing machinery: when a session/participant ends (remove_session, record_absent after grace, transfer retirement) evaluate "fewer than two participants with live participation" and run the same path as `end` with a system command (`system_cmd`), producing `AllParticipationEnded`/`CleanupConfirmed` evidence. Add an idle reaper in `workers.rs` (no connected interval for N minutes → END) and a configurable max call duration. Do not re-ensure the media room for a CONNECTED call whose sessions are all disconnected past the grace window. No new endpoint; no schema change required (existing `call_session_intervals`, `ending_at`).
- Dependencies: CAL-002 (web control rules).
- Complexity: MEDIUM
- Regression risk: MEDIUM (transfer flow temporarily has one participant; enterprise IVR/queue/voicemail calls have a single human participant plus server legs — rule must count legs/captures, as `verify_ended` already does).
- Confidence: CONFIRMED

### CAL-002 — Web: only the call initiator can End/Hold/Resume; after a transfer nobody can end the call from the UI
- Service/module: apps/web `src/app/api/calling/[...path]/route.ts`, `src/components/calling/CallsContent.tsx`
- Expected: Either party of a 1:1 call can hang up; after a transfer the remaining parties can end the call.
- Actual: Adapter l.222: `if (["cancel","end","hold","resume"].includes(path[3]) && call.initiatorPrincipalId !== identity.principalId) allowed = false;` UI l.228 also only renders these buttons when `call.data?.initiatorPrincipalId === principalId`. The recipient gets only "Leave call" (session removal), which per CAL-001 does not end the call. When the initiator transfers the call (`transfer.rs`: transferor is retired on target join), the two remaining parties are both non-initiators → no one can end it via the web. The unit test `tests/calling-adapter.test.mjs` l.173–177 enshrines "recipient cannot control the whole call but can leave their session".
- Evidence: as above; backend `end` has no participant-role restriction (`control.rs` l.162–200) — the restriction is web-only.
- API/Swagger refs: POST `/v1/calls/{id}/end|hold|resume`.
- Affected frontend workflow: Journey E end/hold; Journey G transfer.
- Owner class: FRONTEND (+ PRODUCT_DECISION)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1
- Business impact: Callee cannot hang up a call; caller left talking to nobody; transferred calls are unterminable from the UI.
- Recommended remedy: Allow `end` for any active (non-ended) participant in the adapter (backend already authorizes at application scope), or—preferred together with CAL-001—make "Leave call" end a 2-party call server-side. Allow `hold/resume` for any active participant if the product wants symmetrical hold. Update the adapter test.
- Dependencies: CAL-001
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-003 — No incoming-call push (APNs VoIP/PushKit, FCM, Web Push) and no mobile calling client
- Service/module: services/calling `signaling/push.rs`, `main.rs`; services/communications `devices.rs`; apps/mobile
- Expected: Incoming calls wake mobile devices (APNs VoIP + CallKit, FCM high-priority + ConnectionService) and background browsers (Web Push); mobile app can place/answer calls.
- Actual: `push.rs` l.1–10 "No mobile push vendor is integrated"; `NoPushTransport::dispatch` only logs (l.24–33); `main.rs` l.453 wires `push: Arc::new(NoPushTransport)`. The payload builder exists (`signals.rs` l.184–203) and the contract `packages/contracts/calling/push`. Communications Hub reports `"callRinging":{"owner":"CALLING","transport":"Calling signaling WebSocket; mobile push transport NOT_CONFIGURED"}` (`communications/src/devices.rs` l.435). `grep -rliE 'calling|livekit|voip|callkeep|callkit' apps/mobile` → only meeting files; `apps/mobile/package.json` has no LiveKit/VoIP/CallKeep dependency.
- API/Swagger refs: endpoint `pushTargetRef` field on POST `/v1/calling/endpoints`.
- Affected frontend workflow: Journey E "ring" on any device not currently showing `/calling` with an open WebSocket.
- Owner class: BACKEND + FRONTEND (mobile) + EXTERNAL_DEPENDENCY (Apple/Google push credentials)
- Classification: MISSING
- Severity: P1
- Business impact: Calls are only receivable while a browser tab sits on the Calls page; no phone-like experience.
- Recommended remedy: Implement a `PushDispatcher` that hands the existing payload to Messaging's push adapters (`services/messaging/src/realtime/push_adapters.rs` already has APNS/FCM/WEB_PUSH plumbing) via service-auth, keyed by `pushTargetRef`; add APNs VoIP-specific topic. Mobile: add LiveKit RN SDK + CallKit/ConnectionService. Needs Apple VoIP certificate / FCM server key (EXTERNAL).
- Dependencies: Messaging push credentials; mobile release pipeline.
- Complexity: HIGH
- Regression risk: LOW (additive)
- Confidence: CONFIRMED

### CAL-004 — Web incoming-call experience is page-bound and manual (no auto device registration, no global ring, no ringtone, no caller identity)
- Service/module: apps/web `hooks/calling/useCallingDevice.ts`, `components/calling/CallsContent.tsx`
- Expected: Once signed in, the browser is reachable app-wide; an incoming call shows caller name/avatar with ringtone wherever the user is.
- Actual: `useCallingDevice` registers only when the user presses "Enable this browser" (`CallsContent.tsx` l.162; `register` l.19–33); the hook is mounted only by `CallsContent` and `CallingDevicesPage` (grep), i.e. only on `/calling*` and `/dashboard/calls`. After reload the endpoint state is lost until the button is pressed again. The incoming card says "Someone is calling you" (l.166) — `RecoveredInvitation` has no caller field (`types/calling/invitation.ts`), although the backend push payload has `caller.displayName`. No audio/ringtone or `Notification` usage (`grep -rn "ringtone\|Notification(" apps/web/src` → none).
- API/Swagger refs: GET `/v1/calling/invitations`, WS `/v1/calling/signaling`.
- Affected frontend workflow: Journey E ring/answer.
- Owner class: FRONTEND (+ SHARED_CONTRACT for caller display in recovered invitations)
- Classification: FRONTEND_ONLY_GAP
- Severity: P1
- Business impact: Users miss most calls; cannot decide whether to answer.
- Recommended remedy: Mount the device hook in the authenticated app shell, auto-register with the stored `endpointKey`, render a global incoming-call toast with ringtone; extend the recovered-invitation contract with a safe `caller.displayName` (data already computed for push).
- Dependencies: CAL-003 for background delivery.
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-005 — Recording consent/controls and abuse reporting are unreachable from the web (components unused, adapter returns 404)
- Service/module: apps/web `components/calling/CallRecordingControls.tsx`, `AbuseReportDialog.tsx`, `services/calling/{recording,abuse}.service.ts`, `app/api/calling/[...path]/route.ts`
- Expected: Participants see recording notice, give/decline consent, start/stop (when policy allows), and can report abusive calls.
- Actual: `grep -rn "CallRecordingControls\|AbuseReportDialog" apps/web/src` → only their own files (never imported). Even if mounted, the adapter's `v1/calls/{id}/…` branch (l.214–233) only permits `actions`, `cancel|end|hold|resume|transfer|accept|decline`, `ivr/input`, `sessions/{sid}/(media-credentials|remove)`; `recording-authorizations`, `recording-consent`, `recordings`, `recordings/{rid}/stop`, `abuse-reports`, and `v1/abuse-reports` fall through to `404 "Calling operation not found"` (l.240). Backend routes exist and are tested (`enterprise_recording.rs`, rtc `rtc_recording_through_egress`, `abuse_reports.rs`).
- API/Swagger refs: GET `/v1/calls/{id}/recording-authorizations`, POST `/v1/calls/{id}/recording-consent`, GET/POST `/v1/calls/{id}/recordings`, POST `/v1/calls/{id}/recordings/{rid}/stop`, POST `/v1/calls/{id}/abuse-reports`.
- Affected frontend workflow: in-call recording consent (legal requirement where recording is enabled — production has `CALL_RECORDING_ENABLED=true`), abuse reporting.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P1
- Business impact: Recording policies requiring consent cannot be satisfied by web users (recording starts are refused without consent — backend safe, but feature unusable); no user safety reporting.
- Recommended remedy: Add participant-bound adapter rules (consent with the caller's own participant id injected, as done for transfer), mount `CallRecordingControls` in the call detail/media panel and `AbuseReportDialog` in history. No backend change.
- Dependencies: none
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-006 — End-user voicemail and forwarding settings require the platform-admin allowlist; admins get a cross-user Administrator view
- Service/module: apps/web `app/api/calling/[...path]/route.ts` l.8–33, l.61–70, l.237–239; `components/calling/CallingSettingsPage.tsx` l.74–75; backend `enterprise/http.rs` l.1789–1844
- Expected: Each user can read/play/delete their own voicemail and manage their own forwarding; administrators manage others under audit.
- Actual: `voicemail/mailboxes`, `voicemail/messages*`, `forwarding-rules*` are in `enterpriseRules`, so `requireResourceAdministrator` demands the principal be in env `CALLING_ADMIN_PRINCIPAL_IDS` (403 otherwise). The adapter never injects `principalId`; backend `requester()` (l.1789–1802) then resolves `VoicemailRequester::Administrator` when the shared credential holds `enterprise.voicemail.manage`, so an allow-listed admin can list/play any mailbox in the scope. The Settings page exposes "Voicemail" and "Forwarding" tabs to all users (l.74–75) which will 403 for non-admins.
- API/Swagger refs: GET `/v1/enterprise/voicemail/messages?mailboxId=&principalId=`, POST `…/media-access`, PUT `…/read-state`, `/v1/enterprise/forwarding-rules`.
- Affected frontend workflow: voicemail playback journey; personal forwarding.
- Owner class: FRONTEND (+ SHARED_CONTRACT: mailbox discovery by owner)
- Classification: FRONTEND_ONLY_GAP
- Severity: P1
- Business impact: Voicemail is deposited in production but ordinary users cannot retrieve it via the app.
- Recommended remedy: Add user-scoped adapter rules that always set `principalId=<identity>` on voicemail reads/media-access/read-state/delete and on forwarding rules owned by the user (backend already supports `VoicemailRequester::Principal`). Keep admin rules separate. Consider a "my mailboxes" filter (`ownerPrincipalId`) if not already supported.
- Dependencies: CAL-007
- Complexity: LOW–MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-007 — Web calling runs every CF Meet user through one platform credential/scope: enterprise calling is not multi-organization and admin actions lose the human actor
- Service/module: apps/web `lib/calling/server.ts` l.31–100; services/calling `enterprise/mod.rs` l.195–214
- Expected: Each customer organization has its own enterprise PBX namespace (extensions, ring groups, queues, IVR, policies, recordings) administered by its own org admins, with audit naming the human administrator.
- Actual: `callingIdentity` requires `identity.tenantId/applicationId/environmentId` (from `/api/v2/messaging/me`) to equal env `CALLING_TENANT_ID/APPLICATION_ID/ENVIRONMENT_ID` (l.83–88) and all upstream calls use one `CALLING_PLATFORM_CREDENTIAL` (`callingFetch` l.96–104). Hence every CF Meet organization shares a single enterprise scope: extensions, ring groups, queues and voicemail boxes are global; any user can dial any extension; admin rights are an env allowlist of principal ids (`requireResourceAdministrator` l.61–70), not organization roles. `AdminActor::of` records only `service_account_id`/`credential_id` of the shared web credential (`enterprise/mod.rs` l.204–214), so `/v1/enterprise/audit` cannot tell which human changed a policy or played a recording. Personal-call policy (`require_personal_create`, `communications.rs` l.69–101) applies only if the Communications scope env equals this web scope — not verifiable from the repo.
- API/Swagger refs: all `/v1/enterprise/*`; GET `/v1/enterprise/audit`.
- Affected frontend workflow: Journey G (organization admin configuring queue/ring group; admin review).
- Owner class: SHARED_CONTRACT / PRODUCT_DECISION (architecture)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1
- Business impact: Cannot sell enterprise calling to more than one organization without cross-org leakage of directory/extension namespace and admin access; weak non-repudiation for recording/voicemail access.
- Recommended remedy: Map CF Meet organizations → Calling tenants/applications (Platform Access already models tenant/application/environment), mint per-organization credentials (or delegated platform context per request carrying the acting principal — `internal_auth::delegation` exists), and derive enterprise admin from org role. Add an `actingPrincipalId` to `AdminActor` from a verified delegation header. Data migration: none for calling tables (already scoped by tenant/app/env); operational migration of existing shared-scope data.
- Dependencies: Platform Access / meeting-service org model.
- Complexity: HIGH
- Regression risk: MEDIUM
- Confidence: CONFIRMED (code); production env values REQUIRES_VALIDATION

### CAL-008 — `CALLING_DEV_MODE=true` disables authentication in the web adapter with no production guard
- Service/module: apps/web `lib/calling/server.ts` l.22–46; `app/api/calling/[...path]/route.ts` l.90–160
- Expected: Dev shortcuts cannot be enabled in a production build, or at least never combine with a real platform credential.
- Actual: When `CALLING_DEV_MODE === "true"`, `callingIdentity` returns a fixed identity (`CALLING_DEV_PRINCIPAL_ID` or `dev-user-001`) for any `Authorization: Bearer <anything>` without verification and skips the scope check; only a handful of GET routes are mocked (`devModeResponse`), every other route continues to `callingFetch` with the real `CALLING_PLATFORM_CREDENTIAL`. There is no `NODE_ENV === "production"` refusal (contrast `callingFetch` l.98–100 which does guard production).
- API/Swagger refs: all adapter routes.
- Affected frontend workflow: all calling.
- Owner class: FRONTEND
- Classification: IMPLEMENTED_UNVERIFIED (latent misconfiguration risk)
- Severity: P1 (security hardening; exploitable only if the flag is set in a deployed env)
- Business impact: A single env mistake turns the calling adapter into an unauthenticated proxy acting as an arbitrary principal.
- Recommended remedy: Refuse dev mode when `NODE_ENV==="production"` or when `CALLING_PLATFORM_CREDENTIAL` is set; add a test.
- Dependencies: none
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED (code); deployed value REQUIRES_VALIDATION

### CAL-009 — Shared credential concentrates per-credential limits and serializes call creation platform-wide
- Service/module: services/calling `platform/limits.rs` l.40–56; `calls/service/lifecycle.rs` l.290–400; apps/web adapter
- Expected: Limits sized per customer/organization; call creation throughput scales.
- Actual: Defaults `max_concurrent_communications: 50`, `requests_per_minute: 600` per effective credential (`limits.rs` l.48–53). All web users share one credential (CAL-007), and the adapter multiplies upstream calls (`ownCall`, `ownEndpoints`, block-ownership lookups precede many commands). Call creation always takes a transaction advisory lock on `cfmeet.calling.active-calls:<application_id>` (`lifecycle.rs` l.338–342), serializing all CF Meet call creations. Combined with CAL-001 leaks, the 50-call ceiling will eventually refuse all new calls.
- API/Swagger refs: GET `/v1/calling/limits`.
- Affected frontend workflow: Journey E create.
- Owner class: INFRASTRUCTURE / BACKEND
- Classification: IMPLEMENTED_UNVERIFIED
- Severity: P2
- Business impact: Platform-wide refusals (`limit.exceeded`) at modest scale.
- Recommended remedy: Configure explicit application/credential limits for the web credential (operator `platform_admin`), fix CAL-001, and move to per-org credentials (CAL-007). Load-test creation contention (`control_plane_load.rs` exists — extend with single-application contention).
- Dependencies: CAL-001, CAL-007
- Complexity: LOW (config) / HIGH (architecture)
- Regression risk: LOW
- Confidence: CONFIRMED (code defaults); production limit rows REQUIRES_VALIDATION

### CAL-010 — PSTN telephony is software-complete against simulators but has no carrier, is not deployed, and is flag-disabled in production
- Service/module: services/telephony, infra/telephony, services/calling `calls/app/public/*`, `telephony/*`
- Expected: Inbound/outbound PSTN, phone-to-phone relay, callback relay, external forwarding.
- Actual: Telephony README l.27–35: core, Asterisk/ARI edge ("certified against SIPp simulators"), provider-neutral contract with SIP-trunk and deterministic adapters, failover/reconciliation **Implemented**; "Real lawful telecom interconnect certification (Nigeria first) — **BLOCKED.** It needs a contracted carrier / SIP trunk, a public number and staging credentials"; `tests/real_interconnect_certification.rs` prints NOT RUN. Production: telephony not in the release set (`cfmeet-calling-production.yml` l.100), Calling flags `CALLING_PUBLIC_INBOUND/OUTBOUND/CALLBACK_RELAY_ENABLED=false` (ops doc l.21–25). Synthetic interconnect refused outside local/test (`telephony/src/config.rs` l.171–177).
- API/Swagger refs: Telephony internal v1 (`packages/contracts/telephony/internal/v1.openapi.json`); Calling `/internal/v1/telephony/{events,inbound-calls,dcn-inbound-calls,enterprise-calls,enterprise-keypad,captures}`.
- Affected frontend workflow: none in Phase 2 (PDR §2.1 excludes PSTN).
- Owner class: EXTERNAL_DEPENDENCY (carrier contract, DIDs, trunk credentials) + INFRASTRUCTURE (deploy Telephony + Asterisk edge with public SIP/RTP)
- Classification: EXTERNAL_DEPENDENCY
- Severity: P2 (out of current product scope)
- Business impact: No phone-number calling until a carrier is contracted.
- Recommended remedy: Procure SIP trunk; deploy Telephony + edge via GitHub Actions; run `real_interconnect_certification`; then enable flags one at a time.
- Dependencies: CAL-011 (numbers), legal/regulatory (NCC).
- Complexity: HIGH
- Regression risk: LOW (flags isolate)
- Confidence: CONFIRMED

### CAL-011 — Numbering has only the deterministic synthetic provider; no real number-provider adapter exists
- Service/module: services/numbering `src/providers/{deterministic,registry}.rs`, `numbers/kyc/verifier.rs`
- Expected: Number search/reserve/provision/route/port against a real provider; KYC verification.
- Actual: README l.130–132 "Real carrier and portability adapters are not integrated … deterministic synthetic provider (ADR 0036); no telecom carrier is contacted." `registry.rs` l.3, l.83–86 refuses synthetic providers in staging/production → the service cannot provision any number in production. KYC: `DeterministicKycVerifier` + `ManualKycVerifier` (manual human review is a real path). Not deployed to production. `NUMBERING_PORTING_ENABLED=false`.
- API/Swagger refs: `packages/contracts/bundled/public/numbering/v1.openapi.json`; gateway manifest has 52 numbering routes.
- Affected frontend workflow: none (PDR out of scope).
- Owner class: EXTERNAL_DEPENDENCY (provider contract) + BACKEND (adapter implementation once a provider is chosen — the contract suite `providers/certification.rs` is ready)
- Classification: EXTERNAL_DEPENDENCY
- Severity: P2
- Business impact: No real DCNs; relay numbers and PSTN presentation impossible.
- Recommended remedy: Choose provider; implement adapter against the existing provider contract and run the certification suite; deploy.
- Dependencies: CAL-010
- Complexity: HIGH
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-012 — SMS Gateway has only a deterministic (test) provider, refused in staging/production
- Service/module: services/sms-gateway `src/providers/`, `src/config/mod.rs` l.18–20, l.107–113
- Expected: Real SMS send/receive/delivery receipts.
- Actual: README l.132–137: "The only adapter in this build is `deterministic` … refused in staging and production." With no provider, sends are refused `PROVIDER_UNAVAILABLE`. Not deployed in production.
- API/Swagger refs: `packages/contracts/sms-gateway/http`.
- Affected frontend workflow: none.
- Owner class: EXTERNAL_DEPENDENCY + BACKEND (adapter)
- Classification: EXTERNAL_DEPENDENCY
- Severity: P3 (out of scope)
- Business impact: No SMS.
- Recommended remedy: Implement aggregator adapter against `providers::contract::run`; requires numbers (CAL-011).
- Dependencies: CAL-011
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-013 — Privacy Relay not deployed; web admin pages proxy `/v1/privacy-relay/contacts*` to Calling, which does not serve them
- Service/module: services/privacy-relay; services/calling `privacy_relay/http.rs`; apps/web `route.ts` l.49–52, `lib/calling/resource-routes.ts` l.46–57
- Expected: Protected-contact management reachable through the public Calling ingress (or not offered).
- Actual: Calling exposes only `/v1/privacy-relay/{contexts,sessions,contact-access-grants}…` (route enumeration). `/v1/privacy-relay/contacts…` exists only on the Privacy Relay service (`privacy-relay/src/contact_vault/http.rs` l.43–53), whose listener is cluster-internal (deployment README). The web adapter allowlists `v1/privacy-relay/contacts` and forwards to the Calling base URL → upstream 404. Additionally Privacy Relay is not in the production release and temporary relay numbers require Telephony + carrier numbers.
- API/Swagger refs: `/api-docs/privacy-relay/openapi.json` served by Calling.
- Affected frontend workflow: `/calling/privacy/contacts` admin page.
- Owner class: SHARED_CONTRACT (web ↔ Calling) + EXTERNAL_DEPENDENCY
- Classification: CONTRACT_MISMATCH (web) / EXTERNAL_DEPENDENCY (feature)
- Severity: P2
- Business impact: Broken admin page; feature unusable in production (it is out of PDR scope).
- Recommended remedy: Remove the contacts pages/rules from the web until Calling proxies the Contact Vault ingress (or add a Calling ingress route), and hide privacy-relay admin pages while the service is not deployed.
- Dependencies: CAL-010/011 for relay numbers.
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-014 — Web does not consume group calling or Communications Hub chat/contact-to-call; it runs a parallel launch path
- Service/module: apps/web `CallsContent.tsx` l.172–186; services/communications `calls.rs`, `api.rs`
- Expected: Start a call from a conversation or contact card; start group calls; unified history/timeline call cards.
- Actual: Web sends exactly one `targetPrincipalIds` entry and discovers people only via `/api/v2/messaging/directory` (verified-organization colleagues; empty for users without an org). The Hub's production-verified routes `POST …/contacts/{id}/calls`, `POST …/conversations/{id}/calls`, `GET …/calls/history`, call context/timeline cards (`api.rs` l.396–437) are not referenced anywhere in `apps/web/src`. Two independent launch paths exist (web adapter with shared credential vs Hub with launch credential), with different rate limiting (`LAUNCH_LIMIT_PER_HOUR=120` only in Hub).
- API/Swagger refs: Hub v2 communications routes; POST `/v1/calls`.
- Affected frontend workflow: Journey E "discover contact"; chat-to-call.
- Owner class: FRONTEND (+ PRODUCT_DECISION on the single launch path)
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: Contacts outside one's organization cannot be called; no call button in chats; no group calls in UI.
- Recommended remedy: Use the Hub launch routes from chat/contact UIs (they already enforce communications policy and keep call cards); allow multi-select targets for group calls; converge on one launch path.
- Dependencies: CAL-004 (device registration supplies `endpointId` required by Hub launch)
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-015 — Web media reconnect is manual after LiveKit gives up; status not surfaced
- Service/module: apps/web `components/calling/CallMedia.tsx` l.18–29; `CallsContent.tsx` l.232
- Expected: Visible "Reconnecting…" state and automatic rejoin with a fresh media credential (backend supports same-generation reconnect).
- Actual: `onDisconnected={onLeave}` sets media to `null`; the user must find the "Join audio" button (labelled audio even for video calls). No reconnecting indicator beyond LiveKit internals.
- API/Swagger refs: POST `/v1/calling/sessions/{sid}/media-credentials`.
- Affected frontend workflow: Journey E reconnect.
- Owner class: FRONTEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Network blips drop users from calls.
- Recommended remedy: On disconnect while call non-terminal, auto-request a new credential with bounded backoff; show reconnect UI.
- Dependencies: none
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-016 — Enterprise SIP desk phones depend on Telephony + Asterisk edge that are not deployed
- Service/module: services/calling `calls/app/enterprise/sip.rs` l.1–7; `enterprise/sip_devices.rs`; services/telephony
- Expected: Enterprise customers can register desk phones.
- Actual: Calling commands SIP legs only through the Telephony command outbox; Telephony/edge are not part of the production release. Admin API (`/v1/enterprise/sip-devices`) is live and will accept device bindings that can never place calls. CI certifies with real Asterisk + SIPp (`enterprise_sip_certification.rs`, `ENTERPRISE_SIP_CERT_REQUIRED=1`).
- API/Swagger refs: `/v1/enterprise/sip-devices[/:id]`.
- Affected frontend workflow: admin SIP device page.
- Owner class: INFRASTRUCTURE
- Classification: COMPLETE_VERIFIED (CI) / not deployed
- Severity: P2
- Business impact: Feature advertised in admin UI but non-functional in production.
- Recommended remedy: Deploy Telephony + private SIP edge (no carrier needed for internal SIP), or hide the SIP device admin page / return a capability-unavailable error until deployed.
- Dependencies: none (no carrier required for internal SIP)
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-017 — Admin review gaps: no recording discovery, no org-wide call list, raw-JSON enterprise editors
- Service/module: apps/web adapter, `CallingResourcePage.tsx`, `[...segments]/page.tsx`; services/calling enterprise HTTP
- Expected: Admin can browse recordings and calls of their organization, view queue dashboards, and edit IVR/queues with structured forms.
- Actual: Backend has no `GET /v1/enterprise/recordings` listing (only `/:id`); per-call listing `GET /v1/calls/{id}/recordings` is blocked by the adapter; the web offers "recordings/lookup" by ID only. `GET /v1/calls` always gets `principalId=<self>` injected (adapter l.199–201), so admins cannot review other users' calls. All enterprise resources are edited via JSON textareas (`CallingResourcePage.tsx` l.130, 180, 240).
- API/Swagger refs: `/v1/enterprise/recordings/{id}`, `/v1/calls/{id}/recordings`, `/v1/calls`.
- Affected frontend workflow: Journey G admin review.
- Owner class: BACKEND (recordings list) + FRONTEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Compliance review of recordings and call activity is impractical.
- Recommended remedy: Add a paginated `GET /v1/enterprise/recordings` (filters: callId, principal, date, state) — justified because recordings are otherwise only discoverable per call; admin-scoped call listing via adapter for org admins (after CAL-007); structured forms for IVR/queues.
- Dependencies: CAL-007
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### CAL-018 — Declining on one device does not decline the call for the user; caller keeps ringing until TTL
- Service/module: services/calling `ringing.rs`; ADR 0008
- Expected (product question): "Decline" usually rejects the call for the person (all devices) and tells the caller "declined/busy".
- Actual: By design one device's decline only ends that offer (`app_ringing.rs::one_device_decline_declines_only_that_offer`; ops doc l.47–53 "A decline on one device must not suppress other live offers").
- Owner class: PRODUCT_DECISION
- Classification: DEFERRED_BY_OWNER (deliberate)
- Severity: P3
- Recommended remedy: Optionally add `scope: PARTICIPANT` to decline to end all of that participant's offers.
- Complexity: LOW — Regression risk: LOW — Confidence: CONFIRMED
- Business impact / Dependencies / API refs: POST `/v1/calling/invitations/{id}/decline`; none.
- Affected frontend workflow: Journey E decline.
- Evidence: as cited.

### CAL-019 — Attended (consultative) transfer not implemented
- Service/module: services/calling `calls/app/transfer.rs` l.1–6
- Expected: Optional consult-then-transfer for enterprise agents.
- Actual: Only ring-target-then-retire-transferor transfer; `grep -rn -iE "attended|consult" docs services/calling/src` → no specification.
- Owner class: PRODUCT_DECISION — Classification: NOT_REQUIRED (unspecified) — Severity: P3
- Evidence: grep above. API refs: POST `/v1/calls/{id}/transfer`. Affected workflow: Journey G transfer.
- Business impact: Contact-centre agents cannot consult before transferring.
- Recommended remedy: Specify in an ADR if needed; implement as hold + secondary call + merge.
- Dependencies: none — Complexity: HIGH — Regression risk: MEDIUM — Confidence: CONFIRMED

### CAL-020 — Signaling adapter lacks the production HTTPS guard
- Service/module: apps/web `app/api/calling-signaling/[...path]/route.ts`
- Expected: Same fail-closed rule as `callingFetch` (`server.ts` l.98–100).
- Actual: `const base = process.env.CALLING_API_URL || process.env.NEXT_PUBLIC_CALLING_API_URL || "http://localhost:8090";` — no production/https check; signaling bearer tokens could be sent over plain HTTP on misconfiguration.
- Owner class: FRONTEND — Classification: IMPLEMENTED_UNVERIFIED — Severity: P3
- Evidence: file as cited. API refs: `/v1/calling/invitations*`, `/v1/calling/history`, `/v1/calling/sessions/*/media-credentials`.
- Affected frontend workflow: answer/decline, media credential.
- Business impact: Low (misconfiguration only).
- Recommended remedy: Reuse `callingFetch`-style guard.
- Dependencies: none — Complexity: LOW — Regression risk: LOW — Confidence: CONFIRMED

### CAL-021 — Voicemail/recording artifacts are not malware-scanned (acceptable today)
- Service/module: services/calling voicemail/recording capture; `packages/object-storage`
- Expected (brief): voicemail malware scan.
- Actual: No scanning (`grep -rn -iE "malware|scan" services/calling/src/calls/app/enterprise/{voicemail,recording,capture}.rs` → none). Artifacts are produced server-side by LiveKit Egress / Asterisk capture; no user upload path exists; access is via ≤900 s signed URLs with SSE-S3 (ADR 0023; ops doc).
- Owner class: PRODUCT_DECISION — Classification: NOT_REQUIRED — Severity: P3
- API refs: `…/media-access`. Affected workflow: voicemail playback.
- Business impact: none now; becomes relevant if greeting uploads are added.
- Recommended remedy: If custom greeting uploads are introduced, route them through `security-scan-gateway`.
- Dependencies: none — Complexity: LOW — Regression risk: LOW — Confidence: CONFIRMED

### CAL-022 — Web calling has never been verified end-to-end against the real backend
- Service/module: apps/web calling; production acceptance
- Expected: At least one automated or recorded end-to-end run of the web UI placing/answering a real call.
- Actual: Ops doc (2026-11-03) l.4 "Frontend/Vercel access, deployment and testing are excluded from this release"; l.113–117 OS/browser prompts excluded. Web tests: `tests/calling-adapter.test.mjs` (adapter unit tests with fetch fakes) and `tests/calling.browser.mjs` (Playwright with all network routes mocked). Production acceptance used Python WebRTC clients against the API directly.
- Owner class: FRONTEND — Classification: IMPLEMENTED_UNVERIFIED — Severity: P2
- Evidence: files cited; `gh run list --workflow frontend.yml` shows a failure/cancel mix on 2026-11-04.
- API refs: all web-consumed calling routes. Affected workflow: Journeys E and G.
- Business impact: Defects such as CAL-002/005/006 escaped because no web E2E exists.
- Recommended remedy: Add a Playwright two-browser test against a staging Calling + LiveKit (create → ring → accept → media → end → history), run in CI.
- Dependencies: staging credentials — Complexity: MEDIUM — Regression risk: LOW — Confidence: CONFIRMED


## Messaging, Communications Hub, meeting chat, realtime


Backend (read in full or in the relevant parts):
- `services/messaging/src/messages/{mod.rs,http.rs,managed.rs (send path)}`: send, list/history, resolve, delete, read cursor, delivery cursor, durable event replay.
- `services/messaging/src/realtime/{mod.rs,http.rs,connection.rs,sessions.rs (issue_token)}`: WebSocket protocol, frames, subscribe/ack/typing/presence, sync.required, slow consumer.
- `services/messaging/src/events/outbox.rs` (per-conversation event sequence allocation), `conversations/{service.rs,policy.rs}` (create, profiles, E2EE gate, list ordering, view shape, default policy), `membership/mod.rs` (history window `visible_from_event`), `engagement/mod.rs` (unread), `attachments/{mod.rs,scanner.rs}`, `search/mod.rs`, `safety`, `communications.rs` + `communications/owner.rs` (Hub owner interfaces, capabilities), `config/mod.rs` (E2EE production refusal), `devices/mod.rs`, route registration via `grep '\.route('` over `src/`.
- `services/messaging/tests/*` (test names), `.github/workflows/messaging.yml` (real Postgres 16 + Redis 7 test jobs).
- `services/communications/src/{api.rs,owners.rs,calls.rs,meetings.rs,sync.rs,policy.rs,events.rs}`, `tests/{unified.rs,common/mod.rs}`.
- `services/meeting-service/src/services/unified_messaging/{relay.rs,mod.rs,scoped.rs,store.rs,migration.rs}`, `src/handlers/v2/unified_messaging.rs`, `src/routes/v2.rs` (relay mount), `src/services/communications.rs` (Hub ingress), `src/services/session_bridge.rs`, migrations with `CHAT_MIGRATED` triggers, `tests/unified_messaging_e2e.rs` (web journey harness).
- `services/security-scan-gateway/src/lib.rs` header, `packages/scan-gateway-contract`.
- Contracts: `packages/contracts/bundled/messaging/http/v1.openapi.json` (all paths/query params enumerated), `packages/contracts/openapi/meeting-service.json` (relay methods), `infra/api-gateway/public-routes.json`.
- Docs: ADR 0053, `docs/security/private-e2ee-independent-review-package.md`, `docs/api/communications-unified-handoff.md`, `docs/operations/messaging-native-api-map.md`, `docs/operations/cfmeet-sections-8-23-final-closure.md` (claims only, cross-checked), `services/messaging/README.md`, product spec PDF (text-extracted; F-13, A.12, A.31).
- Web: `apps/web/src/lib/messaging/{engine.ts,routes.ts,api.ts,urls.ts}`, `src/services/{messaging-realtime.ts,messaging-session.ts}`, `src/services/v2/{messaging.service.ts,messaging-http.ts,collaboration.service.ts}`, `src/components/messaging/MessagesContent.tsx`, `src/components/room/{live-room.tsx,ChatProvider.tsx}`, `tests/messaging-e2e.browser.mjs`.
- Mobile: `apps/mobile/features/room/{components/chat-panel.tsx,hooks/use-chat.ts,v2/components/room-sidebar.tsx}`, `features/meetings/v2/{api.ts,hooks.ts}`; full-tree grep for messaging/conversation/communications.

Not inspected in depth: MLS crypto internals (`packages/messaging-crypto`), SMS bridge, backups, compliance exports, KT witness protocol, push adapters (only their configured state).



### MSG-001 — REST event replay uses `afterEventSequence` but Messaging only accepts `after`; web realtime recovery never advances and re-subscribes in a loop
- Service/module: shared contract — `apps/web/src/lib/messaging/engine.ts`, `services/meeting-service/src/services/unified_messaging/relay.rs`, `services/messaging/src/messages/mod.rs`, `docs/operations/messaging-native-api-map.md`.
- Expected: The first-party client resynchronizes the durable event cursor after `sync.required` or a client-detected gap via `GET /v1/conversations/{id}/events?after=N`, then resubscribes from the authoritative cursor.
- Actual:
  - Messaging's `EventsQuery` is `{after, limit}` with `#[serde(deny_unknown_fields)]` (`services/messaging/src/messages/mod.rs:1157-1165`). The contract agrees: `packages/contracts/bundled/messaging/http/v1.openapi.json` → `GET /v1/conversations/{conversationId}/events ['after','limit']`.
  - The Meeting relay allowlists ONLY `["afterEventSequence","limit"]` for that route (`relay.rs:122-127`) and forwards the query unchanged to `/internal/v1/delegated/...` (`relay.rs:604-660`). So `after` can't be sent through the relay at all, and `afterEventSequence` is refused by Messaging (axum `Query` rejection → 400).
  - The web calls `v1/conversations/${id}/events?afterEventSequence=${after}&limit=…` and swallows the error with `.catch(() => ({ events: [] }))` (`engine.ts:493-496`). It then calls `transport.synced(id, after)` with the old cursor (`engine.ts:501`).
  - The web transport always subscribes with `afterEventSequence: 0` (`engine.ts:468, 533, 607, 821`). It treats any first event other than `current + 1` as a gap → `onSyncRequired` (`messaging-realtime.ts:199-207`).
  - On the server, `start = after.or(stored).unwrap_or(head).clamp(0, head).max(member.visible_from_event)` (`realtime/connection.rs:517-521`). If more than `max_replay` (500) events are pending, or the replay window was pruned, the server sends `sync.required` (`connection.rs:539-541, 600-602`).
  - A member added to a GROUP after creation gets `visible_from_event = cursor - 1 > 0` (`membership/mod.rs:497-501`), so their first replayed event is never `1`. Every resubscribe therefore replays from the same point and triggers the same gap again. This also happens on every reconnect for any conversation with more than 500 events.
- Evidence: `grep -rn 'afterEventSequence' services/messaging/src` finds only WebSocket frame fields (`connection.rs:226,371,535,552`) and no REST parameter. `grep -rn 'afterEventSequence\|/events?after' services/meeting-service/tests apps/web/tests services/messaging/tests` finds no test of the messaging event replay through the relay. The native API map documents the wrong name: "replay uses afterEventSequence" (`messaging-native-api-map.md:62-63`).
- API/Swagger refs: messaging http v1 `GET /v1/conversations/{conversationId}/events` (`after`); Meeting `GET /api/v2/messaging/relay/{path}`.
- Affected frontend workflow: web Messages page realtime for (a) group members added after creation, (b) any conversation with more than 500 events or a pruned replay window, (c) every client-detected gap. Messages still show up through REST `sync()` and the 15 s poll, but live updates thrash. Each loop iteration costs REST calls plus a `subscribe` frame and replay, until the frame-rate limit closes the socket (`connection.rs:329-345`, close `FRAME_RATE_EXCEEDED`) and the client reconnects. Acks never advance, so delivery receipts from those devices never advance over WS. The CI web journey only uses DIRECT conversations created with the counterpart (visible_from_event = 0), so it never hits this path.
- Owner class: SHARED_CONTRACT
- Classification: CONTRACT_MISMATCH
- Severity: P1
- Business impact: Group chat is not live for most members. The reconnect/resubscribe storm adds unnecessary load on Meeting and Messaging.
- Recommended remedy: Reuse the existing endpoint. Change the relay allowlist to `["after","limit"]` (or map `afterEventSequence`→`after` in the relay) and change web to `?after=`. Also: (1) make the web subscribe with its last acked cursor, or omit `afterEventSequence` so the server's stored per-device ack cursor is used; (2) treat `subscribed.afterEventSequence` as the client cursor baseline instead of assuming it starts at 0; (3) stop swallowing replay errors; (4) dedupe concurrent `syncAuthoritative` calls per conversation. Add a relay test for the events route and a web test with a group member who joined later. No data model change.
- Dependencies: none.
- Complexity: LOW (contract) / MEDIUM (client cursor handling)
- Regression risk: Low. Only the replay path changes.
- Confidence: CONFIRMED for the mismatch and the 400. The resubscribe loop is inferred from the code (REQUIRES_VALIDATION at runtime).

### MSG-002 — Meeting relay mounts GET/POST/PUT/DELETE only: every allowlisted PATCH (MANAGED edit, member role, conversation update/retention) is unreachable for CF Meet clients
- Service/module: meeting-service relay
- Expected: The allowlisted end-user operations `PATCH v1/conversations/{id}`, `PATCH …/members/{principalId}` and `PATCH …/messages/{messageId}` are reachable at `/api/v2/messaging/relay/...`, as both the allowlist and the docs state.
- Actual: The allowlist contains the three PATCH routes (`relay.rs:296-310`, "Section 8: existing end-user domain operations"). The router mounts only `get(relay_get).post(relay_post).put(relay_put).delete(relay_delete)` (`services/meeting-service/src/routes/v2.rs:468-477`), and no `relay_patch` exists (`grep -n 'pub async fn relay_' src/handlers/v2/unified_messaging.rs` → get/post/put/delete only). The generated contract (`packages/contracts/openapi/meeting-service.json` `/api/v2/messaging/relay/{path}` → `['get','post','put','delete']`) and `infra/api-gateway/public-routes.json` (GET/POST/PUT/DELETE only) match the router, so a PATCH gets 405. `docs/operations/messaging-native-api-map.md:116,122,126` documents them as available.
- Evidence: listed above.
- API/Swagger refs: Messaging `PATCH /v1/conversations/{conversationId}`, `PATCH …/members/{principalId}`, `PATCH …/messages/{messageId}`.
- Affected frontend workflow: Editing MANAGED messages (MANAGED edits must use PATCH; the send path refuses `kind=EDIT`, `managed.rs:280-284`), promoting or demoting members, changing a conversation's retention (disappearing messages) or lifecycle after creation.
- Owner class: BACKEND
- Classification: CONTRACT_MISMATCH
- Severity: P1
- Business impact: Message editing and group administration aren't possible from CF Meet's own apps even though the backend implements and tests them.
- Recommended remedy: Reuse the existing relay. Add `.patch(relay_patch)` (same body limit and identity handling as `relay_put`), add PATCH to the Meeting OpenAPI and `public-routes.json`, and add a relay test per PATCH route. No DB change.
- Dependencies: API gateway public-routes update.
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### MSG-003 — Relay query allowlists drop documented parameters (`scope` on message delete, `reason` on member removal, `threadRootId`, conversation filters)
- Service/module: meeting-service relay `ROUTES`
- Expected: The relay forwards the Messaging contract's query parameters for end-user operations.
- Actual: `DELETE …/messages/{id}` and `DELETE …/members/{id}` have query `&[]` (`relay.rs:105-110, 81-86`). `MESSAGES = ["afterSequence","beforeSequence","limit"]` has no `threadRootId` (`relay.rs:61`). `GET conversations` has only `limit,cursor` (no `lifecycleState`/`contextClass`). The relay refuses any other key with `VALIDATION_ERROR "Unsupported query parameter"` (`relay.rs:604-617`). Messaging supports `scope=EVERYONE|ME` (`messages/mod.rs:760-792`), and the contract lists `scope`, `reason`, `threadRootId`, `lifecycleState`, `contextClass`.
- Evidence: contract enumeration (python over the bundled OpenAPI) compared with `relay.rs` `ROUTES`.
- API/Swagger refs: as above.
- Affected frontend workflow: "Delete for me" can't be used at all by first-party clients. Removal reasons are lost. Thread-filtered history paging and archived/closed conversation filtering are unavailable (`GET …/messages/{id}/thread` is relayed, which partly offsets threads).
- Owner class: BACKEND
- Classification: CONTRACT_MISMATCH
- Severity: P2
- Business impact: Missing privacy feature (hide for me) and weaker inbox filtering.
- Recommended remedy: Extend the per-route query allowlists. Add a test asserting each relayed route's query keys equal the Messaging contract's keys (generate from the bundled OpenAPI).
- Dependencies: none.
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### MSG-004 — In production, first-party users can never get realtime: client tokens need a registered MLS device, and device registration is refused in production
- Service/module: messaging realtime/devices + web engine + Hub capabilities
- Expected: MANAGED (server-readable) messaging, which is the only profile allowed in production, gets live delivery, typing, presence and receipts.
- Actual:
  - Client token issuance requires an ACTIVE registered device (`realtime/sessions.rs:194-215` calls `require_own_device`, `messages/e2ee.rs:37-65`).
  - Device registration is refused unless `MESSAGING_E2EE_DEVICES_ENABLED`, and `true` is a startup error in production (`config/mod.rs:466-483`; `devices/mod.rs:66-75` returns `FEATURE_DISABLED/E2EE_DEVICES_DISABLED`).
  - The web engine only calls `connect()` when the local device is `ready` (`engine.ts:241-249`). Otherwise the phase is `needs-device` and only the 15 s poll runs (`engine.ts:250-255, 112`). The UI then shows "Live updates unavailable (refreshing periodically)" (`MessagesContent.tsx:54`).
  - Hub capabilities report messaging unavailable unless devices are enabled AND a witness quorum is met (`services/messaging/src/communications.rs:231-255`). Production has no witnesses configured (closure report `cfmeet-sections-8-23-final-closure.md:115`: "Not configured: APNs, FCM, Web Push, transparency witnesses").
- Evidence: listed above; closure report line 115 (production `messaging-verify`: "production E2EE device guard active").
- API/Swagger refs: `POST /api/v2/messaging/client-tokens` → `POST /v1/messaging/client-tokens`.
- Affected frontend workflow: Production web Messages, which is MANAGED only: no WebSocket, typing, presence, live receipts or instant delivery (15 s latency).
- Owner class: PRODUCT_DECISION (with BACKEND remedy)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1
- Business impact: Production chat feels broken (delayed delivery). The realtime gateway was certified and routed in production but no first-party user can reach it.
- Recommended remedy: Decide whether MANAGED sessions may use a non-MLS "session device". Option A: a lightweight device kind (no KeyPackages) registrable when E2EE is disabled, bound to client tokens. Option B: allow client tokens bound to an opaque browser session id when the device registry is disabled. Option A needs a migration (device kind column) and must keep MLS operations refusing that kind. Also decouple Hub `capabilities.available` for MANAGED messaging from E2EE/witness readiness.
- Dependencies: product/security decision (ADR 0045/0049 amendment).
- Complexity: MEDIUM
- Regression risk: Medium (device authority model).
- Confidence: CONFIRMED (code). Production runtime behaviour REQUIRES_VALIDATION with a production login.

### MSG-005 — Web Messages UI consumes only a fraction of the verified backend (no typing, reactions, edits, replies, pins, unread, receipts, delete, search, block/report, invitations, back-pagination)
- Service/module: apps/web messaging
- Expected: A chat UI using the implemented and tested Messaging features (product handoff `docs/api/communications-unified-handoff.md:17`).
- Actual: `MessagesContent.tsx` offers create conversation, send text/file, download, remove member, history transfer, and a local title filter (`MessagesContent.tsx:226,281,288,328,358,373,399`). `grep -n 'unread\|reactions\|typing(\|/pins\|/search\|blocks\|reports\|PATCH\|scope=ME\|beforeSequence\|invitation' engine.ts MessagesContent.tsx` finds nothing. The client allowlist `routes.ts` omits reactions, pins, search, blocks, reports, invitations, presence, PATCH, etc., so even a new UI would be refused (`BAD_PATH`). The message state "delivered" is a local label, not a receipt (`engine.ts:926,933,975`). History is loaded forward from sequence 0 in full (`engine.ts:879-905, 961-980`), with no `beforeSequence` paging.
- Evidence: listed above.
- API/Swagger refs: all relayed routes in `relay.rs:330-505`.
- Affected frontend workflow: everything beyond basic send/receive.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P1
- Business impact: Missing everyday chat functionality although the backend is ready.
- Recommended remedy: Extend `routes.ts` to mirror `relay.rs` (ideally generated from it), then build UI for receipts, typing, reactions, reply/thread, edit/delete, unread badges, search, block/report, invitations and virtualized back-pagination.
- Dependencies: MSG-002/003 for edit and hide-for-me; MSG-004 for live typing/receipts in production.
- Complexity: HIGH
- Regression risk: Low (additive).
- Confidence: CONFIRMED

### MSG-006 — The Communications Hub (~70 public endpoints) and the unified Chats/Calls journeys have no frontend consumer; mobile has no Unified Messaging at all
- Service/module: services/communications via Meeting `/api/v2/communications/*`; apps/web, apps/mobile
- Expected: Web and mobile implement the five unified areas (Chats, Calls, Contacts, …) per `docs/api/communications-unified-handoff.md:1-6`, including chat-to-call, meeting share, timeline/activity, inbox overlay, contacts, devices and the sync feed.
- Actual: `grep -rni 'communications' apps/web/src` returns only marketing copy (`DevelopersContent.tsx:183`). `grep -rlni 'messaging\|conversation\|/chat' apps/mobile` returns only in-meeting chat files. Mobile tabs are `index, meetings, schedule, settings` (`apps/mobile/app/(tabs)`). The SDKs (`sdks/typescript|dart`) have generated accessors, but no app uses them.
- Evidence: listed above; `infra/api-gateway/public-routes.json` lists all Hub operations.
- API/Swagger refs: `/api/v2/communications/*` (bootstrap, inbox, activity, timeline, calls, meetings/instant|share, contacts, contact-requests, blocks, devices, sync, events …).
- Affected frontend workflow: chat → start call, share/instant meeting, unified timeline, contacts and discovery by CF Meet ID, personal privacy settings.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P1
- Business impact: The whole Unified Communications product is backend-only.
- Recommended remedy: Build web/mobile consumers on the generated SDK. Prioritize inbox + timeline merge (by `anchorSequence`), chat-to-call (`POST …/conversations/{id}/calls` then Calling signaling) and meeting share.
- Dependencies: MSG-004 (realtime), MSG-007 (inbox data), MSG-009.
- Complexity: HIGH
- Regression risk: Low.
- Confidence: CONFIRMED

### MSG-007 — No server-side inbox model: conversations have no title/avatar, last message/activity or unread, and are ordered by creation time
- Service/module: messaging conversations; Hub inbox
- Expected: The chat list shows a group name, the latest message preview/time and unread count, sorted by latest activity, from one paged call.
- Actual: The conversation view (`conversations/service.rs:120-162`) has no title, description, avatar, `lastSequence`/`lastMessageAt` or unread. `grep -n 'title\|display_name\|avatar' src/conversations/service.rs` → none, and there's no column in the migration. The list is `ORDER BY c.created_at DESC, c.id DESC` (`service.rs:951`). Unread is per conversation only (`engagement/mod.rs:386-410`). The Hub `inbox` only covers conversations with preferences, watermarks or timeline cards, ordered by `conversation_id` (`communications/src/sync.rs:134-151`), and it doesn't re-check membership. The web pages up to 10 pages, then makes one member call per group (`engine.ts:507-524`), every 15 s.
- Evidence: listed above.
- API/Swagger refs: `GET /v1/conversations`, `GET …/unread`, `GET /api/v2/communications/inbox`.
- Affected frontend workflow: chat list ordering, group naming, badges; N+1 polling load.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Poor inbox UX and scalability.
- Recommended remedy: Add `title`/`avatarRef` (MANAGED only; for E2EE the title belongs in the MLS group context or encrypted metadata, which is a product decision) and `last_activity_at`/`last_sequence` maintained in the send transaction (already locking the sequence row). Add `orderBy=lastActivity` and per-member `unreadCount` to the list. Additive migration with a backfill from `messaging_conversation_sequences`. Make the Hub inbox filter on current membership.
- Dependencies: E2EE metadata decision.
- Complexity: MEDIUM
- Regression risk: Medium (hot-path update on send).
- Confidence: CONFIRMED

### MSG-008 — Web MANAGED client: append-only sync, no tombstone/edit refresh, fragile attachments and a non-durable offline queue
- Service/module: apps/web `engine.ts`
- Expected: The client converges with server state (deletions, moderation, expiry, edits), retries idempotently and tolerates asynchronous scanning.
- Actual:
  - `syncManaged` only fetches `afterSequence=cursor` and appends (`engine.ts:957-987`). A later delete, moderation or expiry of an already loaded message (which arrives as an event, `messages/mod.rs:731-757`) is never applied until reload. MANAGED edits are invisible. Attachments are labelled "attachment" with no name/size (`engine.ts:977`).
  - After finalize, any state other than `READY` throws `ATTACHMENT_<state>` (`engine.ts:1091-1092`). With fail-closed scanning, a slow or unavailable scanner leaves `VERIFYING` (`attachments/mod.rs:878+`), so the message is marked failed although the attachment later becomes READY. There is no retry or poll.
  - Pending sends live in memory (`pendingSends`, `engine.ts:172, 996-1006`). Failures are final, and every attempt uses a fresh `clientMessageId` (`engine.ts:1022`), so a manual resend after a lost response can duplicate.
- Evidence: listed above.
- API/Swagger refs: `GET …/messages`, `GET …/events`, attachments finalize/`GET …/{id}`.
- Affected frontend workflow: message lifecycle display, file sending, flaky networks.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: Deleted or moderated content stays visible on open clients (a moderation/safety gap on the client side). Lost or duplicated messages are possible.
- Recommended remedy: Apply `message.deleted|moderated|expired|edited` events (hydrated message in `event.data.message`) to local state. Poll `GET …/attachments/{id}` until READY or QUARANTINED. Persist the outbox (IndexedDB) with a stable `clientMessageId` per logical message.
- Dependencies: MSG-001 for event replay.
- Complexity: MEDIUM
- Regression risk: Low.
- Confidence: CONFIRMED

### MSG-009 — Meeting links and calls in chat exist only as Hub timeline cards (not chat messages), with no realtime and no card for members without a Hub profile
- Service/module: services/communications `meetings.rs`, `calls.rs`, `sync.rs`
- Expected: Sharing or starting a meeting from a chat, or starting a call, appears in the conversation for every member in real time.
- Actual: `record_meeting` inserts `communication_meeting_links` plus per-viewer `communication_timeline_entries` only for members who have a Hub profile (`meetings.rs:42-60`; `Err(NotFound) => continue`). The same applies to call cards (`calls.rs:128-142`). Nothing is posted to the Messaging conversation. Calls launched outside the Hub never produce cards. Card state is refreshed by polling Calling, every 5 s on read and in a sweep (`calls.rs:19, 410-450`), because Calling has no producer. The Hub change feed is pull-only (`GET /sync`, `GET /events`, `sync.rs:40-130`). Its only realtime coupling to Messaging is the account-revocation event (`services/messaging/src/communications.rs:283-330`).
- Evidence: listed above; handoff `communications-unified-handoff.md:99` ("Poll or, on a realtime nudge") — no nudge channel exists.
- API/Swagger refs: `POST /api/v2/communications/conversations/{id}/meetings/{instant,share}`, `…/calls`, `GET …/timeline`.
- Affected frontend workflow: unified communications journey (see Journeys).
- Owner class: BACKEND / PRODUCT_DECISION
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Members using other clients, or without a Hub profile, never see the meeting/call in the chat, and cards aren't live.
- Recommended remedy: Product decision. Either post a MANAGED system message (`managedPresentation`, notify=false) with the meeting code through the existing delegated send, or keep cards and add a realtime nudge (e.g., publish a `communications.timeline.changed` device event through Messaging's existing hub device-event path). Create cards lazily for members who later get a profile.
- Dependencies: MSG-006.
- Complexity: MEDIUM
- Regression risk: Low.
- Confidence: CONFIRMED

### MSG-010 — Hub ↔ owner integration is not tested end-to-end in CI; Hub tests use simulated owners
- Service/module: services/communications tests
- Expected: Hub facades (conversation access, call launch, meeting instant/resolve, attachments/media, devices/push) exercised against the real Messaging, Calling and Meeting processes.
- Actual: `services/communications/tests/common/mod.rs:42` "Simulated canonical owners for the Sections 9-23 private routes" — the mock router answers owner routes (`:309-325, 504`). Owner sides are tested separately with real DBs (`services/messaging/tests/communications_owner.rs`, `services/calling/tests/communications_owner_routes.rs`, `services/meeting-service/tests/communications_meetings.rs`). The closure report claims "PRODUCTION_VERIFIED (Hub-launched voice and video with media)" (`cfmeet-sections-8-23-final-closure.md:138`); that's a manual/production verify, not an automated cross-service test.
- Evidence: listed above.
- API/Swagger refs: `/internal/v1/communications/*` on each owner.
- Affected frontend workflow: all Hub journeys.
- Owner class: BACKEND
- Classification: IMPLEMENTED_UNVERIFIED
- Severity: P2
- Business impact: Contract drift between Hub and owners can go undetected (the relay drift in MSG-001/002 shows the risk).
- Recommended remedy: Add a CI journey that starts the Hub with the real Messaging, Meeting and Calling binaries (as `unified-messaging-e2e.yml` does for Meeting↔Messaging): conversation access → call launch → timeline card → meeting share.
- Dependencies: none.
- Complexity: MEDIUM
- Regression risk: None.
- Confidence: CONFIRMED

### MSG-011 — Offline notifications: push providers and KT witnesses are not configured in production; no client registers push
- Service/module: messaging push adapters; transparency
- Expected: Offline members are notified (APNs/FCM/Web Push), and E2EE key transparency is witnessed.
- Actual: The adapters exist (`realtime/push_adapters.rs`, config `config/mod.rs:42`), but the production verify reports "Not configured: APNs, FCM, Web Push, transparency witnesses" (`cfmeet-sections-8-23-final-closure.md:115`). The handoff says delivery reports `NOT_CONFIGURED` (`communications-unified-handoff.md:187-189`). Web never calls `PUT …/push-registration` (not in `routes.ts`), and mobile has no messaging. Signed heads are `UNWITNESSED` (README).
- Evidence: listed above.
- API/Swagger refs: `PUT/DELETE /v1/messaging/devices/{deviceId}/push-registration`; Hub `/me/notification-delivery-status`.
- Affected frontend workflow: offline delivery notification.
- Owner class: EXTERNAL_DEPENDENCY
- Classification: EXTERNAL_DEPENDENCY
- Severity: P2
- Business impact: Users miss messages while offline.
- Recommended remedy: Provision provider credentials, implement web push registration (needs a device, see MSG-004), and run a physical-device test.
- Dependencies: MSG-004, provider accounts.
- Complexity: MEDIUM
- Regression risk: Low.
- Confidence: CONFIRMED (config claims REQUIRES_VALIDATION on the host).

### MSG-012 — Meeting chat migration cutover would break mobile in-meeting chat (v1 chat refused after cutover; dormant v2 mobile client targets a non-existent route)
- Service/module: apps/mobile, meeting-service migration (ADR 0053)
- Expected: Before `MESSAGING_PRIMARY`, every client uses `/api/v2/conferences/{id}/messages` (spec F-13: "No use of v1 chat messages in a v2 room").
- Actual: The mounted mobile chat panel uses v1 `/meetings/{id}/chat` (`features/room/components/chat-panel.tsx:7,18` → `hooks/use-chat.ts` → `roomApi.getChatMessages`). After cutover, the Meeting DB trigger raises `CHAT_MIGRATED: legacy v1 chat accepts no new messages` (`meeting-service/migration/src/m20261019_000001_unified_messaging_integration.rs:185`). The v2 mobile `chatApi` calls `/conferences/${id}/chat` (`features/meetings/v2/api.ts:295-314`), but the backend route is `/conferences/:id/messages` (`routes/v2.rs:209-216`). That v2 sidebar is not mounted anywhere (`grep -rn "room/v2"` → none). Web uses v2 for v2 rooms (`live-room.tsx:163`, `useCollaboration.ts:47-58`) and v1 only for legacy rooms.
- Evidence: listed above.
- API/Swagger refs: `GET/POST /api/v2/conferences/{id}/messages`; legacy `GET/POST /api/v1/meetings/{id}/chat`.
- Affected frontend workflow: mobile in-meeting chat after cutover.
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P2 (becomes P1 at cutover; it's a cutover gate)
- Business impact: Mobile meeting chat outage on cutover day.
- Recommended remedy: Move mobile to the v2 messages API (fix the path to `/messages` and mount it). Add a cutover checklist gate requiring zero v1 chat traffic.
- Dependencies: ADR 0053 operator decision.
- Complexity: LOW–MEDIUM
- Regression risk: Medium (mobile room).
- Confidence: CONFIRMED

### MSG-013 — PRIVATE_E2EE is correctly gated off in production pending independent review; key transparency is unwitnessed
- Service/module: messaging config/devices/conversations
- Expected: E2EE isn't offered in production until the independent assessment and owner approval (#13).
- Actual: The startup validator refuses `MESSAGING_E2EE_DEVICES_ENABLED=true` in production (`config/mod.rs:466-475`, unit test `:1075-1093`). Conversation create refuses PRIVATE_E2EE with `E2EE_NOT_AVAILABLE/E2EE_DEVICES_DISABLED` and never falls back to MANAGED (`conversations/service.rs:571-591`). The review package status is "READY FOR COMMISSIONING. No independent assessment has been performed" (`private-e2ee-independent-review-package.md:3`). The web engine refuses a profile downgrade (`engine.ts:598-600`).
- Evidence: listed above.
- API/Swagger refs: `POST /v1/conversations` (`securityProfile`).
- Affected frontend workflow: secure messaging setup, which shows "needs-device" in production.
- Owner class: EXTERNAL_DEPENDENCY
- Classification: EXTERNAL_DEPENDENCY
- Severity: P1 (required feature blocked; the gating itself is correct)
- Business impact: Only server-readable MANAGED chat is available in production.
- Recommended remedy: Commission the assessment (#13) and configure KT witnesses. Keep the gate. Make the web UX in production MANAGED-first instead of prompting device setup that will fail.
- Dependencies: external assessor, owner approval.
- Complexity: HIGH (external)
- Regression risk: n/a
- Confidence: CONFIRMED

### MSG-014 — Web E2EE journey fails in CI at multi-device history recovery (known frontend issue #37)
- Service/module: apps/web `MessagesContent.tsx`
- Expected: The "Web Messaging client in Chromium (real services)" job passes.
- Actual: It failed on all 9 recent runs of `unified-messaging-e2e.yml` (e.g. 42182739453 main, 42177519023 staging): `page.click: Timeout … waiting for locator('[data-testid=recover-history]')` at `apps/web/tests/messaging-e2e.browser.mjs:295`. Earlier steps pass: device registration, E2EE create, send/receive, XSS inertness, reload, offline order, reconnect, E2EE attachment, MANAGED label. GitHub issue #37 attributes it to the collapsed device panel from commit `ebf28ce`. Both backend jobs in the workflow are green.
- Evidence: `gh run view 42182739453 --log-failed`; `gh api repos/CosmicforgeHealthnet/cfmeet/issues/37`.
- API/Swagger refs: history-transfer relay routes.
- Affected frontend workflow: adding a second device and recovering history.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: Multi-device recovery is unverified in the shipped UI. CI is red on main.
- Recommended remedy: Fix per #37. Don't reclassify as backend.
- Dependencies: none.
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### MSG-015 — Documentation drift: ADR 0053 scope and the native API map contradict the code
- Service/module: docs
- Expected: Docs match the implementation.
- Actual: ADR 0053 "Scope limits" says breakout/private chat "stay in `conference_messages` … in every state" (`0053-…md` Scope limits). The code migrates them to their own MEETING conversations (`unified_messaging/scoped.rs:1-20`; `store.rs:1-10`: "Used for EVERY meeting chat scope"). The native API map lists relay PATCH routes and `afterEventSequence` replay (MSG-001/002). The handoff points clients at `wss://api.cfmeet.com/v1/realtime` (`communications-unified-handoff.md:31`). That is routed through the Calling ingress (`infra/calling/ingress-ci/realtime_ingress.py`), but `public-routes.json` doesn't list it.
- Evidence: listed above.
- API/Swagger refs: n/a
- Affected frontend workflow: implementers following the docs.
- Owner class: SHARED_CONTRACT
- Classification: CONTRACT_MISMATCH
- Severity: P3
- Business impact: Misleading integration guidance.
- Recommended remedy: Amend ADR 0053 with a "superseded scope" note, regenerate the native API map from the relay allowlist, and document the realtime ingress route.
- Dependencies: MSG-001/002 fixes.
- Complexity: LOW
- Regression risk: None.
- Confidence: CONFIRMED

### MSG-016 — Hub hygiene: `block-events` ingress has no producer; inbox shows conversations the viewer has left
- Service/module: services/communications
- Expected: Block propagation endpoints are used or removed. Overlays are limited to current membership.
- Actual: `/internal/v1/communications/block-events` (`api.rs:36-39`, `events.rs:135-200`) accepts Calling/Messaging events, but `grep -rn 'block-events\|BlockEvent' services/messaging/src services/calling/src` finds no caller. Blocks are pulled fresh instead (`policy.rs:13-40`), so behaviour is correct but the endpoint is dead. `inbox` (`sync.rs:134-151`) doesn't call `conversation()` to re-check membership (unlike `activity`, `calls.rs:540-552`), so former members keep inbox rows with `latestActivity` metadata.
- Evidence: listed above.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P3
- Business impact: Minor stale-data exposure of the user's own call/meeting metadata, and dead code.
- Recommended remedy: Remove or wire up `block-events`. Filter the inbox by membership (batch access check).
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### MSG-017 — Administration needed to run CF Meet's own environment (policy incl. attachments enablement, moderation queue, legal holds) has no first-party surface; attachments default to disabled
- Service/module: messaging policy/moderation; Meeting relay
- Expected: CF Meet operators can enable attachments, triage reports and apply moderation for the FIRST_PARTY environment.
- Actual: The default policy has `attachments.enabled: false` (`conversations/policy.rs:248-252`). Policy, moderation, legal holds and exports are deliberately not relayed (`relay.rs` test `:836-846`) and need a Platform Access credential with `conversations.manage`/`moderation.manage`. There's no CF Meet admin UI (web has only the chat-migration admin hook). Whether the production FIRST_PARTY policy enables attachments wasn't verified. The web CI journey's attachment step passes in its own test environment.
- Evidence: listed above.
- API/Swagger refs: `PUT /v1/messaging/policy`, `GET /v1/messaging/reports`, `POST /v1/messaging/moderation/actions`.
- Affected frontend workflow: file sharing (if not enabled), report handling.
- Owner class: PRODUCT_DECISION
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: Reports may go unhandled. Attachments may be off in production.
- Recommended remedy: Use the existing Developer Platform credential path or an operator runbook to set the FIRST_PARTY policy, and add a minimal admin console for reports/actions (reuse the existing endpoints).
- Dependencies: platform-admin auth model.
- Complexity: MEDIUM
- Regression risk: Low.
- Confidence: REQUIRES_VALIDATION (production policy state)

### MSG-018 — Realtime efficiency: the web ignores the server's per-device ack cursor, and every read/ack writes a conversation-sequenced event
- Service/module: web transport; messaging read/delivery cursors
- Expected: A reconnect resumes from the device's ack cursor. Receipt traffic doesn't serialize with sends in large groups.
- Actual: The server would resume from the stored `messaging_ack_cursors` when `afterEventSequence` is omitted (`connection.rs:509-519`), but the web always sends `0` (`engine.ts:468`). Every reconnect therefore replays up to 500 events or triggers `sync.required` (feeding MSG-001). `advance_delivery` and `set_read_cursor` call `outbox::enqueue_conversation` (`messages/mod.rs:920-937, 944-985`), which takes the same `messaging_conversation_sequences` row lock as sends (`outbox.rs:98-105`) and fans out to every member.
- Evidence: listed above.
- Owner class: SHARED_CONTRACT
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P3
- Business impact: Avoidable load, and lock contention in large groups.
- Recommended remedy: Have the client omit the cursor or pass its acked cursor. Consider coalescing receipt events (per-member latest) or moving them to an ephemeral, non-sequenced channel with REST read-back.
- Complexity: LOW (client) / MEDIUM (server)
- Regression risk: Low / Medium.
- Confidence: CONFIRMED (code); performance impact REQUIRES_VALIDATION (`tests/load.rs` is `#[ignore]`d).

### MSG-019 — Send idempotency edge cases: rate limits charged before dedupe; replay with a different attachment set returns the original
- Service/module: messaging send
- Expected: An idempotent retry neither consumes quota nor masks a conflicting payload.
- Actual: `send_prelude` hits the environment and principal rate limits (`messages/mod.rs:311-343`) before `load_by_client_id` (`managed.rs:301-311`), so retries count against limits and abuse signals. The duplicate check compares text only (`same_text`), so a retry with different `attachmentIds` returns 200 with the original message instead of a conflict.
- Evidence: listed above.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P3
- Business impact: Edge-case 429s on flaky networks, and silent payload mismatch.
- Recommended remedy: Dedupe before rate limiting. Include the attachment set in the replay comparison (409 `IDEMPOTENCY_CONFLICT` on mismatch).
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED


### Realtime interfaces (reference)


- `POST /v1/realtime/tickets`: `Authorization: CF-Client-Token`, exact-origin CORS (`realtime/http.rs:49-140`). `GET /v1/realtime`: subprotocols `cfmeet.messaging.v1`, `cfmeet.ticket.<64-hex>` (single use, 30 s).
- Client → server frames (`connection.rs:364-424`): `ping`, `subscribe{conversationId, afterEventSequence?}`, `unsubscribe`, `ack{conversationId, eventSequence}` (must be ≤ delivered on this connection; records the device ack cursor and DELIVERED in one transaction), `typing{state}`, `presence.set{visibility, shareLastSeen?}`, `presence.query`, `auth.refresh{ticket}`. Unknown types → `UNSUPPORTED_FRAME` without closing. Binary frames are refused.
- Server → client frames: `welcome` (limits), `subscribed{afterEventSequence, headEventSequence}`, `event{event}` (hydrated, per-viewer, tombstone-aware), `sync.required{path}`, `subscription.revoked{reason}`, `ack.recorded`, `typing`, `presence`, `presence.snapshot`, `presence.settings`, `device.event`, `read-cursor.synced`, `auth.refreshed`, `server.draining{reconnectAfterMs}`, `error{code}` (incl. `SLOW_CONSUMER` with resume cursors, `SESSION_EXPIRED`, `RATE_LIMITED`).
- Durable event types (`client_type` strips `messaging.`): `message.created|edited|deleted|moderated|expired|purged|sms-transport-changed`, `member.added|removed|updated|invited|invitation-closed`, `conversation.created|updated|lifecycle.changed|successor-created`, `reaction.added|removed`, `pin.added|removed`, `read.cursor.updated`, `delivery.cursor.updated`, `attachment.ready|quarantined|rejected|deleted|derivative-ready`, `mls.welcome-available`, `device.*`, `client-session.revoked`.
- Correctness: per-conversation event sequence allocated by `UPDATE … RETURNING` under the row lock until commit (`outbox.rs:98-105`), so commit order equals sequence order and the gap-free fanout guarantee holds (test `realtime.rs:416`). The broker carries pointers, and gaps are filled from PostgreSQL (`realtime.rs:378` survives a broker outage). Slow consumers are bounded and resumable (`realtime.rs:472`). Dedupe is by `clientMessageId` (unique constraint) and the conversation-create `Idempotency-Key`.


### Communications Hub facades (who proxies what, and why)


| Public path (Meeting ingress) | Hub handler | Canonical owner call | Why |
|---|---|---|---|
| `/api/v2/communications/*` (any) | Meeting `services/communications.rs:145-151` → `POST /internal/v1/communications/dispatch` | — | Meeting verifies the CF Meet login. The Hub keeps no account authority (`communications.rs:1`) |
| `conversations/{id}/timeline|context|preferences|clear-for-me|shared-groups|media|meetings` | `api.rs:439-486` | Messaging `POST /internal/v1/communications/conversations/access` (`owners.rs:119-153`); media → `…/conversations/attachments` (`chat.rs:494`) | membership re-checked live at the owner; the Hub stores overlays only |
| `conversations/{id}/calls`, `contacts/{id}/calls`, `calls/history`, `calls/{id}/context` | `calls.rs:166-330` | Calling `/internal/v1/communications/calls[/history|/participation]` | Calling owns calls and media; the Hub keeps links and cards |
| `conversations/{id}/meetings/instant|share` | `meetings.rs:94-178` | Meeting `/internal/v1/communications/meetings/instant|resolve` | Meetings owns spaces and admission |
| `devices`, `devices/{id}/push|revoke` | `api.rs:497-527` | Messaging `/internal/v1/communications/devices/verify|revoke|push`, `push-status` | Messaging owns devices and push |
| `me/storage-summary` | `chat.rs:421-440` | Messaging `/internal/v1/communications/storage-summary` | quotas owned by Messaging |
| contacts, requests, invites, favorites, lists, privacy, blocks, profiles, sync/events, bootstrap, inbox, activity | Hub-local (`api.rs:300-556`) | Hub policy pulls Messaging/Calling `…/blocks/check` (`policy.rs:13-40`) | the Hub owns relationships |
| Message CRUD, receipts, attachments, MLS | **not via the Hub** | Meeting `/api/v2/messaging/relay/v1/*` → Messaging `/internal/v1/delegated/*` (`relay.rs:604-660`) | Messaging stays the conversation/message authority |


## Platform: Developer Platform, gateway, SDKs, Swagger, admin


- **API Gateway**: `services/api-gateway/src/{http/mod.rs,http/ingress.rs,catalogue.rs,routing.rs,docs.rs,quota.rs,identity.rs,state.rs,main.rs,config/mod.rs}`, `services/api-gateway/tests/{ingress.rs,platform_access_e2e.rs}`.
- **Gateway infra**: `infra/api-gateway/{route-manifest.json (572 routes, 734 catalogue entries), public-routes.json, sdk-discovery.json, README.md}`, `infra/deployment/part-vi/compose.part-vi.yml`, `docs/phases/phase-06/{deployment-integration.md,developer-console-api-handoff.md}`.
- **Developer Platform**: `services/developer-platform/src/http/{mod.rs,public.rs,operator.rs,internal.rs}`, `src/control/{credentials.rs,activity.rs,operator.rs (header)}`, `src/ledger/mod.rs`, `src/config/webhooks.rs`, `src/webhooks/migration/import.rs`, `tests/{foundation.rs,common/mod.rs}`.
- **Platform Access**: `services/platform-access/src/http/admin.rs`, `src/identity/{credentials.rs,operator.rs,scopes.rs}`, `src/bin/platform-access-admin.rs`, `migration/src/m20261016_000003_access_operations.rs`.
- **Meeting-service legacy developer API**: `src/routes/{mod.rs,api_keys.rs,api.rs,webhooks.rs,admin.rs}`, `src/handlers/{api_keys.rs,webhooks.rs,admin.rs,v2/organizations.rs}`, `src/middleware/api_key.rs`, `src/config/features.rs`, `src/services/{communications.rs,platform/events.rs}`, `src/swagger/mod.rs`.
- **Calling docs**: `services/calling/src/{routes.rs,public_docs.rs}`. Communications dispatch table: `services/communications/src/api.rs:310-534`.
- **Contracts and tooling**: `packages/contracts/**` (all 20 OpenAPI sources plus `bundled/`), `scripts/contracts/{public_api.py,bundle_openapi.py,check_swagger_labels.py}`, `scripts/swagger/build-private-docs.py`, `.github/workflows/{contracts,api-gateway,developer-platform,certification,private-docs-vps,cfmeet-core-images}.yml`.
- **SDKs**: `sdks/{generate.py,generation.json}`, `sdks/typescript/src/{client.ts,generated/*}`, `sdks/dart/lib/src/generated/*`, test directories.
- **Frontend consumers**: `apps/web/src/services/{api-keys.service.ts,webhook.service.ts,api.ts,v2/organizations.service.ts}` and `apps/web/src/components/dashboard/{DevelopersContent,WebhooksContent,ApiKeysContent}.tsx`.
- **Brief look**: `services/security-scan-gateway/src/lib.rs`, `packages/object-storage/src/lib.rs`.
- **Product spec PDF** (via pdftotext): §2.1 scope exclusions, D-07, A.29/A.30/A.32.
- **Certification harness**: `certification/src/lib.rs`, and the test names in `certification/tests/*.rs`.

### Automated checks run (read-only)

| Command | Result |
|---|---|
| `python3 scripts/contracts/public_api.py --check` | `public API catalogue current: 572 public routes; classes {EXCLUDED 72, INTERNAL_SERVICE 33, OPERATOR_ADMIN 36, PROVIDER_CALLBACK 4, PUBLIC_CUSTOMER 572, PUBLIC_INFRASTRUCTURE 17}`, rc=0 |
| `python3 scripts/contracts/bundle_openapi.py --check` | `bundles current and self-contained: 12 documents`, rc=0 |
| `python3 scripts/contracts/check_swagger_labels.py` | `PASS: 568 service API operations ...`, rc=0. It covers only 12 sources and **excludes Developer Platform, API Gateway and meeting-service** contracts (see PLT-007). |
| `python3 sdks/generate.py --check` | `sdks: OK — 572 operation(s) (10 paid telecom ...); 13 contract(s) pinned`, rc=0 |
| `public_api.py --help` | **NOT RUN on purpose.** `main()` only recognises `--check` (`public_api.py:470-484`). Any other argv, `--help` included, **regenerates and writes** the manifest and public bundles. The same holds for `bundle_openapi.py` and `sdks/generate.py`. This is a usability hazard (P3, noted in PLT-007). |
| Custom route cross-check: manifest `upstreamPath`+method vs each upstream's `.route(`/`.nest(` tree (static route extraction script used by this audit) | **0 of 572 manifest routes are dangling.** 72 `/api/v2/communications/**` routes resolve through meeting-service's wildcard relay (`services/meeting-service/src/services/communications.rs:145-151`, `/*path` → `POST /internal/v1/communications/dispatch`). All 72 match a `(method, segments)` arm in `services/communications/src/api.rs:310-534`. The other 500 match a concrete axum route and method. |
| Contract ↔ router diff (`work/diff.py`) for developer-platform, api-gateway, platform-access, calling, messaging, numbering, sms-gateway | DP http+internal: exact match both ways. PA internal: exact (plus an undocumented `/metrics`). Gateway: `/v1/developer-platform/context` is not a gateway router route; it is a manifest route to DP `/internal/v1/gateway/context` (correct). Calling: `privacy-relay/http` contract lists 5 `/v1/privacy-relay/contacts*` ops that live in the privacy-relay process (`services/privacy-relay/src/contact_vault/http.rs:43-53`), not in calling; they are correctly EXCLUDED (`CONTACT_VAULT_PRIVATE`). Messaging: `/witness/v1/*` routes are not in any contract. |



### PLT-001 — The gateway always denies `POST /v1/privacy-relay/contact-access-grants`: its required scope is a prose string, not a scope
- Service/module: shared contract `privacy-relay/http`, then `scripts/contracts/public_api.py`, then `infra/api-gateway/route-manifest.json`, then `services/api-gateway/src/http/ingress.rs`.
- Expected: `requiredScope` is a registry scope (or absent, leaving the per-requested-scope check to the upstream).
- Actual: `packages/contracts/privacy-relay/http/v1.openapi.json:1187` has `"x-cfmeet-required-scope": "each requested privacy.contacts.* scope"`. `public_api.py:234-236` copies it verbatim into the manifest route `privacy-relay.createContactAccessGrant` with `requiredScope: "each requested privacy.contacts.* scope"`. The gateway does `if !customer.has_scope(scope) → InsufficientScope` (`ingress.rs:259-263`), and `has_scope` is an exact set lookup (`identity.rs:46-47`). No credential can hold that string, so every call through the gateway returns 403 INSUFFICIENT_SCOPE.
- Evidence: my script found it is the only manifest scope not in the Platform Access registry (`services/platform-access/src/identity/scopes.rs`). The generator has no scope validation (`grep -n "requiredScope" scripts/contracts/public_api.py` shows line 236 only). Both SDKs also carry the bogus scope (`sdks/typescript/src/generated/operations.ts:588`, `sdks/dart/lib/src/generated/operations.dart:556`). Calling's tests hit the service directly (`calling/tests/privacy_contact_grants.rs:65`), never through the gateway.
- API/Swagger refs: `POST /v1/privacy-relay/contact-access-grants`. The gateway publishes it as PUBLIC_CUSTOMER, while meeting-service public Swagger hides it (`swagger/mod.rs:945-947`).
- Affected frontend workflow: none directly. External integrators granting contact access via the SDK or gateway fail.
- Owner class: SHARED_CONTRACT
- Classification: CONTRACT_MISMATCH
- Severity: P1
- Business impact: a documented public API operation cannot be used through the only public ingress.
- Recommended remedy: drop `x-cfmeet-required-scope` for this op (the upstream already checks each requested scope), or use a real gate scope such as `privacy.contacts.grant`. Add a generator rule in `public_api.py` that rejects any `x-cfmeet-required-scope` absent from the PA registry. Then regenerate the manifest and SDKs. No DB change.
- Dependencies: PA scope registry; SDK regeneration.
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### PLT-002 — Legacy meeting-service API keys are stored and matched in plaintext; the usage counter races
- Service/module: meeting-service `handlers/api_keys.rs`, `middleware/api_key.rs`, migration `m20260116_000008_create_api_keys.rs`.
- Expected: only a hash/HMAC verifier is stored (as in Platform Access, `identity/credentials.rs:1-12`); quota increments are atomic.
- Actual:
  - `create_api_key` stores the raw key: `api_key: Set(raw_api_key.clone())` (`api_keys.rs:98`). It is a 64-char alphanumeric string with no prefix.
  - Authentication runs `.filter(api_keys::Column::ApiKey.eq(api_key_header))` (`middleware/api_key.rs:36-40`).
  - Usage is counted read-modify-write: `used_count = Set(api_key.used_count + 1)` (`:63-66`), so concurrent requests lose increments and can exceed `usage_limit`.
  - Lifetime and limit are hard-coded: 100 uses, 20 days (`api_keys.rs:88-89`). There is no rotation and no scopes.
  - The routes are always mounted: `/api/v1/api-keys` is not even behind `legacy_writes` (`routes/mod.rs:73`). `/api/v1/api` is behind it, but `FEATURE_LEGACY_MEETING_WRITE_DISABLED` defaults to `false` (`config/features.rs:75`).
  - Legacy webhook secrets are also stored in plaintext (`handlers/webhooks.rs:126`).
- Evidence: the files and lines above. `grep -rn "api_keys" services/meeting-service/migration/src` shows a single create migration and no hashing migration.
- API/Swagger refs: `/api/v1/api-keys*`, `/api/v1/api/meetings*` (meeting-service Swagger tag "API Keys"; gateway EXCLUDED `LEGACY_DIRECT_INGRESS`).
- Affected frontend workflow: Settings → API Keys (`apps/web/src/components/dashboard/ApiKeysContent.tsx`).
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1. It is a security defect, but the blast radius is limited to create/join meeting as the key owner, and keys are short-lived.
- Business impact: a DB read (backup leak, SQL injection elsewhere, or support access) yields directly usable credentials.
- Recommended remedy: preferred is to retire the legacy key API in favour of PA/DP credentials (PLT-004). Otherwise add a `key_hash` column (HMAC-SHA256 with a server pepper) with a unique index, backfill-hash existing rows, null out `api_key`, look up by hash, and use `UPDATE … SET used_count = used_count + 1 WHERE used_count < usage_limit RETURNING`. Migration risk: existing keys keep working only if hashed in place.
- Dependencies: product decision D-07 (spec line 1056).
- Complexity: MEDIUM
- Regression risk: MEDIUM (web API-keys screen, any external users of `/api/v1/api/*`)
- Confidence: CONFIRMED

### PLT-003 — Three webhook systems; meeting-service "webhooks" are registered but never delivered, while the web UI promises real-time meeting events
- Service/module: meeting-service legacy webhooks; calling `/v1/webhooks`; developer-platform central webhooks; gateway routing controls.
- Expected: one source of truth for customer webhooks, and any registered endpoint receives events.
- Actual:
  - (a) **Meeting-service** `/api/v1/webhooks` stores `endpoint_url`, `event_type` and `secret` (`handlers/webhooks.rs:108-140`). No code reads the `webhooks` table outside the CRUD handlers: `grep -rn "Webhooks::find" services/meeting-service/src` finds only `handlers/webhooks.rs`, plus relation declarations in `models/users.rs:63,85`. So nothing is ever delivered.
  - (b) **Calling** owns the machine-credential `/v1/webhooks*` routes in the manifest. The gateway default is `upstream: cfmeet.calling`, re-pointed to DP only when `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED` (`catalogue.rs:99-103`, `routing.rs:441-446`).
  - (c) **Developer Platform** has env-scoped console webhooks plus forwarded `/v1/webhooks`. They are disabled by default (`DEVELOPER_PLATFORM_WEBHOOKS_ENABLED=false`) and in `shadow` delivery mode (`config/webhooks.rs` header; `infra/deployment/part-vi/compose.part-vi.yml:27-28`).
  - Meeting events reach DP only if `MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION=developer-platform` (default `none`, `services/platform/events.rs` header), and only for meetings owned by a PA application environment, never user-owned meetings.
  - DP's migration imports only Calling subscriptions (`webhooks/migration/import.rs` header: "keyed by Calling's ids"). Meeting-service legacy webhooks have no migration path.
- Evidence: the files above. `apps/web/src/components/dashboard/WebhooksContent.tsx:149` says "Receive real-time notifications for meeting events."
- API/Swagger refs: `/api/v1/webhooks*` (meeting Swagger), `/v1/webhooks*` (gateway/calling), `/v1/environments/{env}/webhooks*` (DP).
- Affected frontend workflow: Settings → Webhooks (`webhook.service.ts`, `useWebhooks.ts`).
- Owner class: BACKEND (plus PRODUCT_DECISION on the target system)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1
- Business impact: users configure webhooks that silently never fire; three inconsistent ownership models.
- Recommended remedy: decide (D-07) that DP central webhooks are the sole system. Then:
  - mark meeting-service `/api/v1/webhooks` deprecated, and either hide the web panel or show "not delivered";
  - add a one-off import of meeting-service rows into DP env webhooks (needs a user → PA environment mapping, which may not exist for personal users);
  - enable the meeting public-events publisher.
  - Reuse existing DP endpoints; no new endpoint is needed.
- Dependencies: PLT-005 (DP/gateway deployment), DP webhook flags, meeting public events destination.
- Complexity: MEDIUM
- Regression risk: LOW (the legacy path does nothing today)
- Confidence: CONFIRMED

### PLT-004 — Two unrelated API-credential systems; the web developer page documents the legacy one with the wrong host and key format
- Service/module: meeting-service legacy keys vs Platform Access/Developer Platform credentials; `apps/web` dashboard.
- Expected: a single credential authority (PA, per ADR/Part VI) with a migration path; the developer UI uses it.
- Actual:
  - The web app's API-keys UI and docs use legacy `/api/v1/api-keys` (`apps/web/src/services/api-keys.service.ts:10-20`; base `NEXT_PUBLIC_API_URL … /api/v1`, `services/api.ts:6`).
  - `DevelopersContent.tsx:276-553` documents `https://api.cosmicforge.io/api/v1/api/meetings` with `Api-Key: cf_live_…`. The backend issues prefix-less 64-char keys (`api_keys.rs:26-34`), and production is `api.cfmeet.com`.
  - The gateway rejects anything but `cfmk_` platform credentials on credential routes (`ingress.rs:1-25`) and does not route the legacy paths (`route-manifest.json` catalogue: `/api/v1/api-keys*` EXCLUDED `LEGACY_DIRECT_INGRESS`).
  - The DP credential self-service (73 ops) has **no frontend**: the handoff doc says `FRONTEND_IMPLEMENTATION_DEFERRED` (`docs/phases/phase-06/developer-console-api-handoff.md:3`).
  - The spec lists the legacy keys and webhooks as "Developer portal, not the Phase 2 meeting UI" (spec A.29/A.30) and leaves D-07 open.
- Evidence: the files above. `grep -rn "@cfmeet/platform-sdk" apps` finds nothing.
- API/Swagger refs: `/api/v1/api-keys*` vs `/v1/environments/{env}/service-accounts/{sa}/credentials*`.
- Affected frontend workflow: Dashboard → Developers, Settings → API Keys.
- Owner class: PRODUCT_DECISION / FRONTEND
- Classification: FRONTEND_ONLY_GAP (DP console) plus CONTRACT_MISMATCH (web docs)
- Severity: P1
- Business impact: developers are steered to a weak, soon-retired API with incorrect documentation; the real platform is unreachable from the UI.
- Recommended remedy:
  - Short term: fix the host and key format in `DevelopersContent.tsx`, or hide the page.
  - Medium term: build the developer console against existing DP endpoints (no new backend needed), then deprecate the legacy key API (PLT-002).
- Dependencies: D-07, PLT-005.
- Complexity: MEDIUM (console) / LOW (doc fix)
- Regression risk: LOW
- Confidence: CONFIRMED

### PLT-005 — API Gateway and Developer Platform are not deployed; SDK and public-doc base URL assumes the gateway at `api.cfmeet.com`
- Service/module: `infra/deployment/part-vi`, `.github/workflows/*`, `sdks/generate.py`, `scripts/contracts/public_api.py`.
- Expected: the public gateway and DP run in staging/production behind the documented host before the SDKs and docs advertise them.
- Actual:
  - `infra/deployment/part-vi/compose.part-vi.yml:1-3`: "a STAGING-READY manifest, NOT wired into any workflow". All rollout flags are off (`:24-28`, `:95-99`, `API_GATEWAY_PUBLIC_ROUTING_ENABLED: "false"`).
  - `cfmeet-core-images.yml:32` builds an `api-gateway` image but not `developer-platform`.
  - `developer-platform.yml` and `api-gateway.yml` are validation-only ("no deploy job", `deployment-integration.md:37`).
  - The deployment doc requires "a NEW public host / location in front of the gateway" (`deployment-integration.md:28`). Yet the SDK quickstart and discovery use `baseUrl: 'https://api.cfmeet.com'` (`sdks/generate.py:545,552`), and the public OpenAPI `servers` use `GATEWAY_URL = "https://api.cfmeet.com"` (`public_api.py:52`).
  - `api.cfmeet.com` today fronts meeting-service/calling via nginx (e.g. `infra/calling/production/production.py:802` fetches `https://api.cfmeet.com/api-docs/calling.json` from meeting-service Swagger).
- Evidence: the files above. I did not contact production.
- API/Swagger refs: all 572 gateway routes, `/discovery`, `/docs`.
- Affected frontend workflow: none (machine API), but blocks any developer-console work (PLT-004).
- Owner class: INFRASTRUCTURE
- Classification: DEFERRED_BY_OWNER (rollout gated by "authorized change"). The host collision is REQUIRES_VALIDATION.
- Severity: P1 (Part VI platform features exist only in code and CI)
- Business impact: the SDK quickstart and docs do not work against production. Gateway-only families (`/v1/developer-platform/**`, credential self-service, central webhooks/usage) are unavailable.
- Recommended remedy: decide the gateway hostname (either a new host or a path-level cut-over on `api.cfmeet.com`). Add `developer-platform` to the image matrix and add a GitHub Actions deploy job (no direct VPS access, per AGENTS). Then align `GATEWAY_URL` and the SDK `baseUrl`.
- Dependencies: PA trust bundle entries (`deployment-integration.md:14-26`), Redis `rediss://`, DP database roles.
- Complexity: HIGH
- Regression risk: MEDIUM (edge routing for existing calling/meeting traffic)
- Confidence: CONFIRMED (not deployed by the repo). REQUIRES_VALIDATION (live host state).

### PLT-006 — Three Swagger renderers publish contradictory "public" surfaces; calling's Swagger UI is unhardened
- Service/module: `api-gateway/src/docs.rs`, `meeting-service/src/swagger/mod.rs`, `calling/src/public_docs.rs`.
- Expected: one audience policy (the gateway catalogue classes) applied everywhere.
- Actual (computed by simulating `public_doc()` against `bundled/public/*`):
  - **Meeting-service `/swagger-ui/`** publishes 4 `CONTACT_VAULT_PRIVATE` ops (`POST/GET/PATCH /v1/privacy-relay/contacts…`, `…/disable`) that the gateway classifies EXCLUDED. It also publishes 4 calling `/v1/calling/media/rtc*` diagnostics that are not in the gateway catalogue as public.
  - Conversely it hides `/v1/messaging/{policy,legal-holds,reports,moderation,exports,deletion-requests,audit}`, `/v1/abuse-reports` and `/v1/privacy-relay/contact-access-grants` as "private engineering" (`swagger/mod.rs:930-947`). The gateway `/docs` publishes all of them (16 messaging, 1 calling, 1 privacy-relay) as PUBLIC_CUSTOMER.
  - Meeting Swagger serves Calling/Messaging/Numbering/SMS from the **service** bundles (`swagger/mod.rs:736-753`), not the gateway public bundles. That means service-host servers and auth schemes, while external callers are meant to go through the gateway.
  - **Calling `/swagger-ui/`** filters only `/internal/` (`public_docs.rs:104`). It lists the contact-vault ops it does not even host (privacy-relay process). It loads `swagger-ui-dist` from jsdelivr **without SRI** (`public_docs.rs:50`), sets `persistAuthorization: true` (tokens kept in localStorage) and `tryItOutEnabled: true` (`:59-60`), and sends no CSP. The meeting-service and gateway renderers use SRI.
- Evidence: Python simulation in this audit; the line refs above.
- API/Swagger refs: `GET /swagger-ui/`, `/api-docs/*.json` (meeting, calling); `GET /docs`, `/openapi/*.json` (gateway).
- Affected frontend workflow: none (developer docs).
- Owner class: SHARED_CONTRACT
- Classification: CONTRACT_MISMATCH
- Severity: P2
- Business impact: integrators see operations they cannot call, or miss ones they can. Private Contact Vault ingress is advertised. Calling's docs page carries a supply-chain/XSS and token-persistence risk on a public origin.
- Recommended remedy: make meeting and calling renderers serve `bundled/public/<domain>` (the gateway output) or derive filters from `route-manifest.json` classes. Add SRI/CSP to calling and disable `persistAuthorization`. If compliance ops should be private, mark them EXCLUDED in `public-routes.json` instead of hiding them ad hoc.
- Dependencies: none
- Complexity: LOW–MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### PLT-007 — Contract quality gates and the private catalogue skip the Developer Platform, Gateway and Communications contracts
- Service/module: `scripts/contracts/{bundle_openapi.py,check_swagger_labels.py}`, `scripts/swagger/build-private-docs.py`, `packages/contracts/platform-developer/**`.
- Expected: every served contract is bundled, label-checked and present in the private engineering catalogue.
- Actual:
  - `bundle_openapi.py:29-42` and `check_swagger_labels.py:14-27` list 12 sources. They exclude `platform-developer/{http,internal,gateway/http}`, `openapi/meeting-service.json`, `communications/internal` and the three `*/communications-internal` contracts.
  - Label audit of the excluded ones (this audit):
    - `platform-developer/http`: 73/73 ops use tags that are **not declared** at top level.
    - `platform-developer/internal`: 53/53 ops have no tag, and 45 have no summary.
    - `platform-developer/gateway/http`: 7/7 ops have no tag or summary.
  - Meeting-service patches this at runtime (`swagger/mod.rs` comment before the "API Gateway" block: "The source Developer Platform public bundle … omits the top-level tag declarations").
  - `build-private-docs.py:14-27` omits Developer Platform (public, internal and **operator** API), API Gateway, Communications and the communications-internal contracts. So operators have no catalogue of the operator API.
  - Script hazard: `public_api.py`, `bundle_openapi.py` and `sdks/generate.py` treat any argv other than `--check` (including `--help`) as "regenerate and write".
- Evidence: script outputs in the Scope section; the files above.
- API/Swagger refs: DP `/openapi.json` serves the internal contract (`developer-platform/src/http/mod.rs:76-78`).
- Affected frontend workflow: none.
- Owner class: SHARED_CONTRACT
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: operator/admin APIs are undocumented for engineers; DP docs render under "default".
- Recommended remedy: add the missing tags and summaries to the DP/gateway contracts, add them to both SOURCES lists and to `build-private-docs.py`, remove the runtime patch, and add `--help` / unknown-arg handling (exit non-zero without writing).
- Dependencies: none
- Complexity: LOW
- Regression risk: LOW (CI gates catch drift)
- Confidence: CONFIRMED

### PLT-008 — Credential lifecycle lacks last-used tracking and expiry warnings
- Service/module: Platform Access `identity/credentials.rs`; Developer Platform `control/credentials.rs`, `ledger`.
- Expected: create/list/rotate/revoke/scopes/expiry (present), plus last-used timestamp and expiring-credential notice, so owners can find unused keys and rotate before outage.
- Actual:
  - Present: issuance with a one-time secret (HMAC verifier only), rotation with bounded overlap, revoke, suspend/resume, scopes validated against the application, expiry within org policy, per-credential/service-account limits.
  - `CredentialView` (`platform-access/src/identity/credentials.rs:97-125`) has no last-used field. `grep -rni "last_used\|lastUsed"` over PA, DP, gateway and contracts finds nothing for credentials. The usage ledger carries `credential_id` per fact (`ledger/mod.rs:433-458`), but no "last seen" projection is exposed.
  - Platform events include `credential.created/rotated/revoked` (`platform-developer/webhooks` schema line ~125) but nothing like `credential.expiring`. `grep -i "expir"` in DP usage warnings finds nothing.
  - Credentials have a label but no update/rename op.
- Evidence: the files above.
- API/Swagger refs: `GET /v1/environments/{env}/credentials/{id}` and list.
- Affected frontend workflow: future developer console.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: credential hygiene and incident response (which key is in use? what will expire?) are not supported.
- Recommended remedy: add `last_authenticated_at`, updated by PA verification with coarse (≥1 min) debounce to avoid write amplification, exposed in `CredentialView`. Add a DP worker emitting `credential.expiring` N days ahead. This is a PA migration (additive nullable column, low risk).
- Dependencies: PA, DP worker.
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### PLT-009 — Operator/admin plane is fragmented, partly unaudited, has no UI, and audit trails are write-only
- Service/module: PA `http/admin.rs`, `identity/operator.rs`; DP `http/operator.rs`, `control/activity.rs`; meeting-service `handlers/admin.rs`, `handlers/v2/organizations.rs`; numbering `/v1/admin/**`; communications `/internal/v1/communications/admin/*`.
- Expected: operator actions are authorized, audited and reviewable through a single operator console.
- Actual:
  - Six separate admin planes:
    - meeting v1 (`UserRole::Admin`, `admin.rs:33-37`);
    - meeting v2 `/api/v2/admin/*`;
    - DP `/internal/v1/operator/**` (31 routes; `developer-platform.administer` plus `x-cf-operator`);
    - PA `/internal/v1/admin/**` (23 routes plus `platform-access-admin` CLI);
    - numbering `/v1/admin/**` (25 ops);
    - communications `/internal/v1/communications/admin/*`.
  - **No operator console exists.** `cfmeet.operator-console` (`packages/service-auth/src/identity.rs:36`) is only used by tests and `certification/` to sign calls.
  - PA writes `platform_access_audit` rows (`identity/operator.rs:187`), but no route or CLI reads them (grep for SELECT on `platform_access_audit` finds nothing).
  - DP records operator actions in tenant activity (`control/activity.rs` header) but has no operator list/search of organizations (only `GET /operator/organizations/{id}`).
  - Meeting legacy `PATCH /api/v1/admin/users/{id}` changes role/status with **no audit row** (`admin.rs:227-284`). An admin can also change their own *status*; only role self-change is blocked (`:244-251`).
- Evidence: route listings produced by `work/lib.py extract()`; the files above.
- API/Swagger refs: listed above. All are OPERATOR_ADMIN in the catalogue.
- Affected frontend workflow: none (spec: "Do not expose internal/admin migration APIs in a user-facing screen", spec line 778).
- Owner class: BACKEND / PRODUCT_DECISION
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2 (the meeting admin audit gap borders P1 for compliance)
- Business impact: operator actions on users and organizations in the meeting plane cannot be traced. Audit evidence in PA exists but cannot be retrieved without direct DB access.
- Recommended remedy: add an audit insert to meeting `update_user` (reuse the existing meeting event/audit facility). Add `GET /internal/v1/admin/audit` (PA) and `GET /internal/v1/operator/organizations?query=` (DP), read-only and operator-gated. Plan the operator console as a separate app.
- Dependencies: operator console decision.
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED

### PLT-010 — Three organization models (meeting v2 orgs, DP organizations, PA tenants) with no linkage
- Service/module: meeting-service `handlers/v2/organizations.rs`; DP `control/organizations.rs`; PA tenants.
- Expected: one organization identity, or an explicit mapping.
- Actual:
  - Meeting v2 organizations are created by platform admins (`POST /api/v2/admin/organizations`, `organizations.rs:35-58`) and consumed by the web Organizations page (`apps/web/src/services/v2/organizations.service.ts:24-66`).
  - DP organizations are self-service (`POST /v1/organizations`) and map to PA tenants.
  - No foreign key or reference connects them. The meeting directory endpoints are EXCLUDED from the gateway (`MEETING_DIRECTORY_NOT_PUBLIC_FAMILY`).
- Evidence: the files above; the manifest catalogue.
- API/Swagger refs: `/api/v2/admin/organizations*`, `/api/v2/organizations/{id}/members*`, `/v1/organizations*`.
- Affected frontend workflow: Dashboard → Organizations.
- Owner class: PRODUCT_DECISION
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: an enterprise customer must exist twice; membership and roles diverge.
- Recommended remedy: decide the authoritative org. If DP/PA, add a `platform_tenant_id` reference on meeting organizations (nullable, additive) and a sync. Do not add new public endpoints.
- Dependencies: D-07.
- Complexity: HIGH
- Regression risk: MEDIUM
- Confidence: CONFIRMED (existence). REQUIRES_VALIDATION (intended model).

### PLT-011 — Gateway scope pre-check absent for 153 credential routes
- Service/module: contracts for messaging, numbering, sms-gateway and gateway; `ingress.rs:259`.
- Expected: defence in depth, with every PLATFORM_CREDENTIAL route carrying a route scope.
- Actual: 207 of 360 credential routes have `requiredScope`. Messaging (94), numbering (52), sms (6) and `gateway.getDeveloperPlatformContext` (1) have none, because their contracts lack `x-cfmeet-required-scope`. The upstreams do enforce scopes themselves (messaging `http/middleware.rs:76 require_scope`, 28 call sites; numbering/sms `has_scope` checks, 67 sites). The SDK operation metadata therefore shows `scope: null` for these ops.
- Evidence: manifest analysis (Python Counter).
- API/Swagger refs: `/v1/conversations*`, `/v1/messaging/*`, `/v1/numbers/*`, `/v1/sms/*`.
- Affected frontend workflow: none.
- Owner class: SHARED_CONTRACT
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P3
- Business impact: an unscoped credential still costs an upstream round trip and quota; SDK users get no scope hint.
- Recommended remedy: add `x-cfmeet-required-scope` per op to those contracts (coarse family scope where per-op differs) and regenerate.
- Dependencies: PLT-001 generator validation.
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### PLT-012 — Gateway rate limiting covers only platform-credential traffic; stale config comment
- Service/module: `api-gateway/src/http/ingress.rs:265-300`, `quota.rs`, `config/mod.rs:56-57`.
- Expected: some abuse protection (per-IP or per-token) on gateway passthrough routes (MEETING_SESSION, PARTICIPANT_TOKEN, ANONYMOUS, CLIENT_TOKEN), or a documented decision to leave it to services or the edge.
- Actual: quotas run only `if let (Forward::Credential(customer), Some(engine))`. 212 routes accept no platform credential at all (MEETING_SESSION / PARTICIPANT_TOKEN / DEVELOPER_SESSION / SIGNALING / CLIENT_TOKEN / REALTIME_TICKET / ANONYMOUS) and get no gateway throttling. `config/mod.rs:56-57` still says "Configuration only: no quota is enforced by this foundation", which is false since §54.
- Evidence: the lines above. Auth-mode counts come from the manifest.
- API/Swagger refs: e.g. `/api/v2/**` meeting routes, `/api/v2/communications/**`.
- Affected frontend workflow: none.
- Owner class: INFRASTRUCTURE
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P3
- Business impact: rate limiting depends on each service and the nginx edge.
- Recommended remedy: document the decision; optionally add a per-IP token bucket for ANONYMOUS/PARTICIPANT_TOKEN routes in Redis; fix the comment.
- Dependencies: PLT-005.
- Complexity: LOW
- Regression risk: LOW
- Confidence: CONFIRMED

### PLT-013 — SDKs: Dart is untyped and first-party apps don't use the SDKs
- Service/module: `sdks/generation.json`, `sdks/generate.py`, `apps/web`, `apps/mobile`.
- Expected (`generation.json` description): "first-party products and external customers use the same SDKs".
- Actual: the TS SDK has generated models (`typescript/src/generated/models.ts`, 458 KB). Dart has only contracts and an operation table (`generation.json` dart: no `models`). Its quickstart is string-dispatched: `client.domain('gateway').call('getDeveloperPlatformContext')` (`sdk-discovery.json`). `grep -rln "@cfmeet/platform-sdk\|cfmeet_platform_sdk" apps services` finds nothing: web and mobile use hand-written services. Staleness is well controlled (`--check` in `api-gateway.yml:76`, `sdk-dart.yml:105`, `sdk-release.yml:42`, `sdk-typescript.yml:54`; passes now).
- Evidence: the files above.
- API/Swagger refs: all 572 ops.
- Affected frontend workflow: none.
- Owner class: SHARED_CONTRACT
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P3
- Business impact: Dart integrators lose type safety, and SDK regressions are not caught by first-party usage.
- Recommended remedy: generate Dart models; adopt the TS SDK for developer-console work (PLT-004).
- Dependencies: none
- Complexity: MEDIUM
- Regression risk: LOW
- Confidence: CONFIRMED


### Answers to the platform key questions


1. **Manifest consistency.** `public_api.py --check` passes: the manifest equals contracts plus the allow-list. My router cross-check found **no manifest route pointing to a non-existent upstream path or method** (572/572; 72 via the communications relay/dispatch). One manifest defect is semantic: the bogus `requiredScope` (PLT-001). No CI check verifies upstream router existence (contracts are checked against routers per service: DP `foundation.rs:422` for GET routes only, calling `http_contract.rs`, meeting `route_coverage.py --strict`).
2. **SDKs.** Generated from current contracts, with the staleness check passing and enforced in 4+ workflows. 572 operations per SDK by domain: meetings 130, calling 124, messaging 96, communications 73, developer-platform 73, numbering 52, privacy-relay 17, sms 6, gateway 1. 10 paid-telecom ops are never auto-retried. Version 0.1.0 beta.
3. **Overlap.** Yes. Meeting-service legacy API keys and webhooks coexist with PA/DP credentials and DP plus Calling webhooks. They are separate stores with no linkage or migration (only Calling → DP webhook migration exists). The web UI consumes only the legacy ones (PLT-002/003/004).
4. **Swagger.**
   - Gateway `/docs`, `/openapi.json`, `/openapi/{domain}.json`, `/discovery`: unauthenticated, generated public bundles, SRI, no try-it-out, parity-tested.
   - Meeting-service `/swagger-ui/` plus `/api-docs/*.json`: unauthenticated, CSP and SRI, filtered at runtime with its own rules.
   - Calling `/swagger-ui/`, `/api-docs/{calling,privacy-relay}/openapi.json`: unauthenticated, no SRI, try-it-out.
   - DP `/openapi.json` (internal contract, internal listener) and messaging `/openapi.json`.
   - Private catalogue: htpasswd-protected nginx on the VPS (`private-docs-vps.yml`).
   - Handler match: meeting contract == `export_openapi` output (CI diff `backend.yml:180-181`); DP served == contract (`foundation.rs:422`). Publication policies differ (PLT-006).
5. **Admin/operator.** Listed in PLT-009. Missing: an operator console, audit read APIs, org search, and an audit trail for meeting v1 admin actions.
6. **Credential lifecycle.** Create, list, get, rotate (with overlap), revoke, suspend/resume, scopes, expiry with org policy and limits are all present and cert-tested. Last-used and expiry warnings are missing (PLT-008). Legacy keys have create/list/get/revoke only, plaintext storage, fixed 20-day/100-use limits (PLT-002).
7. **Usage/limits/rate limiting.** The gateway enforces environment, service-account and credential minute/day quotas via Redis Lua (degrading to per-instance counters, never unlimited). It flushes durable counts to DP and exposes `x-cf-quota-*` headers. DP has the unified ledger (usage, facts, export, warnings, limits). `/v1/usage` and `/v1/limits` still default to Calling until `API_GATEWAY_CENTRAL_USAGE_ENABLED`. Non-credential routes are not throttled at the gateway (PLT-012).


## Security, reliability, production readiness


- **meeting-service** (legacy `/api/v1` + `/api/v2`): `src/middleware/{auth,api_key,platform_auth,legacy_write}.rs`, `src/services/{auth,actor,communications,livekit,rate_limiter,otp,realtime,rtc_provider}.rs`, `src/handlers/{auth,meetings,participants,admin,api_keys,webhooks,users,internal}.rs`, `src/routes/*.rs`, `src/config/{app,operations}.rs`, `src/services/artifacts/storage.rs`, `src/utils/datetime.rs`, `src/models/*`, all 27 migrations in `migration/src/`.
- **packages/service-auth** (`lib.rs`, `claims.rs`, `verifier.rs`, `jwt.rs`), **packages/webhooks** (`endpoint.rs`), **packages/object-storage** (`lib.rs` TTL caps).
- **api-gateway** `src/identity.rs`, `src/quota.rs` (headers only).
- **platform-access** `src/http/admin.rs`, `src/http/middleware.rs`, `src/config/mod.rs`, `src/identity/{verifier,credentials}.rs`, migrations.
- **communications** `src/{api,auth,store,crypto,policy,relationships}.rs`, `migration/src/lib.rs`, `migration/sql/{001_foundation,002_unified}.sql`.
- **messaging** `src/config/mod.rs` (E2EE gate), `src/attachments/{mod,derivatives}.rs` (authz), `src/realtime/push_adapters.rs`, migrations `lib.rs`.
- **calling** `src/media/livekit.rs` (grants/TTLs), `src/communications.rs` (launch credential), `src/main.rs`, migrations `lib.rs` + `m…000044_platform_access_projection.rs`, `communications.rs`.
- **developer-platform** `src/control/members.rs`, migrations; **privacy-relay** `m…000007_contact_fingerprints.rs`; **sms-gateway/telephony** provider adapters list.
- Infra/ops: `docker-compose.yml`, `docker-compose.prod.yml`, all `services/*/.env.example` (located only), `infra/livekit/*`, `infra/deployment/{backend_runtime.py,backup/*.sh,postgres/provision-service-database.sh}`, `infra/calling/production/{production,communications_release}.py` (grep), `.github/workflows/{deploy-backend,production-v2-cutover,cfmeet-calling-production,cfmeet-certificate-monitor,dependency-security,service-boundaries}.yml` + list of all 70 workflows, `scripts/secret-scan/scan_secrets.py`.
- Docs: `docs/operations/cfmeet-sections-8-23-final-closure.md`, `docs/operations/tls-certificate-automation.md`, `docs/phase2-certification/14-operational-runbook.md` (§83–84), `docs/architecture/part-j-persistence-deployment-certification.md` (grep).
- Frontend only where it touches token handling: `apps/web/src/hooks/useGoogleCallback.ts`, `apps/web/src/services/{api,auth.service}.ts`.

Not exhaustively inspected (sampled only): every IDOR candidate in calling (91 single-id queries), numbering (49), telephony (37), messaging (44), communications (27); realtime WebSocket authz in calling/messaging; telephony/sms provider callback signature code. Where a claim rests on sampling it is marked REQUIRES_VALIDATION.

Method note: the newer Part III–VI services (platform-access, api-gateway, calling, messaging, communications, developer-platform, privacy-relay) are consistently engineered (Ed25519 service tokens, peppered credential hashes, scope-bound HMAC fingerprints, SSRF-safe webhooks, fail-closed authority lookups, TIMESTAMPTZ everywhere, per-service migration tables, least-privilege DB roles). **The concentrated risk is the legacy account authority in meeting-service** (users, login, JWT, API keys, `/api/v1`), which every other service ultimately trusts for first-party user identity.

---



### SEC-001 — Hard-coded JWT signing-secret fallback in meeting-service (account authority)
- Service/module: meeting-service `src/config/app.rs`
- Expected: Service refuses to start without a strong `JWT_SECRET` in non-local runtimes (as platform-access does for its pepper: `services/platform-access/src/config/mod.rs:492-505`, `src/identity/verifier.rs:196-210` reject the identical placeholder).
- Actual: `config/app.rs:48-49` — `std::env::var("JWT_SECRET").unwrap_or_else(|_| "your-secret-key-change-this-in-production"...)`. No `APP_ENV`/production check anywhere (`grep -n production src/main.rs src/config/*.rs src/state.rs` → only this line). The same placeholder is in `services/meeting-service/.env.example:27`. The realtime ticket key is derived from the same secret (`services/realtime.rs:336-337`).
- Evidence: `grep -rn "your-secret-key-change-this" services/` → app.rs:49, .env.example:27 (and platform-access tests that *reject* it). `docker-compose.prod.yml:91` requires `JWT_SECRET` (`:?`), but production is deployed by `deploy-backend.yml` → `infra/deployment/backend_runtime.py`, which clones the running container's env; whether that env contains a strong value cannot be verified from the repo.
- API/Swagger refs: all `/api/v1/**` and `/api/v2/**` bearer-authenticated operations.
- Affected frontend workflow: every signed-in web/mobile flow.
- Owner class: BACKEND
- Classification: IMPLEMENTED_UNVERIFIED (unsafe default)
- Severity: P0 (if the env var is ever absent, anyone can mint admin tokens: `role` is in the token but `auth_middleware` re-reads role from DB — however `sub` of any user, including admins, can be forged → full account takeover).
- Business impact: total compromise of all CF Meet accounts and everything delegated from them (Hub, Calling, Messaging).
- Recommended remedy: fail startup when `JWT_SECRET` is missing, shorter than 32 bytes, or on a placeholder deny-list (reuse platform-access's `CredentialPepper::parse` rules); support key ids for rotation. Add a release-time check in `backend_runtime.py` that asserts presence (not value).
- Dependencies: none. Complexity: LOW. Regression risk: LOW (fails only misconfigured deployments).
- Confidence: CONFIRMED (code); production value REQUIRES_VALIDATION.

### SEC-002 — Access and refresh tokens are interchangeable (refresh token = 7-day bearer token)
- Service/module: meeting-service `src/services/auth.rs`, `src/middleware/auth.rs`, `src/services/actor.rs`
- Expected: distinct token types (`typ`/`aud`), access tokens rejected at `/refresh`, refresh tokens rejected as bearer.
- Actual: `Claims` (`auth.rs:7-15`) has only `sub, role, exp, iat, auth_time`; `generate_access_token` (15 min) and `generate_refresh_token` (7 days) differ only in `exp` (`auth.rs:32-48`); `verify_token` uses `Validation::default()` (`:88-96`). `auth_middleware` (`middleware/auth.rs:44-47`) and v2 `actor.rs:265-270` accept any valid token. So a refresh token works as an access token for 7 days, and any access token can mint a new pair at `/refresh` (`handlers/auth.rs:319`).
- Evidence: code quoted; `grep -n "typ\|token_type" src/services/auth.rs` → none. (Contrast: realtime tickets use a separate key and `typ == "rt"`, `services/realtime.rs:336,524`.)
- API/Swagger refs: POST /api/v1/auth/refresh; all bearer routes.
- Affected frontend workflow: session handling (`apps/web/src/services/api.ts:85-95`).
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1
- Business impact: stolen refresh token (stored in JS-readable cookie store, and passed in a URL by OAuth — SEC-005) grants a week of API access; logout of that token does not stop it on v1 routes (SEC-003).
- Recommended remedy: add `typ` (`access`/`refresh`) and `jti`; reject wrong type in middleware/refresh; rotate refresh tokens with reuse detection (store `jti` family in Redis/DB). Existing tokens lacking `typ` can be treated as access-only during a 15-minute transition, forcing re-login for refresh after deploy.
- Dependencies: frontend must handle forced re-login once. Complexity: MEDIUM. Regression risk: MEDIUM (mobile/web refresh path).
- Confidence: CONFIRMED

### SEC-003 — Logout / password change / reset do not revoke refresh tokens on legacy routes; password change/reset revoke nothing
- Service/module: meeting-service `handlers/auth.rs`, `middleware/auth.rs`
- Expected: logout and credential change invalidate all refresh tokens; every auth path checks the revocation epoch.
- Actual: logout blacklists only the presented access token (`handlers/auth.rs:384-418`) and, when the Hub bridge is configured, advances `credentials_not_before` (`services/communications.rs:651-673`). `require_fresh_credentials` is enforced in `/refresh` (`handlers/auth.rs:347-354`) and v2 actor (`actor.rs:321-327`) but **not** in v1 `auth_middleware` (`middleware/auth.rs:23-107`). Combined with SEC-002, a refresh token remains usable as a v1 bearer for its full 7 days after logout. `reset_password` / `change_password` (`handlers/auth.rs:~600-766`) only update the hash — no blacklist, no epoch advance (grep `record_logout|credentials_not_before` in handlers → only logout). Without the Hub (`state.communications.is_none()`), the epoch check is a no-op (`communications.rs:709-711`).
- Evidence: `grep -n "record_logout\|revoke_sessions\|credentials_not_before\|blacklist" src/handlers/*.rs src/services/*.rs`.
- Owner class: BACKEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1 (attacker with a stolen token keeps access after the victim resets the password).
- Recommended remedy: move `credentials_not_before` to an account-owned column on `users` (not Hub-dependent), advance it on logout, password change/reset, deactivation, admin suspend; check it in `auth_middleware`, actor, refresh and realtime ticket issue.
- Dependencies: SEC-002. Complexity: LOW–MEDIUM. Regression risk: LOW.
- Confidence: CONFIRMED

### SEC-004 — Guest JWTs can be refreshed indefinitely without the participant still existing
- Service/module: meeting-service `handlers/auth.rs:332-334`, `handlers/meetings.rs:900-975`
- Expected: guest credentials are bound to a live, non-kicked participant and expire with the meeting.
- Actual: `/refresh` with `role == "guest"` skips all checks and issues a new 15-min/7-day pair (`handlers/auth.rs:332-334,355-358`). v1 `auth_middleware` only checks the participant row exists (`middleware/auth.rs:50-60`), not its status (kicked/left). Guests receive refresh tokens even in the waiting room (`meetings.rs:968-975`).
- Owner class: BACKEND. Classification: PARTIALLY_IMPLEMENTED. Severity: P2.
- Business impact: kicked/denied guests keep an authenticated identity (e.g., `PATCH /api/v1/participants/{id}/audio` of their own row) indefinitely.
- Recommended remedy: on guest refresh/auth, load participant and require `status ∈ {Waiting, Joined}`, `leave_time IS NULL`, meeting not ENDED; cap guest refresh lifetime to meeting end. Complexity: LOW. Regression risk: LOW.
- Confidence: CONFIRMED

### SEC-005 — Google OAuth: no `aud`/`email_verified` check, link-by-email, tokens returned in URL query string
- Service/module: meeting-service `handlers/auth.rs:838-1049`; web `apps/web/src/hooks/useGoogleCallback.ts`
- Expected: verify ID token (`aud == client_id`, `iss`, `exp`, `email_verified == true`); link accounts by Google `sub`; deliver session via HttpOnly cookie or one-time code, never URL.
- Actual: ID token decoded without signature ("basic decoding without signature verification for now", `:950`); only `iss` checked (`:977-980`); `aud`, `exp`, `email_verified` ignored; `sub` parsed then discarded (`_google_sub`, `:971-974`). Existing user found by exact email (`:984-987`) is logged in and a `PendingVerification` local account is auto-activated. Callback redirects to `{app}/google/callback?access_token=…&refresh_token=…` (`:874-885`), which the web reads from `useSearchParams` (`useGoogleCallback.ts:29-30`). (Skipping signature verification is tolerable only because the token comes straight from Google's token endpoint over TLS; the missing `aud`/`email_verified` and URL leakage are the defects.)
- Owner class: BACKEND + FRONTEND. Classification: PARTIALLY_IMPLEMENTED. Severity: P1.
- Business impact: (a) refresh tokens (7-day, SEC-002) land in browser history, Referer headers and hosting logs (Vercel); (b) account linking relies on email equality without `email_verified`; (c) no stable provider identity.
- Recommended remedy: validate claims (`aud`, `exp`, `email_verified`), persist `auth_provider_subject` (DATA-016), link by subject; replace query-string tokens with a short-lived one-time code exchanged by POST (or HttpOnly cookie). Complexity: MEDIUM. Regression risk: MEDIUM (web callback page).
- Confidence: CONFIRMED

### SEC-006 — Legacy API keys stored and looked up in plaintext; non-atomic quota counter
- Service/module: meeting-service `handlers/api_keys.rs:27-111`, `middleware/api_key.rs:131-183`, `migration/src/m20260116_000008_create_api_keys.rs:31-48`
- Expected: store only a peppered hash (as platform-access does), constant-time compare, atomic `UPDATE … SET used_count = used_count + 1 WHERE used_count < usage_limit`.
- Actual: 64-char key stored in `api_keys.api_key` and matched with `Column::ApiKey.eq(header)` (`api_key.rs:143-147`); quota read-modify-write (`:163-173`) allows over-use under concurrency. `combined_auth_middleware` (`api_key.rs:189-259`) is dead code (no route uses it) and, if ever mounted, skips blacklist and account-status checks for JWTs.
- Owner class: BACKEND. Classification: PARTIALLY_IMPLEMENTED. Severity: P1 (DB read = usable credentials).
- Recommended remedy: add `api_key_hash` + `key_prefix`, backfill hashes, drop plaintext column (two-step migration; risk: one-way, keys cannot be shown again — already only shown once). Atomic conditional update. Delete `combined_auth_middleware`. Long-term: retire in favour of Platform Access credentials (already the Part VI path).
- Complexity: LOW–MEDIUM. Regression risk: LOW. Confidence: CONFIRMED

### SEC-007 — v2 actor honours tokens when Redis revocation check fails (fail-open by default)
- Service/module: meeting-service `src/services/actor.rs:272-285`
- Actual: Redis error → `revocation_checked = false`, request continues; only "strict security profiles" fail closed (`meeting_core.rs:382`, `unified_messaging.rs:51`, `communications.rs:234`). v1 middleware fails closed (`middleware/auth.rs:39` propagates the error) — inconsistent.
- Owner class: BACKEND. Classification: IMPLEMENTED_UNVERIFIED (deliberate trade-off). Severity: P3 (bounded by 15-min access TTL once SEC-002 is fixed; P2 until then because refresh tokens are 7-day bearers).
- Recommended remedy: keep but document; after SEC-002 the residual risk is ≤15 min. Confidence: CONFIRMED

### SEC-008 — Brute-force/enumeration controls are per-email only
- Service/module: meeting-service `services/rate_limiter.rs:11-60`, `services/otp.rs:17-133`, `handlers/auth.rs:221,480,538,552`
- Actual: login limited to 5 attempts / 15 min **per email** (key lowercased while lookup is case-sensitive — DATA-002); no per-IP/global throttle → (a) password spraying across many accounts is unthrottled, (b) any third party can lock out a victim's login for 15 min with 5 bad attempts. `forgot-password` returns 404 "No account found with this email address" (`:538`) and register 409 → account enumeration. Communications discovery is per profile (120/h) — sybil accounts multiply it, but 128-bit CF-IDs make blind enumeration infeasible (`communications/src/crypto.rs:6`).
- Owner class: BACKEND / INFRASTRUCTURE. Classification: PARTIALLY_IMPLEMENTED. Severity: P2.
- Recommended remedy: add per-IP (trusted proxy header) and global sliding windows (Redis), progressive delay instead of hard lockout, uniform responses for forgot-password; optional CAPTCHA after N failures. Complexity: LOW. Regression risk: LOW. Confidence: CONFIRMED

### SEC-009 — Legacy kick does not evict media; legacy LiveKit tokens are 6 h with client-side `room_admin` for hosts
- Service/module: meeting-service `services/livekit.rs:14,46-77`, `handlers/participants.rs:142-200`
- Actual: `TOKEN_TTL_SECS = 6h`; host token carries `room_admin` (client can mute/remove anyone outside server audit). `kick_participant` updates the DB row and session log only — no LiveKit `RemoveParticipant`, so the kicked user stays in the media room and can reconnect with the still-valid token until it expires. (v2/Calling are correct: Calling 30 s upstream, ≤900 s resealed, admin 60 s, no user `roomAdmin`, `calling/src/media/livekit.rs:60-68,399-408`.)
- Classification: PARTIALLY_IMPLEMENTED. Severity: P1 while `/api/v1` writes are enabled (`FEATURE_LEGACY_MEETING_WRITE_DISABLED` default false, `config/features.rs:75`; production value not in repo), else P3.
- Recommended remedy: either set `FEATURE_LEGACY_MEETING_WRITE_DISABLED=true` in production (preferred, v2 exists), or call the provider remove API on kick and cut TTL to ≤1 h without `room_admin`. Complexity: LOW. Confidence: CONFIRMED (code); production flag REQUIRES_VALIDATION.

### SEC-010 — No backup coverage for meeting-service, communications and developer-platform databases; no scheduled backups in repo
- Service/module: `infra/deployment/backup/backup.sh:22-24`, `.github/workflows/*`
- Expected: scheduled, encrypted, tested backups for every owner DB, including the account authority.
- Actual: `backup.sh` accepts only calling, platform-access, numbering, privacy-relay, telephony, sms-gateway, messaging. It also requires a `<service>_seaql_migrations` table (`:30-36`) — meeting-service uses the default `seaql_migrations`, so it cannot be backed up with this tool even if added to the list. No workflow or script schedules backups (`grep -rln "pg_dump\|backup.sh"` → only the scripts, docs and CI restore tests; only cron schedules in workflows are the certificate monitor and weekly dependency scan). DR certification exists in CI for calling (`services/calling/tests/disaster_recovery.rs`), numbering, privacy-relay, sms-gateway. Closure doc §10 records off-server backup as DEFERRED_BY_OWNER and says "same-host copies only" — but the same-host copies documented are **key** recovery copies (`docs/operations/communications-runtime-recovery.md:13`), not database dumps.
- Owner class: INFRASTRUCTURE. Classification: PARTIALLY_IMPLEMENTED (off-server part DEFERRED_BY_OWNER). Severity: P0 for meeting-service (users/passwords/meetings have no backup path at all), P1 for others.
- Business impact: disk/DB corruption or an erroneous migration loses all accounts irrecoverably.
- Recommended remedy: extend `backup.sh` to meeting-service (detect `seaql_migrations` + `users`), communications (`communications_seaql_migrations`), developer-platform; add a GitHub Actions scheduled same-host dump + restore-verify job (respects owner decision on off-server); add restore drills to CI for meeting-service. Complexity: MEDIUM. Regression risk: LOW.
- Confidence: CONFIRMED (repo); host-level cron outside repo REQUIRES_VALIDATION.

### SEC-011 — Production migrations run without a pre-migration snapshot; rollback is container-only
- Service/module: `infra/deployment/backend_runtime.py:51-62`, `.github/workflows/deploy-backend.yml:162-245`, `infra/calling/production/communications_release.py` (no `pg_dump`)
- Actual: `release()` runs `/app/migration up` (and `account-communications-migration up`) against production, then swaps containers; on failure it restores the previous **container** only. No dump is taken first (`grep -n "pg_dump\|backup" communications_release.py messaging_release.py` → none). Closure §9 row 14 claims migrations "rollback-safe": true only for application compatibility of additive migrations; a faulty data migration (e.g. `m20261003_000002_phase2_legacy_backfill.rs`) is not reversible. Meeting-service images are also built on the VPS from a `git reset --hard` checkout (`deploy-backend.yml:216-235`), unlike the registry-digest flow used by the Communications release.
- Owner class: INFRASTRUCTURE. Classification: PARTIALLY_IMPLEMENTED. Severity: P1.
- Recommended remedy: take a verified `pg_dump` (custom format + sha256) of the affected DB immediately before `migration up` in every release workflow and record its path in the release evidence; move meeting-service to the immutable registry image flow. Complexity: LOW–MEDIUM. Confidence: CONFIRMED

### SEC-012 — Internal service tokens are not bound to method/path/body and replay is only blocked by 2 receivers
- Service/module: `packages/service-auth/src/claims.rs:32-47`, receivers
- Actual: claims bind issuer, audience, env, ≤300 s lifetime (+30 s skew), jti, request/correlation ids, delegated context — not HTTP method/path/body digest. Only communications (`src/auth.rs:59-68`, durable nonce table) and scan gateway (`lib.rs:256-268`) reject replayed `jti`. A token captured on the private network (or logged by a proxy) can be replayed to any route of the same audience granting the same permission within ~5.5 min.
- Owner class: SHARED_CONTRACT. Classification: IMPLEMENTED_UNVERIFIED. Severity: P2 (defence-in-depth; private network + TLS reduce exposure).
- Recommended remedy: add optional `htm`/`htu`/`body_sha256` claims (DPoP-like) verified by receivers, and a shared Redis/DB `jti` cache helper in `cf-service-auth` used by all receivers for mutating routes. Complexity: MEDIUM. Regression risk: MEDIUM (all services). Confidence: CONFIRMED

### SEC-013 — No security audit trail for account, admin and API-key events in meeting-service
- Service/module: meeting-service handlers `admin.rs`, `auth.rs`, `api_keys.rs`, `users.rs`
- Actual: `grep -c "audit\|tracing::"` → admin.rs 0, api_keys.rs 0, users.rs 0, auth.rs 5. Admin role/status changes (`admin.rs:~227-280`), API key create/revoke, password change/reset, OAuth link, deactivation leave no durable record. Other services have append-only audit tables (`platform_access_audit`, `platform_operator_audit`, `enterprise_admin_audit`, `messaging_operator_audit`, `conference_audit_events` with immutability trigger `m20261003_000001…:472-478`). Platform Access operator identity is the self-asserted `x-cf-operator` header from a trusted console service (`platform-access/src/http/admin.rs:1-10`) — acceptable but attribution depends on console integrity.
- Owner class: BACKEND. Classification: PARTIALLY_IMPLEMENTED. Severity: P1 (forensics for account takeover impossible).
- Recommended remedy: new `account_audit_events` table (append-only trigger, TIMESTAMPTZ, actor_user_id, subject_user_id, action, request_id, ip hash) written in the same transaction (DATA-015). Complexity: LOW–MEDIUM. Confidence: CONFIRMED

### SEC-014 — Meetings v2 creates lack Idempotency-Key; legacy v1 writes have check-then-act races
- Service/module: meeting-service
- Actual: idempotency only in polls (`collaboration/polls.rs:1344-1365`), webinar registration (`specialized.rs:549-619`), artifacts (`uq_artifact_idempotency`). No Idempotency-Key handling on meeting-space/conference/invitation creation (`grep -rn Idempotency src/handlers/v2` → only the messaging relay). Legacy screen-share start (`participants.rs:657-720`) checks "another sharer" then updates without lock/unique index → two concurrent sharers; API key quota RMW (SEC-006). Other services (calling 17 files, numbering, sms, telephony, messaging, devplat, Hub) implement idempotency records.
- Owner class: BACKEND / SHARED_CONTRACT. Classification: PARTIALLY_IMPLEMENTED. Severity: P2.
- Recommended remedy: reuse the developer-platform/calling idempotency-record pattern (`devplat_idempotency_records`) as `meeting_idempotency_records` and accept `Idempotency-Key` on POST creates; partial unique index `participants(meeting_id) WHERE is_screen_sharing`. Complexity: MEDIUM. Confidence: CONFIRMED

### SEC-015 — Migration binaries ship the full sea-orm CLI (down/fresh/reset/refresh) in production images
- Service/module: all `services/*/migration/src/main.rs` and `communications-main.rs` (`cli::run_cli(...)`)
- Actual: `fresh`/`refresh`/`reset` drop everything; most `down()` functions execute `DROP TABLE` (`calling/m…000005_call_core.rs:258`, all meeting-service legacy downs, platform-access identity tables). Only communications and the Hub-integration histories refuse `down`. Releases invoke `/app/migration up` inside the production image (`backend_runtime.py:59-61`), so the destructive commands are one typo away, run with an owner/DDL role.
- Owner class: BACKEND / INFRASTRUCTURE. Classification: IMPLEMENTED_UNVERIFIED (unsafe tool exposure). Severity: P2.
- Recommended remedy: wrap `run_cli` so production runtime (`CF_RUNTIME_ENVIRONMENT=production`) accepts only `up`/`status`; make data-bearing `down()`s refuse (forward-only, like communications). Complexity: LOW. Regression risk: LOW. Confidence: CONFIRMED

### SEC-016 — No alert rules / paging as code; alert thresholds still "APPROVAL REQUIRED"
- Service/module: ops — `docs/phase2-certification/14-operational-runbook.md:190-236`
- Actual: services expose Prometheus metrics (`/internal/metrics`, token-protected `handlers/internal.rs:84-98`), but there is no Prometheus/Alertmanager/Grafana config in `infra/` (`find infra -iname '*prometheus*' -o -iname '*alert*'` → none). The runbook says "The repository has no dashboard-as-code" and most CRITICAL alert thresholds are **APPROVAL REQUIRED**. The only automated operator notification is the TLS monitor posting to GitHub issue #38 (closure §8).
- Owner class: INFRASTRUCTURE / PRODUCT_DECISION (thresholds). Classification: PARTIALLY_IMPLEMENTED. Severity: P1 for production readiness (DB down, outbox stuck, join failure would go unnoticed).
- Recommended remedy: owner approves thresholds; add a scheduled GitHub Actions health/metrics probe (consistent with "GitHub Actions only" rule) that opens/updates an issue, or a minimal Prometheus + Alertmanager on the VPS. Complexity: MEDIUM. Confidence: CONFIRMED

### SEC-017 — TLS automation covers only the `api.cfmeet.com` lineage
- Service/module: `docs/operations/tls-certificate-automation.md:1-30`, `.github/workflows/cfmeet-certificate-monitor.yml` (cron `23 4 * * *`)
- Actual: renewal, activation hook, daily monitor and notification are scoped to `api.cfmeet.com` ("this lineage only"). Other names appear in infra (`livekit.cfmeet.com`, `rtc.cfmeet.com` in `cfmeet-media-readiness-audit.yml:52`; `internal-docs.cfmeet.com`). If any is served with its own certificate (e.g., TURN/TLS for LiveKit), it is unmonitored. Closure §8 claim (renewal dry-run exit 0, WARN band 45 days, expiry 2026-12-21) is consistent with the runbook but not verifiable from the repo.
- Owner class: INFRASTRUCTURE. Classification: IMPLEMENTED_UNVERIFIED. Severity: P2.
- Recommended remedy: inventory all served certificate names (`private-tls-inspect` already lists consumers) and extend the monitor to each lineage. Complexity: LOW. Confidence: REQUIRES_VALIDATION (whether other lineages exist)

### SEC-018 — Meeting-service runtime DB role has DDL; meeting-service excluded from least-privilege provisioning
- Service/module: `infra/deployment/postgres/provision-service-database.sh:15,37`; `backend_runtime.py:57-61`
- Actual: the two-role model (`<svc>_migrator` vs runtime without DDL) covers communications, calling, platform-access, numbering, privacy-relay, telephony, sms-gateway, messaging, developer-platform — **not meeting-service**. Release runs migrations with `migration_env = env_file(..., p.env_of(c))`, i.e. the runtime container's `DATABASE_URL`, proving the runtime role holds DDL.
- Owner class: INFRASTRUCTURE. Classification: PARTIALLY_IMPLEMENTED. Severity: P2 (SQL-injection blast radius; destructive-migration risk).
- Recommended remedy: provision `meeting_service_migrator` + runtime role, transfer ownership, pass a separate migration URL in release. Migration risk: ownership transfer requires a short maintenance step (`ALTER … OWNER`), reversible. Complexity: MEDIUM. Confidence: CONFIRMED

### SEC-019 — Duplicate S3 presign implementation in meeting-service
- Service/module: `meeting-service/src/services/artifacts/storage.rs:289-310` vs `packages/object-storage/src/lib.rs:78-80,886-930`
- Actual: meeting-service has its own SigV4 presigner; TTL is capped by config clamp (30–3600 s, default 300, `config/operations.rs:104-105`) rather than the shared library's `MAX_SIGNED_READ` (900 s). Two implementations of a security-sensitive primitive.
- Classification: IMPLEMENTED_UNVERIFIED. Severity: P3. Recommended remedy: migrate artifacts to `cf-object-storage`. Complexity: MEDIUM. Confidence: CONFIRMED

### SEC-020 — Tenant-isolation sweep not complete; sampled handlers are correct
- Service/module: calling, numbering, telephony, messaging, communications, developer-platform, privacy-relay
- Actual: counts of single-column `WHERE id = $1` queries: calling 91, numbering 49, messaging 44, telephony 37, developer-platform 33, communications 27, privacy-relay 17. All sampled public paths were scoped (messaging attachments → `access::as_member` + `visible_from_sequence`; devplat members `AND tenant_id = $2`; v2 meeting platform guard 404 on foreign env; calling uses composite tenant FKs). Many single-id queries are worker internals after an authorized lookup.
- Owner class: BACKEND. Classification: IMPLEMENTED_UNVERIFIED. Severity: P2 (assurance gap, not a found defect).
- Recommended remedy: add a repository guard (like `scripts/service-boundaries`) or per-service negative tests "other tenant's id → 404" for every public `/{id}` route generated from the route manifest. Complexity: MEDIUM. Confidence: REQUIRES_VALIDATION

### SEC-021 — Tokens stored in JS-readable cookies on web
- Service/module: `apps/web/src/services/auth.service.ts:44-67` (`cookieStore.setTokens`)
- Actual: access and refresh tokens are written by client JS → readable by any XSS. Combined with SEC-002 (7-day bearer refresh token).
- Owner class: SHARED_CONTRACT (FRONTEND + BACKEND). Classification: PARTIALLY_IMPLEMENTED. Severity: P2.
- Recommended remedy: backend sets refresh token as HttpOnly/SameSite=strict cookie scoped to `/api/v1/auth/refresh`; access token in memory. Complexity: MEDIUM. Confidence: CONFIRMED (frontend call sites); cookie flags REQUIRES_VALIDATION.

### SEC-022 — External dependencies (not software defects)
- PRIVATE_E2EE production: gate enforced in code (`messaging/src/config/mod.rs:466-483`, `true` refused in production) — blocked on independent assessment (#13). EXTERNAL_DEPENDENCY.
- Push: APNs/FCM/Web Push adapters implemented (`messaging/src/realtime/push_adapters.rs`), tested against fakes only; no credentials. EXTERNAL_DEPENDENCY.
- SMS carrier: only `providers/{contract,deterministic}.rs` — no real carrier adapter exists yet; needs a carrier contract **and** an adapter (software work after the contract). EXTERNAL_DEPENDENCY + MISSING.
- Telephony: `providers/sip_trunk.rs` exists; public PSTN off in production (closure §6). EXTERNAL_DEPENDENCY (carrier/numbers).
- Transparency witnesses for messaging KT: none in production (closure §7). EXTERNAL_DEPENDENCY.
- Severity: n/a (tracked). Confidence: CONFIRMED.

### Closure report vs code (cfmeet-sections-8-23-final-closure.md)
| Claim | Code/repo evidence | Assessment |
|---|---|---|
| E2EE gate not bypassed | `messaging/src/config/mod.rs:466-483` | Matches |
| Calling launch credential re-verified on every launch, fails closed | `calling/src/communications.rs:475-515` (authenticate_delegated, 503 on reject/unavailable, requires `calls.write`) | Matches |
| Signed service auth on private routes | `packages/service-auth`, communications `auth.rs` | Matches; replay protection only in 2 receivers (SEC-012) |
| §14 migrations "rollback-safe" | Separate additive histories keep old images bootable; no DB snapshot before migration | Application-rollback only (SEC-011, DATA-011) |
| Off-server backups DEFERRED_BY_OWNER, "same-host copies only" | Same-host copies documented are **keys**; no scheduled DB dumps; meeting-service not coverable by `backup.sh` | Understated gap (SEC-010) |
| TLS automation in production | Runbook + monitor workflow present | Plausible; scope limited to api.cfmeet.com (SEC-017) |
| Section 19 "VPS capacity and operations PRODUCTION_VERIFIED" | Inventory/recovery workflows exist; no alerting as code | Monitoring gap (SEC-016) |
| Meeting-service account security | Not addressed by the closure at all | SEC-001…006 are outside Sections 8–23 scope but affect every Hub journey |

---



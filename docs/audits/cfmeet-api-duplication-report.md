# CF Meet — Duplicate and Overlapping API Register

**Audit phase:** Phase 1 (audit only, no implementation) · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` (`staging` = `main`) · **Date:** 2026-11-06

**Companion files:** [`cfmeet-api-inventory.csv`](cfmeet-api-inventory.csv) (one row per operation), [executive report](cfmeet-platform-comprehensive-audit.md).

## 1. Method

1. I extracted every registered route statically from each service's axum router. The extractor starts at `axum::serve` and follows `.route`/`.nest`/`.merge`, `let` bindings, router parameters and nested router functions. This gives 1,066 distinct operations, including the 73 virtual operations that the Communications service dispatches from its `api.rs` match table.
2. **Exact duplicates:** the same `(method, normalized path)` registered by more than one service.
3. **Structural duplicates:** I removed version, `/internal/v1`, the delegated-mirror prefix and domain prefixes such as `/messaging` and `/calling`, then grouped the remaining "resource signature". Every group with more than one distinct path was reviewed by hand.
4. **Semantic duplicates:** I read the handlers and the owning ADRs for overlapping business behaviour with different paths, for example v1 meetings vs v2 conferences, or the three webhook systems.
5. Each candidate is classified into one of these categories:
   - `EXACT_DUPLICATE`
   - `EQUIVALENT_BEHAVIOUR`
   - `REDUNDANT_IMPLEMENTATION`
   - `INTENTIONAL_FACADE`
   - `INTERNAL_PUBLIC_VARIANT`
   - `LEGACY_COMPATIBILITY`
   - `LIFECYCLE_DISTINCTION`
   - `CONFLICTING_AUTHORITY`

No endpoint is proposed for deletion just because its name is similar to another. Every recommendation below preserves backward compatibility until the owner approves a deprecation window.

## 2. Summary counts

| Category | Candidate groups | Operations involved | Confirmed redundant (recommend retirement after migration) |
|---|---:|---:|---:|
| EXACT_DUPLICATE — same path served by two services (planned authority migration) | 3 groups (webhooks, usage, limits) | 20 (10 paths × 2 services) | 0 today. All 10 Calling copies become redundant once the owner completes the Part VI cut-over. |
| CONFLICTING_AUTHORITY — different paths, same business object, different source of truth | 4 groups (webhooks, API credentials, organizations/members, device registries) | see §4 (each group spans 2–4 services) | 8: the meeting-service legacy webhook routes (4), which are never delivered, and the legacy API-key routes (4), which are plaintext |
| LEGACY_COMPATIBILITY — v1 meeting surface superseded by v2 | 1 group | 23 v1 routes vs their v2 equivalents | 23 after the mobile client migrates (MTG-001, FE-022) |
| INTENTIONAL_FACADE — first-party relays/facades with a distinct security purpose (§6) | 4 groups | 73 Hub ops (behind 2 wildcard relay routes) + 14 meeting messaging bridge/relay routes + 94 delegated-mirror routes + 17 Calling privacy-relay facade ops | 0 |
| INTERNAL_PUBLIC_VARIANT / internal fan-out of one contract (§7) | 4 groups | 10 | 0 |
| LIFECYCLE_DISTINCTION / name-only similarity (not duplicates) | 9 groups | ~25 | 0 |
| **Total duplicate candidates reviewed** | **25 groups** | — | **31 confirmed redundant after migration**: 8 legacy developer-API routes, plus 23 legacy v1 meeting routes once mobile migrates. 0 can be removed today without a client migration. |

## 3. Exact duplicates: same path, two services

The Part VI webhook/usage migration explains all of these (`infra/deployment/part-vi/README.md:33-34,53`). The gateway currently routes every one of these paths to **Calling**, and the Developer Platform copies are dormant until `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED` / `API_GATEWAY_CENTRAL_USAGE_ENABLED` are set (`services/api-gateway/src/config/mod.rs:267-268`; both `"false"` in `infra/deployment/part-vi/compose.part-vi.yml:98-99`).

| # | Method + path | Service A (gateway target today) | Service B (dormant successor) | Behavioural difference | FE / SDK consumers | Canonical | Recommended action |
|---|---|---|---|---|---|---|---|
| D-01 | `POST/GET /v1/webhooks`, `GET/PATCH/DELETE /v1/webhooks/{id}`, `POST /v1/webhooks/{id}/rotate-secret`, `GET /v1/webhooks/{id}/deliveries`, `POST /v1/webhook-deliveries/{id}/replay` (8 ops) | calling `services/calling/src/webhooks/http.rs` | developer-platform `services/developer-platform/src/webhooks/http.rs` (`customer_routes`). Its module doc says it serves "exactly Calling's contract so the gateway can re-point it here at the cut-over" | Same contract. DP adds central delivery across producers, with Calling as the authority until cut-over (`calling/src/webhooks/ownership_migration.rs` header) | SDK TS/Dart: yes (gateway pattern). Web: none | Developer Platform, after cut-over | **LEGACY_COMPATIBILITY / planned migration.** Keep both. Run the runbook `docs/phases/phase-06/webhook-migration-runbook.md` once the DP is deployed (PLT-005), then retire the Calling handlers. |
| D-02 | `GET /v1/usage` | calling | developer-platform `http/public.rs:185` (`compat_usage`) | DP reads the central ledger | SDK: yes | DP after `API_GATEWAY_CENTRAL_USAGE_ENABLED` | Same as D-01 |
| D-03 | `GET /v1/limits` | calling | developer-platform `http/public.rs:186` (`compat_limits`) | DP reads central quotas | SDK: yes | DP after the flag | Same as D-01 |

**Security note:** the DP copies are not reachable through the gateway today, and the DP service is not deployed (PLT-005). No exposure.

## 4. Conflicting authorities: different paths, same business object

| # | Business object | Endpoints | Sources of truth | Consumers | Security implication | Canonical / action |
|---|---|---|---|---|---|---|
| C-01 | **Customer webhooks** | (a) meeting-service `GET/POST /api/v1/webhooks`, `PATCH/DELETE /api/v1/webhooks/:id` (`routes/webhooks.rs`). (b) Calling `/v1/webhooks*` (D-01). (c) Developer Platform `/v1/environments/{env}/webhooks*` (console) plus the D-01 copies. | 3 tables: meeting `webhooks` (secret stored in plaintext, DATA-003), calling `webhook_*`, DP `devplat_webhook_*` | Web Settings → Webhooks uses **(a)** (`apps/web/src/services/webhook.service.ts:14-24`) | (a) registers webhooks and **never delivers them** (PLT-003/DATA-007). Users are misled, and plaintext secrets sit at rest. | **REDUNDANT_IMPLEMENTATION (a).** Canonical = (c) once deployed. Retire (a) after giving web a replacement and an owner-approved notice; meanwhile hide or label the web panel. |
| C-02 | **API credentials** | (a) meeting-service legacy `/api/v1/api-keys` (4 ops) plus `X-API-Key` third-party routes `/api/v1/api/meetings*`. (b) Platform Access / Developer Platform credentials (`/v1/environments/{env}/credentials*`, 153 credential routes) | meeting `api_keys` (plaintext, non-atomic quota, PLT-002) vs `platform_api_credentials` (peppered hash) | Web developer page and settings use (a). Nothing uses (b). | (a) is a P1 security finding | **CONFLICTING_AUTHORITY.** Canonical = (b). Freeze issuance on (a), migrate customers, then retire it (PLT-004). |
| C-03 | **Organizations and members** | meeting-service `GET /api/v2/me/organizations`, `GET/PUT /api/v2/organizations/:id/members[/:user_id]`, admin `POST/PATCH /api/v2/admin/organizations[/:id]`. Developer Platform `/v1/organizations*` (self-service, invitations, roles). Platform Access tenants (internal). | `organizations` + `organization_memberships` (meeting), `devplat_organizations` + `devplat_workspace_members`, `platform_tenants` | Web Organizations page uses meeting-service. DP has no UI. | Two membership/role systems with different role sets (OWNER/ADMIN/MEMBER vs OWNER/ADMIN/DEVELOPER/ANALYST/BILLING), and no mapping between them | **CONFLICTING_AUTHORITY (ORG-001).** This needs a product decision, not endpoint deletion. See the organizations audit. |
| C-04 | **Devices and sessions** | Messaging `POST /v1/messaging/devices[...]` and meeting bridge `POST /api/v2/messaging/devices[...]`. Hub `/api/v2/communications/devices[...]` (link, push, revoke, sync-state). Calling `/v1/calling/endpoints`. | 4 device registries (DATA-005) | Web uses the meeting bridge plus Calling endpoints. Nothing uses the Hub devices. | Revoking in one registry does not revoke the others (ACC-008, Journey H FAIL) | **CONFLICTING_AUTHORITY.** Keep the registries, because each owns a different secret, but add one account-level revocation fan-out (workstream WS-2). |

## 5. Legacy compatibility: v1 meetings vs v2 Meeting Spaces / Conferences

The legacy surface is mounted behind `legacy_write_guard` (`services/meeting-service/src/routes/mod.rs:65-77`, `middleware/legacy_write.rs`). When `FEATURE_LEGACY_MEETING_WRITE_DISABLED` is on, every non-GET returns `410 LEGACY_WRITE_DISABLED`. The flag defaults to off.

| v1 route (meeting-service) | v2 equivalent | Behavioural difference | Current consumers |
|---|---|---|---|
| `POST /api/v1/meetings` | `POST /api/v2/meeting-spaces` + `POST /api/v2/meeting-spaces/:id/conferences` | v1 is one row with `start_time`/`timezone`. v2 separates a reusable space from its occurrences. New v1 meetings are **not mirrored** to v2 (MTG-017). | **Mobile** (with the wrong field names, MTG-001) |
| `POST /api/v1/meetings/instant` | `POST /api/v2/instant-meetings` | — | Mobile |
| `GET /api/v1/meetings`, `GET /api/v1/meetings/:id`, `PUT/DELETE /api/v1/meetings/:id` | `GET/PATCH /api/v2/meeting-spaces[/:id]`, `POST /meeting-spaces/:id/archive`, `POST /conferences/:id/reschedule`, `/cancel` | v1 response `{success,data}` is not what mobile expects | Mobile |
| `POST /api/v1/meetings/:id/join`, `/join/:identifier`, `GET /public/:identifier` | `GET /api/v2/resolve/:identifier`, `POST /api/v2/conferences/:id/join` | v2 adds consent, waiting room, passcode | Mobile, web legacy branch (MTG-019) |
| `GET /api/v1/meetings/:id/waiting`, `POST …/waiting/:pid/admit|deny` | `GET /conferences/:id/waiting-room`, `POST …/waiting-room/admit|deny` | — | Mobile live room using v2 ids (MTG-002) |
| `GET/POST /api/v1/meetings/:id/chat` | `GET/POST /conferences/:id/messages` | v1 chat is refused after the ADR 0053 cut-over (MSG-012) | Mobile |
| `POST /api/v1/meetings/:id/end`, `/screen-share/start|stop` | `POST /conferences/:id/end`, `/screen-share/:pid/stop` | — | Mobile |
| `PATCH /api/v1/participants/:pid/audio|video`, `POST …/kick` | `POST /conferences/:id/participants/:pid/mute|remove|audio-lock|video-lock` | v1 kick does not evict media (SEC-009) | Mobile |
| `POST /api/v1/api/meetings`, `/api/v1/api/meetings/:id/join` (API-key) | no v2 third-party equivalent through meeting-service. Gateway `meetings` domain uses v2 with platform credentials. | — | External integrators, if any (unknown) |

**Classification:** `LEGACY_COMPATIBILITY`, 23 operations. **Recommendation:** migrate mobile to v2 (WS-1). Then turn on `FEATURE_LEGACY_MEETING_WRITE_DISABLED`, keep v1 reads for the deprecation period, and only then remove the routes. Removing them now would break mobile completely.

## 6. Intentional facades: keep, not duplicates

| # | Facade | Owner API | Why it exists (security purpose) | Evidence |
|---|---|---|---|---|
| F-01 | meeting-service `ANY /api/v2/communications/*path` → Communications `POST /internal/v1/communications/dispatch` (73 public operations) | Communications Hub + owner internal contracts in Calling, Messaging and Meeting | The browser holds only a CF Meet session. Meeting verifies it, resolves the principal and signs a delegated service token, so the client never holds a platform credential. | `services/meeting-service/src/services/communications.rs:145-151,224-318`. The 73 dispatch arms equal the 73 contract operations exactly (verified by script; also asserted by test `every_published_communications_operation_is_routed`). |
| F-02 | meeting-service `/api/v2/messaging/{devices,client-tokens,client-sessions,directory,principals,me}` (10 ops) + `/api/v2/messaging/relay/*path` (GET/POST/PUT/DELETE, 4 ops) | Messaging `/v1/messaging/*`, `/v1/conversations/*` (through `/internal/v1/delegated/*`) | Messaging's customer API is for backends with credentials. First-party clients come through Meeting with an explicit allowlist (`services/unified_messaging/relay.rs` header). | Note that the relay omits **PATCH** (MSG-002) and drops some query keys (MSG-003). These are facade defects, not duplication. |
| F-03 | Messaging `/internal/v1/delegated/v1/{messaging,conversations}/*` (94 routes) | the same handlers as `/v1/messaging/*`, `/v1/conversations/*` | Mirrors the customer surface for first-party backends acting through a peer service (`messaging-application.delegate`) | `services/messaging/src/http/mod.rs:104-112` |
| F-04 | Calling `/v1/privacy-relay/*` (17 ops, public) | Privacy Relay `/internal/v1/contexts*`, `/sessions*` | Customer-facing facade with platform-credential auth over the internal authority (ADR 0010). Classified INTERNAL_PUBLIC_VARIANT. | `services/calling/src/privacy_relay/http.rs` |

## 7. Internal vs public variants and internal fan-out (not duplicates)

- `POST /internal/v1/communications/{blocks/check, capabilities, events}` exists in **both Calling and Messaging**. This is the same owner contract implemented by each owner service for the Hub (`packages/contracts/{calling,messaging}/communications-internal`). Keep it.
- `POST /internal/v1/platform/events` exists in Calling and the Developer Platform. Calling receives platform events for its webhook authority; the DP receives producer events. These are different consumers of one event contract. Keep both.
- `POST /internal/v1/communications/principals/resolve` (Hub) vs `POST /internal/v1/principals/resolve` (Platform Access). The Hub resolves communication principals and Platform Access resolves platform principals. Different authorities; keep both.
- `GET /v1/scopes` (DP, public) vs `GET /internal/v1/scopes` (Platform Access). This is a public read projection of the registry. Keep both.

## 8. Name-only similarity or legitimate lifecycle distinctions (not duplicates)

| Signature | Paths | Why they are distinct |
|---|---|---|
| `/blocks` | Calling `/v1/blocks`, Messaging `/v1/messaging/blocks`, Hub `/api/v2/communications/blocks` + `/profiles/{id}/block` | Per-channel block rules, with the Hub as account-level aggregator. Three stores exist (DATA-006); unify the *policy*, not the endpoints. |
| `/invitations` | Calling `/v1/calling/invitations` (ringing invitations), Messaging `/v1/messaging/invitations` (conversation invites), meeting `/meeting-spaces/:id/invitations` (meeting links), Hub `/invites` (contact invites) | Different domain objects |
| `/usage` | `/v1/calling/usage`, `/v1/messaging/usage`, `/v1/numbers/usage`, `/v1/usage` | Per-product usage plus an aggregate |
| `/search` | `/v1/messaging/search`, `/v1/numbers/search` | Different resources |
| `/contacts/{id}` | Hub contacts (social graph) vs Privacy Relay contacts (protected PSTN numbers in the Contact Vault) | Different resources, but the web wrongly proxies Privacy Relay contacts to Calling (CAL-013) |
| `/me` | Hub `/api/v2/communications/me` vs meeting `/api/v2/messaging/me` | Hub profile vs Messaging principal identity |
| `/meetings/instant` | meeting v1 `/api/v1/meetings/instant` (legacy) vs `/internal/v1/communications/meetings/instant` (Hub owner contract) vs v2 `/api/v2/instant-meetings` | v1 is legacy (section 5). The internal route is the Hub's owner contract that creates a v2 instant meeting for a conversation. |
| `/calls` | `/v1/calls` (public) vs `/internal/v1/communications/calls` (Hub-launched) | Different authentication; the internal one carries Hub context |
| `/readiness` | Hub vs Telephony internal readiness | Per-service probes |

## 9. Recommended consolidation (proposal only, not executed)

1. **Retire the meeting-service legacy developer API** (`/api/v1/webhooks*`, `/api/v1/api-keys*`, 8 operations) after the Developer Platform is deployed and the web Developers/Webhooks pages move to it. Until then, freeze issuance and label the webhook panel "not delivered". Workstreams WS-3/WS-6.
2. **Complete the Part VI cut-over** for `/v1/webhooks*`, `/v1/usage` and `/v1/limits`, then remove the 10 Calling copies.
3. **Retire the v1 meeting write surface** after the mobile v2 migration. Keep v1 reads for one deprecation cycle.
4. **Keep** all facades in section 6, and fix their defects (relay PATCH, query allowlist).
5. **Make an organization authority decision** (ORG-001) before any endpoint changes in C-03.
6. **Add a CI gate** (INV-001) that diffs `route-manifest.json` upstream paths against statically extracted routes, so a future refactor cannot silently orphan a gateway route. The extraction used in this audit is a working prototype.

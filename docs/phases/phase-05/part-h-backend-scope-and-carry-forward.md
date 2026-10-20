# CF Meet Part V v2.0 — next backend increment: Part H scope and E–G carry-forward

**Status:** BACKLOG / IMPLEMENTATION SCOPE REGISTER — not implementation evidence, not acceptance, not release authorization.  
**Source of authority:** *CF Meet — Part V Secure Unified Messaging & End-to-End Encryption Platform*, Version 2.0 (18 October 2026), especially Part H §§97–112, Parts I–K, AD-01–AD-10 and implementation order §163.  
**Implementation target:** `staging` only; `main` and production remain unchanged without explicit owner authorization.  
**Prior evidence:** [Part E–G evidence](parts-e-g-evidence.md), [Workstreams A–D evidence](workstreams-a-d-evidence.md), [ADR 0052](../../decisions/0052-messaging-v2-architecture-decision-baseline.md).

## 1. Preserve the original scope

Part V is the independent **backend** Unified Messaging Platform. Its job is to serve CF Meet, Meeting Service, CosmicForge products and external applications through versioned, product-neutral contracts. It must not replace Meeting Service, Platform Access, Privacy Relay, Calling, Numbering or SMS Gateway; cross-service integration must use authenticated published contracts, **never foreign database access**.

The Version 2.0 specification expressly excludes **web and mobile UI implementation** from the backend delivery scope. Reusable client security contracts, SDK integration requirements and real client runtime *certification evidence* remain backend acceptance dependencies, but the consumer-facing chat UI is a separate client workstream. Do not use frontend implementation as a substitute for missing backend integrations.

Maintain two distinct profiles: `PRIVATE_E2EE` (keys/cleartext never on Messaging servers) and `MANAGED` (explicitly disclosed, policy-controlled backend content processing). No silent downgrade, server E2EE key escrow or invented cryptographic certification. SMS is a separate, non-E2EE channel.

The authoritative current Git policy supersedes the original specification's stale baseline instruction: development on reconciled `staging` directly; permanent remote branches **only `main` and `staging`**; no production release without approval.

## 2. Part E/F/G baseline — completed scope is not repeated

| Work | Existing evidence | Carry-forward limitation |
|---|---|---|
| **Part E** §§58–68 — realtime delivery, subscriptions, ordering, replay, typing, presence, push boundary | `04dbcfb`, evidence register and Messaging integration CI | OS push senders not connected; per-device notification jobs may end `NOT_CONFIGURED` |
| **Part F** §§69–80 — protected object storage, E2EE attachment primitives, managed scanning, download grants, quotas/retention | `06df918`, real MinIO/ClamAV tests | Media processing `processing` interface only, no tested transcode/thumbnail/duration verification |
| **Part G** §§81–96 — search, moderation, retention, legal hold, export, deletion, audit | `1f02edc`, evidence register | Production certification separate; real-client local-search/MLS runtime acceptance not demonstrated |
| Milestone 0 missing prerequisites | `e23017b`, `61190a6` | Reuse membership, message state, events, device security; no duplicate implementation |
| Final E–G tested baseline | `93b615d` | These commits are software evidence, not `PRODUCTION PASS` |

Later incremental changes on `staging` include browser Messaging realtime transport, ticket CORS and mobile CI fixes. They do **not** imply that an authenticated CF Meet login already yields a verified Messaging principal/device session or that Web/Android/iOS MLS messaging is runtime-certified.

## 3. Next scope: Part H — Integration and Migration (§§97–112)

**Implement this Part next in specification order**, using narrowly scoped additions to existing services and contracts. No new general-purpose auth service or product-specific chat domain.

| Spec | Backend requirement and acceptance focus |
|---|---|
| **§97** | Meeting chat migration plan; Meeting Service keeps meeting admission/policy while Messaging becomes authoritative for messages/conversation history after cutover |
| **§98** | Eligible Conference Session creates/resolves its idempotent Messaging `MEETING` conversation via authenticated service integration; synchronized membership/context lifecycle |
| **§99** | Compatibility adapter for existing Meeting chat APIs: versioned legacy response mapping, authenticated Messaging client; post-cutover **no new authoritative `chat_messages` writes** |
| **§100** | Safe shadow/dual-run gate: legacy remains authoritative until measured parity; failures must not break meetings, record mismatches; reversible, explicitly approved cutover |
| **§101** | Historical legacy content retains `LEGACY_MANAGED` provenance; never misrepresent legacy plaintext chats as E2EE or silently convert profile |
| **§102** | Generic first-party consumer integration for products such as Karryam, HealthNet and Real Estate via Platform Access tenant/app/environment; no product-name authorization shortcuts or foreign DB joins |
| **§103** | Narrow authenticated Privacy Relay context/authority update and revoke handling without exposing Contact Vault/phone/private contact details |
| **§104** | **Trusted CF Meet native-account → Platform Access principal/device → short-lived Messaging Client Access Token exchange** in a backend authentication/session layer; verify login, scope, tenant, device and revocation; do not make Messaging the password authority and never expose application credentials in clients |
| **§105** | Independent external-tenant integration: create principals/conversations under customer backend API credentials, issue user-scoped client tokens, browser-like WebSocket/REST access and lifecycle webhooks, all with tenant isolation |
| **§106** | SMS Bridge policy requires explicit disclosure and downgrade consent where E2EE content is deliberately exported; the original private conversation remains E2EE |
| **§107** | Keep Messaging's conversation/canonical message ownership distinct from SMS Gateway's carrier execution/callbacks/cost evidence |
| **§108** | Outbound managed bridge and separately authorized E2EE-to-SMS plaintext copy via authenticated boundary, idempotent canonical/transport linkage |
| **§109** | Inbound SMS enters explicitly `MANAGED`/SMS-bridged context only; never inject carrier plaintext as an E2EE message |
| **§110** | Carrier receipt/submission status kept separate from in-app delivered/read state |
| **§111** | Capability/eligibility, opt-out, route, rate limits, spend/cost checks before outbound SMS; safe failure without disturbing in-app messaging |
| **§112** | Real public SMS requires a lawful carrier **only for external-edge certification**. Complete and test internal bridge using deterministic/synthetic SMS Gateway while external integration is pending |

### Part H dependency-aware execution order

1. **H0 — delta audit:** compare the authoritative spec, current source, API contracts, migrations and Part E–G evidence. Create a `PASS/PARTIAL/BLOCKED` requirement ledger; do not rewrite completed work.
2. **H1 — trusted identity bridge (§104):** user JWT verified by its actual owner, account mapped to persisted Platform Access principal in correct app/environment, device activation verified, backend mints a scoped Messaging client session/token without delivering service credentials to web/mobile. Explicit tests for logout, revoked device, suspended tenant/app and cross-tenant impostors. Treat the existing web transport as a separate consumer dependency.
3. **H2 — Meeting chat migration (§§97–101):** service-authenticated client, context and member lifecycle; shadow writes/diffs, legacy provenance, incremental cutover with flag/rollback and end-to-end tests using actual Meeting and Messaging processes.
4. **H3 — platform-neutral integrations (§§102–105):** Privacy Relay authority integration, first-party and synthetic external-tenant journeys; no product-specific tables or privilege.
5. **H4 — SMS bridge (§§106–112):** explicit downgrade boundary, dual channel state, authenticated delivery/ingress, number/capability/cost policy; synthetic carrier acceptance and separate real-edge certification blocker.
6. **H5 — close Part H:** CI, contract parity, service boundaries, migrations, cross-service fault injection, feature-flag rollback, threat model update, evidence register. No `main` merge or production activation.

## 4. Preserve all open E–G items in their proper later gates

These items remain open; they are not retroactively declared Part E–G PASS in their missing dimension. Address engineering dependencies when needed during Part H; carry certification/optional edge work into Parts I–K without expanding H into client UI.

| Item from E–G evidence | Correct disposition / closure evidence |
|---|---|
| **Push notification senders** — APNs / FCM / Web Push not configured | Production-capable adapter and device/revocation tests can be completed under subsequent infrastructure integration; live providers/credentials are an **external-edge** gate. Push cannot contain E2EE plaintext/keys. |
| **Media processing** — thumbnails, transcoding, duration checks are only an interface | Implement only explicitly required `MANAGED` server processors and/or client-produced encrypted derivatives; no E2EE server decryption, never claim verified duration from inaccessible ciphertext. Otherwise keep PARTIAL with reason. |
| **Outside key-transparency witness** | Security architecture / operations work in **Part J** and independent review in **Part K**: resolve equivocation/split-view protection with an independent witness/auditor; operator-only tree heads stay `UNWITNESSED`. |
| **New-device history recovery / transfer** | ADR 0052 AD-07: authorized device-to-device transfer or user-controlled encrypted backup. Backend may store opaque encrypted artifacts; no server recovery master key. Genuine client/device validation required under **Part K**. |
| **Web/Android/iOS target compilation only** | Real-device MLS, encryption/decryption, key storage, offline recovery and browser code-delivery threat tests are **Part K client runtime certification**; compile success is not runtime certification. Client UI work itself remains out of Part V backend scope. |
| **Independent cryptographic/security assessment** | **Part K mandatory external gate**; no self-approval and no claims of production security equivalence. Fix findings before `PRODUCTION PASS`. |
| **AD-01–AD-10 architecture direction** | ADR 0052 selects staging engineering direction, not independent acceptance. Owner's final production security authorization remains separate. |
| **Unexplained realtime test hang** | Retain as a resilience-reliability investigation in **Part J**; replicate or identify reliable exclusion, run repeated real-broker failure/reconnect/load tests and document evidence. |
| **ClamAV production signature quality** | Operations/certification work under **Parts J/K**; EICAR CI smoke is not proof of production malware coverage. |

## 5. Subsequent parts — original specification order

- **Part I (§§113–129):** API families/versioning, typed errors, authenticated internal APIs, realtime contracts, domain events, webhooks, usage metering, service-owned durable state and persistence.
- **Part J (§§130–145):** tenant isolation, authorization, data minimization, rate limits/kill switches, reliability, observability, operations and CI/service-boundary hardening.
- **Part K (§§146–160):** real-infrastructure and cross-service certification, migration rollback, external developer tenant, SMS bridge, device/MLS/security checks, independent audit and production gates.
- **Part L (§§161–166):** A–Z engineering workstreams / prescribed execution and closure; reconcile with work completed under A–K. Do not confuse specification Parts with engineering workstreams.

## 6. Mandatory final report for each increment

For H0–H5 give exact starting/final `staging` SHA, evidence and actual test counts, backend files/migrations/API changes, OpenAPI and event-contract parity, end-to-end cross-service tests, rollback results, remaining `PARTIAL/BLOCKED` rows and confirmation that `main` and production remain untouched. Maintain separate states:

- **SOFTWARE COMPLETE:** CosmicForge-owned work, tests and migrations pass, no need for commercial messaging provider.
- **INTEGRATION READY:** Meeting, Privacy Relay, first-party/external and synthetic SMS dependencies demonstrably integrated.
- **EXTERNAL EDGE CERTIFIED:** applicable real push/carrier paths tested when authorized and available.
- **PRODUCTION PASS:** all required security, resilience, rollout and external assessment gates complete with explicit owner release authorization.

No milestone advances merely because documentation or stub endpoints exist. Never merge to `main`, create permanent extra remote branches or enable production E2EE without the owner's explicit approval.

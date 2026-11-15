# Document 3 — Current and Target Architecture

The diagrams use Mermaid, which renders on GitHub. Every box in the current-state diagram exists on `main` at `6f3743f`. Its production state is shown in brackets.

---

## 1. Current-state architecture (as built, 2026-11-15)

```mermaid
flowchart LR
  subgraph Clients
    WEB[apps/web<br/>3 WebSockets, polling<br/>no service worker]
    MOB[apps/mobile<br/>polling only<br/>no push SDK]
  end

  subgraph Edge["api.cfmeet.com (host nginx, single VPS)"]
    NGX[nginx]
    GW[API Gateway<br/>DP + Messaging families<br/>WS disabled]
  end

  subgraph Meeting["Meeting Service (+ Account authority) — rtc_db"]
    AUTH[Auth / OTP<br/>Redis codes]
    DEV[Account devices<br/>step-up, notices]
    MN[meeting_notifications<br/>worker 30s]
    LEDGER[(email_deliveries<br/>UNIQUE idempotency_key)]
    EW[Email worker<br/>SMTP active / Resend off]
    RELAY[/api/v2/communications relay/]
    ORGEV[organization_events consumer]
    CRT[Conference realtime WS]
  end

  subgraph Hub["Communications Hub — cfmeet_comm_production_hub"]
    PREF[(notification prefs<br/>conv mute, contact mute)]
    POL[policy/check]
    HOUT[(communication_outbox<br/>dest CALLING,MESSAGING)]
    FEED[/sync, /activity/]
  end

  subgraph Messaging["Messaging — cfmeet_messaging"]
    MOUT[(messaging_outbox_events)]
    PJ[(messaging_push_jobs)]
    PD[Push dispatcher<br/>APNs/FCM/WebPush<br/>provider=none]
    MRT[Realtime WS /v1/realtime]
  end

  subgraph Calling["Calling — cfmeet_calling"]
    RING[Ringing + arbitration<br/>call_answer_claims]
    SIG[Signaling WS<br/>pg_notify bus]
    NOP[NoPushTransport]
    COUT[(calling_outbox_events)]
  end

  subgraph Orgs["Organizations — cfmeet_organizations"]
    OOUT[(organization_outbox)]
  end

  subgraph DP["Developer Platform + worker"]
    WH[(devplat_webhook_*)<br/>active]
    CL[credential lifecycle sweep]
  end

  PA[Platform Access<br/>identity change feed]
  SMTP[(Gmail SMTP<br/>rejected per ops)]
  RESEND[(Resend<br/>not activated)]
  APNS[(APNs/FCM/WebPush<br/>not configured)]
  CUST[(Customer endpoints)]

  WEB --> NGX
  MOB --> NGX
  NGX -->|/api/v1,/api/v2| Meeting
  NGX -->|/v1/environments,/v1/webhooks,/v1/messaging...| GW
  NGX -->|/v1/calling| Calling
  GW --> DP
  GW --> Messaging
  RELAY -->|communications-user.forward| Hub
  AUTH --> LEDGER
  DEV --> LEDGER
  MN -->|preferences.read<br/>GRANT MISSING| PREF
  MN --> LEDGER
  LEDGER --> EW --> SMTP
  EW -.-> RESEND
  MOUT --> PJ --> PD -.-> APNS
  PD -->|policy.check| POL
  RING --> SIG
  RING --> NOP
  COUT --> DP
  OOUT -->|13 event types| ORGEV
  HOUT --> Calling
  HOUT --> Messaging
  CL --> WH --> CUST
  Messaging -->|public events| DP
  Meeting -->|public events| DP
  Calling -.poll.-> PA
  Messaging -.poll.-> PA
```

### 1.1 Production state of each delivery path

| Path | State | Evidence |
|---|---|---|
| Email (any category) | Enqueued durably. Transport is SMTP; delivery **not evidenced** | diagnose 43317264046; certify 43670057279 `EMAIL_PROVIDER=smtp` |
| Meeting notices to account holders | **Very probably failing closed** (missing grant) | `communications_release.py:178`; `notifications.rs:994-996` |
| Device push (all) | **Off** (`MESSAGING_PUSH_PROVIDER=none`); no client registers | certify 43670057279; evidence/06 §1 |
| Call ringing | **Working** (WS, foreground or open tab only) | runs 41580265031, 41581639199 |
| Customer webhooks | **Working** | developer-acceptance 30/30 in certify 43670057279 |
| Org events → Meeting | **Working** (35 delivered, 0 dead) | certify 43670057279 |
| In-app notifications | **Absent** | evidence/06 §3 |
| Operator alerts | **Absent** (except TLS monitor) | `monitoring-alerts.md:3-5` |

---

## 2. Current-state findings (Part 6, items 1–9)

1. **Architectural duplication**
   - **Three durable notification-job models**: `email_deliveries`, `meeting_notifications`, `messaging_push_jobs`.
   - **Five outboxes**: Account, Hub, Organizations, Messaging, Calling. All re-implement lease + `SKIP LOCKED` + 2ⁿ backoff + dead letter.
   - **Two push hooks**: Messaging adapters, and Calling `PushDispatcher` with no transport.
   - **Two device-push stores**: Messaging `messaging_push_registrations`, plus the Hub mirror `push_platform`/`push_registered_at`.
   - Outboxes are correctly per-service (each owns its transaction), so their duplication is **acceptable**. The duplication of the *notification* concerns is what needs consolidating: recipient resolution, preference evaluation, channel selection and delivery history.
2. **Unnecessary coupling**
   - The Meeting Service is the de facto notifier for organizations and developer access, through a synchronous relay side effect and an ops script.
   - The Hub's policy check costs about 6 signed calls per decision (`policy.rs:88-219`), and is only usable by Messaging (`api.rs:315-322`).
   - The Organizations event contract promises a "notification owner" consumer that does not exist (`docs/events/organization-event-contracts.md` §2 vs `outbox.rs:41-59`).
3. **Fragmented history**
   - Each delivery is recorded in a different place: ledger rows, meeting recipient digests, push jobs and webhook attempts.
   - There is no per-user history.
   - The Hub `/activity` timeline covers only calls and meeting invitations.
   - Account `/me/security-events` covers only security.
4. **Conflicting preferences**
   - Contact mute (no expiry) vs conversation mute (`muted_until`).
   - `MENTIONS` is treated as `MUTED`.
   - The org scope is not governed at all.
   - `securityEvents:true` is hard-coded but nothing consumes it.
   - 4 of 7 categories are stored and never read.
5. **Missing delivery monitoring**
   - No alert for email failures, push failures, webhook dead letters or meeting job failures.
   - `notifications::status_counts` is unexposed.
   - Alertmanager is not deployed.
6. **Types with no durable processing**
   - Synchronous invitation emails: no job, no retry beyond the ledger.
   - Best-effort security notices: enqueue failure is lost.
   - The Calling push hook: fire-and-forget, no metric.
   - Contact requests, org role changes, missed calls, voicemail deposit: no processing at all.
7. **Correctly centralized**
   - Email transport (one ledger, exact-once enqueue).
   - Customer webhooks (Developer Platform).
   - Ringing and arbitration (Calling).
   - Preference storage (Hub, single writer).
   - Push payload confidentiality (Messaging DB CHECK + whitelist).
   - Service authentication (`cf-service-auth`).
8. **Migration risks**
   - Duplicate meeting notices during handover.
   - Losing in-flight `meeting_notifications`.
   - Activating push before deciding the registry owner (which would create live handles to migrate).
   - Adding NS to Hub or Organizations destinations needs code and CHECK changes.
   - Per-recipient policy fan-out load.
   - The resource gate on a shared VPS.
9. **Security and privacy boundaries**
   - E2EE plaintext never leaves clients.
   - Push payloads are metadata only.
   - OTP authority is Account.
   - Org membership authority is Organizations.
   - Principal authority is Platform Access.
   - Service tokens are single-audience and receiver-authoritative.

### 2.1 Components that must remain untouched

| Component | Why |
|---|---|
| Calling ringing, arbitration, signaling WS, invitation recovery, recording/compliance notices | latency-critical and transactional (ADR 0008, 0024) |
| Account OTP generation, verification, storage; password reset; device step-up | security token authority (ADR 0057, 0060) |
| Messaging ciphertext storage, MLS, realtime gateway, read/delivery cursors | E2EE boundary (ADR 0045-0049, 0060) |
| Meeting `email_deliveries` schema and worker semantics (apart from the N0 bug fixes) | reused as the email transport |
| Developer Platform webhook system | a separate product; production-tested |
| Organizations membership/permission writes | business authority |
| Hub preference **writes** | single-writer rule |
| Telephony, SMS Gateway, Numbering, Privacy Relay configuration | disabled by owner decision |

---

## 3. Option A — Notification orchestration service (recommended)

The new service owns:
- event ingestion;
- recipient resolution, by calling the authoritative owner;
- policy (preferences + precedence);
- channel routing;
- the in-app inbox;
- scheduling;
- the delivery ledger;
- shadow comparison;
- operator diagnostics.

It **delegates transport**:
- email → Meeting ledger (new internal service-auth enqueue API);
- message push → Messaging (unchanged decision path);
- call ring → Calling (unchanged).

Non-message push uses the shared push transport (§4.3, decision D-1).

## 4. Option B — Fully consolidated delivery service

NS absorbs:
- the email ledger, worker and providers (Resend/SMTP, Svix webhooks);
- push registrations, jobs and adapters;
- meeting notification jobs;
- Hub preferences (new writer, or migrating the writer).

Existing services call NS for every send. Compatibility adapters keep the old internal entry points during migration.

### 4.1 Comparison

| Criterion | Option A (orchestration) | Option B (full consolidation) | Evidence |
|---|---|---|---|
| Reliability | OTP/security email stays in the process that owns the token; an NS outage does not block sign-in | an NS outage blocks OTP unless Account keeps a fallback path, which is duplication again | Part 11 requirement; `handlers/auth.rs` |
| Code ownership | transports stay with the teams that built and certified them | NS team owns email, push and preferences, including the E2EE-sensitive dispatch rechecks it cannot evaluate locally (membership and history window live in the Messaging DB) | `push.rs:319-395` |
| Migration risk | **Email: zero data movement.** The same ledger and same `UNIQUE (idempotency_key)` make dual enqueue during handover exact-once | ledger rows (with OTP bodies) and handles must be copied; dual-run dedupe across two ledgers relies on provider idempotency (Resend window-limited; SMTP none) | `email_delivery.rs:124-185, 394-486` |
| Operational complexity | +1 container, +1 DB | +1 container, +1 DB, plus re-certifying email (Resend activation, webhooks) and push in the new home | `docs/operations/resend-email-owner-handoff.md` |
| Performance | one extra internal hop for NS-owned email (async; irrelevant to latency) | the same hop for every send, including OTP (user-visible latency) | — |
| Maintainability | one policy/inbox/history model; transports unchanged | single codebase for all delivery | — |
| Cost | €0 infrastructure (fits headroom) | €0 infrastructure; much higher engineering and certification cost | Document 3 §7 |
| Rollback safety | flip the channel authority back (configuration); old path never removed until Phase 8 | requires the old path to be kept warm anyway, which is effectively Option A plus duplication | — |

**Verdict:** Option A. Option B centralizes code that is already correctly centralized (email), and moves security-critical paths for no functional gain.

### 4.2 Option C — Extend the Communications Hub (evaluated, rejected)

Putting the inbox and routing into the Hub reuses its preferences, device index and relay. But:
- The Hub governs **only the personal-communications triple** (`packages/communications-policy/src/lib.rs:11-55`). Account security, organizations, meetings and developer notifications are outside its authority.
- Its policy engine is already the most expensive internal call.
- The documented decision is that "the Hub only owns preferences" (`CFMEET_FINAL_PRODUCTION_COMPLETION.md` §5).

Option C would have to widen the Hub's authority across four domains. That is more coupling than a narrow new service.

### 4.3 Push transport: the evidence-driven exception (decision D-1)

Push has **no production traffic**: no provider and no client registrations. Consolidating its transport now costs nothing to migrate. Consolidating it after activation means migrating live handles.

**Recommended shape:**

1. **Extract** `services/messaging/src/realtime/push_adapters.rs` into a shared library crate `packages/push-transport`. This is a pure move, with tests and behaviour unchanged. Messaging keeps using it in-process.
2. **Push-target registry at the account-device level.** It is owned by NS (`notif_push_endpoints`) and keyed by the Account device (ADR 0060). It is registered through the **existing** Hub facade `PUT/DELETE /api/v2/communications/devices/{id}/push`; the public contract is unchanged. This fixes:
   - organization/MANAGED clients being unable to register (session devices);
   - Calling's unset `pushTargetRef`. Its contract already says "opaque reference to a push target held by the application" (`packages/contracts/calling/control/v1.schema.json:1098-1105`).
3. **Message wake-ups.** Messaging keeps deciding *whether* a device gets a wake-up: membership, history window, block and Hub policy, all evaluated in its own DB. It then dispatches through NS by `pushTargetRef` (Phase 4.3), or through its own adapters until then.
4. **Call push.** A Calling `PushDispatcher` implementation calls the NS fast path post-commit, with a hard timeout of ≤ 1 s, fire-and-forget. **Ringing never waits for it**: the WS signal and the recovery API are unchanged.

   Alternative if the owner prefers zero runtime coupling for calls: Calling embeds `packages/push-transport` directly and resolves `pushTargetRef` from a replicated read model. This is decision D-2.

**If the owner activates Web Push in Messaging before NS exists** (the `activate-web-push` tooling is ready), the plan still works. Phase 5 copies `messaging_push_registrations` into `notif_push_endpoints`, with Messaging remaining a reader until cutover. This is decision D-1b.

---

## 5. Target architecture (recommended)

```mermaid
flowchart LR
  subgraph Producers["Producers (business logic unchanged)"]
    ACC[Account / Meeting<br/>account_security_events,<br/>meeting changes]
    ORG[Organizations outbox]
    HUB[Hub outbox<br/>+ preferences owner]
    CAL[Calling outbox<br/>missed / voicemail / recording facts]
    DPL[Developer Platform<br/>activity + lifecycle]
  end

  subgraph NS["cfmeet-notification-service (cfmeet_notifications DB)"]
    ING[Ingest API<br/>idempotent, versioned]
    RES[Recipient resolver<br/>calls authorities]
    POLE[Policy evaluator<br/>Hub prefs + precedence]
    SCHED[Scheduler]
    INBOX[(notif_inbox_items)]
    DLV[(notif_deliveries)<br/>dispatcher]
    PEP[(notif_push_endpoints)<br/>D-1]
    SHADOW[Shadow comparator]
  end

  subgraph Transports["Transports (existing owners)"]
    LEDGER[(Meeting email_deliveries)]
    PT[packages/push-transport<br/>APNs/FCM/WebPush]
    MSGP[Messaging wake-up decision]
    RING[Calling ringing + WS<br/>NOT via NS]
  end

  ACC --> ING
  ORG --> ING
  HUB --> ING
  CAL --> ING
  DPL --> ING
  ING --> RES --> POLE
  POLE -->|resolveNotificationPolicy| HUB
  POLE --> INBOX
  POLE --> DLV
  SCHED --> DLV
  DLV -->|enqueueEmailDelivery| LEDGER
  DLV --> PT
  MSGP -->|dispatch by pushTargetRef| PT
  CAL -.fast path post-commit.-> PT
  PEP --- PT
  WEBC[Web / mobile] -->|/api/v2/notifications via Meeting relay| INBOX
  WEBC -->|existing Hub facade| PEP
  RING --> WEBC
```

### 5.1 Ownership boundaries (target)

| Concern | Authority | NS role |
|---|---|---|
| Who is a member / owner / invitee | Organizations, Meeting spaces, Calling mailboxes, Messaging membership | asks; never stores authority |
| Account status, sessions, devices | Account (Meeting) | reads the device list, receives revocation events |
| Principal status | Platform Access | polls the change feed (as Calling and Messaging do) |
| Preferences | Hub (single writer) | reads in batch; caches with version |
| OTP / verification tokens | Account | never sees codes; never sends OTP |
| Email transport | Meeting ledger | enqueues with deterministic keys |
| Push transport | `packages/push-transport` + `notif_push_endpoints` (D-1) | owns registry and dispatch for non-call, non-message categories; serves fast-path dispatch |
| Message wake-up decision | Messaging | — |
| Call ring decision and signal | Calling | — |
| In-app notification history | **NS** | owner |
| Delivery history (cross-channel) | **NS** (+ transport-level records stay with transports) | owner |
| Customer webhooks | Developer Platform | none |
| Operator alerting | Monitoring stack (Prometheus/Alertmanager when deployed) | exports metrics only |

### 5.2 Dependency graph and failure isolation

```
Producers ──(async, outbox retry)──▶ NS ──(async, retried)──▶ Meeting ledger / push transport / Hub (read)
Calling ──(post-commit, ≤1s, best effort)──▶ NS fast path        [never blocks ringing]
Clients ──(HTTP via Meeting relay)──▶ NS inbox                    [inbox down ≠ app down]
NS ──✗──▶ never called by: OTP, sign-in, call establishment, message send, webhook delivery
```

There are no cycles:
- NS calls the Hub (read), Meeting (enqueue), Platform Access (read) and the push providers.
- The Hub, Organizations, Calling and the Developer Platform call NS only through their outbox workers, which are already retrying and asynchronous.
- Messaging calls NS only for dispatch after its own decision (Phase 4.3).

### 5.3 Event flow examples

**Rule: token-bearing messages stay with the token minter.** Meeting invitations (`/invite#<token>`) and organization invitations (`/organizations/invitations#<token>`) carry a one-time bearer secret. Only the minting request ever sees it: Meeting never stores it, and Organizations stores only its SHA-256 (`organization_invitations.token_hash`). Accepting requires the token (`services/organizations/src/http/invitations.rs:395-505`). NS must never receive, store or forward such a token. So the token-bearing **email** stays a synchronous side effect in the Meeting relay, as today. NS adds only the *token-free* parts.

**Organization invitation (target):**
1. Organizations creates the invitation and writes `invitation.created` to `organization_outbox`, with destination `cfmeet.notifications` (new subscriber row). The payload carries IDs only, never an email or token (contract rule, `docs/events/organization-event-contracts.md`).
2. The Meeting relay still sends the token-bearing email when `sendEmail` is set. This is unchanged.
3. NS ingests the event, deduping by `(producer, eventId)`, and asks Organizations or the Hub whether the invitee is an existing account. **Owner decision D-5:** this needs an Organizations read that maps an invitation to the invitee account reference.
4. If the invitee is an existing account, NS creates an inbox item: "You were invited to *Org* — open the invitation email to accept".
5. **Owner decision D-5b:** an Organizations-owned "accept by invitation id for the matching verified email" would allow in-app acceptance. That is an Organizations business-rule change and is out of NS scope.
6. On `invitation.accepted|declined|revoked|expired`, NS marks the inbox item resolved and can notify the inviter (category `ORG_INVITATION_RESPONSE`).

**Missed call (target):**
1. Calling commits the terminal transition.
2. It writes a new derived outbox fact `CALL_PARTICIPANT_MISSED` for each participant whose offers all expired or were cancelled. This applies the ADR 0025 rule inside Calling and does not change it.
3. The fact goes to the notifications destination.
4. NS checks the Hub `missed_calls` preference.
5. NS creates an inbox item, plus a push wake-up if a push endpoint exists, coalesced per caller within 10 min.

---

## 6. Operational alerts: classification (Part 4.8)

| Signal | End-user Notification Service? | Recommended home |
|---|---|---|
| Health / readiness failures, container restarts, OOM | No | Prometheus + Alertmanager (deploy the existing rules; fill receivers) |
| Outbox / webhook / email / push dead letters | No (operator) | Same. NS exports `notif_*` metrics and rules |
| Security incidents (`calling_incidents`, `CREDENTIAL_GUESSING`, abuse bursts) | No | Monitoring. An optional incident → operator-email bridge can later use the Meeting ledger through monitoring, not NS |
| TLS certificate | No | Keep the existing GitHub-issue monitor |
| Deployment results | No | GitHub Actions |
| Account-security events (new device, password changed) | **Yes** (user-facing) | Account emails them; NS adds inbox copies (Phase 4.4, optional) |

---

## 7. Infrastructure recommendation (Part 13)

### 7.1 Measured headroom

| Measure | Value | Source |
|---|---|---|
| Host | 6 vCPU, 11.7 GiB RAM, 4 GiB swap | `docs/operations/messaging-section-8-delivery.md:292-300` (2026-11-05) |
| MemAvailable (latest) | **5,076 MiB** | certify 43670057279 (2026-11-15) |
| Disk free | **141.1 GiB** | same |
| Release resource gate | `MemAvailable − (new limits + 832 MiB transient) ≥ 1024 MiB` | `infra/calling/production/coordinated_release.py:166-190` |
| Existing limits | Organizations / Numbering / Privacy Relay / SMS / Telephony / API Gateway 192 MiB; Developer Platform 256; DP worker 128; Communications 512; Messaging 1024 | certify 43670057279 |
| Current notification volumes | email ledger "all counts 0" at 2026-11-12; Org events 35 delivered in total; push 0 | diagnose 43317264046; certify 43670057279 |

### 7.2 Sizing

| Resource | Recommendation | Rationale |
|---|---|---|
| Containers | 1 (`cfmeet-notification-service`, API and workers in one process, as Organizations and the Hub do) | low volume. Split a worker later only if measured |
| Memory limit | **256 MiB** (expected resident < 64 MiB, comparable to Organizations at 192) | Gate: 5,076 − (256 + 832) = 3,988 ≥ 1,024 ✔ |
| CPU | no limit needed; < 1 % of one vCPU at current volumes | Postgres polling at 1 s like its peers |
| Database | new database `cfmeet_notifications` in the existing `cfmeet-core-db` (Postgres 16), with `notifications_migrator` + runtime role via `provision-service-database.sh` | same isolation model as the 11 other service databases |
| Queue | **Postgres tables with `FOR UPDATE SKIP LOCKED`**; no Redis, no managed queue | identical to every existing worker; measured volume is tiny |
| Storage | inbox item ≈ 0.5–1 KB; delivery ≈ 0.3 KB. Even 10k users × 20 notifications/month × 90-day retention ≈ 0.6M rows ≈ < 1 GiB | 141 GiB free |
| Network | internal TLS through `cfmeet-core-tls` (new 84xx port); outbound only to Apple/Google/Web Push endpoints when push is activated | no new public ingress (Meeting relay) |
| Monitoring | `/metrics` on a private listener; rules file `notifications.rules.yml` (CI-validated like the others) | deploying Alertmanager receivers is a separate owner decision (D-9) |
| Backups | add to `infra/deployment/backup/catalog.conf` and to the coordinated `backup` operation | consistent |
| Cost | **€0 additional infrastructure.** Provider costs only if the owner activates them: Resend (already planned for OTP recovery), Apple Developer Program (APNs), FCM and Web Push (no fee from Google / browser vendors) | no purchase authorized or required |

**A second server, managed queue or paid provider is not justified by any measurement.** Re-measure at Phase 6, and recheck if sustained MemAvailable falls below 2 GiB.

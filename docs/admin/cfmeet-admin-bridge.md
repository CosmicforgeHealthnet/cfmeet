# CF Meet Admin Bridge: CF Meet managed from the CosmicForge central admin (HQ)

| | |
|---|---|
| Status | Draft for owner review (2026-11-19) |
| Repo / path | `cfmeet` → `docs/admin/cfmeet-admin-bridge.md`, plus ADR `docs/decisions/0068-hq-managed-admin-bridge.md` |
| Owner | CF Meet backend |
| Counterpart | `centralize-admin/docs/product-admin/cfmeet-admin-design.md`, which pins the contract this document owns |

## 1. Goal

**Every CF Meet admin task is done from HQ.** That covers:
- users, meetings, chat and calls;
- partners and apps;
- verification and pricing;
- operations and switching features on.

**Staff never touch CF Meet directly.** They don't use CF Meet servers, command-line tools, GitHub buttons, or lists of reviewer names.

**One service is the doorway.** HQ reaches CF Meet only through a new service, the **admin bridge**. HQ never calls a CF Meet service directly.

```
HQ web → HQ API → Product Integration Core (PIC) → cfmeet-admin (centralize-admin repo)
   → services/admin-bridge (this repo; identity cfmeet.admin-bridge)
   → Meeting · Organizations · Developer Platform · Platform Access · Messaging · Calling · Communications · Notifications
```

**What the bridge does and doesn't do:**
- It does: authenticate HQ, check permissions, call the owning services, write audit, and keep small read summaries.
- It never stores business data.
- It never reads end-to-end encrypted content.
- It never touches Docker, the servers or secrets.

**Owner decisions applied:**

| Topic | Decision |
|---|---|
| Environments | HQ staging connects to CF Meet production, read-only at first. Actions are switched on one family at a time after certification |
| Pricing | Covers partners and CF Meet users (Free / Pro / Business). Card charging waits for a payment provider |
| Feature switch-ons | HQ can request them with two-person approval. The bridge runs the existing release workflow |
| Reviewer name lists | Removed. Approvals come from HQ staff |
| Verification | CF Meet's verification queues appear in HQ's Verification page |
| Approval expiry | Every two-person approval expires after **12 hours** |

## 2. Service facts

| Item | Value |
|---|---|
| Location | `services/admin-bridge` (Rust, axum, sea-orm; its own Cargo workspace with a `migration` crate) |
| Identity | `cfmeet.admin-bridge` in `packages/service-auth` |
| Database | `cfmeet_admin_bridge`, with separate migrator and runtime roles. The runtime role can't be superuser, create objects, bypass row security, or own the audit tables |
| Listeners | HQ: private mTLS port. Loopback: `/health`, `/ready`, `/metrics`. Owner events: private network only |
| Base path | `/internal/cfmeet-admin/v1` |
| Defaults | Everything off: `ADMIN_BRIDGE_ENABLED`, `ADMIN_BRIDGE_WRITES_ENABLED`, plus one switch per module |
| Container | Non-root (uid 10001, the CF Meet convention), built from the repo root, with a `.dockerignore` allowlist |

## 3. How HQ calls the bridge

Each request passes these checks, in this order:

1. **Request shape.**
   - Path at most 4096 characters, with no encoded or dotted parts.
   - Body at most 32 KiB.
   - If the bridge is off, it answers 503.
2. **Client certificate.**
   - TLS 1.3.
   - The certificate must carry a single URI name equal to the enrolled HQ workload.
   - Its fingerprint must match a live row for `cosmicforge-hq-cfmeet-admin`. This is checked on every request.
3. **Signed request (HMAC v2).**
   - The signature covers `2`, the service name, method, URI, time, a one-time number, and the body hash.
   - Headers: `X-HQ-Key-ID`, `X-Service-Name`, `X-Service-Timestamp`, `X-Service-Nonce`, `X-Service-Signature-Version: 2`, `X-Service-Signature`.
   - Clocks may differ by at most ±30 s. Each key has its own expiry.
4. **Staff token.** An Ed25519 JWS whose header is exactly `{alg: EdDSA, typ: JWT, kid}`.
   - Claims: `iss, sub, aud, product="cfmeet", roles, permissions, market/organization/zone/tenant scopes, session_id, auth_time, acr, jti, iat, nbf, exp, request_digest`.
   - Approved actions also carry `approval {approval_id, requester_ref, approver_ref, intent_digest, expires_at}`.
   - The token lives at most 60 s.
   - For actions at risk R2 or higher, the staff member must have passed MFA within the last 300 s.
5. **Tied to the request.** `request_digest` is the sha256 of the method, URI, `cfmeet` and the body hash.
6. **Revocations.** Checked per staff member and per session.
7. **Replay protection.** The one-time number and the token ID are stored together in one transaction for 11 minutes. A repeat gets 409.
8. **Capability lookup.** The route is looked up in the cached capability list. Anything not listed gets 404.
9. **Permission.**
   - The signed access-policy version must match.
   - The permission, purpose and role must all be allowed.
   - All four scopes must be inside the token's scopes.
   - R3 and R4 also require strong sign-in.
10. **Rate limit.** Per staff member, permission and scope.
11. **Audit before answering.** The audit row is written before any answer. If writing it fails, the bridge answers 503 and returns no data.

**Scope on every request:**

| HQ scope | Meaning in CF Meet |
|---|---|
| `market` | Country, or `GLOBAL` |
| `organization` | `cfmeet` |
| `zone` | `production` or `sandbox` |
| `tenant` | CF Meet's own tenant, or a partner's tenant |
| `purpose` | Required, from a fixed list: `support-review`, `verification-review`, `abuse-review`, `billing-review`, `pricing-change`, `ops-incident`, `release-request`, `privacy-request` |

**Answer format.** `{contract_version:"1.0", product:"cfmeet", correlation_id, as_of, freshness, result, error}`. Missing data is shown as `null` or UNAVAILABLE, never as zero.

**What we do better than KarryGo's bridge:**
- one route table and one authorization path, instead of one giant router;
- the capability list is cached;
- the replay and rate-limit tables get cleanup jobs;
- the audit can't be truncated and is hash-chained;
- there is one readiness version;
- a staff member's first role is not used by default.

## 4. How the bridge calls CF Meet services

- **Service authentication.** The bridge uses CF Meet's own service auth: Ed25519 `cf-service+jwt` tokens with a single audience, valid at most 300 s. The HQ token is never passed on.
- **Who did it.** Every call carries `X-CF-Operator: hq:<staff>`. Approved actions also carry `X-CF-Approver: hq:<staff>`, `X-CF-Approval-Ref` and `X-CF-Intent-Digest`. Owners accept these headers only from `cfmeet.admin-bridge`.
- **Owners record the person, not the service.** These need changes:
  - Calling abuse review;
  - Notifications operations;
  - Communications admin;
  - Organizations, which today trusts only the Meeting Service for the acting user.
- **Second guard for risky actions.** For R3/R4 actions, each owner keeps an append-only decision record. A database rule enforces that the approver is not the person carrying out the action.
- **Permissions.** Permissions come only from each owner's trust bundle. New grants ship through the release tool under the activation `activate-admin-bridge`.

## 5. The contract HQ pins

Location: `services/admin-bridge/docs/contract/`. The repo is public, so HQ's CI can read it at a pinned commit.

| File | Content |
|---|---|
| `openapi.json` | All bridge routes |
| `CFMEET_ADMIN_CAPABILITY_MATRIX.json` | Per capability: `id, module, owner, endpoint, method, kind, state, permission, risk, privacy, approval, feature_flag, tests, blocker` |
| `schemas/` | Closed JSON schemas for every request and result |
| `fixtures/` | Example staff token, command, error |
| `event-adapters.v1.json` | The owner events the bridge accepts |
| `signing-profile.md` | §3 in detail |
| `SHA256SUMS` | Hashes of everything above |

CI generates these files, checks them for drift, and refuses breaking changes. A capability marked AVAILABLE never grants anything at runtime by itself.

**Risk levels:**

| Level | Covers |
|---|---|
| R0 | Totals and health |
| R1 | Reads of personal or business-sensitive data (every read is logged) |
| R2 | Reversible operational changes |
| R3 | Security or access changes |
| R4 | Money, anything irreversible, data export or deletion, going live, switching features on |

R3 and R4 always need a second person.

## 6. What HQ can manage

**Status values used in the tables below:**
- **Exists**: an owner API already exists.
- **New**: a new owner endpoint is needed (§7).
- **CLI→API**: today it is only a server command-line tool.
- **DB→API**: today it is only in the database.

### 6.1 Accounts (Meeting Service)

| Capability | Risk | Status |
|---|---|---|
| Search users by email, name, ID or CF Meet ID; user detail | R1 | New |
| Change status (also signs the user out everywhere and updates their identity) | R3 | Exists, moved. Also: block admins changing their own status |
| Change role | R3 | Exists, moved |
| Email change by support | R3 | Exists, moved |
| List sessions / revoke all sessions | R1 / R3 | New / Exists |
| List and revoke devices | R3 | New |
| MFA status / MFA reset | R1 / R3 | New |
| Security history (sign-ins and changes) | R1 | Exists |
| Duplicate accounts: list / enforce | R1 / R3 | Exists |
| Data export or account deletion by staff (respects legal holds) | R4 | New |
| Test accounts | R1 | New (today an environment setting) |

### 6.2 Meetings (Meeting Service)

| Capability | Risk | Status |
|---|---|---|
| Search meeting spaces and meetings across everyone | R1 | New |
| Live meetings and their health | R0 / R1 | Partly exists |
| Participants | R1 | New |
| End a live meeting | R3 | New |
| Archive or transfer a space | R3 | Exists, moved |
| Recordings and transcripts: list / delete | R1 / R4 | New |
| Webinars and registrations | R1 | New |
| Meeting audit trail | R1 | DB→API |
| Meeting abuse reports: queue and review | R2 / R3 | New (doesn't exist today) |
| Chat migration controls; email delivery; critical notification lane | R1 / R3 | Exists, moved |

### 6.3 Chat (Messaging)

These are new staff tools for CF Meet's own chats, closing gap MSG-017.

| Capability | Risk | Status |
|---|---|---|
| Reports queue; report detail (message evidence only where policy allows) | R1 | New |
| Moderation: remove a message, mute, remove a member, restrict, dismiss | R3 | New |
| Chat policy, including attachments on or off | R3 | New |
| Legal holds | R3 | New |
| Exports (unencrypted chats only; encrypted chats are always refused) | R4 | New |
| Deletion requests | R4 | New |
| Chat audit | R1 | New |
| Emergency off-switches: sending, new chats, encrypted setup, uploads, SMS, webhooks | R3 | CLI→API |
| Incidents; retrying failed messages | R2 | CLI→API |
| Key-transparency status and checkpoint | R0 / R3 | CLI→API |

### 6.4 Calls (Calling)

| Capability | Risk | Status |
|---|---|---|
| Call search and history / live calls | R1 / R0 | New / DB→API |
| Abuse reports: list and resolve | R2 | Exists (not yet granted) |
| Blocks; limits; routing and enterprise features | R3 | CLI→API |
| Spend limits | R4 | CLI→API |
| Recordings and voicemail (every access is logged) | R1 / R4 | New |
| Retention holds | R4 | CLI→API |
| Incidents | R2 | CLI→API |
| Webhook keys | R3 | CLI→API |
| Staff audit | R1 | DB→API |

### 6.5 Profiles (Communications)

| Capability | Risk | Status |
|---|---|---|
| Find a profile by CF Meet ID | R1 | New |
| Suspend or restore a profile | R3 | Exists, not yet granted; must record the person |
| Reports queue; review a report (optionally place a hold) | R1 / R3 | Exists, not yet granted |
| List holds / audit | R1 | New / DB→API |
| Backfill, replay, metrics | R0 / R2 | Exists |

### 6.6 Business organizations (Organizations)

| Capability | Risk | Status |
|---|---|---|
| List, search, create | R1 / R2 | Exists |
| Suspend, archive, reactivate; ownership recovery; service bindings | R3 | Exists |
| Export | R4 | Exists |
| Audit: one organization / across all | R1 | Exists / New |
| Retry provisioning; replay events | R2 | Exists |
| Members, teams, policies: read / staff changes | R1 / R3 | New |

### 6.7 Partners (Developer Platform)

| Capability | Risk | Status |
|---|---|---|
| Workspaces: list, search, overview, staff activity | R1 | Exists |
| Workspace state; register a first-party workspace | R3 / R4 | Exists |
| Projects, environments, members, invitations | R1 | New |
| Keys and service accounts: list / revoke or suspend | R1 / R3 | New |
| Allowed websites; push key details (never the key itself) | R1 / R3 | New |
| Partner users: list / sign out | R1 / R3 | New |
| Webhooks, deliveries, retry | R1 / R2 | New |
| Usage operations | R0 / R2 | Exists |
| Support tier | R2 | Exists |
| Incidents: list / record | R2 | New / Exists |

### 6.8 Identity (Platform Access)

| Capability | Risk | Status |
|---|---|---|
| Tenants, applications, environments, service accounts: list | R1 | CLI→API |
| Status changes | R3 | Exists |
| Scope grants (normal / privileged) | R3 / R4 | Exists / CLI→API |
| Keys: issue, rotate, revoke, suspend, resume | R4 | Exists |
| Identity records: register, set status, remove | R4 | Exists |
| OAuth apps, app grants, partner users: staff views | R1 / R3 | New |
| Identity audit | R1 | Exists |

### 6.9 Apps (Developer Platform)

| Capability | Risk | Status |
|---|---|---|
| App list and detail | R1 | Exists |
| Suspend (removes every install) / lift a suspension | R3 | Exists / New |
| Review decisions | — | Move to Verification (§6.10) |

### 6.10 Verification (feeds HQ's Verification page)

Every product uses one shared case shape, so HQ's Verification page can show them together:

```json
{
  "case_id": "cfmeet:<kind>:<id>",
  "product": "cfmeet",
  "kind": "PARTNER_PRODUCTION_ACCESS | APP_REVIEW | ORGANIZATION | NUMBER_KYC",
  "subject": {"type": "…", "id": "…", "display_name": "…"},
  "submitted_at": "…",
  "status": "PENDING | IN_REVIEW | APPROVED | REJECTED | SUSPENDED | EXPIRED",
  "risk": "R3 | R4",
  "evidence_refs": ["…"],
  "assigned_to": "hq:<staff> | null",
  "required_approvals": 2,
  "approvals": [{"by": "hq:<staff>", "at": "…"}]
}
```

| Capability | Risk | Status |
|---|---|---|
| Case list, detail and history | R1 | New view over existing data |
| Partner go-live: claim / approve / reject / suspend | R4 | Exists. Replaces `DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS` |
| App review: claim / approve / reject / publish | R4 | Exists. Replaces `DEVELOPER_PLATFORM_APP_REVIEWERS` |
| Business verification | R3 | Future |
| Phone-number identity checks | R4 | Disabled while telecom is off |

A staff member who belongs to the partner's own workspace can't decide that partner's case. Staff can link their CF Meet account so this check works.

### 6.11 Pricing and plans

See §8.

### 6.12 Finance reports

- **Reports:** usage per partner and per plan, estimated charges, and how users are spread across plans.
- **Money rules:** each figure is in one currency, in minor units, with no conversion.
- **Revenue:** nothing is labelled "recognized revenue" until a revenue policy exists.

### 6.13 Notifications

| Capability | Risk | Status |
|---|---|---|
| Deliveries; failed deliveries and retry | R1 / R2 | Exists, not yet granted |
| Email suppressions | R2 | Exists |
| Routing per category and channel | R3 | DB→API |
| Cancel a reroute | R3 | Exists |
| Push and email provider status | R0 | New |

### 6.14 Privacy requests

One request is tracked across every service that holds the person's data:
- Services covered: Meeting, Messaging, Calling, Communications, Organizations, Developer Platform, Notifications.
- Request types: access/export, erasure, correction, restriction.
- Each service reports one of: pending, done, not held, legal hold.
- The request counts as complete only when every service is done.

### 6.15 Operations

| Capability | Risk | Status |
|---|---|---|
| Health and readiness of every service | R0 | New view |
| Incidents across services | R1 / R2 | CLI→API |
| Switches held in databases (chat off-switches, notification routing, numbering controls) | R3 | CLI or SQL → API |
| Feature switch-on requests | R4 | New (§10) |
| Release history | R0 | New |

### 6.16 Audit

- One search across every service's audit.
- New read APIs are needed for the audit logs that are database-only today: calling staff, chat staff, profiles, meetings, app and go-live transitions, and retention.

### 6.17 Telecom

Numbers, phone calls, SMS and the privacy relay are shown read-only or disabled while telecom is off.

## 7. New staff endpoints needed in each CF Meet service

**Rules for every new endpoint:**
- It lives under `/internal/v1/operator/*`, on the private network only.
- It needs a new `*.administer` permission that only the bridge can hold.
- It requires `X-CF-Operator`. R3/R4 actions also require `X-CF-Approver`, who must be a different person.
- It writes an audit row and has tests.

| Service | Permissions | Work |
|---|---|---|
| Meeting Service | `account.administer`, `meeting.administer` | Staff tools that don't need a CF Meet admin login: everything marked New in 6.1–6.2, plus meeting reports and meeting audit. The old admin routes are retired later |
| Messaging | `messaging-firstparty.administer` | §6.3. Replaces the `messaging-admin` command-line tool for routine work |
| Calling | `calling.administer`; grant the existing abuse-review permission | §6.4. The `platform_admin` tool stays for emergencies only |
| Communications | grant the existing profile-suspend permission; new `communications.administer` | Profile lookup, holds list, audit; record the person |
| Organizations | grant `organization.administer`; new `organization.operator-read` | Cross-organization audit; members, teams and policies |
| Developer Platform | grant `developer-platform.administer`; new `pricing.administer` | Staff reads for partner resources; incident list; lifting app suspensions; recording HQ approvals on decisions; the pricing module |
| Platform Access | grant `platform-identity.administer` | List APIs for everything that is command-line only today; OAuth and partner-user views |
| Notifications | grant the existing `notifications-*` permissions; new `notifications.administer` | Routing changes through an API instead of SQL; provider status |
| service-auth | — | New identity and permission entries; contract enum update; manifest version bump |

## 8. Pricing and plans

This is new, and owned by the Developer Platform.

**Price books**
- **Lineages.** There is one price book per product line, currency and market. Product lines are `PARTNER_PLATFORM` and `CONSUMER`.
- **Version states.** DRAFT → SUBMITTED → APPROVED or REJECTED → SUPERSEDED.
  - A version is frozen once it leaves DRAFT.
  - Its content is hashed. Any change to the content cancels its approval.
- **Approval.** A second person must approve, and the approval expires after **12 hours**.
- **Activation.** Each activation has an `effective_from` date. You can roll back to an older approved version. The history is append-only.
- **Market freeze.** A market can be frozen. Unfreezing needs approval.

**Partner rates (`PARTNER_PLATFORM`)**
- **Priced per usage type:** monthly active users, meeting minutes, in-app call minutes, messages, recording minutes, storage, API calls.
- **Each rate supports:** volume tiers, included amounts, and a minimum monthly charge.
- **Amounts:** in minor units.

**CF Meet user plans (`CONSUMER`)**
- **Starting set:** **Free, Pro, Business**. HQ can rename them, change their limits and prices, or add tiers.
- **Limits each plan controls:**

| Limit | Free | Pro | Business |
|---|---|---|---|
| Meeting length | set in HQ | set in HQ | set in HQ |
| Participants per meeting | set in HQ | set in HQ | set in HQ |
| Recording | set in HQ | set in HQ | set in HQ |
| Storage | set in HQ | set in HQ | set in HQ |
| Attachment size | set in HQ | set in HQ | set in HQ |
| Webinars | set in HQ | set in HQ | set in HQ |
| Price (monthly / yearly) | 0 | set in HQ | set in HQ |

- **Assignment.** Plans are assigned to a user or a business organization, with history. Everyone starts on Free.
- **Enforcement (new).**
  - The Meeting Service and Messaging read the current plan of the user or organization (cached) and enforce its limits.
  - Partner limits that exist today but aren't enforced get enforced too: participants, concurrent calls, attachment size.

**Not yet:** card payments, invoices and tax. They wait for an owner decision on a payment provider. A billing-account reference is kept ready for that.

**HQ actions and their risk:**

| Action | Risk |
|---|---|
| Draft, edit or submit a price version | R3 |
| Approve or reject a price version | R4, needs a second person |
| Schedule, activate or roll back | R4 |
| Freeze or unfreeze a market | R4 |
| Assign a plan | R3 / R4 |
| Plan overrides | R4 |
| Simulate charges | R1 |

## 9. Two-person approvals and running an action

Approvals are kept in `cfmeet-admin`, on the HQ side, following HQ's Section 11 model.

1. **Propose.** Staff A proposes the exact action. The intent digest covers the action, the target with its expected version, the parameters, the scope, and the requester.
2. **Approve.** Staff B approves. B must be a different person, hold the whole scope, and have passed MFA within the last 300 s. The approval is valid for **12 hours** (between 60 s and 43,200 s).
3. **Run.** A runs the action with the approval reference. The approval can be used only once.
4. **Bridge check.** The bridge receives the approval inside the signed staff token and checks three things:
   - the digest matches the actual request;
   - the approver is not the requester;
   - the approval hasn't expired.
5. **Owner record.** The owner records both people in its decision log.
6. **Send once.**
   - A clear answer means SUCCEEDED; a refusal means FAILED.
   - A timeout or broken answer means UNKNOWN_OUTCOME. That is settled only by reading the result back. The action is never sent again.

**High-risk families (R3/R4).** These are switched on only after two named reviewers certify them. A certification lasts at most 180 days and can be revoked.

**Database rules.** Settled operations can't change. There is only one live slot per target and expected version.

## 10. Switching features on from HQ

- **Action.** `cfmeet.operations.activation.request` (R4, two people).
  - Parameters: `{operation, release_sha, confirm}`.
  - `operation` must be one of the release workflow's options.
  - `confirm` carries the owner confirmation words where they are required.
- **What the bridge does.** It starts `cfmeet-coordinated-release.yml` on `main` through GitHub's dispatch API, with a token that can only start that workflow, and records the run.
- **What HQ sees.** The run's status and a cleaned-up release history.
- **What stays the same.** The CI gate, the journal, rollback and the acceptance checks.
- **What never happens.** The bridge never touches Docker, the servers or secrets.

## 11. Events and metrics for HQ

**Owner events**
- Owners send signed events to the bridge's private event listener.
- Accepted event types:
  - `verification.case.changed.v1`
  - `abuse.report.changed.v1`
  - `partner.workspace.changed.v1`
  - `app.review.changed.v1`
  - `incident.changed.v1`
  - `pricing.version.changed.v1`
- The bridge deduplicates events, checks their versions, and quarantines unknown ones.
- It keeps small summaries and an append-only feed for HQ:
  - `GET /hq/events` and `POST /hq/event-acknowledgements`;
  - `GET /hq/projections/status`, which reports freshness, coverage, gaps, quarantine and lag.

**Datasets for HQ**

| Dataset | Metrics |
|---|---|
| `cfmeet.engagement_daily` | Daily and monthly active users, sign-ups |
| `cfmeet.meetings_daily` | Meetings, participant minutes, peak live meetings |
| `cfmeet.messaging_daily` | Messages sent (count only) |
| `cfmeet.partners_daily` | Workspaces by state, partners live, partner monthly active users |
| `cfmeet.apps_daily` | Apps by state, installs |
| `cfmeet.verification_queue` | Pending, in review, approved, rejected |
| `cfmeet.trust_safety_queue` | Open abuse reports by source |
| `cfmeet.operations_health` | Failed messages and deliveries, incidents, sync health |

**Reports:** usage, estimated charges, plan spread, verification pipeline, abuse pipeline.

## 12. Privacy rules

- **Encrypted content** is never read, exported or copied. Unencrypted chat evidence appears only inside a report, and only where policy allows it.
- **HQ's banned-field list applies:** passwords, one-time codes, card and bank numbers, ID documents, location trails, medical data, secrets and tokens.
- **Personal details:**
  - Names and emails appear only in R1 detail reads, and every such read is logged.
  - Summaries hold IDs and counts only.
  - Restricted personal data never goes to HQ.
- **Exports:**
  - encrypted;
  - downloadable once, by the requester only, within 15 minutes;
  - limited to 93 days and 10,000 rows;
  - need approval.

## 13. Bridge tables

All tables are append-only or protected by triggers. The audit can't be updated, deleted or truncated, and is hash-chained.

| Area | Tables |
|---|---|
| Security | `admin_remote_principals`, `admin_security_claims` (cleaned up), `admin_revocations`, `admin_rate_limits` (cleaned up), `admin_policy_revisions` |
| Audit | `admin_audit_entries` |
| Actions | `admin_operations`, `admin_operation_audit` |
| Events | `admin_event_ingress_nonces`, `admin_event_inbox`, `admin_event_quarantine`, `admin_projections`, `admin_projection_gaps`, `admin_hq_outbox`, `admin_hq_consumer_cursors`, `admin_backfill_runs`, `admin_reconciliation_runs` |
| Privacy and exports | `admin_privacy_requests`, `admin_privacy_dispositions`, `admin_exports`, `admin_export_downloads` |
| Releases | `admin_activation_requests` |

## 14. Deployment

- **Release tool** (`infra/calling/production/coordinated_release.py`). Add the bridge to:
  - `SERVICES`, `ORDER`, `DEPENDS` and `IMAGES`;
  - database and roles, signing key and trust bundle;
  - environment template and safe defaults (all off);
  - permission grants under `activate-admin-bridge`.
- **CI.**
  - A new `admin-bridge.yml` covering format, lint, Postgres tests and contract drift.
  - Also add the bridge to: the image list, the dependency scan, the release gate's required checks, the backup catalog, and the database provisioning scripts.
- **HQ access.**
  - A dedicated private mTLS port on the CF Meet host, with TLS passed straight through.
  - Only the HQ staging server is allowed to connect (production HQ later), and HQ's certificate must be enrolled.
  - It is never reachable through `api.cfmeet.com`.
- **Owner events.** Accepted on the private CF Meet network only.

## 15. Build phases

Each phase is its own PR. It ships switched off and has an acceptance check. The matching central-admin phase is in brackets.

| Phase | Work | Done when |
|---|---|---|
| A0 | Contract files, ADR, new identity and permissions | HQ can pin the contract [B0] |
| A1 | Bridge skeleton: security chain, audit, health, discovery, read-only datasets | HQ staging reads CF Meet metrics; forged and replayed requests are refused [B1] |
| A2 | Reads for every module, plus the owner read endpoints | Staff can look up everything, read-only [B2] |
| A3 | Verification and approvals: partner go-live, app review | First partner approved from HQ by two people; reviewer name lists removed [B3] |
| A4 | Account and safety actions | Support and safety work is handled from HQ [B4] |
| A5 | Chat and calling staff tools (command-line tools become APIs) | Off-switches, failed-message retries and limits are handled from HQ [B5] |
| A6 | Pricing, Free/Pro/Business plans, enforcement | Prices and plans are set and enforced from HQ [B6] |
| A7 | Events, exports, privacy requests | Live metrics; one data request completed end to end [B7] |
| A8 | Operations and feature switch-on requests | A switch-on requested, approved and run from HQ [B8] |
| A9 | Production certification | Certified [B9] |

## 16. Testing

- **Each new owner endpoint:** unit and Postgres tests. Wrong permission is refused, self-approval is refused, and an audit row is written.
- **Bridge:** an HQ simulator over real mTLS. It covers:
  - forged signature, replay, expired token, wrong scope, missing MFA;
  - audit failure, digest mismatch, single-use approvals;
  - reading back an unknown outcome;
  - event ordering, gaps and quarantine.
- **Cross-service:** two-tenant isolation. One tenant never reaches another tenant's data.
- **Contract:** drift and compatibility checks.
- **Production:** an acceptance check for each activation.

## 17. HQ interface v1: what was agreed with HQ (Track B)

Agreed with the `centralize-admin` session building `services/cfmeet-admin` (2026-11-19). The generated, authoritative rules are in `services/admin-bridge/docs/contract/signing-profile.md`.

### 17.1 Pinnable commit

| Contract version | Commit on `main` | Notes |
|---|---|---|
| 1.0.0 (A0) | `7325f30447891ed91ac0b421c8eb363a9c356609` (merge of #121, 2026-11-20) | Contract only. Every capability is DISABLED or BLOCKED_DEPENDENCY; nothing is served |
| 1.1.0 (A1) | `df610aca769970bafb8b3a48ef928235d9a168e9` (merge of #122, 2026-11-20) | Runtime. `bridge.capabilities.read`, `bridge.health.read`, `bridge.datasets.read` and `hq.projections.status` are READ_ONLY; new error code `BRIDGE_UNAVAILABLE`. Additive over 1.0.0 |
| 1.2.0 (A2.0) | `568328cca32749e207a1fe0be7f664da6380202c` (merge of #132, 2026-11-20) | Organizations reads served: search, detail, audit (one and across all), members, teams, policies, service bindings; Health in HQ's shape; new error code `RESOURCE_NOT_FOUND`. Additive over 1.1.0 |
| 1.4.0 (A2.2) | *recorded when A2.2 merges* | Notifications reads served: deliveries and failed deliveries (time window), suppression lookup by address or digest (keyed digest only), routing, provider status. Additive (1.3.0 is the Developer Platform slice) |
| 1.5.0 (A2.1) | *recorded when A2.1 merges* | Developer Platform reads served: partner workspaces (search, detail, staff activity), projects, environments, members, invitations, push key details, webhooks and deliveries; apps (search, detail, versions, installs). Keys, service accounts, websites and partner users wait for Platform Access; incidents have no list at the owner; usage waits for HQ's Summary shape. Additive over 1.4.0 |
| 1.6.0 (A2.3) | *recorded when A2.3 merges* | Platform Access reads served: identity tenants (search), applications, environments, service accounts, scope grants, API keys (public id and last four only), identity-record search, OAuth apps and their grants, partner users, identity audit; the partner keys, service accounts, allowed websites and users reads move to Platform Access (a workspace id is its tenant id). Never a secret, verifier, label or external reference. `identity.records.list` joins `x-contact-search` (a partner-chosen principal reference may be an email address). Additive over 1.5.0 |
| 1.7.0 (A2.4) | *recorded when A2.4 merges* | Communications reads served: profile by CF Meet ID (display name, activation state, operator hold; never contact fields), reports (safety evidence, no free text), operator holds, Hub audit (details dropped), metrics as HQ's `Summary` (with `Scope`, published verbatim). Additive over 1.6.0 (Platform Access) |
| 1.8.0 (A2.5) | *recorded when A2.5 merges* | Meeting Service reads served: account search (email matched, never returned) and detail (email masked), sessions, devices, MFA status, security history; meeting spaces, meeting search and detail, live meetings, participants, conference audit. No address, token, session label, device key, MFA secret, IP address, location, content or media. Additive over 1.7.0 |
| 1.9.0 (A2.6) | *recorded when A2.6 merges* | Messaging reads served: reports (list and detail with moderation actions), legal holds, exports, deletion requests, chat audit, emergency off-switches, incidents, failed messages (outbox dead letters), key-transparency status. Ids, categories, states, counts and times only: never message content, ciphertext, keys, report evidence, audit detail, payloads, export object keys or case references. `messaging.policy.read` stays blocked (per-environment free-form document; no environment selector). Additive over 1.8.0 |
| 1.10.0 (A2.7) | *recorded when A2.7 merges* | Calling reads served, every one scoped to the request's tenant (a Platform Access tenant id; another tenant's data is never reached): calls (search, detail with participants by id), live calls, open abuse reports, block rules (a blocked contact only as present), limits, routing, enterprise and spend policies (bounded, `truncated`), recording and voicemail metadata, retention holds and incidents (tenant and platform), webhook sealing keys (counts), operator audit. Never a telephone number, SIP URI, display name, object key or key material. Additive over 1.9.0 |

HQ pins a merged commit on `main` (the branch CF Meet production deploys from) and recomputes `SHA256SUMS`. The running bridge reports the sha256 of its own `openapi.json` in `bridge.capabilities.read`; HQ trusts runtime states only when it equals the pinned hash.

### 17.2 Deviations from HQ interface v1 (`cfmeet-admin-parallel-workplan.md` §3)

| Topic | Workplan §3 | Agreed |
|---|---|---|
| Contract directory | `services/admin-bridge/docs/openapi/` | `services/admin-bridge/docs/contract/` |
| Event registry file | `admin-events.v1.json` | `event-adapters.v1.json` (karryam adapter shape, one entry per source service: 13 entries) |
| Query shapes | `BASE`, `SCOPED`, `SCOPED_WINDOW` | No `BASE`: every route is scoped. `SCOPED`, `SCOPED_PAGE`, `SCOPED_WINDOW`, `SCOPED_SEARCH`, exact order `market, organization, zone, tenant, purpose`, then `limit, cursor`, then `start, end` or `q` |
| `zone_scopes`, `tenant_scopes` | optional | always present |
| Clock skew | 300 s | ±30 s (HMAC timestamp and assertion times) |
| Assertion lifetime | `iat + 20 s` | at most 60 s at the bridge (HQ sends 20 s) |
| Approval claim | `ref, requester_sub, approver_sub, …` | `approval_id, capability, requester_ref, approver_ref, intent_digest, decided_at, expires_at` (refs are raw HQ subs) |
| Purposes | design list | design list plus `admin-ui-capabilities`, `metrics-read`, `event-sync`; hyphenated (discovery is `purpose=admin-ui-capabilities`, not the karryam underscore) |
| Discovery result | karryam metadata | `{contract_version, openapi_sha256, policy_revision (integer ≥ 1), capabilities[{id, state, blocker}]}`, runtime states never above the artifact state |
| Unbuilt reads | `READ_ONLY` at A0 | `BLOCKED_DEPENDENCY` with a blocker naming the phase; `READ_ONLY` means the bridge serves it |
| Unbuilt results | — | `properties.result = {"not": {}}`: no success can validate until the phase defines the schema (contract minor bump) |
| Verification case shape | §3.6 (`case_id` uuid, `kind` PARTNER_GO_LIVE / APP_REVIEW / APP_PUBLISH, `state` OPEN / CLAIMED / …) | Agreed: A3 adopts HQ's §3.6 shared shape unchanged, plus an optional `approvals_required` (integer). Mapping: production-access requests → PARTNER_GO_LIVE, app review → APP_REVIEW, publish → APP_PUBLISH. ORGANIZATION and NUMBER_KYC stay out of the shared enum. This supersedes the shape in §6.10 |

### 17.3 Datasets

Served by `bridge.datasets.read` (`GET /datasets/{dataset_id}`, `SCOPED_WINDOW`, purpose `metrics-read`), classification INTERNAL. Machine-readable: `docs/contract/datasets.v1.json`. Coverage is COMPLETE only when every listed source reported for every day of the window; a metric without a report is `null`, never zero.

| Dataset | Metrics | Sources |
|---|---|---|
| `cfmeet.engagement_daily` | `dau`, `mau`, `signups` | Meeting Service (pull) |
| `cfmeet.meetings_daily` | `meetings`, `participant_minutes`, `peak_live_meetings` | Meeting Service (pull) |
| `cfmeet.messaging_daily` | `messages_sent` | Messaging (pull, counts only) |
| `cfmeet.partners_daily` | `workspaces_active`, `workspaces_restricted`, `workspaces_suspended`, `partners_live`, `partner_mau` | Developer Platform (`partner.workspace.changed.v1`, pull) |
| `cfmeet.apps_daily` | `apps_draft`, `apps_in_review`, `apps_published`, `apps_suspended`, `installs` | Developer Platform (`app.review.changed.v1`, pull) |
| `cfmeet.verification_queue` | `pending`, `in_review`, `approved`, `rejected` | Developer Platform (`verification.case.changed.v1`) |
| `cfmeet.trust_safety_queue` | `open_messaging`, `open_calling`, `open_communications`, `open_meetings` | `abuse.report.changed.v1` from Messaging, Calling, Communications, Meeting Service |
| `cfmeet.operations_health` | `failed_messages`, `failed_deliveries`, `open_incidents`, `projection_gaps`, `quarantined_events` | `incident.changed.v1` from Messaging, Calling, Notifications, Meeting Service, Developer Platform; pulls from Messaging, Notifications and the bridge itself |

HQ's own sync health (`cfmeet.sync_health`) stays HQ-local. Privacy requests and plan spread are capabilities and reports, not datasets.

### 17.4 Access policy: role → permission → purpose → scope

Bridge roles equal HQ's product-grant roles (identity role map), plus the HQ worker's feed identity `cfmeet.source_sync`. The ceiling, per role and per permission with its allowed purposes, is `docs/contract/access-policy.v1.json`. The runtime policy is signed by CF Meet, pinned by revision, and refused at load if it grants anything outside the ceiling. Scopes for every role: market as enrolled for HQ, organization `cfmeet`, zone `production` / `sandbox`, tenant as enrolled.

| Role | Modules reached (reads and commands) | Purposes |
|---|---|---|
| `cfmeet.viewer` | discovery, health, datasets | `admin-ui-capabilities`, `metrics-read`, `ops-incident` |
| `cfmeet.support` | accounts, meetings, communications, organizations, partners, notifications | `support-review`, `abuse-review`, `verification-review`, `ops-incident`, `privacy-request` |
| `cfmeet.trust_safety` | accounts, meetings, messaging, calling, communications, apps | `abuse-review`, `support-review`, `ops-incident`, `privacy-request`, `verification-review` |
| `cfmeet.verification.reviewer` | verification, apps | `verification-review`, `abuse-review` |
| `cfmeet.partner_ops` | partners, apps, identity, organizations | `support-review`, `verification-review`, `ops-incident`, `billing-review`, `privacy-request`, `abuse-review` |
| `cfmeet.finance` | finance, pricing (read) | `billing-review`, `pricing-change` |
| `cfmeet.pricing.editor` | pricing: draft, edit, submit, assign | `pricing-change`, `billing-review` |
| `cfmeet.pricing.approver` | pricing: approve, reject, schedule, activate, roll back, freeze, overrides | `pricing-change`, `billing-review` |
| `cfmeet.platform_ops` | operations, notifications, messaging and calling tools, meetings, communications, organizations (replay), telecom, projection status | `ops-incident`, `abuse-review`, `support-review`, `privacy-request`, `billing-review`, `event-sync` |
| `cfmeet.security_admin` | accounts (role, MFA, devices, export, deletion), identity, privacy, audit, messaging exports | `privacy-request`, `support-review`, `abuse-review`, `ops-incident`, `verification-review` |
| `cfmeet.release_manager` | operations: feature switch-on requests, releases | `release-request`, `ops-incident` |
| `cfmeet.source_sync` | `hq.events.feed`, `hq.events.ack`, `hq.projections.status` (`event-sync`) and `bridge.datasets.read` (`metrics-read`), nothing else; the worker pulls datasets with `limit=31` and a window of at most 31 days ending now | `event-sync`, `metrics-read` |

Every staff role also reaches discovery (`admin-ui-capabilities`) and datasets (`metrics-read`). A role's purposes are limited per permission; `cfmeet.source_sync` is additionally limited to `event-sync` and `metrics-read` whatever a capability allows.

### 17.5 Reads that use POST

Only `pricing.charges.simulate` (its input is a usage vector). Every other non-GET route is a command.

### 17.6 Commands: the operations ledger and readback (A3.0, agreed with HQ)

No command is served by A3.0 itself; it is the core every A3+ family runs through (`services/admin-bridge/src/routes/commands.rs`).

- **One UUID per HQ command.** `Idempotency-Key` = body `operation_id` = the readback key. A body whose `operation_id` differs is `COMMAND_BODY_INVALID`.
- **Every command body** is `{operation_id, expected_version, reason_code?, note?}` plus the family's own fields; unknown fields are refused. `expected_version` is a string or the owner's non-negative integer row version (the same slot either way).
- **Ledger** (`admin_operations`), keyed by staff member + `Idempotency-Key`:
  - A replay with the same intent (capability, path parameters, purpose, scope, exact body, approval) returns the stored outcome and does not consume the approval again.
  - A different intent under the same key is `IDEMPOTENCY_CONFLICT`.
  - Only one non-failed operation may hold a target's expected version: `VERSION_CONFLICT` (409, nothing done). An owner's 409 is also `VERSION_CONFLICT`.
- **Send once.** `PENDING` → `SENT` is recorded before the owner call. The owner gets the bridge's own service token, `X-CF-Operator`, `Idempotency-Key` and, for two-person commands, `X-CF-Approver`, `X-CF-Approval-Ref`, `X-CF-Intent-Digest`.
  - 2xx: `SUCCEEDED`.
  - 4xx: `FAILED` (404 `RESOURCE_NOT_FOUND`, 409 `VERSION_CONFLICT`, otherwise `OWNER_REFUSED`).
  - Never sent (owner not configured): `FAILED`, `CAPABILITY_DISABLED`.
  - Anything else: `UNKNOWN_OUTCOME` (502).
  - Every outcome is audited after the accepted request's own audit row.
- **Settlement, never a resend.** `GET /internal/v1/operator/operations/{idempotency_key}` on the owner returns `{idempotency_key, outcome: APPLIED|REFUSED, source_status, source_version}`, or 404.
  - Every owner that serves a command must provide this record. It must write the key in the same transaction as the change.
  - The bridge asks on each readback and in a one-minute sweep.
  - With no record 600 s after sending, the operation is `FAILED` with `source_status` `NOT_RECEIVED`.
  - A `PENDING` row older than 120 s was never sent: `FAILED`, `NOT_SENT`.
- **`bridge.operations.read`**, `GET /hq/operations/{idempotency_key}` (SCOPED). Its roles and purposes are those of every command.
  - It finds only the calling staff member's own operations; another's key is `RESOURCE_NOT_FOUND`.
  - The original command's purpose (`PURPOSE_NOT_ALLOWED` otherwise) and exact scope (`SCOPE_DENIED` otherwise) are required.
  - Result: `{idempotency_key, capability_id, status, source_status, source_version, settled_at}`.
  - HQ settles on `SUCCEEDED` and `FAILED`; `PENDING`, `SENT` and `UNKNOWN_OUTCOME` stay unknown.
- **Verification scope (A3.1+).** `verification.*` reads and commands are operator functions scoped to CF Meet's first-party Platform Access tenant (the one HQ enrols). The partner's tenant is the case field `partner_tenant_id`.
- **Staff link (A3.2).**
  - Without `x-cf-operator-user-id`, an R4 decision is allowed and the owner records `link_status=UNLINKED`.
  - With it, a member of the partner's workspace is refused (no self-review).
  - Two-person approval (requester ≠ approver) applies either way.

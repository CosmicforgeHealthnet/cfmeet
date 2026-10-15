# ADR 0031 — Reliability, operations, recovery and deployment separation

Status: Accepted (certification: `docs/architecture/part-i-reliability-certification.md`,
`part-j-persistence-deployment-certification.md`, `part-k-production-acceptance.md`)

## Context

Parts I–K (§§81–110) ask for durable, multi-instance-safe background work, operational
visibility, graceful degradation, tested recovery, least-privilege deployment and a
production acceptance programme. Parts B–H already built most of it (transactional
outboxes, leased SKIP LOCKED workers, reconciliation, incidents, bounded metrics). This
ADR records only the decisions taken to close the remaining gaps. No new service, no
generic worker framework and no shared incident service were introduced: reliability
lives in the service that owns the durable state (ADR 0005).

## Decisions

1. **Intent commits with state — including the release of paid legs.** When a Call enters
   ENDING or a terminal state, a deferred constraint trigger in Calling writes the
   TERMINATE for every live public / enterprise SIP leg in the same transaction (one per
   leg, idempotent). A crash between the transition and a post-commit enqueue can no
   longer leave a paid leg running.
2. **Every durable queue is claimed, capped and visible.** Each claim carries a lease and
   a claim token; reschedule / complete / attempt recording are guarded by that token, so
   a worker that lost its lease cannot act. Every queue has an attempt cap and a terminal
   state (Calling Telephony commands `DEAD`, outbox `DEAD_LETTER`, Privacy Relay
   enforcement / outbox `DEAD_LETTER`, Telephony commands / outbox `DEAD_LETTER`) that
   raises one deduplicated incident. Failures are classified permanent / transient /
   unknown: permanent refusals dead-letter at once, transient ones back off with a cap,
   and a request that may have reached a provider is never re-sent (UNKNOWN_OUTCOME →
   reconciliation). A TERMINATE is never abandoned. A first ORIGINATE is sent only while
   the Call is still active and the dispatcher still holds its claim, so a revocation
   that wins the race sends nothing paid.
3. **Setup deadline.** A Call that never reaches a connected state and records no event
   for `CALLING_CALL_SETUP_DEADLINE_SECONDS` (default 2 h, floor above the longest
   legitimate queue wait) fails with TIMEOUT through the canonical transition; the
   decision 1 trigger releases its paid legs.
4. **Incidents stay per service** (Calling `calling_incidents`, Privacy Relay
   `privacy_relay_incidents`, Telephony `telephony_incidents`), one shape: category,
   severity, dedupe key unique while OPEN, occurrences, first / last seen, safe summary.
   Lifecycle OPEN → RESOLVED (no ACKNOWLEDGED state: nobody needed it). Resolution is an
   operator action of the service's admin CLI, requires `--operator` / `CF_OPERATOR_ID`
   and writes the append-only operator audit in the same transaction; the audit rows are
   the mitigation history. New categories cover dead letters, worker backlog, credential
   guessing, Contact Vault key unavailability, ambiguous relay mapping, Privacy Relay
   unavailability and SIP edge degradation.
5. **Observability is bounded and per service.** Metrics carry only enum labels (no
   tenant, application, principal, Call, contact, number, SIP URI, provider event id,
   object key or secret), guarded by source and rendered-registry tests in each service.
   Privacy Relay gains a metrics listener separate from its API. RTC quality is recorded
   only from evidence the platform has (admitted sessions, reconnects, connected
   participants); transport statistics (RTT, jitter, loss) are not invented. Per-tenant
   quality is a query over durable records, never a metric label.
6. **Correlation is continuous.** The request's correlation id is stored on the Call and
   on every durable command / outbox row and continued on every hop (Calling → Privacy
   Relay → Telephony → Calling); a new id is minted only when none exists.
7. **Credential guessing** is throttled per presented key id (unknown ids share one
   bucket, so guessing cannot grow state): above the threshold further *failures* get 429
   while a correct secret is always accepted (no lock-out DoS), with one incident.
8. **Kill switches** are server-side environment flags that refuse only NEW work of their
   capability (master, app calling, public outbound / inbound, callback relay, enterprise,
   voicemail, recording, Privacy Relay, Contact Vault, relay numbers); ending, cleanup,
   reconciliation and artifact deletion never consult a switch. Providers / routes are
   disabled by audited operator commands, not flags.
9. **Recovery is per service.** `infra/deployment/backup/` dumps and restores exactly one
   service database (checksummed, into an empty database, one transaction). Key material
   is never in a database backup; restoring Privacy Relay without its keyring fails
   closed. After a restore, expired leases make pending work reclaimable exactly as after
   a crash. Each service certifies this in `tests/disaster_recovery.rs`.
10. **Least-privilege databases.** Each service database has a migrator role (owner, DDL,
    release time only) and a DML-only runtime role; CONNECT is revoked from PUBLIC so no
    role of one service can open another service's database
    (`infra/deployment/postgres/`, certified in CI with the real migrations).
11. **Secret blast radius** is enforced by the service-boundary guard
    (`FOREIGN_SECRET_REFERENCE`): production code and deployment configuration of one
    service never name another service's secrets.
12. **Supply chain.** RustSec advisories gate every Part III lockfile; each image's root
    filesystem is secret-scanned and its Debian base must have no pending security
    update (`dependency-security.yml`). An advisory is ignored only with a written
    justification in the workflow.

## Consequences

* New migrations: Calling 000043, Privacy Relay 000014, Telephony 000015 (additive; DOWN
  supported). New tables: `platform_auth_failure_windows` (Calling),
  `privacy_relay_incidents`, `privacy_relay_operator_audit` (Privacy Relay).
* Dead-lettered work is final; re-drive is an explicit operator action where one exists
  (Calling outbox replay) and otherwise driven from the incident.
* Real-carrier capacity, media-plane capacity and the Telecom production gate remain
  outside what this repository can certify (ADR 0016).

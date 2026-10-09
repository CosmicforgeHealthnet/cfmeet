# Service data ownership

Every deployable service under `services/<name>/` owns its source, its migrations, its
database and the meaning of its data. Other services reach that data only through the
owner's authenticated API or the owner's durable events. This page is the reference; the
rules are enforced by
[`scripts/service-boundaries/check_service_boundaries.py`](../../scripts/service-boundaries/check_service_boundaries.py)
(`make service-boundaries`, CI: `.github/workflows/service-boundaries.yml`) and by each
service's own architecture tests. Decision record:
[ADR 0005](../decisions/0005-contract-authority-and-runtime-boundaries.md).

## Owners (Part III §13)

This is the authoritative five-domain ownership model. Each domain is owned by exactly
one service; *planned* and *future* services are boundaries only — no directory, code,
schema or contract exists for them until their implementation phase begins.

| Service | Status | Owns | Must not |
|---|---|---|---|
| **Calling** (`services/calling`) | **Active.** Platform-consumer foundation, Calling core (Part C §15–§19: calls, participants, sessions, routes, legs, attempts, authorizations, events, quality, usage), application calling (§20–§25: endpoints, invitations, answer claims, media permissions, private media bindings, command idempotency, transfers, verified media-provider evidence) and Part D §26–§30 evidence (`call_privacy_authorizations`: the Privacy Relay decisions protected calls relied on; `call_privacy_enforcements`: received enforcement commands, deduplicated) and Part E (§37–§38 Routing Engine + `call_routing_policies`; §44–§51 Calling's side of public-network calls: `call_public_parties` by ContactReference, the durable `call_telephony_commands` outbox, received `call_telephony_events`, `call_public_bridges`, `call_spend_policies` / `call_spend_reservations`, `call_inbound_owners`) implemented. **Part F (§52–§62, ADRs 0018–0026):** enterprise calling configuration and continuity — enterprise call policies, departments, extensions, ring groups, call queues (entries, offers), IVR flows / immutable versions / sessions, enterprise SIP device bindings to principals, business hours, forwarding rules, public forwarding targets as ContactReferences only, per-Call enterprise contexts and routing steps, voicemail mailboxes and messages, call-recording policies / authorizations / consents / recordings / access audit, the protected artifact references in its private object-storage bucket, derived call history, its own operational incidents (`calling_incidents`) | • Calls<br>• Participants<br>• Sessions<br>• Routes<br>• Authorizations<br>• Call records<br>• Call quality<br>• Call-level usage<br>• Platform-consumer foundation *(implemented)*: tenants, applications, application environments, service accounts, API credentials and their scopes, limit policy, request-rate windows, communication principals<br>• Enterprise calling configuration and continuity (Part F): `enterprise_*`, `voicemail_*`, `call_recording*`, `calling_incidents`; voicemail / recording audio in Calling's private bucket (Telephony never holds storage credentials) | • read/write Privacy Relay private tables directly<br>• read/write Telephony private tables directly<br>• read Meeting Service tables<br>• receive ordinary plaintext protected contacts |
| **Privacy Relay** (`services/privacy-relay`) | **Active (Part D §26–§36).** Communication Contexts, participant reassignment / revocation, Privacy Relay Sessions, relay authorization decisions, audit + outbox, command idempotency and durable enforcement jobs ([ADR 0010](../decisions/0010-communication-context-and-privacy-relay-session-architecture.md)); the Contact Vault — protected ContactReferences, encrypted contact values, scoped keyed fingerprints, verification, access audit, resolution grants ([ADR 0011](../decisions/0011-contact-vault-cryptography-and-resolution.md)); the temporary relay-number inventory, assignments and inbound mapping ([ADR 0012](../decisions/0012-temporary-relay-number-assignment-and-cooldown.md)) | • Communication Contexts<br>• Relay sessions<br>• Relay authorization<br>• Protected ContactReferences<br>• Contact Vault metadata and ciphertext<br>• Contact verification<br>• Contact access audit<br>• Contact Resolution Grants<br>• Temporary relay-number inventory<br>• Relay-number assignments<br>• Inbound relay resolutions | • expose plaintext contacts to ordinary consumers<br>• become a general product/order database<br>• read Calling's principals, calls or credentials (it receives a signed delegated scope and CF Meet ids only)<br>• terminate calls itself (it asks Calling: `communication-authority.enforce`) |
| **Telephony** (`services/telephony`) | **Active (Part E §39–§51).** Public-network execution with its own database ([ADR 0014](../decisions/0014-telephony-service-core.md)–[ADR 0017](../decisions/0017-provider-failover.md)); the SIP / media edge (Asterisk + ARI, `infra/telephony/`) is its execution plane. Real-carrier certification pending (ADR 0016) | • Telephony legs (executions of Calling's CallLegs)<br>• Durable telephony commands (transmit fence, UNKNOWN_OUTCOME)<br>• Provider bindings (provider call refs, correlation keys)<br>• Provider / carrier events (deduplicated, restricted, retention-bounded)<br>• Telecom providers, routes, circuits, health, capacity<br>• SIP peer configuration REFERENCES (never credentials)<br>• Relay-number ↔ interconnect bindings<br>• Bridges<br>• Provider-level usage and cost evidence<br>• Incidents, reconciliation runs, outbox to Calling<br>• Part F execution only: enterprise device references ↔ edge peers, enterprise SIP legs, ANSWER / PROMPT / CAPTURE / CAPTURE_STOP commands and the transient capture spool (deleted after Calling confirms delivery) | • independently decide whether a business user is authorized to call<br>• own enterprise configuration, routing decisions, voicemail / recording policy or artifacts<br>• bypass Calling / Privacy Relay policy<br>• own or redefine the logical Call<br>• store a private destination number or trunk password<br>• read another service's database |
| **Meeting Service** (`services/meeting-service`) | **Current** | • Meeting Spaces<br>• Conference Sessions<br>• Meeting participants<br>• Meeting-specific media control | • act as the universal backend for calls or telephony |
| **Numbering** (future `services/numbering`) | **Future — not yet created; no code** | • Permanent public-number inventory<br>• Public-number assignment<br>• Public-number lifecycle (Privacy Relay's temporary relay pool is NOT this) | • be prematurely implemented as Part III relay-number logic |

Notes on the model:

* **Each service has its own database.** Calling's is configured by its own
  `DATABASE_URL`; its tables are the platform foundation (`platform_*`), the call
  domain (`calls`, `call_*`, `calling_outbox_events`) and, since Part F, enterprise
  configuration and continuity (`enterprise_*`, `voicemail_*`, `calling_incidents`).
  Enterprise calls are ordinary Calls: there is no `services/pbx`, no
  `services/enterprise-calling`, no `services/voicemail` / `services/ivr`, and no second
  Call table. Telephony remains execution only. Calling's `call_legs` is the
  canonical, provider-independent Calling-side leg; provider bindings, carrier ids, SIP
  dialogs, callbacks and reconciliation state belong to Telephony's own database and are
  referenced by Calling only through an opaque UUID (`execution_binding_ref`). The Meeting Service's database additionally holds the private
  records that support its domains (accounts, organizations and memberships,
  scheduling and recurrence, chat, polls, Q&A, breakouts, transcripts, artifacts,
  webinar registrations, meeting webhooks, `api_keys`, its outbox); no other service
  reads them.
* **Authorizations and relay authorization are separate records.** Calling owns
  Authorizations; Privacy Relay owns Relay authorization. Each service keeps its own
  records in its own database; Calling obtains a relay decision through Privacy Relay's
  authenticated API (`communication.authorize`, received by Privacy Relay) — never by
  reading its tables — and stores only the decision's ids and versions it relied on
  (`call_privacy_authorizations`). Privacy Relay never reads calls: when authority is
  narrowed it sends Calling an enforcement command (`communication-authority.enforce`,
  received by Calling), and Calling decides which of its calls match.
* **Privacy Relay's tables** (from its migrations): `communication_contexts`,
  `communication_context_participants`, `communication_context_channels`,
  `communication_context_directions`, `privacy_relay_sessions`,
  `privacy_relay_session_participants`, `privacy_relay_session_channels`,
  `privacy_relay_session_directions`, `relay_authorization_decisions`,
  `relay_audit_events`, `privacy_relay_outbox`, `privacy_relay_commands`,
  `privacy_relay_enforcement_jobs`; the Contact Vault's `protected_contacts`,
  `contact_fingerprints`, `contact_access_audit`, `contact_verification_events`,
  `contact_resolution_grants`; the relay pool's `relay_number_inventory`,
  `relay_number_events`, `relay_number_assignments`, `relay_inbound_resolutions`. A
  private contact value exists only as AES-256-GCM ciphertext in `protected_contacts`
  (keys outside the database); the only number stored in clear is CF Meet's own
  platform-owned PUBLIC relay number in `relay_number_inventory`. Privacy Relay database
  backups therefore contain encrypted contact data and are sensitive (useless without the
  keyrings, which are never stored with them).
* **Call evidence stays Calling's.** Privacy-safe call evidence of a Relay Session
  (`GET /v1/privacy-relay/sessions/{id}/evidence`) is composed by Calling from its own
  Call records after Privacy Relay proves the session belongs to the caller; Privacy
  Relay never stores call records.
* **Routes and telephony routes are separate records.** Calling owns Routes; Telephony
  owns Telephony routes. Neither service reads the other's.
* **Plaintext contacts.** The only path to a plaintext contact is Privacy Relay's
  `protected-contact.resolve-for-execution`, which the permission registry in
  `packages/service-auth` allows to be granted to Telephony only, for executing a
  specific authorized call leg with a Contact Resolution Grant that Calling requested
  (`protected-contact.grant-for-execution`; the grant carries no value). Calling and
  every other consumer receive protected contact references, never plaintext; Calling
  has no Contact Vault or Privacy Relay database URL. Inbound caller numbers enter the
  vault directly from Telephony (`protected-contact.protect-inbound`).
* **Telephony executes, it does not authorize.** Telephony acts on call legs Calling
  asks it to execute (`call-leg.execute` is grantable to Calling only), within the policy
  Calling and Privacy Relay have already applied.
* **Numbering is future work.** Part III relay-number logic must not be built as, or
  grow into, the Numbering service; permanent public-number inventory, assignment and
  lifecycle are designed with Numbering itself.

Identity is also owned:

* customer platform identity (tenant → application → environment → service account →
  credential) is Calling's (ADR 0003);
* communication principals are resolved by Calling within that scope (ADR 0003);
* service identity is a signed service token verified against the receiver's trust
  bundle (ADR 0004) — never a database lookup in another service;
* the Meeting Service is not an identity provider for Calling or any other service
  (ADR 0005).

Table ownership is **derived from migrations**, so it cannot drift: the guard collects
every `CREATE TABLE` and SeaORM `X::Table` in `services/<name>/**/migration/**`. Two
services may each own a table with the same name in their own databases (for example an
outbox); that is not shared data. Planned services are already known to the guard
through the service identities registered in `packages/service-auth` (`telephony`), so
Calling cannot read their database locations even before they exist;
Numbering becomes known when it is registered or created.

## How to use another service's data

| You need | Do |
|---|---|
| An answer now (authorize, resolve, execute) | Call the owner's `/internal/v1/` endpoint with `InternalServiceClient`; the owner grants the permission in **its** trust bundle |
| To react when something changes | Consume the owner's durable event (`cfmeet.platform.event-envelope`) |
| To query it often | Maintain your own projection fed by the owner's events; you own the projection, not the source data |
| Shared code | Put it in `packages/<name>` (no dependency on any service) |

Never: a second database connection to another service, a read replica of another
service's database, a "temporary" view or foreign table, or copying another service's
entities / migrations into your crate.

## Machine-enforced rules

| Rule | Fails when |
|---|---|
| `CARGO_CROSS_SERVICE_DEPENDENCY` | a Cargo manifest of service S has any path (dependency of any kind, `[patch]`, workspace member, bin / lib path) into `services/T` |
| `PACKAGE_DEPENDS_ON_SERVICE` | a manifest under `packages/` has a path into `services/` |
| `CARGO_PATH_OUTSIDE_REPOSITORY` | a manifest path leaves the repository |
| `CROSS_SERVICE_SOURCE_INCLUDE` | `include!` / `include_str!` / `include_bytes!` / `#[path]` (also via `CARGO_MANIFEST_DIR`) in S or a package resolves into `services/T` |
| `CROSS_SERVICE_PATH_LITERAL` | Rust source of S (including tests) names a path inside `services/T` |
| `CROSS_SERVICE_DATABASE_URL` | code, tests or deployment configuration of S name another service's database location, e.g. `MEETING_DATABASE_URL`, `LEGACY_MEETING_DB_URL`, `TELEPHONY_DATABASE_URL` (known services = `services/*` plus identities registered in `packages/service-auth`) |
| `CROSS_SERVICE_TABLE_ACCESS` | production SQL of S names a table only another service's migrations create |
| `CROSS_SERVICE_DB_HELPER` | production code of S defines or calls a helper named for another service's data store, e.g. `load_user_from_meeting_db`, `telephony_pool` |

Comments are ignored (documentation may discuss the rules); the data rules apply to
production code with `#[cfg(test)]` items removed, except database-location names, which
are refused in tests and configuration too. Calling's
`tests/architecture_boundaries.rs` additionally pins Calling-specific rules (own
`platform_*` and call-domain tables only, no Meeting Service crate or `ActorContext`, no customer
credential machinery in internal service auth, no implicit conversions between context
types).

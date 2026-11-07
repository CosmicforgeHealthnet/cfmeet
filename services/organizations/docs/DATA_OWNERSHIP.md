# Organizations data ownership

Repository rule: [`docs/architecture/service-data-ownership.md`](../../../docs/architecture/service-data-ownership.md).
Authority rules: [ADR 0056](../../../docs/decisions/0056-first-party-organizations-and-workspace-boundaries.md).

## Owned (own database, `organizations` / `organization_*` tables)

| Table | Content |
|---|---|
| `organizations` | id (preserved on import), name, slug, lifecycle status / reason, provisioning state, creation source, creator (opaque user id), timestamps, version, profile policy |
| `organization_profiles` | descriptive profile; optional legal registration (SELF_DECLARED until verified) |
| `organization_memberships` | user (opaque id), role, status, how it joined, version; one non-removed membership per organization + user |
| `organization_membership_history` | append-only record of every membership change |
| `organization_invitations` | SHA-256 token digest, normalized email (binding only; never in events, audit or logs), role, status, expiry |
| `organization_audit_events` | append-only audit (actor kind, user, calling service, request / correlation ids) |
| `organization_outbox` | transactional outbox (event envelopes without email, token or profile text) |
| `organization_idempotency_records` | stored responses for `Idempotency-Key` replay (never a token) |
| `organization_service_nonces` | first use of each service token (replay refusal) |
| `organization_import_runs` | migration import receipts |
| `organization_platform_scopes`, `organization_provisioning` | Section 7 scope mapping / provisioning (ids and states only; empty in Section 5) |

Sections 6–7 add `organization_teams`, `organization_team_members` (+ append-only
`organization_team_member_history`), `organization_policies` (typed document, revision) and
put `organization_platform_scopes` / `organization_provisioning` to use: Platform Access ids
and verification evidence only — Platform Access stays the tenant authority and is reached
over its signed API (`platform-identity.provision-first-party`), never through its database.

## Not owned

* Accounts, passwords, sessions, Google identities — the CF Meet account authority
  (Meeting Service). Users are opaque `user_id` references.
* Meetings, messages, calls, recordings, PBX data — their owning services.
* Developer workspaces and their roles — Developer Platform.
* Technical tenants, applications, environments, credentials — Platform Access.

Other services read organization data only through this service's signed API (and, from
Sections 7–14, its events). It never reads another service's database.

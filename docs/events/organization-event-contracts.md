# Organization event contracts (proposal — Blueprint v1.0 Section 1, frozen in Section 3)

**Producer:** `services/organizations` (transactional outbox). **Consumers:** meeting-service (organization projection, ORGANIZATION_ONLY admission), communications (colleague discovery), calling (enterprise principals and admin capability), messaging (organization conversation membership), platform-access (scope suspension via the provisioning worker, never by event alone).
**Decision record:** [ADR 0056](../decisions/0056-first-party-organizations-and-workspace-boundaries.md) §6.

## 1. Envelope (schemaVersion 1)

| Field | Type | Rule |
|---|---|---|
| `eventId` | UUID | Unique; consumer dedupe key |
| `eventType` | string | One of §2; dotted, past tense |
| `schemaVersion` | integer | Additive changes keep the version; breaking changes add a new `eventType` version |
| `organizationId` | UUID | Always present (aggregate root) |
| `aggregateType` | `organization` \| `membership` \| `invitation` \| `team` \| `team_member` \| `policy` \| `scope` \| `provisioning` | |
| `aggregateId` | UUID | |
| `aggregateVersion` | integer | Monotonic per aggregate; consumers ignore versions ≤ the last applied |
| `occurredAt` | RFC 3339 UTC | Database commit time of the change |
| `actor` | `{ kind: USER\|SYSTEM\|OPERATOR, userId?, serviceId }` | Trusted, from the signed request that caused the change |
| `correlationId`, `causationId` | UUID | Trace and saga linkage |
| `payload` | object | Minimal and scoped: ids, statuses, roles, versions. No email, phone, profile text or tokens |

Transport: outbox rows are delivered at least once to each consumer's signed internal ingress (`packages/service-auth`, one audience per consumer, receiver-authoritative permission). Final transport details follow the existing outbox conventions recorded in Section 3. Consumers must store `(eventId)` for dedupe, apply by `aggregateVersion`, expose lag, dead-letter after bounded retries, and support full reconciliation from `GET /internal/v1/organizations/{id}` and the membership resolve operation.

## 2. Event types

| Event | Emitted when | Payload (beyond envelope) | Consumer obligation |
|---|---|---|---|
| `organization.created` | Organization row committed | `slug`, `lifecycleStatus`, `provisioningState` | Create projection row (inactive for scoped features until `organization.provisioned`) |
| `organization.profile_updated` | Name/slug/profile change | `changedFields[]` | Refresh display data (meeting-service projection, Hub) |
| `organization.provisioning_changed` | Saga step state change | `provisioningState`, `step`, `errorClass?` | Informational |
| `organization.provisioned` | All required scopes verified READY | `scopes[]: {capability, tenantId, applicationId, environmentId, mappingVersion}` | Calling/Messaging may accept organization-scoped operations |
| `organization.suspended` | Lifecycle → SUSPENDED | `reason class` | **Fail closed**: deny all organization-scoped operations immediately |
| `organization.reactivated` | SUSPENDED → ACTIVE | — | Re-enable after reconciling membership |
| `organization.archived` | Lifecycle → ARCHIVED | — | Deny new operations; keep records under retention |
| `membership.activated` | Invitation accepted, creator owner, or migration import | `userId`, `role`, `membershipVersion` | Grant organization context |
| `membership.role_changed` | Role or grant change | `userId`, `fromRole`, `toRole` | Re-evaluate privileges; a downgrade applies before any later upgrade |
| `membership.suspended` / `membership.removed` | Status change or leave | `userId` | **Fail closed**: revoke organization privileges, organization conversation access, enterprise admin; personal data untouched |
| `invitation.created` / `invitation.accepted` / `invitation.declined` / `invitation.revoked` / `invitation.expired` | Invitation lifecycle | `invitationId`, `role`, `inviteeUserId?` (never the email or token) | Notification owner delivers transactional email (Section 11) |
| `ownership.transferred` | Atomic transfer | `fromUserId`, `toUserId` | Audit display |
| `team.created` / `team.updated` / `team.archived` | Section 6 (aggregate `team`, version = team version) | `teamId`, `parentTeamId?`, `changedFields?`, `teamVersion` | Team-scoped resources re-check |
| `team.member_added` / `team.member_role_changed` / `team.member_removed` | Section 6 (aggregate `team_member`, id = team-membership id, version = team-membership version) | `teamId`, `userId`, `role` / `fromRole`+`toRole` / `reason` (`REMOVED`, `MEMBERSHIP_ENDED`, `TEAM_ARCHIVED`), `teamMemberVersion` | Team-scoped resources re-check; removal fails closed |
| `policy.updated` | Organization policy revision (aggregate `policy`, id = organization id, version = revision) | `policyRevision`, `changedKeys[]` | Owners re-read `GET …/policies/effective`; owner policy stays stricter-wins |
| `scope.mapping_changed` | Technical scope mapping version change (aggregate `scope`, id = mapping id, version = `mappingVersion`) | `capability`, ids, `mappingState`, `mappingVersion` | Calling/Messaging refresh scope resolution (`POST /internal/v1/organizations/scopes/resolve`) |

Section 7 note: `organization.provisioning_changed` and `organization.provisioned` use the
`provisioning` aggregate (id = workflow id, version = workflow version); READY emits only
`organization.provisioned`. Provisioning never bumps the organization version.

## 3. Ordering and safety rules

1. Within one aggregate, events are emitted in `aggregateVersion` order from the same transaction as the change.
2. A consumer that cannot establish the current membership state (lag beyond its bound, dead-lettered events, Organizations unreachable) denies privileged organization operations and keeps personal operations working.
3. Replays are harmless: every handler is idempotent on `eventId` and monotonic on `aggregateVersion`.
4. Events never carry secrets, invitation tokens, email addresses or phone numbers.

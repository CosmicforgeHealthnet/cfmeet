# ADR 0039 — Numbering: ownership and assignment

Status: Accepted (Part IV Workstream J). Extends ADR 0033–0038.

## Context

A provisioned number (ADR 0038) has no owner and carries no customer traffic. Turning it
into a DCN number means binding it to a LEGAL OWNER and to where its traffic goes,
without letting a request claim someone else's identity, letting an employee walk away
with an employer's number (or an employer take a person's), or declaring a number active
before the services that carry its traffic accepted it.

## Decisions

1. **Ownership classes and owners from trusted context.** PERSONAL / PROFESSIONAL numbers
   belong to a principal; ORGANIZATION, EMPLOYEE_DIRECT, DEPARTMENT, CAMPAIGN and
   SHARED_TEAM belong to the organization — the authenticated tenant of the Platform
   Access authority chain. A request never names an organization; a personal /
   professional owner (and the employee of a direct line) is a principal Platform Access
   resolves ACTIVE in the caller's environment (`platform-principal.resolve`, signed
   delegated scope). An unreachable directory fails closed (`PRINCIPAL_UNVERIFIED`).
   Ownership is immutable on the record (database-enforced since Workstream D).
2. **Target policy per class.** Routing targets are opaque CF Meet references; the kinds
   a class may route to are bounded (`TARGET_NOT_PERMITTED`): a personal / professional
   number routes to its owner only (or the owner's voicemail, approved forwarding,
   professional identity); an employee's direct line to one employee; organizational
   classes to their constructs (queues, ring groups, IVRs, departments, shared teams,
   applications). Every class may be unrouted. New voice kinds PROFESSIONAL_IDENTITY,
   SHARED_TEAM, VOICEMAIL, APPROVED_FORWARDING and SMS kinds SHARED_TEAM, DEPARTMENT are
   database-checked.
3. **Eligibility bound to the owner.** The decision must be the caller's, of the requested
   class, about the owner for an individual class and about this number
   (`ELIGIBILITY_SUBJECT_MISMATCH`). It is RE-EVALUATED by the F engine at creation,
   activation and class change; the re-evaluated decision is the recorded basis.
4. **Creation.** From a PROVISIONED acquisition (its reservation is linked) or a HELD
   reservation (taken over: CONVERTED `ASSIGNMENT`, provisioned afterwards; a reservation
   whose provisioning is in flight is refused). One transaction with the scoped
   idempotency key: REQUESTED → PENDING_ELIGIBILITY → ELIGIBLE → RESERVED, the routing
   profile created and pinned, the optional pool membership. Concurrent requests: one
   assignment (the open-assignment index).
5. **Activation only on proven readiness.** RESERVED → ACTIVATING → ACTIVE requires the
   number PROVISIONED, eligibility re-evaluated, the owning principal still ACTIVE and
   EVERY routed family READY (ADR 0038: configured on the current binding AND
   acknowledged by Telephony / the SMS Gateway) — `ROUTING_NOT_READY` with the failing
   family otherwise. Nothing is fabricated; the number becomes ACTIVE in the same
   transaction.
6. **Routing changes are revisions.** A change supersedes the profile revision, pins the
   next one and advances the number's routing revision — the value routing grants are
   bound to (Workstream K), so a change revokes outstanding grants. Employee offboarding
   (EMPLOYEE_DIRECT only) unroutes the line or moves it to a replacement (never the
   departing employee); the owner stays the organization.
7. **Transfers keep the legal owner.** Same class: a routing revision. Another class of
   the same owner: the ACTIVE assignment closes (`TRANSFERRED_INTERNALLY`, the only
   ACTIVE → CLOSED edge) and a successor opens, admitted on a re-evaluated decision of the
   new class, takes over the hold and activates — one transaction; `predecessor_` /
   `successor_assignment_id` are database-linked (deferred FK; the successor must match
   the predecessor's owner, scope and number). A different legal owner is
   `CROSS_OWNER_TRANSFER_UNSUPPORTED` before anything changes; release, quarantine and
   re-acquisition are Workstream Q.
8. **Organizational pools.** `number_pools` / `number_pool_members` group an
   organization's numbers with opaque usage / subscription policy references; only
   organization-owned assignments of the pool's scope join (trigger), an assignment is in
   at most one pool, memberships are ended (never deleted) and follow a transfer to the
   successor. Members read ASSIGNED while routed, AVAILABLE while unrouted. Queue,
   ring-group and business-hours behaviour stays in Calling.
9. **APIs.** `numbers.assign` creates, activates and transfers; `numbers.manage` changes
   routing, offboards and manages pools; `numbers.read` reads; the operator view of a
   number's history across owners needs `numbers.admin` on a first-party application and
   `X-CF-Operator`. Every object is checked against the trusted scope (other scopes are
   NOT_FOUND).

## Consequences

Migration `m20261022_000008_ownership_assignment` (DOWN refuses once transfers, pools or
J target kinds exist). Suspension, release, quarantine and cross-owner moves remain
Workstreams P–Q; real Telephony / SMS Gateway acknowledgements remain Workstreams L–O.

# ADR 0013: Routing Engine, route classes and immutable Route Plans

- **Status:** Accepted
- **Date:** 2026-10-08
- **Scope:** CF Meet Part E §37–§38. Implemented in `services/calling/src/routing/`
  (`engine.rs`, `policy.rs`, `availability.rs`, `store.rs`), Calling migration 000017
  (`call_routes` plan columns, `call_routing_policies`) and the operator CLI
  `platform-admin routing-policy set | show`.

## Context

Up to Part D every call was application RTC. Part E adds paid public-network legs. Route
choice decides cost, privacy exposure and fraud surface, so it must be deterministic,
explainable, fail closed on missing facts and must never be influenced by provider
names or by a client choosing "any phone route".

## Decision

### Ownership

Calling owns the route decision (it owns the logical Call, its canonical legs and the
authorization proof). Telephony only reports **route availability** in a neutral form
(`routes/availability`: per route class and destination country — available or not,
capability and health classes; never provider names, carriers or trunks). Privacy Relay
supplies contact facts (verification class, country) through the protected-call
authorization, never numbers.

### Route classes

`APP_RTC`, `APP_TO_PSTN`, `PSTN_TO_APP`, `PSTN_RELAY`, `CALLBACK_RELAY`,
`ENTERPRISE_INTERNAL`. Each has a network class (application / public), a paid flag and
an executable flag (`ENTERPRISE_INTERNAL` is reserved: never executable in Part E).
Public classes are privacy routes: they require a protected-call context.

### Policy

`RoutingPolicy` (one per environment, `call_routing_policies`, revisioned, typed with
`deny_unknown_fields`): allowed classes, app-first, whether a customer may select a
public route, public-calling / callback-relay switches, verified-contact requirement,
destination allow / deny lists with a default, failover attempt cap (≤ 3), maximum
estimated leg cost, and duplicate-risk tolerance. The default policy is restrictive
(application RTC only). A missing or unreadable policy row loads the default — fail
closed, never open. Policy versions are `ROUTING_POLICY_V1.default` /
`ROUTING_POLICY_V1.r{n}`.

### Engine

`select(policy, inputs, now)` is a pure function over typed inputs where every fact is
`Evidence::Known(..)` or `Evidence::Unknown`. Unknown evidence never satisfies a
requirement (unknown verification, country, health or availability → denial with a
stable `RouteDenial` reason). App-first: when the callee has a reachable application
endpoint, APP_RTC wins and no paid leg is planned. A public plan lists candidate routes
by class / capability / health / estimated cost, with no provider identity.

### Route Plan

The selected plan is persisted on `call_routes` (`routing_class`, `route_plan`,
`plan_fingerprint`, `selected_route_ref`, `max_attempts`, `plan_expires_at`,
`route_version`). The fingerprint (SHA-256 of the canonical plan with timestamps zeroed)
makes route recording idempotent; the class is validated before an idempotent replay so a
retry cannot change the class. Plan columns are immutable once written (trigger); one
active plan per call (unique index).

### Revision and failover

`revise(plan, outcome)` returns `Keep`, `Failover(next)` or `Stop(reason)`:

* only before CONNECTED — a connected call needs a transfer, never a re-route
  (`ConnectedRequiresTransfer`);
* **an UNKNOWN outcome never fails over** (`AwaitReconciliation`): the first leg may be
  live, and a second leg would double-dial and double-bill;
* attempts are capped by the plan (`Exhausted`); each revision bumps `route_version`.

## Consequences

* Application calls run through the same engine and record an `APP_RTC` plan; a denial
  maps onto the existing `FAILED / ROUTING` path.
* Adding a route class or a fact is a typed change with tests, not configuration.
* Telephony can be absent or down: availability is then `Unknown`, and public classes are
  denied while app RTC continues.

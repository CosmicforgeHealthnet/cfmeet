# Architecture decision records

Short, numbered records of significant architectural decisions.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-platform-monorepo-layout.md) | Platform monorepo layout and modular-monolith core API | Accepted (historical naming; service naming superseded by ADR 0002) |
| [0002](0002-meeting-service-domain-name.md) | Rename core-api to meeting-service before Phase 2 | Accepted |
| [0003](0003-calling-platform-trust-model.md) | Calling platform consumer, credential and principal trust model | Accepted |
| [0004](0004-internal-service-authentication.md) | Internal service authentication (Ed25519 service tokens, receiver-authoritative permissions) | Accepted |
| [0005](0005-contract-authority-and-runtime-boundaries.md) | Contract authority, data ownership and runtime dependency direction | Accepted |
| [0006](0006-canonical-call-aggregate-and-state-machine.md) | Canonical Call aggregate and state machine (Part C §15–§19) | Accepted |
| [0007](0007-calling-media-provider-boundary.md) | Calling media provider boundary; LiveKit as an adapter; join-then-admit; stale re-entry (Part C §22–§24) | Accepted |
| [0008](0008-calling-endpoints-ringing-and-answer-arbitration.md) | Endpoints, signaling credentials, ringing and per-participant answer arbitration (Part C §20) | Accepted |
| [0009](0009-calling-call-control-idempotency-and-transfer.md) | Call control, idempotency, hold semantics and application transfer (Part C §21, §25) | Accepted |
| [0010](0010-communication-context-and-privacy-relay-session-architecture.md) | Communication Context and Privacy Relay Session architecture; protected calls and active-call enforcement (Part D §26–§30) | Accepted |
| [0011](0011-contact-vault-cryptography-and-resolution.md) | Contact Vault cryptography, scoped keyed equality, verification trust model, Contact Resolution Grants and Telephony-only resolution (Part D §31–§33) | Accepted |
| [0012](0012-temporary-relay-number-assignment-and-cooldown.md) | Temporary relay-number inventory, deterministic assignment, cooldown and inbound mapping (Part D §34–§35) | Accepted |
| [0013](0013-routing-engine-and-route-plans.md) | Routing Engine, route classes, fail-closed policy and immutable Route Plans (Part E §37–§38) | Accepted |
| [0014](0014-telephony-service-core.md) | Telephony service core: legs, durable commands, transmit fence, provider-neutral events (Part E §39–§40) | Accepted |
| [0015](0015-sip-media-edge-stack-and-network-topology.md) | SIP / media edge stack (Asterisk 20 + ARI) and network topology (Part E §41) | Accepted |
| [0016](0016-first-telecom-interconnect.md) | Provider-neutral interconnect contract; first interconnect = SIP trunk on the CF Meet edge; real-carrier certification pending (Part E §42–§43) | Accepted (certification pending) |
| [0017](0017-provider-failover.md) | Provider failover: definite vs unknown, Calling decides whether / Telephony decides which, duplicate-call prevention at both boundaries, circuit breaker, late-success race (Part E §48, §51) | Accepted (real-carrier failover pending) |
| [0018](0018-enterprise-calling-architecture.md) | Enterprise calling architecture: configuration and continuity inside Calling, one Call, tenant as the organization boundary, typed policy, explicit scopes, versioning and audit (Part F §52–§62) | Accepted |
| [0019](0019-ring-group-and-queue-routing.md) | Ring-group and queue routing on one Call: answer groups with a single winner, ordered stepping by the expiry worker, FIFO queues with advisory-locked dispatch, one pending offer per entry / agent, overflow and abandonment (Part F §55–§56) | Accepted |
| [0020](0020-ivr-and-enterprise-sip-devices.md) | Declarative versioned IVR on one Call (typed graph, pinned versions, bounded sessions, built-in prompts, keys never stored) and enterprise SIP devices on the CF Meet edge (ENTERPRISE_INTERNAL legs, ANSWER / PROMPT commands) (Part F §57) | Accepted |

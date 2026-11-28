//! CF Meet Notification Service producer client (`packages/notification-client`,
//! roadmap N1, Document 4 §12).
//!
//! Keeps every producer integration identical:
//!
//! * [`catalogue`] — the closed catalogue of the 47 inbound event types and the control
//!   event (mirrors `x-cfmeet-catalogue` in `cfmeet.notifications.event`);
//! * [`envelope`] — the `cfmeet.notifications.event` v1 builder and validator (forbidden
//!   data keys, sealed allowance, legacy idempotency-key formats, CRITICAL rules, size);
//! * [`seal`] — HPKE (RFC 9180) sealing of codes, tokens, addresses and account-notice
//!   facts to the service key (`open` feature: the service renderer only), and of partner
//!   push provider credentials ([`seal::seal_push_credential`], ADR 0063 N-P1);
//! * [`routing`] — the routing fence protocol v1 decision helper (§9.3), the guard-trigger
//!   rule (§9.4) and the CRITICAL reroute rules (§9.8).
//!
//! Nothing here talks to the network or a database: producers persist envelopes in their
//! own transactional outbox, and their relays post them to I1.

pub mod catalogue;
pub mod envelope;
pub mod routing;
pub mod seal;

pub use catalogue::{
    parse_platform_principal, platform_principal_ref, spec, EventSpec, Lane, LegacyKey, Priority,
    Producer, RecipientKind, CATALOGUE, CONTROL_EVENT, EMAIL_CHANGED_SIDES, PARTNER_EVENT_TYPES,
    STEP_UP_PURPOSES,
};
pub use envelope::{validate, Envelope, EnvelopeBuilder, EnvelopeError};
pub use routing::{decide, Decision, Route, RouteRef, RoutingMode, RoutingState, ROUTING_PROTOCOL};
pub use seal::{
    push_credential_fingerprint, seal_push_credential, Sealed, SealedPlaintext, SealingKey,
};

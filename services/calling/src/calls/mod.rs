//! Calling core (CF Meet Part III, Part C §15–§19).
//!
//! `services/calling` is the canonical authority for the **logical Call**. A Call is the
//! same CF Meet object whatever later executes it — application RTC, SIP, the public
//! network, a relay, enterprise infrastructure, several network legs or several provider
//! attempts. Its identity is a CF Meet-generated UUID; provider room / dialog / carrier /
//! request identifiers are never Call identities and never appear in this domain.
//!
//! ```text
//! Call (aggregate root: state, version, lifecycle timestamps)
//! ├── CallParticipant      logical identity of one trusted principal (one per principal)
//! │   └── CallSession      one device / network participation (server media identity)
//! │       └── interval     one connected period (reconnect = next interval)
//! ├── CallRoute            provider-independent routing decision
//! ├── CallLeg              canonical Calling-side leg (execution detail lives elsewhere)
//! ├── CallAttempt          numbered attempts, explicit unknown outcome
//! ├── CallAuthorization    persisted APPROVED / DENIED decision evidence
//! ├── CallEvent            immutable, gap-free per-call evidence (+ transactional outbox)
//! ├── CallQualityRecord    bounded provider-independent metrics
//! └── CallUsageRecord      idempotent consumption facts (not billing)
//! ```
//!
//! * [`state`] — the 17 canonical states and the transition graph (§17).
//! * [`transitions`] — actions, evidence and the pure transition plan (§18). Clients
//!   request actions; the server derives target states.
//! * [`service::CallService`] — the only writer: one transaction per operation with the
//!   mutation, its immutable events and their outbox rows.
//! * [`http`] — read-only public routes (`GET /v1/calls`, `GET /v1/calls/{call_id}`).
//!
//! Deferred to later sections: multi-device ringing and answer arbitration (§20), route /
//! attempt orchestration (§21), application voice / video calling and the RTC provider
//! contract (§22–§24), provider execution of cancel / end / hold / transfer (§25), Privacy
//! Relay, Contact Vault, Telephony, public-network execution, signaling, push ringing,
//! webhooks and event delivery.

pub mod aggregate;
pub mod attempts;
pub mod attributes;
pub mod authorizations;
pub mod command;
pub mod duration;
pub mod errors;
pub mod events;
pub mod http;
pub mod ids;
pub mod kinds;
pub mod legs;
pub mod participants;
pub mod quality;
pub mod read_model;
pub mod routes;
pub mod service;
pub mod sessions;
pub mod state;
pub mod transitions;
pub mod usage;

pub use aggregate::Call;
pub use command::{CallActor, CallCommandContext, CallScope};
pub use errors::{CallError, CallResult};
pub use service::{CallService, CallServiceConfig};
pub use state::CallState;
pub use transitions::{CallAction, ConnectedEvidence, EndedEvidence};

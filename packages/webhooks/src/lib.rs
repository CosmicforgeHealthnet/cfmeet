//! CF Meet customer webhook primitives (Part G §68), provider-neutral and service-neutral.
//!
//! * [`signing`] — the signature scheme customers verify: HMAC-SHA256 over a canonical
//!   input binding the version, the timestamp, the delivery id and the exact body bytes;
//!   constant-time verification with a timestamp tolerance (replay protection).
//! * [`endpoint`] — the SSRF policy: URL validation (scheme, credentials, ports, names,
//!   address literals) and a resolver that filters prohibited addresses AT CONNECTION TIME
//!   (DNS rebinding safe), plus the delivery client (no redirects, no proxy).
//! * [`retry`] — outcome classification and bounded exponential backoff with jitter.
//! * [`secret_box`] — AEAD (AES-256-GCM, `ring`) protection of signing secrets at rest.
//!
//! The owning service keeps subscriptions, events and delivery records in its own
//! database; nothing here knows any business vocabulary.

pub mod endpoint;
pub mod retry;
pub mod secret_box;
pub mod signing;

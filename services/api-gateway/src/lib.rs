//! CF Meet Platform API Gateway — the single customer ingress of the CF Meet Platform (Part VI).
//!
//! * [`routing`]: the default-deny route table (REST and WebSocket routes; internal,
//!   provider-callback, operator and Telephony surfaces are never routable).
//! * [`identity`]: customer credentials are verified by Platform Access only; tenant,
//!   application and environment come from its authority chain.
//! * [`headers`]: client-supplied internal authority headers are never forwarded.
//! * [`http`]: probes and the ingress pipeline, which forwards with a short-lived signed
//!   delegated context (`packages/service-auth`). Domain services keep final resource
//!   authorization.
//! * [`user_tokens`]: the stateless pre-check of CF Meet user tokens (ADR 0062); the
//!   owning service verifies them live.
//! * [`origins`]: partner-website origins per environment (ADR 0062), from Platform
//!   Access's change feed, for CORS.
//! * [`config`]: typed configuration, feature flags (default off) and the Redis settings
//!   of future shared quotas.
//!
//! The gateway owns NO database and no communications data, stores no E2EE material and
//! has no product-specific route.

pub mod catalogue;
pub mod config;
pub mod context;
pub mod docs;
pub mod headers;
pub mod http;
pub mod identity;
pub mod origins;
pub mod quota;
pub mod routing;
pub mod state;
pub mod user_tokens;
pub mod ws;

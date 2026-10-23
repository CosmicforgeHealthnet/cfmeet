//! CF Meet public API Gateway — Part VI customer ingress (foundation).
//!
//! * [`routing`]: the default-deny route table (REST and WebSocket routes; internal,
//!   provider-callback, operator and Telephony surfaces are never routable).
//! * [`identity`]: customer credentials are verified by Platform Access only; tenant,
//!   application and environment come from its authority chain.
//! * [`headers`]: client-supplied internal authority headers are never forwarded.
//! * [`http`]: probes and the ingress pipeline, which forwards with a short-lived signed
//!   delegated context (`packages/service-auth`). Domain services keep final resource
//!   authorization.
//! * [`config`]: typed configuration, feature flags (default off) and the Redis settings
//!   of future shared quotas.
//!
//! The gateway owns NO database and no communications data, stores no E2EE material and
//! has no product-specific route.

pub mod config;
pub mod context;
pub mod headers;
pub mod http;
pub mod identity;
pub mod routing;
pub mod state;

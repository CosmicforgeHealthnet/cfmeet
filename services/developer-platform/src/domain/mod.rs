//! Request / correlation / causation identity of the work being done.

pub mod context;

pub use context::{current_context, scoped, CausationId, CorrelationId, RequestContext, RequestId};

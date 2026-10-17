//! Cross-cutting domain primitives: time and request / correlation context.

pub mod clock;
pub mod context;

pub use clock::{Clock, ManualClock, SharedClock, SystemClock};
pub use context::{current_context, scoped, CausationId, RequestContext};

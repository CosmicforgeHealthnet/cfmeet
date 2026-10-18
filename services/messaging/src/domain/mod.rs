//! Cross-cutting domain primitives: time, request / correlation context, security
//! profiles and the encrypted / managed content boundary.

pub mod clock;
pub mod context;
pub mod profile;

pub use clock::{Clock, ManualClock, SharedClock, SystemClock};
pub use context::{current_context, scoped, CausationId, RequestContext};
pub use profile::SecurityProfile;

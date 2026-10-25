//! Request, correlation and causation identifiers.
//!
//! The request id and correlation id are server-controlled (`cf-service-auth` formats):
//! a request entering CF Meet here gets new ones; an authenticated internal request
//! continues the signed correlation id of its upstream hop. The **causation** id is the
//! id of whatever directly caused a change — the request, or the event / job that
//! triggered work — and is recorded on every outbox event and job, so a chain of effects
//! can be followed across workers and services.

use std::future::Future;

pub use cf_service_auth::{CorrelationId, RequestId};

/// What directly caused a change: a request id, an outbox event id or a job id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausationId(String);

impl CausationId {
    /// Accepts `[A-Za-z0-9_-]{8,64}` only (the persisted format).
    pub fn parse(value: &str) -> Option<Self> {
        let ok = (8..=64).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
        ok.then(|| Self(value.to_string()))
    }

    pub fn of_request(id: &RequestId) -> Self {
        Self(id.as_str().to_string())
    }

    pub fn of_record(prefix: &str, id: uuid::Uuid) -> Self {
        Self(format!("{prefix}_{}", id.simple()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Server-controlled identity of the work being done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    pub request_id: RequestId,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<CausationId>,
}

impl RequestContext {
    /// Work that starts a new operation in this service.
    pub fn generate() -> Self {
        Self {
            request_id: RequestId::generate(),
            correlation_id: CorrelationId::generate(),
            causation_id: None,
        }
    }

    /// Follow-up work of `correlation` caused by `causation` (a worker continuing an
    /// operation).
    pub fn continuing(correlation: CorrelationId, causation: CausationId) -> Self {
        Self {
            request_id: RequestId::generate(),
            correlation_id: correlation,
            causation_id: Some(causation),
        }
    }

    /// The causation id recorded on effects of this work: the explicit cause, else the
    /// request itself.
    pub fn cause(&self) -> CausationId {
        self.causation_id
            .clone()
            .unwrap_or_else(|| CausationId::of_request(&self.request_id))
    }
}

tokio::task_local! {
    static CURRENT: RequestContext;
}

/// The context of the request being served, if any.
pub fn current_context() -> Option<RequestContext> {
    CURRENT.try_with(|c| c.clone()).ok()
}

/// Runs `fut` with `context` as the current request context.
pub async fn scoped<F: Future>(context: RequestContext, fut: F) -> F::Output {
    CURRENT.scope(context, fut).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causation_ids_are_bounded_and_default_to_the_request() {
        assert!(CausationId::parse("short").is_none());
        assert!(CausationId::parse("has space here").is_none());
        assert!(CausationId::parse(&"x".repeat(65)).is_none());
        let ctx = RequestContext::generate();
        assert_eq!(ctx.cause().as_str(), ctx.request_id.as_str());
        let job = uuid::Uuid::new_v4();
        let follow = RequestContext::continuing(
            ctx.correlation_id.clone(),
            CausationId::of_record("job", job),
        );
        assert_eq!(follow.correlation_id, ctx.correlation_id);
        assert!(follow.cause().as_str().starts_with("job_"));
        assert!(CausationId::parse(follow.cause().as_str()).is_some());
    }
}

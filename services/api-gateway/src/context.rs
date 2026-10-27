//! Server-controlled request / correlation ids of the request being served.
//!
//! A customer request entering CF Meet here always gets NEW ids (client-supplied ids are
//! never adopted); the correlation id is signed into every upstream hop's service token
//! and returned to the client as `X-Correlation-Id`, the request id as `X-Request-Id`.

use std::future::Future;

pub use cf_service_auth::{CorrelationId, RequestId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    pub request_id: RequestId,
    pub correlation_id: CorrelationId,
}

impl RequestContext {
    pub fn generate() -> Self {
        Self {
            request_id: RequestId::generate(),
            correlation_id: CorrelationId::generate(),
        }
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

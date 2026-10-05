//! Legacy write retirement guard (`FEATURE_LEGACY_MEETING_WRITE_DISABLED`, Parts J–L §39).
//!
//! Mounted on the legacy `/api/v1` meeting, participant and third-party meeting routers.
//! When the flag is on, every state-changing method (anything but GET / HEAD / OPTIONS)
//! answers `410 LEGACY_WRITE_DISABLED` before authentication or any database access, so
//! a retired legacy handler can never overwrite v2 state. Legacy reads keep working for
//! the deprecation period and `/api/v2` is unaffected. Off by default.

use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{
    error::{ApiError, ErrorCode},
    services::ops_metrics::{inc, OPS_METRICS as OM},
    state::AppState,
};

pub fn is_write(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

pub async fn legacy_write_guard(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if state.meeting.core.flags.legacy_meeting_write_disabled && is_write(request.method()) {
        inc(&OM.legacy_writes_rejected);
        tracing::info!(
            method = %request.method(),
            path = %request.uri().path(),
            "legacy meeting write rejected (FEATURE_LEGACY_MEETING_WRITE_DISABLED)"
        );
        return ApiError::domain(
            ErrorCode::LegacyWriteDisabled,
            "Legacy meeting writes are retired; use the /api/v2 meeting API",
        )
        .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_safe_methods_are_reads() {
        assert!(!is_write(&Method::GET));
        assert!(!is_write(&Method::HEAD));
        assert!(!is_write(&Method::OPTIONS));
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(is_write(&m));
        }
    }
}

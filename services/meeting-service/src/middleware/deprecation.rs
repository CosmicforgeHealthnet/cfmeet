//! Deprecation headers for retiring legacy surfaces (Blueprint v1.0 Section 16).
//!
//! RFC 9745 `Deprecation` (structured date), RFC 8594 `Sunset` and a
//! `Link rel="deprecation"` pointing at the Swagger tag that carries the migration text.
//! Mounted outside authentication, so error answers (401 / 400 / 404 / 410) carry them
//! too. The API Gateway already relays `deprecation`, `sunset` and `link`.
//!
//! The `Sunset` date is the earliest planned removal; removal itself still needs the
//! D-9 per-endpoint record (measured zero consumers) and the owner's approval
//! (`docs/operations/retirement-tooling-section-16.md`).

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};

/// RFC 9745 `Deprecation`: 2026-11-07T00:00:00Z (the Section 16 deprecation record).
pub const DEPRECATED_SINCE: &str = "@1794009600";

/// RFC 8594 `Sunset`: earliest planned removal of the deprecated legacy surfaces.
pub const SUNSET: &str = "Sat, 01 May 2027 00:00:00 GMT";

/// `Link rel="deprecation"` for the legacy webhook routes.
pub const WEBHOOKS_LINK: &str = "</swagger-ui/#/Webhooks>; rel=\"deprecation\"; type=\"text/html\"";

/// `Link rel="deprecation"` for the legacy v1 meeting create routes.
pub const MEETING_CREATE_LINK: &str =
    "</swagger-ui/#/Meetings>; rel=\"deprecation\"; type=\"text/html\"";

fn apply(response: &mut Response, link: &'static str) {
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("deprecation"),
        HeaderValue::from_static(DEPRECATED_SINCE),
    );
    headers.insert(
        HeaderName::from_static("sunset"),
        HeaderValue::from_static(SUNSET),
    );
    headers.append(
        HeaderName::from_static("link"),
        HeaderValue::from_static(link),
    );
}

/// `/api/v1/webhooks*` (PLT-003: registered, never delivered).
pub async fn legacy_webhooks(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    apply(&mut response, WEBHOOKS_LINK);
    response
}

/// `POST /api/v1/meetings`, `POST /api/v1/meetings/instant`, `POST /api/v1/api/meetings`
/// (MTG-017 / DATA-008: legacy v1 creation is deprecated in favour of `/api/v2`).
pub async fn legacy_meeting_create(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    apply(&mut response, MEETING_CREATE_LINK);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::get, Router};
    use tower::ServiceExt;

    #[tokio::test]
    async fn every_answer_carries_the_deprecation_headers() {
        let app = Router::new()
            .route(
                "/",
                get(|| async { (axum::http::StatusCode::UNAUTHORIZED, "no") }),
            )
            .layer(axum::middleware::from_fn(legacy_webhooks));
        let res = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), 401);
        assert_eq!(res.headers()["deprecation"], DEPRECATED_SINCE);
        assert_eq!(res.headers()["sunset"], SUNSET);
        assert!(res.headers()["link"]
            .to_str()
            .unwrap()
            .contains("rel=\"deprecation\""));
    }

    #[test]
    fn header_values_are_well_formed() {
        // RFC 9745: "@" + integer seconds; RFC 8594: IMF-fixdate.
        assert!(DEPRECATED_SINCE[1..].parse::<i64>().is_ok());
        assert!(chrono::DateTime::parse_from_rfc2822(SUNSET).is_ok());
        let since =
            chrono::DateTime::from_timestamp(DEPRECATED_SINCE[1..].parse().unwrap(), 0).unwrap();
        let sunset = chrono::DateTime::parse_from_rfc2822(SUNSET).unwrap();
        assert!(sunset > since, "sunset after deprecation");
    }
}

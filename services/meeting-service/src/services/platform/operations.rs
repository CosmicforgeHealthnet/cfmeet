//! The `/api/v2` operations a machine caller (platform credential, forwarded by the API
//! Gateway) may perform, and the scope each one requires.
//!
//! This table is the single source of truth for:
//!
//! * the receiver middleware ([`crate::middleware::platform_auth`]): any other route is
//!   refused with the normalized 401, the scope is enforced (defence in depth over the
//!   gateway's pre-check), and the target meeting must belong to the caller's
//!   application environment (otherwise 404, indistinguishable from an unknown id);
//! * the OpenAPI contract ([`crate::swagger`]): these operations — and only these — carry
//!   the `platformCredential` security requirement and `x-cfmeet-required-scope`, from
//!   which the API Gateway's route manifest is generated.
//!
//! Chosen for server-side integrations: meeting spaces (create, list, read, update,
//! archive), instant meetings, conference scheduling and lifecycle (start / end /
//! cancel), participants and the waiting room (list, admit, deny, remove), invitations
//! (create, list, revoke) and attendance. Joining, media, chat, collaboration,
//! recordings / artefacts, transcripts and organization directory operations stay
//! human / participant only.

use axum::http::Method;

use super::scopes;

/// What the route's `:id` path parameter names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// No meeting in the path (create / list).
    None,
    /// A meeting space (UUID; also a meeting code on the read route).
    Space,
    /// A conference.
    Conference,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformOperation {
    pub method: &'static str,
    /// Axum route template (matched path).
    pub route: &'static str,
    /// OpenAPI path template.
    pub path: &'static str,
    pub scope: &'static str,
    pub target: Target,
}

macro_rules! ops {
    ($(($m:literal, $route:literal, $path:literal, $scope:expr, $target:ident)),* $(,)?) => {
        pub const OPERATIONS: &[PlatformOperation] = &[
            $(PlatformOperation { method: $m, route: $route, path: $path, scope: $scope, target: Target::$target }),*
        ];
    };
}

ops! {
    ("GET", "/api/v2/meeting-spaces", "/api/v2/meeting-spaces", scopes::MEETINGS_READ, None),
    ("POST", "/api/v2/meeting-spaces", "/api/v2/meeting-spaces", scopes::MEETINGS_WRITE, None),
    ("GET", "/api/v2/meeting-spaces/:id", "/api/v2/meeting-spaces/{id}", scopes::MEETINGS_READ, Space),
    ("PATCH", "/api/v2/meeting-spaces/:id", "/api/v2/meeting-spaces/{id}", scopes::MEETINGS_WRITE, Space),
    ("POST", "/api/v2/meeting-spaces/:id/archive", "/api/v2/meeting-spaces/{id}/archive", scopes::MEETINGS_WRITE, Space),
    ("POST", "/api/v2/instant-meetings", "/api/v2/instant-meetings", scopes::MEETINGS_WRITE, None),
    ("GET", "/api/v2/meeting-spaces/:id/conferences", "/api/v2/meeting-spaces/{id}/conferences", scopes::MEETINGS_READ, Space),
    ("POST", "/api/v2/meeting-spaces/:id/conferences", "/api/v2/meeting-spaces/{id}/conferences", scopes::MEETINGS_WRITE, Space),
    ("GET", "/api/v2/meeting-spaces/:id/invitations", "/api/v2/meeting-spaces/{id}/invitations", scopes::MEETINGS_READ, Space),
    ("POST", "/api/v2/meeting-spaces/:id/invitations", "/api/v2/meeting-spaces/{id}/invitations", scopes::MEETINGS_WRITE, Space),
    ("POST", "/api/v2/meeting-spaces/:id/invitations/:invitation_id/revoke", "/api/v2/meeting-spaces/{id}/invitations/{invitationId}/revoke", scopes::MEETINGS_WRITE, Space),
    ("GET", "/api/v2/conferences/:id", "/api/v2/conferences/{id}", scopes::MEETINGS_READ, Conference),
    ("POST", "/api/v2/conferences/:id/start", "/api/v2/conferences/{id}/start", scopes::MEETINGS_WRITE, Conference),
    ("POST", "/api/v2/conferences/:id/end", "/api/v2/conferences/{id}/end", scopes::MEETINGS_WRITE, Conference),
    ("POST", "/api/v2/conferences/:id/cancel", "/api/v2/conferences/{id}/cancel", scopes::MEETINGS_WRITE, Conference),
    ("GET", "/api/v2/conferences/:id/participants", "/api/v2/conferences/{id}/participants", scopes::MEETINGS_READ, Conference),
    ("GET", "/api/v2/conferences/:id/waiting-room", "/api/v2/conferences/{id}/waiting-room", scopes::MEETINGS_READ, Conference),
    ("POST", "/api/v2/conferences/:id/waiting-room/admit", "/api/v2/conferences/{id}/waiting-room/admit", scopes::MEETINGS_WRITE, Conference),
    ("POST", "/api/v2/conferences/:id/waiting-room/deny", "/api/v2/conferences/{id}/waiting-room/deny", scopes::MEETINGS_WRITE, Conference),
    ("POST", "/api/v2/conferences/:id/participants/:participant_id/remove", "/api/v2/conferences/{id}/participants/{participantId}/remove", scopes::MEETINGS_WRITE, Conference),
    ("GET", "/api/v2/conferences/:id/attendance", "/api/v2/conferences/{id}/attendance", scopes::MEETINGS_READ, Conference),
}

/// The machine-callable operation of `(method, matched route)`, if any.
pub fn lookup(method: &Method, route: &str) -> Option<&'static PlatformOperation> {
    OPERATIONS
        .iter()
        .find(|o| o.method == method.as_str() && o.route == route)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_are_unique_and_route_templates_match_the_contract_paths() {
        for (i, o) in OPERATIONS.iter().enumerate() {
            assert!(
                OPERATIONS[i + 1..]
                    .iter()
                    .all(|p| (p.method, p.route) != (o.method, o.route)),
                "{} {}",
                o.method,
                o.route
            );
            let route: Vec<&str> = o.route.split('/').collect();
            let path: Vec<&str> = o.path.split('/').collect();
            assert_eq!(route.len(), path.len(), "{}", o.route);
            for (r, p) in route.iter().zip(&path) {
                assert_eq!(r.starts_with(':'), p.starts_with('{'), "{}", o.route);
                if !r.starts_with(':') {
                    assert_eq!(r, p);
                }
            }
            assert!(o.scope == scopes::MEETINGS_READ || o.scope == scopes::MEETINGS_WRITE);
            // Reads need meetings.read, mutations meetings.write (no scope implies another).
            assert_eq!(
                o.method == "GET",
                o.scope == scopes::MEETINGS_READ,
                "{}",
                o.route
            );
        }
    }

    #[test]
    fn human_only_operations_are_never_machine_callable() {
        for (m, r) in [
            (Method::POST, "/api/v2/conferences/:id/join"),
            (Method::POST, "/api/v2/conferences/:id/media-credential"),
            (Method::POST, "/api/v2/conferences/:id/messages"),
            (Method::GET, "/api/v2/conferences/:id/recordings"),
            (Method::GET, "/api/v2/conferences/:id/artifacts"),
            (Method::POST, "/api/v2/meeting-spaces/:id/owner-transfer"),
            (Method::POST, "/api/v2/meeting-spaces/:id/members"),
            (Method::GET, "/api/v2/me/organizations"),
            (Method::GET, "/api/v2/resolve/:identifier"),
            (Method::POST, "/api/v2/invitations/:token/resolve"),
        ] {
            assert!(lookup(&m, r).is_none(), "{m} {r}");
        }
        assert!(lookup(&Method::DELETE, "/api/v2/meeting-spaces/:id").is_none());
        assert_eq!(
            lookup(&Method::POST, "/api/v2/instant-meetings").map(|o| o.scope),
            Some(scopes::MEETINGS_WRITE)
        );
    }
}

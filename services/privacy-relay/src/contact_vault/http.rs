//! Contact Vault customer routes (§31): the ONLY routes of CF Meet that accept a raw
//! telephone number (contract `cfmeet.privacy-relay.http-api`, paths marked
//! `x-cfmeet-served-by: cfmeet.privacy-relay`).
//!
//! Authentication is a Contact Vault access token in `X-CF-Contact-Authorization`
//! (minted by Calling after it authenticated the customer's platform credential and its
//! `privacy.contacts.*` scopes). A customer `Authorization` header, a service token or a
//! token of another environment / audience is refused with the same
//! `CONTACT_DELEGATION_INVALID`. The token's delegated platform scope is the only scope;
//! its contact scopes gate each route.
//!
//! Bodies are tiny (4 KiB), parsed into redacted secret types and never logged; JSON
//! errors become a fixed message that never quotes the input.

use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, FromRequestParts, Path, Request, State},
    http::{header, request::Parts, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use cf_service_auth::{ContactAccess, ContactScope, CorrelationId, RequestId};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

use super::{
    service::{CreateContactRequest, DisableContactRequest, UpdateContactRequest},
    verification::VerificationAssertion,
    ContactActor,
};
use crate::{
    error::{ErrorCode, RelayError},
    middleware::current_request_id,
    state::AppState,
};

const MAX_BODY_BYTES: usize = 4 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/privacy-relay/contacts", post(create_contact))
        .route(
            "/v1/privacy-relay/contacts/:contact_reference",
            get(get_contact).patch(update_contact),
        )
        .route(
            "/v1/privacy-relay/contacts/:contact_reference/disable",
            post(disable_contact),
        )
        .route(
            "/v1/privacy-relay/contacts/:contact_reference/verification",
            post(assert_verification),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

/// The verified delegation of the current contact request.
#[derive(Debug, Clone)]
pub struct ContactCaller {
    access: ContactAccess,
    correlation_id: CorrelationId,
}

impl ContactCaller {
    fn actor(&self, required: ContactScope) -> Result<ContactActor, RelayError> {
        if !self.access.has_scope(required) {
            tracing::warn!(scope = required.as_str(), "contact scope denied");
            return Err(RelayError::Refused(ErrorCode::ContactScopeDenied));
        }
        Ok(ContactActor::delegated_client(
            &self.access,
            current_request_id().unwrap_or_else(RequestId::generate),
            self.correlation_id.clone(),
        ))
    }
}

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for ContactCaller {
    type Rejection = RelayError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<ContactCaller>()
            .cloned()
            .ok_or(RelayError::Refused(ErrorCode::ContactDelegationInvalid))
    }
}

/// Route layer of every contact route: a valid Contact Vault access token or nothing.
pub async fn require_contact_delegation(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(verifier) = state.contact_delegation.as_ref() else {
        // No trust configured: nothing can be authenticated; fail closed.
        return RelayError::Refused(ErrorCode::ContactVaultDisabled).into_response();
    };
    match verifier.verify_headers(request.headers()) {
        Ok(access) => {
            request.extensions_mut().insert(ContactCaller {
                access,
                correlation_id: CorrelationId::generate(),
            });
            next.run(request).await
        }
        Err(reason) => {
            tracing::warn!(
                reason = reason.as_str(),
                "contact vault delegation rejected"
            );
            let mut response =
                RelayError::Refused(ErrorCode::ContactDelegationInvalid).into_response();
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"cfmeet-contact-vault\""),
            );
            response
        }
    }
}

fn body<T: DeserializeOwned>(raw: Result<Json<T>, JsonRejection>) -> Result<T, RelayError> {
    // Never surface the parser's message: it may quote the offending input.
    raw.map(|Json(b)| b)
        .map_err(|_| RelayError::Validation("The request body is invalid."))
}

fn reference(raw: &str) -> Result<Uuid, RelayError> {
    Uuid::parse_str(raw).map_err(|_| RelayError::Refused(ErrorCode::ContactNotFound))
}

fn respond(status: StatusCode, body: Value) -> Response {
    let mut response = (status, Json(body)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn create_contact(
    State(state): State<AppState>,
    caller: ContactCaller,
    raw: Result<Json<CreateContactRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let actor = caller.actor(ContactScope::Write)?;
    let request = body(raw)?;
    let (status, body) = state.contacts.create(&actor, request).await?;
    Ok(respond(
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        body,
    ))
}

async fn get_contact(
    State(state): State<AppState>,
    caller: ContactCaller,
    Path(id): Path<String>,
) -> Result<Response, RelayError> {
    let actor = caller.actor(ContactScope::Read)?;
    Ok(respond(
        StatusCode::OK,
        state.contacts.get(&actor, reference(&id)?).await?,
    ))
}

async fn update_contact(
    State(state): State<AppState>,
    caller: ContactCaller,
    Path(id): Path<String>,
    raw: Result<Json<UpdateContactRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let actor = caller.actor(ContactScope::Write)?;
    let id = reference(&id)?;
    let request = body(raw)?;
    Ok(respond(
        StatusCode::OK,
        state.contacts.update(&actor, id, request).await?,
    ))
}

async fn disable_contact(
    State(state): State<AppState>,
    caller: ContactCaller,
    Path(id): Path<String>,
    raw: Result<Json<DisableContactRequest>, JsonRejection>,
) -> Result<Response, RelayError> {
    let actor = caller.actor(ContactScope::Write)?;
    let id = reference(&id)?;
    let request = body(raw)?;
    Ok(respond(
        StatusCode::OK,
        state.contacts.disable(&actor, id, request).await?,
    ))
}

async fn assert_verification(
    State(state): State<AppState>,
    caller: ContactCaller,
    Path(id): Path<String>,
    raw: Result<Json<VerificationAssertion>, JsonRejection>,
) -> Result<Response, RelayError> {
    // A trusted verification integration only: privacy.contacts.write is not enough.
    let actor = caller.actor(ContactScope::Verify)?;
    let id = reference(&id)?;
    let request = body(raw)?;
    Ok(respond(
        StatusCode::OK,
        state
            .contacts
            .assert_verification(&actor, id, request)
            .await?,
    ))
}

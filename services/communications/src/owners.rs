//! Typed calls to canonical owners (Calling, Messaging, Meetings). The Hub never stores
//! their state; it asks them, under the user's delegated context, and relays only safe
//! fields. Every failure is closed: transport problems become `Unavailable`.
use crate::{
    error::{Error, Result},
    model::Profile,
    State,
};
use axum::http::Method;
use cf_service_auth::{DelegatedPlatformContext, OutboundServiceRequest, ServiceId};
use serde_json::{json, Value};
use uuid::Uuid;

/// Error codes an owner may return that are safe to relay to the consumer unchanged.
const RELAYED_CODES: &[&str] = &[
    "TARGET_UNAVAILABLE",
    "IDEMPOTENCY_CONFLICT",
    "E2EE_NOT_AVAILABLE",
    "E2EE_DEVICES_DISABLED",
    "ENDPOINT_NOT_ACTIVE",
    "GROUP_TOO_LARGE",
    "MEETING_V2_DISABLED",
    "COMMUNICATIONS_POLICY_UNAVAILABLE",
    "RATE_LIMITED",
];

pub struct Owner<'a> {
    state: &'a State,
    scope: &'a DelegatedPlatformContext,
    service: ServiceId,
}
impl State {
    pub fn calling<'a>(&'a self, scope: &'a DelegatedPlatformContext) -> Result<Owner<'a>> {
        if !self.config.calling_configured {
            return Err(Error::DependencyNotConfigured);
        }
        Ok(Owner {
            state: self,
            scope,
            service: ServiceId::calling(),
        })
    }
    pub fn messaging<'a>(&'a self, scope: &'a DelegatedPlatformContext) -> Result<Owner<'a>> {
        if !self.config.messaging_configured {
            return Err(Error::DependencyNotConfigured);
        }
        Ok(Owner {
            state: self,
            scope,
            service: ServiceId::messaging(),
        })
    }
    pub fn meetings<'a>(&'a self, scope: &'a DelegatedPlatformContext) -> Owner<'a> {
        Owner {
            state: self,
            scope,
            service: ServiceId::meeting_service(),
        }
    }
}
impl Owner<'_> {
    /// POST and return `(status, body)`. Non-JSON or schema-less replies are failures.
    pub async fn post(&self, path: &str, body: &Value) -> Result<(u16, Value)> {
        let req = OutboundServiceRequest::new_operation(self.service.clone())
            .with_delegated_context(self.scope.clone());
        let response = self
            .state
            .config
            .client
            .exchange_json(&req, Method::POST, path, Some(body))
            .await
            .map_err(|_| Error::Unavailable)?;
        let status = response.status();
        let value: Value = response.json().map_err(|_| Error::Unavailable)?;
        Ok((status, value))
    }
    /// POST requiring 200 with `schemaVersion: 1`; owner 404 becomes `NotFound`, 403
    /// `Forbidden`, relayable conflicts keep their code, anything else `Unavailable`.
    pub async fn call(&self, path: &str, body: &Value) -> Result<Value> {
        let (status, value) = self.post(path, body).await?;
        match status {
            200 | 201 if value["schemaVersion"] == 1 => Ok(value),
            200 | 201 => Err(Error::Unavailable),
            404 => Err(Error::NotFound),
            403 => Err(Error::Forbidden),
            400 | 409 | 410 | 422 | 429 => Err(relay(status, &value)),
            _ => Err(Error::Unavailable),
        }
    }
}
fn relay(status: u16, value: &Value) -> Error {
    let code = value["error"]["code"]
        .as_str()
        .or_else(|| value["code"].as_str())
        .unwrap_or("");
    match RELAYED_CODES.iter().find(|c| **c == code) {
        Some(code) => Error::Owner { status, code },
        None if status == 429 => Error::RateLimited,
        None if status == 400 || status == 422 => Error::Validation,
        None => Error::ActionNotAllowed,
    }
}

/// Canonical Messaging view of one conversation for this member (MS-1).
#[derive(Debug, Clone)]
pub struct Conversation {
    pub id: Uuid,
    pub context_class: String,
    pub security_profile: String,
    pub lifecycle_state: String,
    pub last_sequence: i64,
    pub retention_seconds: Option<i64>,
    pub members: Vec<Uuid>,
    pub message: Option<Value>,
    pub raw: Value,
}
impl State {
    /// K9 (MS-1b): the access view of up to 50 messages in one Messaging call, keyed by
    /// (conversation, message): `None` for a conversation the profile is not an ACTIVE
    /// member of, otherwise the message state MS-1 reports. A reference Messaging did not
    /// answer is absent. `NotFound` means an older Messaging image without the route.
    pub async fn messages_access(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        items: &[(Uuid, Uuid)],
    ) -> Result<std::collections::HashMap<(Uuid, Uuid), Option<String>>> {
        let refs: Vec<Value> = items
            .iter()
            .map(|(c, m)| json!({"conversationId":c,"messageId":m}))
            .collect();
        let v = self
            .messaging(scope)?
            .call(
                "/internal/v1/communications/conversations/messages/access",
                &json!({"principalId":p.principal_id,"items":refs}),
            )
            .await?;
        let answered = v["items"].as_array().ok_or(Error::Unavailable)?;
        let mut out = std::collections::HashMap::new();
        for a in answered {
            let id = |k: &str| a[k].as_str().and_then(|s| Uuid::parse_str(s).ok());
            let (Some(c), Some(m)) = (id("conversationId"), id("messageId")) else {
                return Err(Error::Unavailable);
            };
            let state = if a["member"] == true {
                Some(a["state"].as_str().unwrap_or("NOT_FOUND").to_owned())
            } else {
                None
            };
            out.insert((c, m), state);
        }
        Ok(out)
    }
    /// Fails with `NotFound` unless the profile is an ACTIVE member right now.
    pub async fn conversation(
        &self,
        scope: &DelegatedPlatformContext,
        p: &Profile,
        conversation: Uuid,
        message: Option<Uuid>,
    ) -> Result<Conversation> {
        let mut body = json!({"principalId":p.principal_id,"conversationId":conversation});
        if let Some(m) = message {
            body["messageId"] = json!(m);
        }
        let v = self
            .messaging(scope)?
            .call("/internal/v1/communications/conversations/access", &body)
            .await?;
        if v["member"] != true {
            return Err(Error::NotFound);
        }
        if v["conversationId"] != json!(conversation) {
            return Err(Error::Unavailable);
        }
        let members = v["memberPrincipalIds"]
            .as_array()
            .ok_or(Error::Unavailable)?
            .iter()
            .map(|m| m.as_str().and_then(|s| Uuid::parse_str(s).ok()))
            .collect::<Option<Vec<_>>>()
            .ok_or(Error::Unavailable)?;
        Ok(Conversation {
            id: conversation,
            context_class: v["contextClass"]
                .as_str()
                .ok_or(Error::Unavailable)?
                .to_owned(),
            security_profile: v["securityProfile"]
                .as_str()
                .unwrap_or("UNKNOWN")
                .to_owned(),
            lifecycle_state: v["lifecycleState"].as_str().unwrap_or("UNKNOWN").to_owned(),
            last_sequence: v["lastSequence"]
                .as_i64()
                .filter(|s| *s >= 0)
                .ok_or(Error::Unavailable)?,
            retention_seconds: v["retentionSeconds"].as_i64(),
            members,
            message: v.get("message").filter(|m| !m.is_null()).cloned(),
            raw: v,
        })
    }
}

//! Blueprint Section 11: notification preference reads for the Account (Meeting Service).
//!
//! The Hub owns personal notification preferences (`communication_notification_preferences`).
//! Before the Account emails a user about a meeting it asks which of the recipients turned
//! the category off. Only account references go in and only the opted-out references come
//! out: no preference values, profile data or principal ids leave the Hub. Accounts without
//! a profile or a stored preference are not listed (the default is enabled).
use crate::{
    auth,
    error::{Error, Result},
    store, State,
};
use axum::{extract::State as HttpState, Extension, Json};
use cf_communications_policy::Scope;
use cf_service_auth::{InternalServiceContext, ServiceId};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const MAX_ACCOUNTS: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreferenceQuery {
    pub category: String,
    pub account_references: Vec<String>,
}

fn valid_reference(r: &str) -> bool {
    r.strip_prefix("cfmeet-user:")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .is_some_and(|id| format!("cfmeet-user:{id}") == r)
}

impl State {
    /// The references among `query.account_references` (in this scope) that turned the
    /// category off, sorted.
    pub async fn notification_opt_outs(
        &self,
        scope: &cf_service_auth::DelegatedPlatformContext,
        query: &PreferenceQuery,
    ) -> Result<Vec<String>> {
        let column = match query.category.as_str() {
            "MEETINGS" => "meeting_invitations",
            _ => return Err(Error::Validation),
        };
        let unique: BTreeSet<&str> = query
            .account_references
            .iter()
            .map(String::as_str)
            .collect();
        if unique.is_empty()
            || unique.len() != query.account_references.len()
            || unique.len() > MAX_ACCOUNTS
            || !unique.iter().all(|r| valid_reference(r))
        {
            return Err(Error::Validation);
        }
        auth::validate_scope(self, scope).await?;
        let mut values = store::scoped(Scope::of(scope));
        values.push(json!(unique.iter().collect::<Vec<_>>()).into());
        let rows: Vec<Value> = store::rows(
            &self.store.db,
            &format!(
                "SELECT jsonb_build_object('ref', p.account_reference) doc \
                   FROM communication_profiles p \
                   JOIN communication_notification_preferences n ON n.profile_id = p.id \
                  WHERE p.tenant_id = $1 AND p.application_id = $2 AND p.environment_id = $3 \
                    AND p.account_reference IN (SELECT jsonb_array_elements_text($4::jsonb)) \
                    AND n.{column} = false \
                  ORDER BY p.account_reference"
            ),
            values,
        )
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|v| v["ref"].as_str().map(str::to_owned))
            .collect())
    }
}

/// `POST /internal/v1/communications/notification-preferences/resolve` (Meeting Service
/// only, `communications-preferences.read`).
pub async fn resolve(
    HttpState(s): HttpState<State>,
    Extension(ctx): Extension<InternalServiceContext>,
    Json(query): Json<PreferenceQuery>,
) -> Result<Json<Value>> {
    if ctx.authenticated_service() != &ServiceId::meeting_service() {
        return Err(Error::Forbidden);
    }
    let scope = auth::delegated(&ctx)?;
    let opted_out = s.notification_opt_outs(&scope, &query).await?;
    Ok(Json(
        json!({"schemaVersion":1,"category":query.category,"optedOut":opted_out}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_are_canonical_account_references() {
        let id = uuid::Uuid::new_v4();
        assert!(valid_reference(&format!("cfmeet-user:{id}")));
        assert!(!valid_reference(&format!("cfmeet-user:{}", id.simple())));
        assert!(!valid_reference(&format!(
            "cfmeet-user:{}",
            id.to_string().to_uppercase()
        )));
        assert!(!valid_reference(&format!("other:{id}")));
        assert!(!valid_reference("cfmeet-user:"));
    }
}

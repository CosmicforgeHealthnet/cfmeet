//! Owner APIs (Document 4 §3.4, P2 "authorities are asked, not copied"): the service asks
//! Account (X1, X6, X7), Meeting (X2), Messaging (X3), the Communications Hub (X4) and
//! Organizations (X5) at send time instead of copying their data.
//!
//! [`Authorities`] is the port; [`HttpAuthorities`] signs each call with the service key
//! (`cf-service-auth`, single-audience tokens) and talks only to the owners configured in
//! `NOTIFICATIONS_AUTHORITIES`; an owner that is not configured answers
//! [`AuthorityError::NotConfigured`], which the processor turns into "unresolved" (a
//! SHADOW record, or a retry while ACTIVE). [`StaticAuthorities`] is the in-memory test
//! double.

use std::{collections::HashMap, sync::Arc, sync::Mutex};

use async_trait::async_trait;
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    OutboundServiceRequest, ServiceId, ServiceSigningKey, ServiceTokenSigner, SharedClock,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::config::{notifications_service_id, NotificationsConfig};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityError {
    #[error("owner API not configured")]
    NotConfigured,
    #[error("owner API unavailable")]
    Unavailable,
    #[error("owner API answered an unexpected response")]
    InvalidResponse,
}

/// X1 result for one account.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountContact {
    pub account_ref: String,
    pub status: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub devices: Vec<AccountDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDevice {
    pub account_device_ref: String,
    pub state: String,
}

impl AccountContact {
    pub fn active(&self) -> bool {
        self.status == "ACTIVE"
    }
}

/// X2 result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceRecipients {
    pub conference_id: String,
    pub still_true: bool,
    pub presented_title: String,
    pub starts_at: String,
    #[serde(default)]
    pub time_zone: Option<String>,
    /// Contract amendment (experimental, N6): the join path under the app URL
    /// (`/<meeting code>`), and the scheduled end.
    #[serde(default)]
    pub join_path: Option<String>,
    #[serde(default)]
    pub ends_at: Option<String>,
    pub recipients: Vec<ConferenceRecipient>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ConferenceRecipient {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub email: Option<String>,
}

/// X3 result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeupAuthorization {
    pub conversation_active: bool,
    pub security_profile: String,
    pub conversation_kind: String,
    pub entitled: Vec<EntitledPrincipal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntitledPrincipal {
    pub principal: String,
    #[serde(default)]
    pub account_ref: Option<String>,
    pub mentions_recipient: bool,
}

/// X4 result for one recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Restriction {
    pub blocked: bool,
    pub privacy_denied: bool,
    pub operator_hold: bool,
}

/// X6 / X7 outcome per decision.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseResult {
    pub decision_id: Uuid,
    pub outcome: String,
    #[serde(default)]
    pub lease_until: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait Authorities: Send + Sync {
    /// Which owner APIs are configured (reported by `/ready`).
    fn configured(&self) -> Vec<&'static str>;
    /// X1.
    async fn contacts(
        &self,
        account_refs: &[String],
    ) -> Result<HashMap<String, AccountContact>, AuthorityError>;
    /// X2.
    async fn conference(
        &self,
        conference_id: Uuid,
        kind: &str,
        starts_at: Option<&str>,
    ) -> Result<ConferenceRecipients, AuthorityError>;
    /// X3.
    async fn wakeups(
        &self,
        conversation_id: Uuid,
        event_sequence: i64,
        sender: &str,
    ) -> Result<WakeupAuthorization, AuthorityError>;
    /// X4.
    async fn restrictions(
        &self,
        actor: Option<&str>,
        recipients: &[String],
        action: &str,
    ) -> Result<HashMap<String, Restriction>, AuthorityError>;
    /// X5: (status, invitee account ref).
    async fn invitee(
        &self,
        invitation_id: Uuid,
    ) -> Result<(String, Option<String>), AuthorityError>;
    /// X6.
    async fn claim_leases(
        &self,
        decision_ids: &[Uuid],
        lease_seconds: u32,
    ) -> Result<Vec<LeaseResult>, AuthorityError>;
    /// X7.
    async fn complete(&self, decision_ids: &[Uuid]) -> Result<Vec<LeaseResult>, AuthorityError>;
}

/// No owner configured (dark deployment): every lookup is `NotConfigured`.
pub struct NoAuthorities;

#[async_trait]
impl Authorities for NoAuthorities {
    fn configured(&self) -> Vec<&'static str> {
        Vec::new()
    }
    async fn contacts(
        &self,
        _: &[String],
    ) -> Result<HashMap<String, AccountContact>, AuthorityError> {
        Err(AuthorityError::NotConfigured)
    }
    async fn conference(
        &self,
        _: Uuid,
        _: &str,
        _: Option<&str>,
    ) -> Result<ConferenceRecipients, AuthorityError> {
        Err(AuthorityError::NotConfigured)
    }
    async fn wakeups(
        &self,
        _: Uuid,
        _: i64,
        _: &str,
    ) -> Result<WakeupAuthorization, AuthorityError> {
        Err(AuthorityError::NotConfigured)
    }
    async fn restrictions(
        &self,
        _: Option<&str>,
        _: &[String],
        _: &str,
    ) -> Result<HashMap<String, Restriction>, AuthorityError> {
        Err(AuthorityError::NotConfigured)
    }
    async fn invitee(&self, _: Uuid) -> Result<(String, Option<String>), AuthorityError> {
        Err(AuthorityError::NotConfigured)
    }
    async fn claim_leases(&self, _: &[Uuid], _: u32) -> Result<Vec<LeaseResult>, AuthorityError> {
        Err(AuthorityError::NotConfigured)
    }
    async fn complete(&self, _: &[Uuid]) -> Result<Vec<LeaseResult>, AuthorityError> {
        Err(AuthorityError::NotConfigured)
    }
}

/// Signed HTTP clients of the configured owners.
pub struct HttpAuthorities {
    client: Arc<InternalServiceClient>,
    account: bool,
    meeting: bool,
    messaging: bool,
    communications: bool,
    organizations: bool,
}

impl HttpAuthorities {
    pub fn from_config(
        config: &NotificationsConfig,
        clock: SharedClock,
    ) -> Result<Option<Self>, String> {
        let Some(out) = &config.outbound else {
            return Ok(None);
        };
        let key = ServiceSigningKey::from_pkcs8_pem_file(out.key_id.clone(), &out.private_key_file)
            .map_err(|e| format!("CF_SERVICE_AUTH_PRIVATE_KEY_FILE: {e}"))?;
        let signer = Arc::new(ServiceTokenSigner::new(
            notifications_service_id(),
            config.runtime_environment,
            key,
            out.token_ttl,
            clock,
        ));
        let a = &config.authorities;
        let mut targets = InternalTargets::new(config.runtime_environment);
        let meeting_url = a.account.as_ref().or(a.meeting.as_ref());
        if let Some(url) = meeting_url {
            targets
                .insert(ServiceId::meeting_service(), url)
                .map_err(|e| format!("CF_MEETING_SERVICE_BASE_URL: {e}"))?;
        }
        if let Some(url) = &a.messaging {
            targets
                .insert(ServiceId::messaging(), url)
                .map_err(|e| format!("CF_MESSAGING_BASE_URL: {e}"))?;
        }
        if let Some(url) = &a.communications {
            targets
                .insert(ServiceId::communications(), url)
                .map_err(|e| format!("CF_COMMUNICATIONS_BASE_URL: {e}"))?;
        }
        if let Some(url) = &a.organizations {
            targets
                .insert(ServiceId::organizations(), url)
                .map_err(|e| format!("CF_ORGANIZATIONS_BASE_URL: {e}"))?;
        }
        let client = InternalServiceClient::new(signer, targets, out.http_timeout)
            .map_err(|e| e.to_string())?;
        Ok(Some(Self {
            client: Arc::new(client),
            account: a.account.is_some(),
            meeting: a.meeting.is_some(),
            messaging: a.messaging.is_some(),
            communications: a.communications.is_some(),
            organizations: a.organizations.is_some(),
        }))
    }

    async fn post<T: for<'de> Deserialize<'de>>(
        &self,
        target: ServiceId,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, AuthorityError> {
        let request = OutboundServiceRequest::new_operation(target);
        let response = self
            .client
            .post_json(&request, path, body)
            .await
            .map_err(|_| AuthorityError::Unavailable)?;
        response
            .json::<T>()
            .map_err(|_| AuthorityError::InvalidResponse)
    }
}

#[derive(Deserialize)]
struct Results<T> {
    results: Vec<T>,
}

#[async_trait]
impl Authorities for HttpAuthorities {
    fn configured(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        for (n, on) in [
            ("account", self.account),
            ("meeting", self.meeting),
            ("messaging", self.messaging),
            ("communications", self.communications),
            ("organizations", self.organizations),
        ] {
            if on {
                v.push(n);
            }
        }
        v
    }

    async fn contacts(
        &self,
        account_refs: &[String],
    ) -> Result<HashMap<String, AccountContact>, AuthorityError> {
        if !self.account {
            return Err(AuthorityError::NotConfigured);
        }
        let mut out = HashMap::new();
        for chunk in account_refs.chunks(100) {
            let r: Results<AccountContact> = self
                .post(
                    ServiceId::meeting_service(),
                    "/internal/v1/accounts/notification-contacts/resolve",
                    &json!({ "accountRefs": chunk }),
                )
                .await?;
            for c in r.results {
                out.insert(c.account_ref.clone(), c);
            }
        }
        Ok(out)
    }

    async fn conference(
        &self,
        conference_id: Uuid,
        kind: &str,
        starts_at: Option<&str>,
    ) -> Result<ConferenceRecipients, AuthorityError> {
        if !self.meeting {
            return Err(AuthorityError::NotConfigured);
        }
        let mut body = json!({ "kind": kind });
        if let Some(s) = starts_at {
            body["startsAt"] = json!(s);
        }
        self.post(
            ServiceId::meeting_service(),
            &format!("/internal/v1/conferences/{conference_id}/notification-recipients/resolve"),
            &body,
        )
        .await
    }

    async fn wakeups(
        &self,
        conversation_id: Uuid,
        event_sequence: i64,
        sender: &str,
    ) -> Result<WakeupAuthorization, AuthorityError> {
        if !self.messaging {
            return Err(AuthorityError::NotConfigured);
        }
        self.post(
            ServiceId::messaging(),
            "/internal/v1/notifications/wakeups/authorize",
            &json!({ "conversationId": conversation_id, "eventSequence": event_sequence, "senderPrincipal": sender }),
        )
        .await
    }

    async fn restrictions(
        &self,
        actor: Option<&str>,
        recipients: &[String],
        action: &str,
    ) -> Result<HashMap<String, Restriction>, AuthorityError> {
        if !self.communications {
            return Err(AuthorityError::NotConfigured);
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Row {
            recipient_ref: String,
            blocked: bool,
            privacy_denied: bool,
            operator_hold: bool,
        }
        let mut out = HashMap::new();
        for chunk in recipients.chunks(100) {
            let r: Results<Row> = self
                .post(
                    ServiceId::communications(),
                    "/internal/v1/communications/restrictions/check",
                    &json!({ "actorRef": actor, "recipientRefs": chunk, "action": action }),
                )
                .await?;
            for row in r.results {
                out.insert(
                    row.recipient_ref,
                    Restriction {
                        blocked: row.blocked,
                        privacy_denied: row.privacy_denied,
                        operator_hold: row.operator_hold,
                    },
                );
            }
        }
        Ok(out)
    }

    async fn invitee(
        &self,
        invitation_id: Uuid,
    ) -> Result<(String, Option<String>), AuthorityError> {
        if !self.organizations {
            return Err(AuthorityError::NotConfigured);
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Row {
            status: String,
            invitee_account_ref: Option<String>,
        }
        let r: Row = self
            .post(
                ServiceId::organizations(),
                &format!("/internal/v1/organization-invitations/{invitation_id}/invitee/resolve"),
                &json!({}),
            )
            .await?;
        Ok((r.status, r.invitee_account_ref))
    }

    async fn claim_leases(
        &self,
        decision_ids: &[Uuid],
        lease_seconds: u32,
    ) -> Result<Vec<LeaseResult>, AuthorityError> {
        if !self.account {
            return Err(AuthorityError::NotConfigured);
        }
        let r: Results<LeaseResult> = self
            .post(
                ServiceId::meeting_service(),
                "/internal/v1/accounts/notification-route-decisions/leases",
                &json!({ "decisionIds": decision_ids, "leaseSeconds": lease_seconds }),
            )
            .await?;
        Ok(r.results)
    }

    async fn complete(&self, decision_ids: &[Uuid]) -> Result<Vec<LeaseResult>, AuthorityError> {
        if !self.account {
            return Err(AuthorityError::NotConfigured);
        }
        let r: Results<LeaseResult> = self
            .post(
                ServiceId::meeting_service(),
                "/internal/v1/accounts/notification-route-decisions/completions",
                &json!({ "decisionIds": decision_ids }),
            )
            .await?;
        Ok(r.results)
    }
}

/// In-memory owners for tests and local runs.
#[derive(Default)]
pub struct StaticAuthorities {
    pub contacts: Mutex<HashMap<String, AccountContact>>,
    pub conferences: Mutex<HashMap<Uuid, ConferenceRecipients>>,
    pub wakeups: Mutex<HashMap<Uuid, WakeupAuthorization>>,
    pub restrictions: Mutex<HashMap<String, Restriction>>,
    pub invitees: Mutex<HashMap<Uuid, (String, Option<String>)>>,
    /// Decision id → outcome of the next lease claim (default GRANTED).
    pub lease_outcomes: Mutex<HashMap<Uuid, String>>,
    pub leases_claimed: Mutex<Vec<Uuid>>,
    pub completed: Mutex<Vec<Uuid>>,
    pub unavailable: Mutex<bool>,
}

impl StaticAuthorities {
    pub fn with_contact(self, account_ref: &str, email: &str, name: &str) -> Self {
        self.contacts.lock().unwrap().insert(
            account_ref.to_string(),
            AccountContact {
                account_ref: account_ref.to_string(),
                status: "ACTIVE".into(),
                email: Some(email.to_string()),
                display_name: Some(name.to_string()),
                locale: Some("en".into()),
                time_zone: Some("UTC".into()),
                devices: Vec::new(),
            },
        );
        self
    }

    fn check(&self) -> Result<(), AuthorityError> {
        if *self.unavailable.lock().unwrap() {
            Err(AuthorityError::Unavailable)
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl Authorities for StaticAuthorities {
    fn configured(&self) -> Vec<&'static str> {
        vec![
            "account",
            "meeting",
            "messaging",
            "communications",
            "organizations",
        ]
    }
    async fn contacts(
        &self,
        account_refs: &[String],
    ) -> Result<HashMap<String, AccountContact>, AuthorityError> {
        self.check()?;
        let all = self.contacts.lock().unwrap();
        Ok(account_refs
            .iter()
            .filter_map(|r| all.get(r).map(|c| (r.clone(), c.clone())))
            .collect())
    }
    async fn conference(
        &self,
        conference_id: Uuid,
        _: &str,
        _: Option<&str>,
    ) -> Result<ConferenceRecipients, AuthorityError> {
        self.check()?;
        self.conferences
            .lock()
            .unwrap()
            .get(&conference_id)
            .cloned()
            .ok_or(AuthorityError::InvalidResponse)
    }
    async fn wakeups(
        &self,
        conversation_id: Uuid,
        _: i64,
        _: &str,
    ) -> Result<WakeupAuthorization, AuthorityError> {
        self.check()?;
        self.wakeups
            .lock()
            .unwrap()
            .get(&conversation_id)
            .cloned()
            .ok_or(AuthorityError::InvalidResponse)
    }
    async fn restrictions(
        &self,
        _: Option<&str>,
        recipients: &[String],
        _: &str,
    ) -> Result<HashMap<String, Restriction>, AuthorityError> {
        self.check()?;
        let all = self.restrictions.lock().unwrap();
        Ok(recipients
            .iter()
            .map(|r| (r.clone(), all.get(r).copied().unwrap_or_default()))
            .collect())
    }
    async fn invitee(
        &self,
        invitation_id: Uuid,
    ) -> Result<(String, Option<String>), AuthorityError> {
        self.check()?;
        Ok(self
            .invitees
            .lock()
            .unwrap()
            .get(&invitation_id)
            .cloned()
            .unwrap_or(("PENDING".into(), None)))
    }
    async fn claim_leases(
        &self,
        decision_ids: &[Uuid],
        lease_seconds: u32,
    ) -> Result<Vec<LeaseResult>, AuthorityError> {
        self.check()?;
        let outcomes = self.lease_outcomes.lock().unwrap();
        let mut claimed = self.leases_claimed.lock().unwrap();
        Ok(decision_ids
            .iter()
            .map(|d| {
                let outcome = outcomes.get(d).cloned().unwrap_or_else(|| "GRANTED".into());
                if outcome == "GRANTED" {
                    claimed.push(*d);
                }
                LeaseResult {
                    decision_id: *d,
                    lease_until: (outcome == "GRANTED")
                        .then(|| Utc::now() + chrono::Duration::seconds(lease_seconds as i64)),
                    outcome,
                }
            })
            .collect())
    }
    async fn complete(&self, decision_ids: &[Uuid]) -> Result<Vec<LeaseResult>, AuthorityError> {
        self.check()?;
        self.completed
            .lock()
            .unwrap()
            .extend_from_slice(decision_ids);
        Ok(decision_ids
            .iter()
            .map(|d| LeaseResult {
                decision_id: *d,
                outcome: "COMPLETED".into(),
                lease_until: None,
            })
            .collect())
    }
}

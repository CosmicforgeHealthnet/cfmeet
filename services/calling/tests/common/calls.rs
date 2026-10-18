//! Call-domain test fixtures: an authenticated consumer, trusted principals and helpers
//! that drive a Call to any canonical state with real (synthetic, provider-independent)
//! evidence. No RTC, provider or telephony execution exists or is simulated here.

use crate::common::IssuedCredential;
use crate::common::NewPrincipal;
use calling::{
    calls::{
        attributes::SafeAttributes,
        authorizations::{CallAuthorization, NewAuthorization},
        ids::{CallId, CallSessionId},
        kinds::{AuthorizationReason, CallKind, EndpointCategory},
        routes::PolicyVersion,
        service::{CreatedCall, NewCall, TransitionOutcome},
        Call, CallAction, CallCommandContext, CallError, CallResult, CallScope, CallService,
        CallState, ConnectedEvidence, EndedEvidence,
    },
    domain::{EnvironmentKind, PrincipalKind, TenantClassification},
    platform::{
        context::PlatformAuthContext,
        limits::LimitValues,
        principals::{PrincipalReference, ResolvedPrincipal},
        scopes::names,
    },
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};

use super::{Consumer, Harness};

/// One authenticated application environment with call scopes, generous limits and two
/// trusted principals (initiator, recipient).
pub struct CallWorld {
    pub h: Harness,
    pub consumer: Consumer,
    pub kind: EnvironmentKind,
    pub token: IssuedCredential,
    pub ctx: PlatformAuthContext,
    pub cmd: CallCommandContext,
    pub initiator: ResolvedPrincipal,
    pub recipient: ResolvedPrincipal,
}

impl CallWorld {
    /// A world in a fresh database.
    pub async fn new() -> Option<Self> {
        let h = Harness::new().await?;
        Some(Self::in_harness(h, "acme", EnvironmentKind::Production).await)
    }

    pub async fn in_harness(h: Harness, slug: &str, kind: EnvironmentKind) -> Self {
        let consumer = h.consumer(slug, TenantClassification::External).await;
        Self::for_consumer(h, consumer, kind).await
    }

    pub async fn for_consumer(h: Harness, consumer: Consumer, kind: EnvironmentKind) -> Self {
        let token = h
            .issue(
                consumer.sa(kind),
                &[
                    names::CALLS_READ,
                    names::CALLS_WRITE,
                    names::PRINCIPALS_READ,
                ],
            )
            .await;
        // Generous limits: state-machine suites create many calls.
        h.platform
            .set_credential_limits(
                token.credential.id,
                LimitValues {
                    max_concurrent_communications: Some(100_000),
                    requests_per_minute: Some(100_000),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let ctx = h.ctx(token.secret.expose_once()).await;
        let cmd = CallCommandContext::customer(&ctx);
        let initiator = resolve(&h, &consumer, kind, &ctx, "initiator-1").await;
        let recipient = resolve(&h, &consumer, kind, &ctx, "recipient-1").await;
        Self {
            h,
            consumer,
            kind,
            token,
            ctx,
            cmd,
            initiator,
            recipient,
        }
    }

    pub fn calls(&self) -> &CallService {
        &self.h.state.calls
    }

    pub fn scope(&self) -> CallScope {
        CallScope::of_platform(&self.ctx)
    }

    pub fn secret(&self) -> &str {
        self.token.secret.expose_once()
    }

    /// Registers and resolves another principal of this world's environment.
    pub async fn principal(&self, reference: &str) -> ResolvedPrincipal {
        resolve(&self.h, &self.consumer, self.kind, &self.ctx, reference).await
    }

    pub async fn create(&self) -> CreatedCall {
        self.calls()
            .create_requested_call(
                &self.ctx,
                &self.initiator,
                NewCall::of_kind(CallKind::Voice),
            )
            .await
            .expect("call created")
    }

    pub async fn call(&self, id: CallId) -> Call {
        self.calls().call(self.scope(), id).await.unwrap()
    }

    pub async fn authorize_decision(
        &self,
        call: CallId,
        reason: AuthorizationReason,
    ) -> CallAuthorization {
        self.calls()
            .record_authorization(
                &self.cmd,
                call,
                NewAuthorization {
                    reason,
                    policy_version: PolicyVersion::parse("policy-v1").unwrap(),
                    policy_snapshot: SafeAttributes::new().with("rule", "default-allow").unwrap(),
                    limits_snapshot: SafeAttributes::new()
                        .with("max_concurrent_communications", 100_000)
                        .unwrap(),
                },
            )
            .await
            .unwrap()
    }

    pub async fn approve(&self, call: CallId) -> CallAuthorization {
        self.authorize_decision(call, AuthorizationReason::PolicySatisfied)
            .await
    }

    /// Applies `action` against the Call's current version.
    pub async fn act(&self, call: CallId, action: CallAction) -> CallResult<TransitionOutcome> {
        let version = self.call(call).await.version;
        self.calls()
            .transition(&self.cmd, call, version, action)
            .await
    }

    /// Two connected application sessions (initiator + recipient): real evidence of a
    /// bidirectional media path between two participants of the Call.
    pub async fn connect_media(&self, call: CallId) -> Vec<CallSessionId> {
        let participants = self.calls().participants(self.scope(), call).await.unwrap();
        let initiator = participants
            .iter()
            .find(|p| p.principal_id == self.initiator.id())
            .unwrap()
            .id;
        let (recipient, _) = self
            .calls()
            .add_participant(&self.cmd, call, &self.recipient, None)
            .await
            .unwrap();
        let mut ids = Vec::new();
        for participant in [initiator, recipient.id] {
            let session = self
                .calls()
                .create_session(&self.cmd, call, participant, EndpointCategory::Mobile)
                .await
                .unwrap();
            self.calls()
                .connect_session(&self.cmd, call, session.id)
                .await
                .unwrap();
            ids.push(session.id);
        }
        ids
    }

    async fn connected_sessions(&self, call: CallId) -> Vec<CallSessionId> {
        let sessions: Vec<_> = self
            .calls()
            .sessions(self.scope(), call)
            .await
            .unwrap()
            .into_iter()
            .filter(|s| s.state == calling::calls::kinds::SessionState::Connected)
            .map(|s| s.id)
            .collect();
        if sessions.len() >= 2 {
            sessions
        } else {
            self.connect_media(call).await
        }
    }

    /// The action that moves a Call from `from` to `to` along the canonical graph, with
    /// whatever evidence it requires (authorization decisions, connected sessions).
    pub async fn action_for(&self, call: CallId, from: CallState, to: CallState) -> CallAction {
        use CallState::*;
        match (from, to) {
            (_, Authorized) => CallAction::Authorize {
                authorization_id: self.approve(call).await.id,
            },
            (_, Resolving) => CallAction::BeginResolution,
            (_, Routing) => CallAction::BeginRouting,
            (_, Dialing) => CallAction::BeginDialing,
            (_, Ringing) => CallAction::MarkRinging,
            (_, Answered) => CallAction::Accept,
            (from, Connected) => {
                let evidence = ConnectedEvidence::RtcMediaPathEstablished {
                    session_ids: self.connected_sessions(call).await,
                };
                match from {
                    Held => CallAction::Resume { evidence },
                    Transferring => CallAction::CompleteTransfer { evidence },
                    _ => CallAction::ConfirmConnected { evidence },
                }
            }
            (_, Held) => CallAction::Hold,
            (_, Transferring) => CallAction::BeginTransfer,
            (_, Ending) => CallAction::RequestEnd,
            (_, Ended) => CallAction::ConfirmEnded {
                evidence: EndedEvidence::CleanupConfirmed,
            },
            (_, Declined) => CallAction::Decline,
            (_, Busy) => CallAction::MarkBusy,
            (_, NoAnswer) => CallAction::MarkNoAnswer,
            (_, Failed) => CallAction::Fail {
                category: calling::calls::kinds::FailureCategory::Media,
            },
            (_, Cancelled) => CallAction::Cancel,
            (_, Requested) => unreachable!("nothing enters REQUESTED"),
        }
    }

    /// Drives a fresh Call to `target` along the canonical path.
    pub async fn call_in(&self, target: CallState) -> Call {
        let created = self.create().await;
        let id = created.call.id;
        let mut state = CallState::Requested;
        for next in canonical_path(target) {
            let action = self.action_for(id, state, *next).await;
            let outcome = self.act(id, action).await.unwrap_or_else(|e| {
                panic!("cannot drive {state} -> {next}: {e:?}");
            });
            assert_eq!(outcome.call.state, *next);
            state = *next;
        }
        self.call(id).await
    }

    pub async fn count(&self, sql: &str) -> i64 {
        super::scalar_i64(self.h.conn(), sql).await
    }

    pub async fn outbox_count(&self, call: CallId) -> i64 {
        self.count(&format!(
            "SELECT count(*) FROM calling_outbox_events WHERE aggregate_id = '{call}'"
        ))
        .await
    }

    pub async fn event_count(&self, call: CallId) -> i64 {
        self.count(&format!(
            "SELECT count(*) FROM call_events WHERE call_id = '{call}'"
        ))
        .await
    }

    pub async fn state_event_count(&self, call: CallId) -> i64 {
        self.count(&format!(
            "SELECT count(*) FROM call_events WHERE call_id = '{call}' AND event_type = 'CALL_STATE_CHANGED'"
        ))
        .await
    }
}

/// Registers (idempotently) an EXTERNAL_APPLICATION_PRINCIPAL and resolves it through the
/// trusted resolver in `ctx`'s scope.
pub async fn resolve(
    h: &Harness,
    consumer: &Consumer,
    kind: EnvironmentKind,
    ctx: &PlatformAuthContext,
    reference: &str,
) -> ResolvedPrincipal {
    let (view, _) = h
        .platform
        .register_principal(
            consumer.env(kind).id,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: reference.to_string(),
            },
        )
        .await
        .unwrap();
    h.state
        .principals
        .resolve(ctx, &PrincipalReference::Id(view.id))
        .await
        .unwrap()
}

/// Canonical path from REQUESTED to `target` (excluding REQUESTED).
pub fn canonical_path(target: CallState) -> &'static [CallState] {
    use CallState::*;
    const TO_RINGING: &[CallState] = &[Authorized, Resolving, Routing, Dialing, Ringing];
    match target {
        Requested => &[],
        Authorized => &TO_RINGING[..1],
        Resolving => &TO_RINGING[..2],
        Routing => &TO_RINGING[..3],
        Dialing => &TO_RINGING[..4],
        Ringing => TO_RINGING,
        Answered => &[Authorized, Resolving, Routing, Dialing, Ringing, Answered],
        Connected => &[
            Authorized, Resolving, Routing, Dialing, Ringing, Answered, Connected,
        ],
        Held => &[
            Authorized, Resolving, Routing, Dialing, Ringing, Answered, Connected, Held,
        ],
        Transferring => &[
            Authorized,
            Resolving,
            Routing,
            Dialing,
            Ringing,
            Answered,
            Connected,
            Transferring,
        ],
        Ending => &[
            Authorized, Resolving, Routing, Dialing, Ringing, Answered, Connected, Ending,
        ],
        Ended => &[
            Authorized, Resolving, Routing, Dialing, Ringing, Answered, Connected, Ending, Ended,
        ],
        Declined => &[Authorized, Resolving, Routing, Dialing, Ringing, Declined],
        Busy => &[Authorized, Resolving, Routing, Dialing, Ringing, Busy],
        NoAnswer => &[Authorized, Resolving, Routing, Dialing, Ringing, NoAnswer],
        Failed => &[Failed],
        Cancelled => &[Cancelled],
    }
}

/// Every action whose target is `to`, with evidence that would be valid if the source
/// allowed it (used to prove disallowed transitions fail for every action).
pub async fn actions_into(w: &CallWorld, call: CallId, to: CallState) -> Vec<CallAction> {
    // Real connected sessions when the Call has them, so CONNECTED is refused by the state
    // machine and not merely for lack of evidence.
    let sessions: Vec<CallSessionId> = w
        .calls()
        .sessions(w.scope(), call)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    let evidence = || ConnectedEvidence::RtcMediaPathEstablished {
        session_ids: if sessions.len() >= 2 {
            sessions.clone()
        } else {
            vec![
                CallSessionId::from_uuid(uuid::Uuid::new_v4()),
                CallSessionId::from_uuid(uuid::Uuid::new_v4()),
            ]
        },
    };
    use CallState::*;
    match to {
        Requested => vec![],
        Authorized => vec![CallAction::Authorize {
            authorization_id: calling::calls::ids::CallAuthorizationId::from_uuid(
                uuid::Uuid::new_v4(),
            ),
        }],
        Resolving => vec![CallAction::BeginResolution],
        Routing => vec![CallAction::BeginRouting],
        Dialing => vec![CallAction::BeginDialing],
        Ringing => vec![CallAction::MarkRinging],
        Answered => vec![CallAction::Accept],
        Connected => vec![
            CallAction::ConfirmConnected {
                evidence: evidence(),
            },
            CallAction::Resume {
                evidence: evidence(),
            },
            CallAction::CompleteTransfer {
                evidence: evidence(),
            },
        ],
        Held => vec![CallAction::Hold],
        Transferring => vec![CallAction::BeginTransfer],
        Ending => vec![CallAction::RequestEnd],
        Ended => vec![
            CallAction::ConfirmEnded {
                evidence: EndedEvidence::CleanupConfirmed,
            },
            CallAction::ConfirmEnded {
                evidence: EndedEvidence::AllParticipationEnded,
            },
        ],
        Declined => vec![CallAction::Decline],
        Busy => vec![CallAction::MarkBusy],
        NoAnswer => vec![CallAction::MarkNoAnswer],
        Failed => vec![
            CallAction::Fail {
                category: calling::calls::kinds::FailureCategory::Internal,
            },
            CallAction::DenyByPolicy {
                authorization_id: calling::calls::ids::CallAuthorizationId::from_uuid(
                    uuid::Uuid::new_v4(),
                ),
            },
        ],
        Cancelled => vec![CallAction::Cancel],
    }
}

/// Whether `err` is a state-machine refusal (not an infrastructure failure).
pub fn is_refusal(err: &CallError) -> bool {
    !matches!(err, CallError::Database { .. })
}

/// Executes raw SQL, returning the error text on failure.
pub async fn sql(h: &Harness, statement: &str) -> Result<(), String> {
    h.conn()
        .execute(Statement::from_string(
            DbBackend::Postgres,
            statement.to_string(),
        ))
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

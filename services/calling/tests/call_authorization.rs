//! Part C §16 / §18: call authorization decisions and the paid public-network guard.
//!
//! * REQUESTED → AUTHORIZED only with the Call's effective APPROVED decision;
//! * no decision, a DENIED decision, a superseded approval or a foreign decision never
//!   authorizes; DENIED maps to FAILED / POLICY only through DenyByPolicy;
//! * decisions persist safe actor / principal / request evidence and supersede each other
//!   (exactly one effective decision per Call, database-enforced);
//! * the paid public-network guard refuses until the Call is authorized, approved and in
//!   a live-telecom environment, and executes nothing.

mod common;

use calling::{
    calls::{
        attributes::SafeAttributes,
        authorizations::NewAuthorization,
        ids::CallAuthorizationId,
        kinds::{
            ActorKind, AuthorizationDecision, AuthorizationReason, FailureCategory, TerminalReason,
        },
        routes::PolicyVersion,
        CallAction, CallCommandContext, CallError, CallScope, CallState,
    },
    domain::EnvironmentKind,
    internal_auth::delegation::delegated_context,
};
use cf_service_auth::{OutboundServiceRequest, ServiceId};
use common::{
    calls::{sql, CallWorld},
    Harness,
};

#[tokio::test]
async fn an_effective_approved_decision_authorizes_and_nothing_else_does() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    // No decision at all.
    let call = w.create().await.call;
    assert!(matches!(
        w.act(
            call.id,
            CallAction::Authorize {
                authorization_id: CallAuthorizationId::from_uuid(uuid::Uuid::new_v4())
            }
        )
        .await
        .unwrap_err(),
        CallError::AuthorizationRequired
    ));
    // A DENIED decision never authorizes.
    let denied = w
        .authorize_decision(call.id, AuthorizationReason::PolicyDenied)
        .await;
    assert_eq!(denied.decision, AuthorizationDecision::Denied);
    assert!(matches!(
        w.act(
            call.id,
            CallAction::Authorize {
                authorization_id: denied.id
            }
        )
        .await
        .unwrap_err(),
        CallError::AuthorizationDenied
    ));
    assert_eq!(w.call(call.id).await.state, CallState::Requested);

    // A later approval supersedes the denial; the old denial id does not authorize.
    let approved = w.approve(call.id).await;
    assert!(w
        .act(
            call.id,
            CallAction::Authorize {
                authorization_id: denied.id
            }
        )
        .await
        .is_err());
    // A decision of another call is not evidence for this one.
    let other = w.create().await.call;
    let foreign = w.approve(other.id).await;
    assert!(matches!(
        w.act(
            call.id,
            CallAction::Authorize {
                authorization_id: foreign.id
            }
        )
        .await
        .unwrap_err(),
        CallError::AuthorizationRequired
    ));
    let authorized = w
        .act(
            call.id,
            CallAction::Authorize {
                authorization_id: approved.id,
            },
        )
        .await
        .unwrap();
    assert_eq!(authorized.call.state, CallState::Authorized);
    assert!(authorized.call.timestamps.authorized_at.is_some());
    assert_eq!(
        authorized.event.data["authorization_id"],
        approved.id.to_string()
    );

    // A superseded approval no longer authorizes.
    let call = w.create().await.call;
    let first = w.approve(call.id).await;
    let second = w.approve(call.id).await;
    assert!(matches!(
        w.act(
            call.id,
            CallAction::Authorize {
                authorization_id: first.id
            }
        )
        .await
        .unwrap_err(),
        CallError::AuthorizationRequired
    ));
    assert!(w
        .act(
            call.id,
            CallAction::Authorize {
                authorization_id: second.id
            }
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn a_denial_maps_to_failed_policy_only_with_denied_evidence() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await.call;
    // POLICY is not available to the generic Fail action.
    assert!(matches!(
        w.act(
            call.id,
            CallAction::Fail {
                category: FailureCategory::Policy
            }
        )
        .await
        .unwrap_err(),
        CallError::FailureCategoryNotAllowed(FailureCategory::Policy)
    ));
    // DenyByPolicy needs an effective DENIED decision.
    let approved = w.approve(call.id).await;
    assert!(matches!(
        w.act(
            call.id,
            CallAction::DenyByPolicy {
                authorization_id: approved.id
            }
        )
        .await
        .unwrap_err(),
        CallError::InsufficientEvidence(_)
    ));
    let denied = w
        .authorize_decision(call.id, AuthorizationReason::LimitExceeded)
        .await;
    let failed = w
        .act(
            call.id,
            CallAction::DenyByPolicy {
                authorization_id: denied.id,
            },
        )
        .await
        .unwrap();
    assert_eq!(failed.call.state, CallState::Failed);
    assert_eq!(failed.call.failure_category, Some(FailureCategory::Policy));
    assert_eq!(failed.call.terminal_reason, Some(TerminalReason::Failed));
    assert_eq!(failed.event.data["authorization_reason"], "LIMIT_EXCEEDED");
    // Terminal: no further decisions are recorded.
    assert!(w
        .calls()
        .record_authorization(
            &w.cmd,
            call.id,
            NewAuthorization {
                reason: AuthorizationReason::PolicySatisfied,
                policy_version: PolicyVersion::parse("v2").unwrap(),
                policy_snapshot: SafeAttributes::new(),
                limits_snapshot: SafeAttributes::new(),
            },
        )
        .await
        .is_err());
}

#[tokio::test]
async fn decisions_persist_safe_evidence_and_exactly_one_is_effective() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await.call;
    let first = w.approve(call.id).await;
    let revoked = w
        .authorize_decision(call.id, AuthorizationReason::Revoked)
        .await;
    let all = w.calls().authorizations(w.scope(), call.id).await.unwrap();
    assert_eq!(all.len(), 2);
    let (a, b) = (&all[0], &all[1]);
    assert_eq!(a.id, first.id);
    assert_eq!(a.superseded_by, Some(revoked.id));
    assert!(a.superseded_at.is_some() && !a.is_effective());
    assert!(b.is_effective());
    for decision in &all {
        assert_eq!(decision.actor_kind, ActorKind::CustomerApplication);
        assert_eq!(
            decision.evidence_service_account_id,
            w.ctx.service_account_id()
        );
        assert_eq!(decision.evidence_credential_id, Some(w.ctx.credential_id()));
        assert_eq!(decision.evidence_principal_id, w.initiator.id());
        assert_eq!(decision.request_id, w.ctx.request_id().as_str());
        assert_eq!(decision.correlation_id, w.ctx.correlation_id().as_str());
        assert_eq!(decision.policy_version, "policy-v1");
    }
    // Recorded as call events (decision, never the snapshots' raw content).
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM call_events WHERE call_id = '{}' AND event_type = 'CALL_AUTHORIZATION_RECORDED'",
            call.id
        ))
        .await,
        2
    );
    // Database: one effective decision per call, decisions immutable, never deleted.
    let err = sql(
        &w.h,
        &format!("UPDATE call_authorizations SET superseded_at = NULL, superseded_by_authorization_id = NULL WHERE id = '{}'", first.id),
    )
    .await
    .unwrap_err();
    assert!(err.contains("cannot be reassigned"), "{err}");
    let err = sql(
        &w.h,
        &format!(
            "UPDATE call_authorizations SET decision = 'APPROVED' WHERE id = '{}'",
            revoked.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("immutable"), "{err}");
    assert!(sql(&w.h, "DELETE FROM call_authorizations").await.is_err());
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_authorizations SELECT gen_random_uuid(), call_id, tenant_id, application_id, environment_id, \
             decision, reason_code, policy_version, policy_snapshot, limits_snapshot, actor_kind, evidence_service_account_id, \
             evidence_credential_id, evidence_service, evidence_principal_id, request_id, correlation_id, decided_at, \
             NULL, NULL, created_at FROM call_authorizations WHERE id = '{}'",
            revoked.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("uq_call_authorizations_one_effective"),
        "{err}"
    );
    // An APPROVED decision with a denial reason is impossible.
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_authorizations SELECT gen_random_uuid(), call_id, tenant_id, application_id, environment_id, \
             'APPROVED', 'POLICY_DENIED', policy_version, policy_snapshot, limits_snapshot, actor_kind, evidence_service_account_id, \
             evidence_credential_id, evidence_service, evidence_principal_id, request_id, correlation_id, decided_at, \
             now(), id, created_at FROM call_authorizations WHERE id = '{}'",
            revoked.id
        ),
    )
    .await
    .unwrap_err();
    assert!(
        err.contains("ck_call_authorizations_reason_matches_decision"),
        "{err}"
    );
}

#[tokio::test]
async fn internal_service_decisions_carry_the_authenticated_service_as_actor() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await.call;
    let delegated = delegated_context(&w.ctx, Some(&w.initiator)).unwrap();
    let signed =
        w.h.peers
            .privacy_relay
            .sign(
                &OutboundServiceRequest::new_operation(ServiceId::calling())
                    .with_delegated_context(delegated),
            )
            .unwrap();
    let internal =
        w.h.state
            .service_auth
            .verifier()
            .verify_headers(&signed.headers())
            .unwrap();
    let cmd = CallCommandContext::internal(&internal).unwrap();
    assert_eq!(cmd.scope(), w.scope());
    let decision = w
        .calls()
        .record_authorization(
            &cmd,
            call.id,
            NewAuthorization {
                reason: AuthorizationReason::PolicySatisfied,
                policy_version: PolicyVersion::parse("relay-policy-3").unwrap(),
                policy_snapshot: SafeAttributes::new(),
                limits_snapshot: SafeAttributes::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(decision.actor_kind, ActorKind::InternalService);
    assert_eq!(
        decision.evidence_service.as_deref(),
        Some("cfmeet.privacy-relay")
    );
    assert_eq!(decision.evidence_credential_id, None);
    assert_eq!(
        decision.evidence_service_account_id,
        w.ctx.service_account_id()
    );
    assert_eq!(decision.request_id, internal.request_id().as_str());
    // Without a delegated context there is no scope to act in.
    let bare =
        w.h.peers
            .privacy_relay
            .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
            .unwrap();
    let bare =
        w.h.state
            .service_auth
            .verifier()
            .verify_headers(&bare.headers())
            .unwrap();
    assert!(CallCommandContext::internal(&bare).is_err());
}

#[tokio::test]
async fn the_paid_public_network_guard_requires_authorization_and_live_telecom() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let guard = |call| {
        w.calls()
            .require_public_network_authorization(w.scope(), call)
    };
    let call = w.create().await.call;
    let events_before = w.event_count(call.id).await;
    // Before any approval.
    assert!(matches!(
        guard(call.id).await.unwrap_err(),
        CallError::AuthorizationRequired
    ));
    // Approved but the Call never entered AUTHORIZED.
    let approved = w.approve(call.id).await;
    assert!(matches!(
        guard(call.id).await.unwrap_err(),
        CallError::AuthorizationRequired
    ));
    w.act(
        call.id,
        CallAction::Authorize {
            authorization_id: approved.id,
        },
    )
    .await
    .unwrap();
    // Authorized, but this PRODUCTION environment is not allowed live telecom yet.
    assert!(matches!(
        guard(call.id).await.unwrap_err(),
        CallError::LiveTelecomNotAllowed
    ));
    w.h.platform
        .set_live_telecom_allowed(w.consumer.env(EnvironmentKind::Production).id, true)
        .await
        .unwrap();
    let ok = guard(call.id).await.unwrap();
    assert_eq!(ok.call_id(), call.id);
    assert_eq!(ok.authorization_id(), approved.id);
    assert_eq!(ok.call_version(), 2);
    // A later denial (revocation) closes the guard.
    w.authorize_decision(call.id, AuthorizationReason::Revoked)
        .await;
    assert!(matches!(
        guard(call.id).await.unwrap_err(),
        CallError::AuthorizationDenied
    ));
    // The guard itself records nothing and executes nothing.
    assert_eq!(w.event_count(call.id).await, events_before + 3);

    // ENDING and terminal Calls are not executable.
    let ending = w.call_in(CallState::Ending).await;
    assert!(matches!(
        guard(ending.id).await.unwrap_err(),
        CallError::InvalidTransition { .. }
    ));
    let ended = w.call_in(CallState::Ended).await;
    assert!(matches!(
        guard(ended.id).await.unwrap_err(),
        CallError::Terminal { .. }
    ));
    // Connected (authorized, approved, live) passes.
    assert!(guard(w.call_in(CallState::Connected).await.id)
        .await
        .is_ok());
    // Another scope never sees the Call.
    let foreign = CallScope::of_platform(
        &CallWorld::in_harness(
            Harness::on(w.h.db.clone(), w.h.clock.clone()),
            "other",
            EnvironmentKind::Production,
        )
        .await
        .ctx,
    );
    assert!(matches!(
        w.calls()
            .require_public_network_authorization(foreign, call.id)
            .await
            .unwrap_err(),
        CallError::NotFound
    ));
}

#[tokio::test]
async fn sandbox_calls_never_pass_the_paid_public_network_guard() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let w = CallWorld::in_harness(h, "sbx", EnvironmentKind::Sandbox).await;
    let call = w.call_in(CallState::Connected).await;
    assert!(matches!(
        w.calls()
            .require_public_network_authorization(w.scope(), call.id)
            .await
            .unwrap_err(),
        CallError::LiveTelecomNotAllowed
    ));
    // And sandbox can never be enabled.
    assert!(w
        .h
        .platform
        .set_live_telecom_allowed(w.consumer.env(EnvironmentKind::Sandbox).id, true)
        .await
        .is_err());
}

//! The media-provider contract (Part C §36–§38), client-independent part: every
//! `CallMediaProvider` adapter — the test provider and the real LiveKit adapter — must
//! satisfy it. Client-driven behaviour (join, publish, eviction) is certified separately
//! against the real provider (`tests/rtc_certification.rs`).

use calling::{
    calls::ids::CallId,
    media::{
        provider::{
            CallMediaProvider, EventVerificationError, MediaCredentialRequest, MediaParticipantRef,
            MediaResourceName, MediaSessionRef, MediaSessionSpec, PermissionOutcome,
            RemovalOutcome, TerminationOutcome,
        },
        MediaPermissions,
    },
};
use chrono::{Duration, Utc};

/// Runs the contract against `provider`.
pub async fn run(provider: &dyn CallMediaProvider) {
    // Capabilities: application calls need every enforcement capability.
    let caps = provider.capabilities();
    assert!(
        caps.supports_app_calls(),
        "{}: {caps:?}",
        provider.adapter_name()
    );
    assert!(!provider.adapter_name().is_empty());

    // Deterministic resource name, never the Call id itself.
    let call = CallId::from_uuid(uuid::Uuid::new_v4());
    let name = MediaResourceName::for_call(call);
    assert_eq!(name.as_str(), format!("cfc-{}", call.as_uuid().simple()));
    assert_ne!(name.as_str(), call.to_string());
    assert_eq!(MediaResourceName::call_id(name.as_str()), Some(call));
    let session = MediaSessionRef { name };
    let spec = MediaSessionSpec {
        max_participants: 4,
        empty_timeout: Duration::minutes(2),
    };

    // A resource that does not exist yet.
    let absent = provider.reconcile(&session).await.unwrap();
    assert!(!absent.exists && absent.participants.is_empty());
    assert!(provider
        .list_participants(&session)
        .await
        .unwrap()
        .is_empty());

    // Ensure is idempotent.
    let first = provider
        .create_or_ensure_session(&session, &spec)
        .await
        .unwrap();
    let again = provider
        .create_or_ensure_session(&session, &spec)
        .await
        .unwrap();
    assert_eq!(first.resource_ref, again.resource_ref);
    let snapshot = provider.reconcile(&session).await.unwrap();
    assert!(snapshot.exists && snapshot.participants.is_empty());

    // Join credentials: bounded lifetime, redacted debug output.
    let identity = format!("cfmid_{}", "c".repeat(64));
    let before = Utc::now();
    let credential = provider
        .issue_credentials(&MediaCredentialRequest {
            session: session.clone(),
            identity: identity.clone(),
            generation: 3,
            ttl: Duration::minutes(5),
        })
        .await
        .unwrap();
    assert!(!credential.token.expose().is_empty());
    assert!(credential.url.starts_with("ws://") || credential.url.starts_with("wss://"));
    assert!(credential.expires_at > before);
    assert!(credential.expires_at <= Utc::now() + Duration::minutes(5) + Duration::seconds(5));
    let debug = format!("{credential:?}");
    assert!(
        !debug.contains(credential.token.expose()),
        "credential debug leaks the token"
    );

    // Operations on an absent connection report it (idempotent enforcement).
    let participant = MediaParticipantRef {
        session: session.clone(),
        identity,
    };
    assert_eq!(
        provider
            .update_permissions(&participant, MediaPermissions::NONE)
            .await
            .unwrap(),
        PermissionOutcome::NotPresent
    );
    assert_eq!(
        provider.remove_session(&participant).await.unwrap(),
        RemovalOutcome::NotPresent
    );

    // Termination is idempotent and final.
    assert_eq!(
        provider.terminate_session(&session).await.unwrap(),
        TerminationOutcome::Terminated
    );
    assert_eq!(
        provider.terminate_session(&session).await.unwrap(),
        TerminationOutcome::AlreadyAbsent
    );
    assert!(!provider.reconcile(&session).await.unwrap().exists);

    // Callbacks: unsigned and forged ones are rejected before parsing.
    let body = br#"{"event":"room_started","id":"EV_contract"}"#;
    assert_eq!(
        provider.verify_event(None, body).unwrap_err(),
        EventVerificationError::MissingSignature
    );
    assert_eq!(
        provider
            .verify_event(Some("eyJhbGciOiJIUzI1NiJ9.e30.forged"), body)
            .unwrap_err(),
        EventVerificationError::InvalidSignature
    );
}

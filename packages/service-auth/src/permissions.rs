//! Internal permission registry.
//!
//! Permissions are least-privilege, `<resource>.<action>` and TARGET-SERVICE
//! AUTHORITATIVE: only the receiving service's trust bundle grants them, never the
//! caller's token. Each permission names the single service that receives (enforces) it
//! and the issuers it may ever be granted to; a trust bundle that grants it elsewhere is
//! refused at startup.
//!
//! Privacy invariant: `protected-contact.resolve-for-execution` (resolving a protected
//! contact reference for execution of an authorized call leg) is received only by Privacy
//! Relay and grantable only to Telephony. Calling can never be granted plaintext-contact
//! resolution.
//!
//! Communication authority: Calling (the authenticated platform ingress) may be granted
//! the communication-context / relay-session proxy permissions and `communication.authorize`
//! by Privacy Relay; Privacy Relay may be granted `communication-authority.enforce` by
//! Calling, which can only ever terminate protected calls, never grant authority.
//!
//! There are no wildcard, `admin`, `all` or `root` permissions.

use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};

use crate::identity::services;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum InternalPermission {
    /// Privacy Relay: authorize a communication.
    #[serde(rename = "communication.authorize")]
    CommunicationAuthorize,
    /// Privacy Relay: read the permitted communication context.
    #[serde(rename = "communication-context.read")]
    CommunicationContextRead,
    /// Privacy Relay: resolve a protected contact reference for execution of a specific,
    /// authorized call leg. Telephony only.
    #[serde(rename = "protected-contact.resolve-for-execution")]
    ProtectedContactResolveForExecution,
    /// Telephony: originate one authorized, paid public-network call leg (Part E).
    #[serde(rename = "telephony-leg.originate")]
    TelephonyLegOriginate,
    /// Telephony: terminate a public-network call leg.
    #[serde(rename = "telephony-leg.terminate")]
    TelephonyLegTerminate,
    /// Telephony: hold a public-network call leg.
    #[serde(rename = "telephony-leg.hold")]
    TelephonyLegHold,
    /// Telephony: resume a held public-network call leg.
    #[serde(rename = "telephony-leg.resume")]
    TelephonyLegResume,
    /// Telephony: create and end bridges between legs / application media.
    #[serde(rename = "telephony-bridge.manage")]
    TelephonyBridgeManage,
    /// Telephony: read normalized leg status and route availability.
    #[serde(rename = "telephony-status.read")]
    TelephonyStatusRead,
    /// Calling: receive tightly scoped operational callbacks.
    #[serde(rename = "operational-callback.deliver")]
    OperationalCallbackDeliver,
    /// Privacy Relay: create, update and change the lifecycle of communication contexts
    /// on behalf of the delegated application (authenticated platform ingress).
    #[serde(rename = "communication-context.write")]
    CommunicationContextWrite,
    /// Privacy Relay: read privacy relay sessions of the delegated application.
    #[serde(rename = "relay-session.read")]
    RelaySessionRead,
    /// Privacy Relay: create privacy relay sessions and change their lifecycle on behalf
    /// of the delegated application.
    #[serde(rename = "relay-session.write")]
    RelaySessionWrite,
    /// Calling: enforce a restrictive communication-authority change (revocation,
    /// participant removal, suspension, expiry) on protected calls. Restrictive only.
    #[serde(rename = "communication-authority.enforce")]
    CommunicationAuthorityEnforce,
    /// Privacy Relay: issue a short-lived Contact Resolution Grant for one specific,
    /// authorized protected call leg (Part D §33). Reveals no contact value.
    #[serde(rename = "protected-contact.grant-for-execution")]
    ProtectedContactGrantForExecution,
    /// Calling: request, read and release the temporary relay-number assignment of a
    /// Privacy Relay Session (Part D §34). Reveals no number and no provider binding.
    #[serde(rename = "relay-number.assign")]
    RelayNumberAssign,
    /// Telephony: resolve the called relay number of an inbound public-network call to
    /// its current safe relay mapping (Part D §35). Telephony only.
    #[serde(rename = "relay-number.resolve-inbound")]
    RelayNumberResolveInbound,
    /// Telephony: protect an inbound caller's presented number in the scope of a
    /// resolved relay mapping and get its ContactReference (Part D §35). Telephony only.
    #[serde(rename = "protected-contact.protect-inbound")]
    ProtectedContactProtectInbound,
    /// Calling: apply a normalized Telephony execution event to the canonical Call (Part E).
    #[serde(rename = "telephony-event.apply")]
    CallsTelephonyEventApply,
    /// Calling: apply a normalized Telephony reconciliation result (Part E §50).
    #[serde(rename = "telephony-reconciliation.apply")]
    CallsTelephonyReconciliationApply,
    /// Calling: offer an authenticated inbound public-network call with its resolved
    /// relay mapping and protected caller reference (Part E §45).
    #[serde(rename = "telephony-inbound.offer")]
    CallsTelephonyInboundOffer,
    /// Telephony: answer an internal enterprise SIP leg, play BUILT-IN prompts on it and
    /// capture its media (Part F §57, §60, §61). Never a public-network leg.
    #[serde(rename = "telephony-leg.media")]
    TelephonyLegMedia,
    /// Calling: offer an internal enterprise SIP dialog (opaque device reference and the
    /// dialled extension) and deliver its keypad input for an IVR (Part F §54, §57).
    #[serde(rename = "telephony-enterprise.offer")]
    CallsTelephonyEnterpriseOffer,
    /// Telephony -> Calling: deliver (or report the failure of) an edge media capture —
    /// the captured audio of a voicemail / recording, with its integrity metadata
    /// (Part F §105–§110).
    #[serde(rename = "telephony-capture.deliver")]
    CallsTelephonyCaptureDeliver,
    /// Privacy Relay -> Calling: publish a committed Privacy Relay domain event (the
    /// platform envelope from its transactional outbox) for normalized customer webhooks
    /// (Part G §67 / §68). Calling projects it; nothing is forwarded verbatim.
    #[serde(rename = "platform-event.publish")]
    PlatformEventPublish,
    /// Platform Access: verify a presented customer API credential and return its trusted
    /// authority chain and effective scopes (Part IV §B3). Never returns a verifier.
    #[serde(rename = "platform-credential.authenticate")]
    PlatformCredentialAuthenticate,
    /// Platform Access: resolve communication principals inside the signed delegated
    /// scope (never a request-supplied tenant).
    #[serde(rename = "platform-principal.resolve")]
    PlatformPrincipalResolve,
    /// Platform Access: register the TELEPHONE_ENDPOINT principal of a protected public
    /// party and tombstone principals on an operator retention decision.
    #[serde(rename = "platform-principal.register")]
    PlatformPrincipalRegister,
    /// Platform Access: read the identity change feed, trusted context, effective scopes,
    /// credential status and identity-import receipts (no verifier material).
    #[serde(rename = "platform-identity.read")]
    PlatformIdentityRead,
    /// Platform Access: administer the identity hierarchy (operator tooling only).
    #[serde(rename = "platform-identity.administer")]
    PlatformIdentityAdminister,
    /// Numbering: read canonical Number Resources and DCN assignments (CF Meet ids,
    /// provider-independent; no provider binding identifiers).
    #[serde(rename = "number-resource.read")]
    NumberResourceRead,
    /// Numbering: acknowledge that a configured number route is ready in the consumer that
    /// carries its traffic (Telephony for voice, the SMS Gateway for SMS) — an activation
    /// prerequisite, never fabricated by Numbering.
    #[serde(rename = "number-route.acknowledge")]
    NumberRouteAcknowledge,
    /// Numbering: resolve a received call / message on a public number to the owner scope
    /// and routing target of its ACTIVE assignment, with an inbound routing grant (the
    /// carrier of the family's traffic only: Telephony voice, SMS Gateway SMS).
    #[serde(rename = "number-route.resolve")]
    NumberRouteResolve,
    /// Numbering: obtain a single-use CALLER_IDENTITY grant for a number the delegated
    /// customer scope actively owns (never an arbitrary caller id).
    #[serde(rename = "number-caller-identity.authorize")]
    NumberCallerIdentityAuthorize,
    /// Numbering: obtain a single-use SMS_SENDER grant for a number the delegated customer
    /// scope actively owns.
    #[serde(rename = "number-sms-sender.authorize")]
    NumberSmsSenderAuthorize,
    /// Numbering: verify (and consume) a routing grant addressed to the caller against
    /// CURRENT state (assignment, routing revision, capability, single use).
    #[serde(rename = "number-grant.verify")]
    NumberGrantVerify,
    /// Numbering (Part IV M): resolve the DESTINATION number of a received SMS to its owner
    /// scope and SMS routing target with an INBOUND_SMS grant — the SMS Gateway only. The
    /// voice resolver (`number-route.resolve`) is never used for SMS.
    #[serde(rename = "number-sms-route.resolve")]
    NumberSmsRouteResolve,
    /// Numbering (Part IV S): report the per-number usage a carrier metered (voice seconds:
    /// Telephony; SMS segments: the SMS Gateway) — internal cost evidence, idempotent.
    #[serde(rename = "number-usage.report")]
    NumberUsageReport,
    /// Numbering (Part IV T): report an abuse / fraud signal about a number (SMS pumping,
    /// spam calls, premium destinations, cost spikes) for number-level risk decisions.
    #[serde(rename = "number-risk.report")]
    NumberRiskReport,
    /// Numbering (Part IV P §§119–120): import the relay-number inventory, lease a relay
    /// number for a relay session and resolve its lease — Privacy Relay only.
    #[serde(rename = "number-relay.lease")]
    NumberRelayLease,
    /// Platform Access (Part V H §104): resolve or idempotently create the NATIVE_CF_USER /
    /// TEMPORARY_EXTERNAL_PARTICIPANT principal of an account the calling first-party
    /// backend authenticated, inside the signed delegated scope (FIRST_PARTY tenants only).
    #[serde(rename = "platform-principal.ensure")]
    PlatformPrincipalEnsure,
    /// Messaging (Part V H / I §116): act as the delegated first-party application backend
    /// on Messaging's standard customer operations (devices, client tokens, conversations,
    /// members, messages) inside the signed delegated scope. Never moderation or export.
    #[serde(rename = "messaging-application.delegate")]
    MessagingApplicationDelegate,
    /// Messaging (Part V H §§97–101): resolve meeting conversations, apply version-fenced
    /// meeting membership / lifecycle changes and import legacy meeting chat.
    #[serde(rename = "messaging-meeting.sync")]
    MessagingMeetingSync,
    /// Messaging (Part V H §103): apply a versioned Privacy Relay communication-authority
    /// update (context status / revocation) to the conversations bound to that context.
    #[serde(rename = "messaging-context-authority.apply")]
    MessagingContextAuthorityApply,
    /// Messaging (Part V H §§109–110): deliver an authenticated inbound SMS for an
    /// authorized bridge and the transport state of outbound bridge requests.
    #[serde(rename = "messaging-sms-bridge.deliver")]
    MessagingSmsBridgeDeliver,
    /// SMS Gateway (Part V H §108): submit one authorized outbound SMS Bridge transport
    /// request for a MANAGED bridge copy (Messaging only; never E2EE ciphertext).
    #[serde(rename = "sms-bridge.submit")]
    SmsBridgeSubmit,
    /// Developer Platform (Part VI §§1–5): receive a customer request the API Gateway
    /// authenticated through Platform Access, with the signed delegated context the
    /// gateway derived from the authority chain. API Gateway only.
    #[serde(rename = "developer-request.forward")]
    DeveloperRequestForward,
    /// Platform Access (Part VI §50): create and change the lifecycle of the canonical
    /// tenant / application / non-production environment behind a developer workspace,
    /// on behalf of an authorized human member. EXTERNAL tenants and STANDARD scopes only;
    /// never credentials, verifiers, privileged scopes or PRODUCTION environments.
    #[serde(rename = "platform-identity.delegate")]
    PlatformIdentityDelegate,
    /// Calling (Part VI §51): receive a customer request (Calling and the Privacy Relay
    /// customer proxy) the API Gateway authenticated through Platform Access.
    #[serde(rename = "calling-request.forward")]
    CallingRequestForward,
    /// Messaging (Part VI §51): receive a customer request the API Gateway authenticated
    /// through Platform Access.
    #[serde(rename = "messaging-request.forward")]
    MessagingRequestForward,
    /// Numbering (Part VI §51): receive a customer `/v1/numbers` request the API Gateway
    /// authenticated through Platform Access (never `/v1/admin`).
    #[serde(rename = "numbering-request.forward")]
    NumberingRequestForward,
    /// SMS Gateway (Part VI §51): receive a customer `/v1/sms` request the API Gateway
    /// authenticated through Platform Access.
    #[serde(rename = "sms-request.forward")]
    SmsRequestForward,
    /// Meeting Service (Part VI §50): verify a presented CF Meet human session (the
    /// session-to-organization authorization bridge of the Developer Console).
    #[serde(rename = "user-session.verify")]
    UserSessionVerify,
    /// Developer Platform (Part VI §51): read the effective platform entitlements (API
    /// request quotas) of an application environment.
    #[serde(rename = "developer-entitlement.read")]
    DeveloperEntitlementRead,
    /// Developer Platform (Part VI §51): record durable daily API request accounting.
    #[serde(rename = "developer-usage.record")]
    DeveloperUsageRecord,
    /// Developer Platform (Part VI §53): publish a committed domain event (platform
    /// envelope, the publisher's own events only) for central customer webhooks.
    #[serde(rename = "developer-event.publish")]
    DeveloperEventPublish,
    /// Calling (Part VI §53): export Calling-owned webhook records for the controlled
    /// migration to the Developer Platform (signing secrets only re-sealed under the
    /// migration transfer key; never plaintext).
    #[serde(rename = "webhook-migration.export")]
    WebhookMigrationExport,
    /// Developer Platform (Part VI §50): operator administration of the control plane
    /// (organization restriction / suspension, plan versions and assignments, explicit
    /// first-party registration). Operator tooling only.
    #[serde(rename = "developer-platform.administer")]
    DeveloperPlatformAdminister,
}

/// Registry entry: which service receives the permission and who may be granted it.
#[derive(Debug, Clone, Copy)]
pub struct PermissionDefinition {
    pub permission: InternalPermission,
    pub name: &'static str,
    pub receiver: &'static str,
    pub grantable_to: &'static [&'static str],
    /// Grants access to protected (privacy-sensitive) data.
    pub sensitive: bool,
    pub description: &'static str,
}

pub const REGISTRY: &[PermissionDefinition] = &[
    PermissionDefinition {
        permission: InternalPermission::CommunicationAuthorize,
        name: "communication.authorize",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Authorize a communication within a communication context",
    },
    PermissionDefinition {
        permission: InternalPermission::CommunicationContextRead,
        name: "communication-context.read",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING, services::TELEPHONY, services::MESSAGING],
        sensitive: false,
        description: "Read the permitted communication context (opaque references only)",
    },
    PermissionDefinition {
        permission: InternalPermission::ProtectedContactResolveForExecution,
        name: "protected-contact.resolve-for-execution",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::TELEPHONY],
        sensitive: true,
        description:
            "Resolve a protected contact reference for execution of a specific authorized call leg",
    },
    PermissionDefinition {
        permission: InternalPermission::TelephonyLegOriginate,
        name: "telephony-leg.originate",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Originate one authorized, paid public-network call leg (with authorization, grant and spend proof)",
    },
    PermissionDefinition {
        permission: InternalPermission::TelephonyLegTerminate,
        name: "telephony-leg.terminate",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Terminate a public-network call leg",
    },
    PermissionDefinition {
        permission: InternalPermission::TelephonyLegHold,
        name: "telephony-leg.hold",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Hold a public-network call leg",
    },
    PermissionDefinition {
        permission: InternalPermission::TelephonyLegResume,
        name: "telephony-leg.resume",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Resume a held public-network call leg",
    },
    PermissionDefinition {
        permission: InternalPermission::TelephonyBridgeManage,
        name: "telephony-bridge.manage",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Create and end bridges between public legs and application media",
    },
    PermissionDefinition {
        permission: InternalPermission::TelephonyStatusRead,
        name: "telephony-status.read",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Read normalized leg status and route availability (no provider data)",
    },
    PermissionDefinition {
        permission: InternalPermission::OperationalCallbackDeliver,
        name: "operational-callback.deliver",
        receiver: services::CALLING,
        grantable_to: &[services::PRIVACY_RELAY, services::TELEPHONY],
        sensitive: false,
        description: "Deliver tightly scoped operational callbacks to Calling",
    },
    PermissionDefinition {
        permission: InternalPermission::CommunicationContextWrite,
        name: "communication-context.write",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Create, update and change the lifecycle of communication contexts for the delegated application",
    },
    PermissionDefinition {
        permission: InternalPermission::RelaySessionRead,
        name: "relay-session.read",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Read privacy relay sessions of the delegated application (opaque references only)",
    },
    PermissionDefinition {
        permission: InternalPermission::RelaySessionWrite,
        name: "relay-session.write",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Create privacy relay sessions and change their lifecycle for the delegated application",
    },
    PermissionDefinition {
        permission: InternalPermission::CommunicationAuthorityEnforce,
        name: "communication-authority.enforce",
        receiver: services::CALLING,
        grantable_to: &[services::PRIVACY_RELAY],
        sensitive: false,
        description: "Enforce a restrictive communication-authority change on protected calls (terminate only)",
    },
    PermissionDefinition {
        permission: InternalPermission::ProtectedContactGrantForExecution,
        name: "protected-contact.grant-for-execution",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Issue a short-lived Contact Resolution Grant for one authorized protected call leg (no contact value)",
    },
    PermissionDefinition {
        permission: InternalPermission::RelayNumberAssign,
        name: "relay-number.assign",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Request, read and release the temporary relay-number assignment of a Privacy Relay Session (no number, no provider binding)",
    },
    PermissionDefinition {
        permission: InternalPermission::RelayNumberResolveInbound,
        name: "relay-number.resolve-inbound",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::TELEPHONY],
        sensitive: true,
        description: "Resolve the called relay number of an inbound call to its current safe relay mapping (context / session / assignment references only)",
    },
    PermissionDefinition {
        permission: InternalPermission::ProtectedContactProtectInbound,
        name: "protected-contact.protect-inbound",
        receiver: services::PRIVACY_RELAY,
        grantable_to: &[services::TELEPHONY],
        sensitive: true,
        description: "Protect an inbound caller's presented number in the scope of a resolved relay mapping (returns a ContactReference only)",
    },
    PermissionDefinition {
        permission: InternalPermission::CallsTelephonyEventApply,
        name: "telephony-event.apply",
        receiver: services::CALLING,
        grantable_to: &[services::TELEPHONY],
        sensitive: false,
        description: "Apply a normalized Telephony execution event to the canonical Call (Calling validates it)",
    },
    PermissionDefinition {
        permission: InternalPermission::CallsTelephonyReconciliationApply,
        name: "telephony-reconciliation.apply",
        receiver: services::CALLING,
        grantable_to: &[services::TELEPHONY],
        sensitive: false,
        description: "Apply a normalized Telephony reconciliation result (Calling validates it)",
    },
    PermissionDefinition {
        permission: InternalPermission::CallsTelephonyInboundOffer,
        name: "telephony-inbound.offer",
        receiver: services::CALLING,
        grantable_to: &[services::TELEPHONY],
        sensitive: false,
        description: "Offer an authenticated inbound public-network call (resolved relay mapping and protected caller reference only)",
    },
    PermissionDefinition {
        permission: InternalPermission::TelephonyLegMedia,
        name: "telephony-leg.media",
        receiver: services::TELEPHONY,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Answer an internal enterprise SIP leg, play built-in prompts on it and capture its media",
    },
    PermissionDefinition {
        permission: InternalPermission::CallsTelephonyEnterpriseOffer,
        name: "telephony-enterprise.offer",
        receiver: services::CALLING,
        grantable_to: &[services::TELEPHONY],
        sensitive: false,
        description: "Offer an internal enterprise SIP dialog (opaque device reference, dialled extension) and deliver its keypad input",
    },
    PermissionDefinition {
        permission: InternalPermission::CallsTelephonyCaptureDeliver,
        name: "telephony-capture.deliver",
        receiver: services::CALLING,
        grantable_to: &[services::TELEPHONY],
        sensitive: true,
        description: "Deliver an edge media capture (voicemail / recording audio with integrity metadata) or report its failure",
    },
    PermissionDefinition {
        permission: InternalPermission::PlatformEventPublish,
        name: "platform-event.publish",
        receiver: services::CALLING,
        grantable_to: &[services::PRIVACY_RELAY, services::SMS_GATEWAY, services::MESSAGING],
        sensitive: false,
        description: "Publish a committed Privacy Relay, SMS Gateway or Messaging domain event (platform envelope, the publisher's own events only) for normalized customer webhooks",
    },
    PermissionDefinition {
        permission: InternalPermission::PlatformCredentialAuthenticate,
        name: "platform-credential.authenticate",
        receiver: services::PLATFORM_ACCESS,
        grantable_to: &[services::CALLING, services::NUMBERING, services::SMS_GATEWAY, services::MESSAGING, services::API_GATEWAY],
        sensitive: false,
        description: "Verify a presented customer API credential; returns the trusted authority chain and effective scopes (never verifier material)",
    },
    PermissionDefinition {
        permission: InternalPermission::PlatformPrincipalResolve,
        name: "platform-principal.resolve",
        receiver: services::PLATFORM_ACCESS,
        grantable_to: &[services::CALLING, services::NUMBERING, services::SMS_GATEWAY, services::MESSAGING],
        sensitive: false,
        description: "Resolve communication principals inside the signed delegated scope",
    },
    PermissionDefinition {
        permission: InternalPermission::PlatformPrincipalRegister,
        name: "platform-principal.register",
        receiver: services::PLATFORM_ACCESS,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Register the TELEPHONE_ENDPOINT principal of a protected public party (opaque ContactReference only) and tombstone principals on an operator retention decision",
    },
    PermissionDefinition {
        permission: InternalPermission::PlatformIdentityRead,
        name: "platform-identity.read",
        receiver: services::PLATFORM_ACCESS,
        grantable_to: &[services::CALLING, services::NUMBERING, services::MESSAGING, services::DEVELOPER_PLATFORM],
        sensitive: false,
        description: "Read the identity change feed, trusted tenant / application / environment context, effective scopes, credential status and import receipts (no verifier material)",
    },
    PermissionDefinition {
        permission: InternalPermission::PlatformIdentityAdminister,
        name: "platform-identity.administer",
        receiver: services::PLATFORM_ACCESS,
        grantable_to: &[services::OPERATOR_CONSOLE],
        sensitive: true,
        description: "Administer the platform identity hierarchy (tenants, applications, environments, service accounts, credentials, scopes, principals); operator tooling only",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberResourceRead,
        name: "number-resource.read",
        receiver: services::NUMBERING,
        grantable_to: &[services::CALLING, services::TELEPHONY, services::SMS_GATEWAY],
        sensitive: false,
        description: "Read canonical Number Resources and DCN assignments (CF Meet ids only, no provider binding identifiers)",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberRouteAcknowledge,
        name: "number-route.acknowledge",
        receiver: services::NUMBERING,
        grantable_to: &[services::TELEPHONY, services::SMS_GATEWAY],
        sensitive: false,
        description: "Acknowledge that a configured number route (voice: Telephony, SMS: SMS Gateway) is ready to carry traffic; an activation prerequisite",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberRouteResolve,
        name: "number-route.resolve",
        receiver: services::NUMBERING,
        grantable_to: &[services::TELEPHONY],
        sensitive: true,
        description: "Resolve a received call on a public number to its owner scope and voice routing target, with a short-lived INBOUND_VOICE grant (Telephony only; SMS uses number-sms-route.resolve)",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberCallerIdentityAuthorize,
        name: "number-caller-identity.authorize",
        receiver: services::NUMBERING,
        grantable_to: &[services::CALLING],
        sensitive: false,
        description: "Obtain a single-use caller-identity grant (audience Telephony) for a number the delegated customer scope actively owns",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberSmsSenderAuthorize,
        name: "number-sms-sender.authorize",
        receiver: services::NUMBERING,
        grantable_to: &[services::SMS_GATEWAY],
        sensitive: false,
        description: "Obtain a single-use SMS sender grant for a number the delegated customer scope actively owns",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberGrantVerify,
        name: "number-grant.verify",
        receiver: services::NUMBERING,
        grantable_to: &[services::CALLING, services::TELEPHONY, services::SMS_GATEWAY],
        sensitive: false,
        description: "Verify (and consume) a number routing grant addressed to the caller against current state",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberSmsRouteResolve,
        name: "number-sms-route.resolve",
        receiver: services::NUMBERING,
        grantable_to: &[services::SMS_GATEWAY],
        sensitive: true,
        description: "Resolve the destination number of a received SMS to its owner scope and SMS routing target, with a short-lived INBOUND_SMS grant (the SMS Gateway only)",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberUsageReport,
        name: "number-usage.report",
        receiver: services::NUMBERING,
        grantable_to: &[services::TELEPHONY, services::SMS_GATEWAY],
        sensitive: false,
        description: "Report the usage a carrier metered for a number (voice seconds / SMS segments) as idempotent internal cost evidence",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberRiskReport,
        name: "number-risk.report",
        receiver: services::NUMBERING,
        grantable_to: &[services::CALLING, services::TELEPHONY, services::SMS_GATEWAY],
        sensitive: false,
        description: "Report an abuse, fraud or cost signal about a number for number-level risk decisions (restrictive only)",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberRelayLease,
        name: "number-relay.lease",
        receiver: services::NUMBERING,
        grantable_to: &[services::PRIVACY_RELAY],
        sensitive: true,
        description: "Import Privacy Relay's relay-number inventory into Numbering, lease a relay number for a relay session and resolve its lease (Privacy Relay only)",
    },
    PermissionDefinition {
        permission: InternalPermission::PlatformPrincipalEnsure,
        name: "platform-principal.ensure",
        receiver: services::PLATFORM_ACCESS,
        grantable_to: &[services::MEETING_SERVICE],
        sensitive: false,
        description: "Resolve or create the native-account / meeting-participant principal of a first-party backend's authenticated user in its delegated scope",
    },
    PermissionDefinition {
        permission: InternalPermission::MessagingApplicationDelegate,
        name: "messaging-application.delegate",
        receiver: services::MESSAGING,
        grantable_to: &[services::MEETING_SERVICE],
        sensitive: false,
        description: "Act as the delegated first-party application backend on standard Messaging operations (no moderation, no export)",
    },
    PermissionDefinition {
        permission: InternalPermission::MessagingMeetingSync,
        name: "messaging-meeting.sync",
        receiver: services::MESSAGING,
        grantable_to: &[services::MEETING_SERVICE],
        sensitive: false,
        description: "Resolve meeting conversations, apply version-fenced meeting membership / lifecycle and import legacy meeting chat",
    },
    PermissionDefinition {
        permission: InternalPermission::MessagingContextAuthorityApply,
        name: "messaging-context-authority.apply",
        receiver: services::MESSAGING,
        grantable_to: &[services::PRIVACY_RELAY],
        sensitive: false,
        description: "Apply a versioned communication-authority update to the conversations bound to a communication context",
    },
    PermissionDefinition {
        permission: InternalPermission::MessagingSmsBridgeDeliver,
        name: "messaging-sms-bridge.deliver",
        receiver: services::MESSAGING,
        grantable_to: &[services::SMS_GATEWAY],
        sensitive: false,
        description: "Deliver an authenticated inbound SMS for an authorized bridge and bridge transport state",
    },
    PermissionDefinition {
        permission: InternalPermission::SmsBridgeSubmit,
        name: "sms-bridge.submit",
        receiver: services::SMS_GATEWAY,
        grantable_to: &[services::MESSAGING],
        sensitive: false,
        description: "Submit one authorized outbound SMS Bridge transport request (MANAGED copies only)",
    },
    PermissionDefinition {
        permission: InternalPermission::DeveloperRequestForward,
        name: "developer-request.forward",
        receiver: services::DEVELOPER_PLATFORM,
        grantable_to: &[services::API_GATEWAY],
        sensitive: false,
        description: "Forward a customer request the API Gateway authenticated through Platform Access (signed delegated context derived from the authority chain only)",
    },
    PermissionDefinition {
        permission: InternalPermission::PlatformIdentityDelegate,
        name: "platform-identity.delegate",
        receiver: services::PLATFORM_ACCESS,
        grantable_to: &[services::DEVELOPER_PLATFORM],
        sensitive: false,
        description: "Create and change the lifecycle of the canonical tenant / application / non-production environment of a developer workspace for an authorized human member (EXTERNAL tenants, STANDARD scopes; no credentials, verifiers or production)",
    },
    PermissionDefinition {
        permission: InternalPermission::CallingRequestForward,
        name: "calling-request.forward",
        receiver: services::CALLING,
        grantable_to: &[services::API_GATEWAY],
        sensitive: false,
        description: "Receive a customer request the API Gateway authenticated through Platform Access (credential re-verified with Platform Access)",
    },
    PermissionDefinition {
        permission: InternalPermission::MessagingRequestForward,
        name: "messaging-request.forward",
        receiver: services::MESSAGING,
        grantable_to: &[services::API_GATEWAY],
        sensitive: false,
        description: "Receive a customer request the API Gateway authenticated through Platform Access (credential re-verified with Platform Access)",
    },
    PermissionDefinition {
        permission: InternalPermission::NumberingRequestForward,
        name: "numbering-request.forward",
        receiver: services::NUMBERING,
        grantable_to: &[services::API_GATEWAY],
        sensitive: false,
        description: "Receive a customer /v1/numbers request the API Gateway authenticated through Platform Access (never /v1/admin)",
    },
    PermissionDefinition {
        permission: InternalPermission::SmsRequestForward,
        name: "sms-request.forward",
        receiver: services::SMS_GATEWAY,
        grantable_to: &[services::API_GATEWAY],
        sensitive: false,
        description: "Receive a customer /v1/sms request the API Gateway authenticated through Platform Access (credential re-verified with Platform Access)",
    },
    PermissionDefinition {
        permission: InternalPermission::UserSessionVerify,
        name: "user-session.verify",
        receiver: services::MEETING_SERVICE,
        grantable_to: &[services::DEVELOPER_PLATFORM],
        sensitive: false,
        description: "Verify a presented CF Meet human session (revocation and account status checked) and return the user id and verified email",
    },
    PermissionDefinition {
        permission: InternalPermission::DeveloperEntitlementRead,
        name: "developer-entitlement.read",
        receiver: services::DEVELOPER_PLATFORM,
        grantable_to: &[services::API_GATEWAY],
        sensitive: false,
        description: "Read the effective platform entitlements (API request quotas) of an application environment",
    },
    PermissionDefinition {
        permission: InternalPermission::DeveloperUsageRecord,
        name: "developer-usage.record",
        receiver: services::DEVELOPER_PLATFORM,
        grantable_to: &[services::API_GATEWAY],
        sensitive: false,
        description: "Record durable daily API request accounting of application environments",
    },
    PermissionDefinition {
        permission: InternalPermission::DeveloperEventPublish,
        name: "developer-event.publish",
        receiver: services::DEVELOPER_PLATFORM,
        grantable_to: &[
            services::MEETING_SERVICE,
            services::CALLING,
            services::PRIVACY_RELAY,
            services::MESSAGING,
            services::NUMBERING,
            services::SMS_GATEWAY,
        ],
        sensitive: false,
        description: "Publish a committed domain event (platform envelope, the publisher's own events only) for central customer webhooks",
    },
    PermissionDefinition {
        permission: InternalPermission::WebhookMigrationExport,
        name: "webhook-migration.export",
        receiver: services::CALLING,
        grantable_to: &[services::DEVELOPER_PLATFORM],
        sensitive: false,
        description: "Export Calling-owned webhook subscriptions, events, deliveries and attempts for the controlled migration (secrets re-sealed under the migration transfer key only)",
    },
    PermissionDefinition {
        permission: InternalPermission::DeveloperPlatformAdminister,
        name: "developer-platform.administer",
        receiver: services::DEVELOPER_PLATFORM,
        grantable_to: &[services::OPERATOR_CONSOLE],
        sensitive: true,
        description: "Administer the developer control plane (organization restriction / suspension, plan versions and assignments, first-party registration); operator tooling only",
    },
];

/// Values that must never be used as an ordinary permission.
const WILDCARD_LIKE: &[&str] = &["*", "admin", "all", "root", "any", "superuser"];

impl InternalPermission {
    pub const ALL: &'static [InternalPermission] = &[
        InternalPermission::CommunicationAuthorize,
        InternalPermission::CommunicationContextRead,
        InternalPermission::ProtectedContactResolveForExecution,
        InternalPermission::TelephonyLegOriginate,
        InternalPermission::TelephonyLegTerminate,
        InternalPermission::TelephonyLegHold,
        InternalPermission::TelephonyLegResume,
        InternalPermission::TelephonyBridgeManage,
        InternalPermission::TelephonyStatusRead,
        InternalPermission::OperationalCallbackDeliver,
        InternalPermission::CommunicationContextWrite,
        InternalPermission::RelaySessionRead,
        InternalPermission::RelaySessionWrite,
        InternalPermission::CommunicationAuthorityEnforce,
        InternalPermission::ProtectedContactGrantForExecution,
        InternalPermission::RelayNumberAssign,
        InternalPermission::RelayNumberResolveInbound,
        InternalPermission::ProtectedContactProtectInbound,
        InternalPermission::CallsTelephonyEventApply,
        InternalPermission::CallsTelephonyReconciliationApply,
        InternalPermission::CallsTelephonyInboundOffer,
        InternalPermission::TelephonyLegMedia,
        InternalPermission::CallsTelephonyEnterpriseOffer,
        InternalPermission::CallsTelephonyCaptureDeliver,
        InternalPermission::PlatformEventPublish,
        InternalPermission::PlatformCredentialAuthenticate,
        InternalPermission::PlatformPrincipalResolve,
        InternalPermission::PlatformPrincipalRegister,
        InternalPermission::PlatformIdentityRead,
        InternalPermission::PlatformIdentityAdminister,
        InternalPermission::NumberResourceRead,
        InternalPermission::NumberRouteAcknowledge,
        InternalPermission::NumberRouteResolve,
        InternalPermission::NumberCallerIdentityAuthorize,
        InternalPermission::NumberSmsSenderAuthorize,
        InternalPermission::NumberGrantVerify,
        InternalPermission::NumberSmsRouteResolve,
        InternalPermission::NumberUsageReport,
        InternalPermission::NumberRiskReport,
        InternalPermission::NumberRelayLease,
        InternalPermission::PlatformPrincipalEnsure,
        InternalPermission::MessagingApplicationDelegate,
        InternalPermission::MessagingMeetingSync,
        InternalPermission::MessagingContextAuthorityApply,
        InternalPermission::MessagingSmsBridgeDeliver,
        InternalPermission::SmsBridgeSubmit,
        InternalPermission::DeveloperRequestForward,
        InternalPermission::PlatformIdentityDelegate,
        InternalPermission::CallingRequestForward,
        InternalPermission::MessagingRequestForward,
        InternalPermission::NumberingRequestForward,
        InternalPermission::SmsRequestForward,
        InternalPermission::UserSessionVerify,
        InternalPermission::DeveloperEntitlementRead,
        InternalPermission::DeveloperUsageRecord,
        InternalPermission::DeveloperEventPublish,
        InternalPermission::WebhookMigrationExport,
        InternalPermission::DeveloperPlatformAdminister,
    ];

    pub fn definition(&self) -> &'static PermissionDefinition {
        REGISTRY
            .iter()
            .find(|d| d.permission == *self)
            .expect("every permission has a registry entry")
    }

    pub fn as_str(&self) -> &'static str {
        self.definition().name
    }

    pub fn parse(value: &str) -> Option<Self> {
        REGISTRY
            .iter()
            .find(|d| d.name == value)
            .map(|d| d.permission)
    }

    /// Whether `value` looks like a wildcard / super-permission.
    pub fn is_wildcard_like(value: &str) -> bool {
        let lower = value.trim().to_ascii_lowercase();
        value.contains('*')
            || WILDCARD_LIKE.contains(&lower.as_str())
            || lower
                .split(['.', ':', '-'])
                .any(|part| WILDCARD_LIKE.contains(&part))
    }
}

impl fmt::Display for InternalPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Permissions granted to an authenticated caller by the receiver's policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionSet(BTreeSet<InternalPermission>);

impl PermissionSet {
    pub(crate) fn new(permissions: BTreeSet<InternalPermission>) -> Self {
        Self(permissions)
    }

    pub fn contains(&self, permission: InternalPermission) -> bool {
        self.0.contains(&permission)
    }

    pub fn iter(&self) -> impl Iterator<Item = &InternalPermission> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.0.iter().map(InternalPermission::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_consistent() {
        assert_eq!(REGISTRY.len(), InternalPermission::ALL.len());
        for p in InternalPermission::ALL {
            let d = p.definition();
            assert_eq!(InternalPermission::parse(d.name), Some(*p));
            assert_eq!(serde_json::to_value(p).unwrap(), d.name);
            assert!(
                !d.grantable_to.contains(&d.receiver),
                "{} grantable to its own receiver",
                d.name
            );
            assert!(!InternalPermission::is_wildcard_like(d.name));
            let (resource, action) = d.name.split_once('.').unwrap();
            for part in [resource, action] {
                assert!(part
                    .split('-')
                    .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase())));
            }
        }
    }

    #[test]
    fn protected_contact_resolution_is_telephony_only() {
        let d = InternalPermission::ProtectedContactResolveForExecution.definition();
        assert_eq!(d.receiver, services::PRIVACY_RELAY);
        assert_eq!(d.grantable_to, &[services::TELEPHONY]);
        assert!(d.sensitive);
        // No sensitive permission is ever grantable to Calling.
        for d in REGISTRY.iter().filter(|d| d.sensitive) {
            assert!(!d.grantable_to.contains(&services::CALLING), "{}", d.name);
        }
    }

    #[test]
    fn communication_authority_flows_are_least_privilege() {
        use InternalPermission::*;
        // Calling -> Privacy Relay: the platform ingress proxy and call authorization.
        for p in [
            CommunicationAuthorize,
            CommunicationContextWrite,
            RelaySessionRead,
            RelaySessionWrite,
        ] {
            let d = p.definition();
            assert_eq!(d.receiver, services::PRIVACY_RELAY, "{}", d.name);
            assert_eq!(d.grantable_to, &[services::CALLING], "{}", d.name);
        }
        // Inbound relay resolution and inbound caller protection: Telephony only.
        for p in [RelayNumberResolveInbound, ProtectedContactProtectInbound] {
            let d = p.definition();
            assert_eq!(
                (d.receiver, d.grantable_to, d.sensitive),
                (services::PRIVACY_RELAY, &[services::TELEPHONY][..], true)
            );
        }
        // Calling may REQUEST a Contact Resolution Grant but never resolve plaintext.
        let d = ProtectedContactGrantForExecution.definition();
        assert_eq!(
            (d.receiver, d.grantable_to),
            (services::PRIVACY_RELAY, &[services::CALLING][..])
        );
        assert!(!d.sensitive);
        // Privacy Relay -> Calling: restrictive enforcement only.
        let d = CommunicationAuthorityEnforce.definition();
        assert_eq!(d.receiver, services::CALLING);
        assert_eq!(d.grantable_to, &[services::PRIVACY_RELAY]);
        assert!(!d.sensitive);
    }

    #[test]
    fn telephony_flows_are_least_privilege() {
        use InternalPermission::*;
        // Calling -> Telephony: commands and status, Calling only.
        for p in [
            TelephonyLegOriginate,
            TelephonyLegTerminate,
            TelephonyLegHold,
            TelephonyLegResume,
            TelephonyBridgeManage,
            TelephonyStatusRead,
        ] {
            let d = p.definition();
            assert_eq!(
                (d.receiver, d.grantable_to),
                (services::TELEPHONY, &[services::CALLING][..]),
                "{}",
                d.name
            );
        }
        // Telephony -> Calling: normalized facts only, Telephony only.
        for p in [
            CallsTelephonyEventApply,
            CallsTelephonyReconciliationApply,
            CallsTelephonyInboundOffer,
        ] {
            let d = p.definition();
            assert_eq!(
                (d.receiver, d.grantable_to),
                (services::CALLING, &[services::TELEPHONY][..]),
                "{}",
                d.name
            );
        }
    }

    #[test]
    fn platform_access_flows_are_least_privilege() {
        use InternalPermission::*;
        for p in [
            PlatformCredentialAuthenticate,
            PlatformPrincipalResolve,
            PlatformPrincipalRegister,
            PlatformIdentityRead,
            PlatformIdentityAdminister,
        ] {
            assert_eq!(p.definition().receiver, services::PLATFORM_ACCESS);
        }
        // Identity administration is operator tooling only, never a service.
        let d = PlatformIdentityAdminister.definition();
        assert_eq!(d.grantable_to, &[services::OPERATOR_CONSOLE]);
        assert!(d.sensitive);
        // Principal registration (public parties, tombstones) is Calling's only.
        assert_eq!(
            PlatformPrincipalRegister.definition().grantable_to,
            &[services::CALLING]
        );
        // Numbering reads; it never administers identity.
        let d = NumberResourceRead.definition();
        assert_eq!(d.receiver, services::NUMBERING);
        assert!(!d.grantable_to.contains(&services::NUMBERING));
    }

    #[test]
    fn sms_gateway_flows_are_least_privilege() {
        use InternalPermission::*;
        // The SMS destination resolver is the SMS Gateway's alone; the voice resolver is
        // Telephony's alone (no family is resolved through the other's permission).
        let d = NumberSmsRouteResolve.definition();
        assert_eq!(
            (d.receiver, d.grantable_to, d.sensitive),
            (services::NUMBERING, &[services::SMS_GATEWAY][..], true)
        );
        assert_eq!(
            NumberRouteResolve.definition().grantable_to,
            &[services::TELEPHONY]
        );
        // Sender grants: the SMS Gateway only; caller identity never.
        assert_eq!(
            NumberSmsSenderAuthorize.definition().grantable_to,
            &[services::SMS_GATEWAY]
        );
        assert!(!NumberCallerIdentityAuthorize
            .definition()
            .grantable_to
            .contains(&services::SMS_GATEWAY));
        // Customer authentication, principal resolution, number reads (route readiness)
        // and webhook publication — nothing administrative.
        for p in [
            PlatformCredentialAuthenticate,
            PlatformPrincipalResolve,
            NumberResourceRead,
            PlatformEventPublish,
        ] {
            assert!(p.definition().grantable_to.contains(&services::SMS_GATEWAY));
        }
        let receiving: Vec<&str> = REGISTRY
            .iter()
            .filter(|d| d.receiver == services::SMS_GATEWAY)
            .map(|d| d.name)
            .collect();
        assert_eq!(
            receiving,
            ["sms-bridge.submit", "sms-request.forward"],
            "only Messaging's SMS Bridge submissions and the API Gateway's authenticated customer requests are received by the SMS Gateway"
        );
        assert_eq!(
            SmsRequestForward.definition().grantable_to,
            &[services::API_GATEWAY]
        );
        assert_eq!(
            SmsBridgeSubmit.definition().grantable_to,
            &[services::MESSAGING]
        );
    }

    #[test]
    fn messaging_flows_are_least_privilege() {
        // Part V H: exactly four narrow internal operations reach Messaging, each from
        // the one peer that owns the corresponding authority, and Messaging holds nothing
        // beyond what the registry lists for it explicitly (never a sensitive grant).
        let receiving: Vec<_> = REGISTRY
            .iter()
            .filter(|d| d.receiver == services::MESSAGING)
            .map(|d| (d.name, d.grantable_to))
            .collect();
        assert_eq!(
            receiving,
            [
                (
                    "messaging-application.delegate",
                    &[services::MEETING_SERVICE][..]
                ),
                ("messaging-meeting.sync", &[services::MEETING_SERVICE][..]),
                (
                    "messaging-context-authority.apply",
                    &[services::PRIVACY_RELAY][..]
                ),
                ("messaging-sms-bridge.deliver", &[services::SMS_GATEWAY][..]),
                ("messaging-request.forward", &[services::API_GATEWAY][..]),
            ]
        );
        // Principal creation is the Meeting Service's (first-party login authority) only.
        assert_eq!(
            InternalPermission::PlatformPrincipalEnsure
                .definition()
                .grantable_to,
            &[services::MEETING_SERVICE]
        );
        let granted: Vec<_> = REGISTRY
            .iter()
            .filter(|d| d.grantable_to.contains(&services::MESSAGING))
            .map(|d| d.name)
            .collect();
        assert_eq!(
            granted, MESSAGING_GRANTS,
            "Messaging's grantable permissions"
        );
        for d in REGISTRY.iter().filter(|d| d.sensitive) {
            assert!(!d.grantable_to.contains(&services::MESSAGING), "{}", d.name);
        }
    }

    /// Every permission Messaging may ever be granted (registry order).
    /// Part V B: credential verification, principal resolution in the signed delegated
    /// scope, and identity reads (change feed for bounded authorization caching). Part V
    /// H: reading the communication context it is bound to (reconciliation), publishing
    /// committed events for customer webhooks and submitting MANAGED SMS Bridge copies.
    /// Never principal registration or identity administration.
    const MESSAGING_GRANTS: &[&str] = &[
        "communication-context.read",
        "platform-event.publish",
        "platform-credential.authenticate",
        "platform-principal.resolve",
        "platform-identity.read",
        "sms-bridge.submit",
        "developer-event.publish",
    ];

    #[test]
    fn part_vi_developer_platform_and_gateway_flows_are_least_privilege() {
        let granted = |service: &str| -> Vec<&str> {
            REGISTRY
                .iter()
                .filter(|d| d.grantable_to.contains(&service))
                .map(|d| d.name)
                .collect()
        };
        let received = |service: &str| -> Vec<&str> {
            REGISTRY
                .iter()
                .filter(|d| d.receiver == service)
                .map(|d| d.name)
                .collect()
        };
        // The gateway authenticates customers through Platform Access and forwards to the
        // owning customer services and the Developer Platform; it receives nothing
        // (customer ingress only, no internal API).
        assert_eq!(
            granted(services::API_GATEWAY),
            [
                "platform-credential.authenticate",
                "developer-request.forward",
                "calling-request.forward",
                "messaging-request.forward",
                "numbering-request.forward",
                "sms-request.forward",
                "developer-entitlement.read",
                "developer-usage.record",
            ]
        );
        assert!(received(services::API_GATEWAY).is_empty());
        // The Developer Platform reads identity and delegates the canonical workspace
        // hierarchy; it never administers, registers principals or authenticates
        // credentials (Platform Access stays the only authority).
        assert_eq!(
            granted(services::DEVELOPER_PLATFORM),
            [
                "platform-identity.read",
                "platform-identity.delegate",
                "user-session.verify",
                "webhook-migration.export",
            ]
        );
        assert_eq!(
            received(services::DEVELOPER_PLATFORM),
            [
                "developer-request.forward",
                "developer-entitlement.read",
                "developer-usage.record",
                "developer-event.publish",
                "developer-platform.administer",
            ]
        );
        assert_eq!(
            InternalPermission::DeveloperPlatformAdminister
                .definition()
                .grantable_to,
            &[services::OPERATOR_CONSOLE]
        );
        for d in REGISTRY.iter().filter(|d| d.sensitive) {
            for s in [services::API_GATEWAY, services::DEVELOPER_PLATFORM] {
                assert!(!d.grantable_to.contains(&s), "{} -> {s}", d.name);
            }
        }
        // Telephony, Platform Access administration and operator tooling are never
        // reachable from the customer ingress.
        for d in REGISTRY
            .iter()
            .filter(|d| d.grantable_to.contains(&services::API_GATEWAY))
        {
            assert_ne!(d.receiver, services::TELEPHONY, "{}", d.name);
            assert_ne!(d.permission, InternalPermission::PlatformIdentityAdminister);
        }
    }

    #[test]
    fn wildcard_like_values_are_detected() {
        for bad in [
            "*",
            "admin",
            "ALL",
            "root",
            "calls.*",
            "admin.all",
            "*.read",
            "superuser",
        ] {
            assert!(InternalPermission::is_wildcard_like(bad), "{bad}");
            assert!(InternalPermission::parse(bad).is_none());
        }
    }
}

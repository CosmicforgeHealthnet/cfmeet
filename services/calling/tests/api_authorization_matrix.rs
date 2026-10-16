//! Part K §101: the RUNTIME authorization matrix of the Calling API. Every public mutable
//! operation (POST / PUT / PATCH / DELETE) of the Calling HTTP contract
//! (`packages/contracts/calling/http/v1.openapi.json`) is exercised against the real
//! router of a fully provisioned deployment (tests/common/deployment.rs: every resource
//! family exists, so path ids are real), and for EACH operation:
//!
//! * no credential → 401;
//! * a valid credential lacking the operation's scope (every other scope granted) → 403
//!   (each required scope withheld on its own as well); for internal routes a trusted
//!   service without the receiver-granted permission → 403; for endpoint routes a
//!   platform API credential → 401 and another endpoint's signaling credential → 404
//!   (hidden resource); for the provider callback a wrong signature → 401;
//! * malformed input with full authority → 400 / 422 (or the documented 404 for a
//!   malformed id of an operation without input);
//! * NONE of these probes changed anything: per-table row counts AND content
//!   fingerprints of the whole database are identical before and after (rate-limit
//!   accounting excepted, see [`BOOKKEEPING`]).
//!
//! Every mutable operation must be classified in [`CLASSIFIED`]; the test FAILS when the
//! contract gains a mutable operation that is not classified (or a classified one
//! disappears). Operations that cannot be reached in the test world would be listed in
//! [`UNREACHABLE`] with a reason (currently none: every operation is reached).
//!
//! Further: targeted success and version / state / idempotency conflict (409) checks for
//! a representative set of operations, and the combined scope × ownership rule (owning
//! a resource without the scope is refused; holding the scope for another tenant's
//! resource is 404). Conflict semantics of the remaining operations are certified by
//! their own suites (enterprise_foundation.rs, enterprise_extensions.rs,
//! enterprise_ring_groups.rs, enterprise_queues.rs, enterprise_ivr.rs,
//! enterprise_hours_forwarding.rs, enterprise_voicemail.rs, enterprise_recording.rs,
//! webhooks.rs, call_api.rs, app_control.rs, app_ringing.rs, blocks.rs).

mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use axum::http::{Method, StatusCode};
use calling::{domain::TenantClassification, platform::scopes::names};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
    ServiceTokenSigner,
};
use common::{
    app::Device,
    deployment::{Deployment, Receiver, ALL_TEST_SCOPES},
    enterprise::{credential, default_rules, id_of, strings},
    TestResponse,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const CALLING_HTTP: &str = "calling/http/v1.openapi.json";

/// How the malformed-input probe of an operation is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// A request body of the wrong shape (a JSON array) → 400 / 422.
    Body,
    /// A malformed required query parameter (`expectedVersion=not-a-number`) → 400 / 422.
    Query,
    /// No body and no query: a malformed path id, answered exactly like an unknown /
    /// hidden resource → 404 (the contract's documented behaviour).
    HiddenPath,
}

/// Every mutable operation of the Calling HTTP contract and its malformed-input probe.
const CLASSIFIED: &[(&str, Probe)] = &[
    ("POST /internal/v1/privacy-relay/enforcement", Probe::Body),
    ("POST /internal/v1/telephony/events", Probe::Body),
    ("POST /internal/v1/telephony/inbound-calls", Probe::Body),
    ("POST /internal/v1/telephony/dcn-inbound-calls", Probe::Body),
    ("POST /v1/calls", Probe::Body),
    ("POST /v1/calls/{call_id}/accept", Probe::Body),
    ("POST /v1/calls/{call_id}/decline", Probe::Body),
    ("POST /v1/calls/{call_id}/cancel", Probe::Body),
    ("POST /v1/calls/{call_id}/end", Probe::Body),
    ("POST /v1/calls/{call_id}/hold", Probe::Body),
    ("POST /v1/calls/{call_id}/resume", Probe::Body),
    ("POST /v1/calls/{call_id}/transfer", Probe::Body),
    (
        "POST /v1/calls/{call_id}/sessions/{session_id}/media-credentials",
        Probe::Body,
    ),
    (
        "POST /v1/calls/{call_id}/sessions/{session_id}/remove",
        Probe::Body,
    ),
    ("POST /v1/calling/endpoints", Probe::Body),
    (
        "DELETE /v1/calling/endpoints/{endpoint_id}",
        Probe::HiddenPath,
    ),
    ("POST /v1/calling/signaling-credentials", Probe::Body),
    (
        "POST /v1/calling/invitations/{invitation_id}/accept",
        Probe::Body,
    ),
    (
        "POST /v1/calling/invitations/{invitation_id}/decline",
        Probe::Body,
    ),
    (
        "POST /v1/calling/sessions/{session_id}/media-credentials",
        Probe::Body,
    ),
    ("POST /internal/provider/media/events", Probe::Body),
    ("POST /internal/v1/telephony/enterprise-calls", Probe::Body),
    ("POST /internal/v1/telephony/enterprise-keypad", Probe::Body),
    ("POST /internal/v1/telephony/captures", Probe::Body),
    ("POST /v1/enterprise/policies", Probe::Body),
    ("PATCH /v1/enterprise/policies/{policy_id}", Probe::Body),
    ("DELETE /v1/enterprise/policies/{policy_id}", Probe::Query),
    ("POST /v1/enterprise/departments", Probe::Body),
    (
        "PATCH /v1/enterprise/departments/{department_id}",
        Probe::Body,
    ),
    (
        "DELETE /v1/enterprise/departments/{department_id}",
        Probe::Query,
    ),
    (
        "PUT /v1/enterprise/departments/{department_id}/members",
        Probe::Body,
    ),
    ("POST /v1/enterprise/extensions", Probe::Body),
    (
        "PATCH /v1/enterprise/extensions/{extension_id}",
        Probe::Body,
    ),
    (
        "DELETE /v1/enterprise/extensions/{extension_id}",
        Probe::Query,
    ),
    ("POST /v1/enterprise/ring-groups", Probe::Body),
    (
        "PATCH /v1/enterprise/ring-groups/{ring_group_id}",
        Probe::Body,
    ),
    (
        "DELETE /v1/enterprise/ring-groups/{ring_group_id}",
        Probe::Query,
    ),
    (
        "PUT /v1/enterprise/ring-groups/{ring_group_id}/members",
        Probe::Body,
    ),
    ("POST /v1/enterprise/queues", Probe::Body),
    ("PATCH /v1/enterprise/queues/{queue_id}", Probe::Body),
    ("DELETE /v1/enterprise/queues/{queue_id}", Probe::Query),
    ("PUT /v1/enterprise/queues/{queue_id}/members", Probe::Body),
    ("POST /v1/enterprise/ivr-flows", Probe::Body),
    ("PATCH /v1/enterprise/ivr-flows/{ivr_flow_id}", Probe::Body),
    (
        "DELETE /v1/enterprise/ivr-flows/{ivr_flow_id}",
        Probe::Query,
    ),
    (
        "POST /v1/enterprise/ivr-flows/{ivr_flow_id}/versions",
        Probe::Body,
    ),
    (
        "PUT /v1/enterprise/ivr-flows/{ivr_flow_id}/activation",
        Probe::Body,
    ),
    ("POST /v1/enterprise/sip-devices", Probe::Body),
    (
        "DELETE /v1/enterprise/sip-devices/{sip_device_id}",
        Probe::Query,
    ),
    ("POST /v1/calls/{call_id}/ivr/input", Probe::Body),
    ("POST /v1/enterprise/business-hours", Probe::Body),
    (
        "PUT /v1/enterprise/business-hours/{business_hours_id}",
        Probe::Body,
    ),
    (
        "DELETE /v1/enterprise/business-hours/{business_hours_id}",
        Probe::Query,
    ),
    (
        "POST /v1/enterprise/business-hours/{business_hours_id}/evaluation",
        Probe::Body,
    ),
    ("POST /v1/enterprise/public-contacts", Probe::Body),
    (
        "DELETE /v1/enterprise/public-contacts/{public_contact_id}",
        Probe::Query,
    ),
    ("POST /v1/enterprise/forwarding-rules", Probe::Body),
    (
        "PATCH /v1/enterprise/forwarding-rules/{forwarding_rule_id}",
        Probe::Body,
    ),
    (
        "DELETE /v1/enterprise/forwarding-rules/{forwarding_rule_id}",
        Probe::Query,
    ),
    ("POST /v1/enterprise/voicemail/mailboxes", Probe::Body),
    (
        "PATCH /v1/enterprise/voicemail/mailboxes/{mailbox_id}",
        Probe::Body,
    ),
    (
        "DELETE /v1/enterprise/voicemail/mailboxes/{mailbox_id}",
        Probe::Query,
    ),
    (
        "DELETE /v1/enterprise/voicemail/messages/{message_id}",
        Probe::Query,
    ),
    (
        "PUT /v1/enterprise/voicemail/messages/{message_id}/read-state",
        Probe::Body,
    ),
    (
        "POST /v1/enterprise/voicemail/messages/{message_id}/media-access",
        Probe::Body,
    ),
    ("POST /v1/enterprise/recording-policies", Probe::Body),
    (
        "PATCH /v1/enterprise/recording-policies/{recording_policy_id}",
        Probe::Body,
    ),
    (
        "DELETE /v1/enterprise/recording-policies/{recording_policy_id}",
        Probe::Query,
    ),
    ("POST /v1/calls/{call_id}/recording-consent", Probe::Body),
    ("POST /v1/calls/{call_id}/recordings", Probe::Body),
    (
        "POST /v1/calls/{call_id}/recordings/{recording_id}/stop",
        Probe::Body,
    ),
    (
        "DELETE /v1/enterprise/recordings/{recording_id}",
        Probe::Query,
    ),
    (
        "POST /v1/enterprise/recordings/{recording_id}/media-access",
        Probe::Body,
    ),
    ("POST /v1/webhooks", Probe::Body),
    ("PATCH /v1/webhooks/{webhook_id}", Probe::Body),
    ("DELETE /v1/webhooks/{webhook_id}", Probe::Query),
    ("POST /v1/webhooks/{webhook_id}/rotate-secret", Probe::Body),
    (
        "POST /v1/webhook-deliveries/{delivery_id}/replay",
        Probe::HiddenPath,
    ),
    ("POST /internal/v1/platform/events", Probe::Body),
    ("POST /v1/blocks", Probe::Body),
    ("DELETE /v1/blocks/{block_id}", Probe::HiddenPath),
    ("POST /v1/calls/{call_id}/abuse-reports", Probe::Body),
];

/// Operations that cannot be reached in the test world, with the reason (none today).
const UNREACHABLE: &[(&str, &str)] = &[];

/// Tables that legitimately change on a refused request, and why.
const BOOKKEEPING: &[(&str, &str)] = &[(
    "platform_request_rate_windows",
    "per-credential request accounting: every authenticated request counts toward the \
     rate limit, refused or not (it is how abuse is throttled), never a resource",
)];

/// Append-only evidence of REFUSED access (Part H §79.29): a refusal of a protected
/// artifact in the caller's own scope is audited as DENIED. Such rows (and only such rows:
/// outcome DENIED, version unchanged) may appear on a refused request; every other row of
/// these tables is fingerprinted like any table.
const DENIAL_EVIDENCE: &[(&str, &str)] = &[
    (
        "enterprise_admin_audit",
        "details->>'outcome' = 'DENIED' AND old_version IS NOT DISTINCT FROM new_version",
    ),
    ("call_recording_access_audit", "outcome = 'DENIED'"),
];

/// Operations whose handler validates the body BEFORE checking the scope (a credential
/// without the scope gets 400 for a malformed body, never a side effect). Their
/// missing-scope probe sends a well-formed body so authorization alone decides. None any
/// more: `POST /v1/blocks` and `POST /v1/calls/{call_id}/abuse-reports` check the scope
/// first.
const VALIDATED_BEFORE_AUTHORIZATION: &[&str] = &[];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Auth {
    Platform,
    Service,
    Signaling,
    ProviderSignature,
}

#[derive(Debug, Clone)]
struct Operation {
    key: String,
    method: Method,
    template: String,
    auth: Auth,
    /// Scopes (platform) or the receiver-granted permission (service).
    requires: Vec<String>,
}

fn contract() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/contracts")
        .join(CALLING_HTTP);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Every mutable operation of the contract.
fn mutable_operations() -> Vec<Operation> {
    let doc = contract();
    let mut out = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let method = match method.as_str() {
                "post" => Method::POST,
                "put" => Method::PUT,
                "patch" => Method::PATCH,
                "delete" => Method::DELETE,
                _ => continue,
            };
            let security = op["security"][0].as_object().unwrap();
            let (scheme, requires) = security.iter().next().unwrap();
            let auth = match scheme.as_str() {
                "platformCredential" => Auth::Platform,
                "cfServiceAuth" => Auth::Service,
                "signalingCredential" => Auth::Signaling,
                "mediaProviderSignature" => Auth::ProviderSignature,
                other => panic!("{method} {path}: unclassified security scheme {other}"),
            };
            out.push(Operation {
                key: format!("{method} {path}"),
                method,
                template: path.clone(),
                auth,
                requires: requires
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_str().unwrap().to_string())
                    .collect(),
            });
        }
    }
    out
}

#[test]
fn every_mutable_operation_of_the_contract_is_classified() {
    let documented: BTreeSet<String> = mutable_operations().into_iter().map(|o| o.key).collect();
    let classified: BTreeSet<String> = CLASSIFIED.iter().map(|(k, _)| k.to_string()).collect();
    let unclassified: Vec<_> = documented.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "new mutable operations must be classified in CLASSIFIED (and covered by the matrix): {unclassified:?}"
    );
    let stale: Vec<_> = classified.difference(&documented).collect();
    assert!(
        stale.is_empty(),
        "classified operations no longer in the contract: {stale:?}"
    );
    for (op, reason) in UNREACHABLE {
        assert!(documented.contains(*op), "{op}");
        assert!(!reason.is_empty());
    }
    // The contract's scopes are scopes Calling enforces (a typo would make the 403 probe
    // vacuous); `scopes::calling_scopes_are_registered_by_platform_access` proves each is
    // registered by Platform Access.
    for op in mutable_operations() {
        if op.auth == Auth::Platform {
            for scope in &op.requires {
                assert!(names::ALL.contains(&scope.as_str()), "{}: {scope}", op.key);
                assert!(
                    ALL_TEST_SCOPES.contains(&scope.as_str()),
                    "{}: {scope}",
                    op.key
                );
            }
        }
    }
}

/// Real ids of every resource family of the provisioned deployment.
struct Provisioned {
    d: Deployment,
    ids: BTreeMap<&'static str, Uuid>,
    full: String,
    caller_device: Device,
    callee_device: Device,
    caller_signaling: String,
    callee_signaling: String,
}

async fn provision() -> Option<Provisioned> {
    let d = Deployment::new().await?;
    d.enable_artifacts().await;
    let full = d.credential(ALL_TEST_SCOPES).await;
    let full = full.secret.expose_once().to_string();
    let mut ids: BTreeMap<&'static str, Uuid> = BTreeMap::new();
    let a = d.a();
    let send = |method: Method, path: &str, body: Value| {
        let (d, full, path) = (&d, full.clone(), path.to_string());
        async move {
            let r = d.e.send(&full, method, &path, Some(body)).await;
            assert!(r.status.is_success(), "{path}: {}", r.text);
            r.json
        }
    };
    // Enterprise configuration.
    let member = d.desk("matrix-member").await;
    // The default policy permits public forwarding (public contacts) and every
    // administration capability the matrix exercises.
    let mut rules = default_rules();
    rules["externalPublicCallingAllowed"] = json!(true);
    rules["publicForwardingAllowed"] = json!(true);
    rules["publicDestinationCountries"] = json!(["NG"]);
    rules["adminCapabilities"] = json!([
        "MANAGE_ROUTING",
        "MANAGE_VOICEMAIL",
        "MANAGE_RECORDING",
        "MANAGE_PUBLIC_FORWARDING"
    ]);
    let policy = send(
        Method::POST,
        "/v1/enterprise/policies",
        json!({ "name": "Matrix policy", "isDefault": true, "rules": rules }),
    )
    .await;
    ids.insert("policy_id", id_of(&policy, "policyId"));
    ids.insert(
        "department_id",
        id_of(&d.e.department("Matrix desk").await, "departmentId"),
    );
    let ext =
        d.e.extension("6100", "ENTERPRISE_MEMBER", member.0.id())
            .await;
    ids.insert("extension_id", id_of(&ext, "extensionId"));
    let group = send(
        Method::POST,
        "/v1/enterprise/ring-groups",
        json!({ "name": "Matrix group", "strategy": "SIMULTANEOUS", "memberRingSeconds": 10,
                "groupTimeoutSeconds": 30 }),
    )
    .await;
    ids.insert("ring_group_id", id_of(&group, "ringGroupId"));
    let queue = send(
        Method::POST,
        "/v1/enterprise/queues",
        json!({ "name": "Matrix queue", "maxWaitSeconds": 60, "offerTimeoutSeconds": 10,
                "capacity": 5 }),
    )
    .await;
    ids.insert("queue_id", id_of(&queue, "queueId"));
    let flow = send(
        Method::POST,
        "/v1/enterprise/ivr-flows",
        json!({ "name": "Matrix menu" }),
    )
    .await;
    ids.insert("ivr_flow_id", id_of(&flow, "ivrFlowId"));
    let sip = send(
        Method::POST,
        "/v1/enterprise/sip-devices",
        json!({ "deviceRef": "dev-matrix-01", "principalId": member.0.id(),
                "displayLabel": "Matrix desk" }),
    )
    .await;
    ids.insert("sip_device_id", id_of(&sip, "sipDeviceId"));
    let hours = send(
        Method::POST,
        "/v1/enterprise/business-hours",
        json!({ "name": "Matrix hours", "timezone": "Africa/Lagos",
                "openDestination": { "type": "ENTERPRISE_MEMBER", "id": member.0.id().to_string() },
                "closedDestination": { "type": "ENTERPRISE_MEMBER", "id": member.0.id().to_string() },
                "weeklyWindows": [{ "dayOfWeek": "MONDAY", "start": "09:00", "end": "17:00" }] }),
    )
    .await;
    ids.insert("business_hours_id", id_of(&hours, "businessHoursId"));
    let contact = send(
        Method::POST,
        "/v1/enterprise/public-contacts",
        json!({ "label": "Matrix phone", "contactReference": d.contact,
                "privacyRelaySessionId": d.session }),
    )
    .await;
    ids.insert("public_contact_id", id_of(&contact, "publicContactId"));
    // Voicemail: a mailbox, a forwarding rule and a READY message.
    let caller = d.desk("matrix-caller").await;
    let owner = d.desk("matrix-owner").await;
    let (_, message, mailbox) = d.voicemail(&full, &owner.0, &caller, "6200").await;
    ids.insert("message_id", message);
    ids.insert("mailbox_id", mailbox);
    let rule = strings(
        d.h().conn(),
        &format!(
            "SELECT id::text FROM enterprise_forwarding_rules WHERE principal_id = '{}'",
            owner.0.id()
        ),
    )
    .await;
    ids.insert("forwarding_rule_id", rule[0].parse().unwrap());
    // Recording: a policy and a recording of a CONNECTED enterprise call.
    let rec_policy = send(
        Method::POST,
        "/v1/enterprise/recording-policies",
        json!({ "name": "Matrix recording", "mode": "MANUAL_ALLOWED", "noticeRequired": false,
                "consentParticipants": "NONE", "jurisdictionRef": "NG-NDPA-2023/v2",
                "retentionDays": 30, "maxRecordingSeconds": 600, "isDefault": true }),
    )
    .await;
    ids.insert(
        "recording_policy_id",
        id_of(&rec_policy, "recordingPolicyId"),
    );
    let agent = d.desk("matrix-agent").await;
    d.e.extension("6300", "ENTERPRISE_MEMBER", agent.0.id())
        .await;
    let r =
        d.e.call_extension("VOICE", &caller.0, &caller.1, "6300", "matrix-rec")
            .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let rec_call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let inv = a.invitation_for(rec_call, agent.1.endpoint).await.unwrap();
    let (_, v) = a.state(rec_call).await;
    assert_eq!(
        a.accept(rec_call, inv, &agent.1, v).await.status,
        StatusCode::OK
    );
    for dev in [&caller.1, &agent.1] {
        let s = a.session_on(rec_call, dev.endpoint).await.unwrap();
        a.connect(rec_call, s).await;
    }
    let rec = send(
        Method::POST,
        &format!("/v1/calls/{rec_call}/recordings"),
        json!({}),
    )
    .await;
    ids.insert(
        "recording_id",
        rec["recording"]["recordingId"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
    );
    // Webhooks: a subscription and a delivery.
    let rx = Receiver::start().await;
    let hook = send(
        Method::POST,
        "/v1/webhooks",
        json!({ "url": rx.url, "eventTypes": ["call.ringing"] }),
    )
    .await;
    ids.insert(
        "webhook_id",
        hook["webhook"]["webhookId"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
    );
    a.ringing_call("VIDEO").await;
    let transport =
        calling::webhooks::worker::LocalWebhookTransport::new((*d.h().state.webhooks).clone());
    d.h()
        .state
        .webhooks
        .publish_outbox_once(&transport, 10_000)
        .await
        .unwrap();
    let delivery = strings(
        d.h().conn(),
        "SELECT id::text FROM webhook_deliveries LIMIT 1",
    )
    .await;
    ids.insert("delivery_id", delivery[0].parse().unwrap());
    // A block rule.
    let block = send(
        Method::POST,
        "/v1/blocks",
        json!({ "principalId": a.w.initiator.id(), "blockedPrincipalId": member.0.id(),
                "reasonCode": "SPAM" }),
    )
    .await;
    ids.insert("block_id", id_of(&block, "blockId"));
    // Last (its ring window is fresh): a ringing application call — invitation,
    // sessions, endpoints, signaling credentials.
    let (call, caller_device, callee_device) = a.ringing_call("VOICE").await;
    ids.insert("call_id", call);
    ids.insert(
        "invitation_id",
        a.invitation_for(call, callee_device.endpoint)
            .await
            .unwrap(),
    );
    ids.insert(
        "session_id",
        a.session_on(call, caller_device.endpoint).await.unwrap(),
    );
    ids.insert("endpoint_id", callee_device.endpoint);
    let caller_signaling = a.signaling_token(&caller_device).await;
    let callee_signaling = a.signaling_token(&callee_device).await;
    Some(Provisioned {
        d,
        ids,
        full,
        caller_device,
        callee_device,
        caller_signaling,
        callee_signaling,
    })
}

impl Provisioned {
    fn path(&self, template: &str) -> String {
        let mut out = template.to_string();
        while let Some(start) = out.find('{') {
            let end = out[start..].find('}').unwrap() + start;
            let name = &out[start + 1..end];
            let id = self.ids.get(name).unwrap_or_else(|| {
                panic!("no provisioned id for path parameter {name} ({template})")
            });
            out = format!("{}{}{}", &out[..start], id, &out[end + 1..]);
        }
        out
    }

    /// The path with its first id replaced by a malformed one.
    fn malformed_path(&self, template: &str) -> String {
        let start = template.find('{').expect("a path id");
        let end = template[start..].find('}').unwrap() + start;
        self.path(&format!(
            "{}not-a-uuid{}",
            &template[..start],
            &template[end + 1..]
        ))
    }

    fn h(&self) -> &common::Harness {
        self.d.h()
    }

    fn delegated(&self) -> DelegatedPlatformContext {
        let w = &self.d.e.a.w;
        let s = w.scope();
        DelegatedPlatformContext::new(
            s.tenant_id().as_uuid(),
            s.application_id().as_uuid(),
            s.environment_id().as_uuid(),
            ApplicationEnvironmentKind::Production,
            w.consumer.sa(w.kind).as_uuid(),
        )
    }

    async fn as_service(
        &self,
        signer: &ServiceTokenSigner,
        delegated: bool,
        method: Method,
        path: &str,
        body: Value,
    ) -> TestResponse {
        let mut outbound = OutboundServiceRequest::new_operation(ServiceId::calling());
        if delegated {
            outbound = outbound.with_delegated_context(self.delegated());
        }
        let signed = signer.sign(&outbound).unwrap();
        let headers: Vec<(String, String)> = signed
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
            .collect();
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        self.h()
            .request_with(method, path, None, Some(body), &refs)
            .await
    }
}

/// Per-table (row count, content fingerprint) of the whole database, bookkeeping excepted.
async fn snapshot(conn: &sea_orm::DatabaseConnection) -> BTreeMap<String, String> {
    let tables = strings(
        conn,
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1",
    )
    .await;
    let sql = tables
        .iter()
        .filter(|t| !BOOKKEEPING.iter().any(|(b, _)| b == t))
        .map(|t| {
            let filter = DENIAL_EVIDENCE
                .iter()
                .find(|(d, _)| d == t)
                .map(|(_, denial)| format!(" WHERE NOT ({denial})"))
                .unwrap_or_default();
            format!(
                "SELECT '{t}' || ':' || count(*) || ':' || md5(COALESCE(string_agg(x::text, '|' ORDER BY x::text), '')) FROM \"{t}\" x{filter}"
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    conn.query_all(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap()
        .into_iter()
        .map(|r| {
            let line = r.try_get_by_index::<String>(0).unwrap();
            let (t, rest) = line.split_once(':').unwrap();
            (t.to_string(), rest.to_string())
        })
        .collect()
}

fn changed(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> Vec<String> {
    a.iter()
        .filter(|(t, v)| b.get(*t) != Some(v))
        .map(|(t, _)| t.clone())
        .collect()
}

fn query_suffix(op: &Operation) -> &'static str {
    if op.method == Method::DELETE
        && CLASSIFIED
            .iter()
            .any(|(k, p)| *k == op.key && *p == Probe::Query)
    {
        "?expectedVersion=1"
    } else {
        ""
    }
}

#[tokio::test]
async fn every_mutable_operation_refuses_missing_insufficient_and_malformed_requests_without_effect(
) {
    let Some(p) = provision().await else {
        return;
    };
    let conn = p.h().conn().clone();
    let mut failures: Vec<String> = Vec::new();
    // Credentials lacking exactly the required scopes (all other scopes granted), issued
    // up front so no probe window contains a credential write.
    let mut lacking: BTreeMap<Vec<String>, String> = BTreeMap::new();
    for op in mutable_operations()
        .iter()
        .filter(|o| o.auth == Auth::Platform)
    {
        let mut withheld_sets = vec![op.requires.clone()];
        if op.requires.len() > 1 {
            withheld_sets.extend(op.requires.iter().map(|s| vec![s.clone()]));
        }
        for withheld in withheld_sets {
            if lacking.contains_key(&withheld) {
                continue;
            }
            let scopes: Vec<&str> = ALL_TEST_SCOPES
                .iter()
                .copied()
                .filter(|s| !withheld.iter().any(|w| w == s))
                .collect();
            let c = p.d.credential(&scopes).await;
            lacking.insert(withheld, c.secret.expose_once().to_string());
        }
    }
    let a = p.d.a();
    let well_formed: BTreeMap<&str, Value> = BTreeMap::from([
        (
            "POST /v1/blocks",
            json!({ "principalId": a.w.initiator.id(), "blockedPrincipalId": a.w.recipient.id(),
                    "reasonCode": "SPAM" }),
        ),
        (
            "POST /v1/calls/{call_id}/abuse-reports",
            json!({ "principalId": a.w.initiator.id(), "category": "SPAM" }),
        ),
    ]);
    assert!(VALIDATED_BEFORE_AUTHORIZATION
        .iter()
        .all(|k| well_formed.contains_key(k)));
    let mut checked = 0;
    for op in mutable_operations() {
        if UNREACHABLE.iter().any(|(k, _)| *k == op.key) {
            continue;
        }
        let probe = CLASSIFIED
            .iter()
            .find(|(k, _)| *k == op.key)
            .map(|(_, p)| *p)
            .unwrap_or_else(|| panic!("{} is not classified", op.key));
        let path = format!("{}{}", p.path(&op.template), query_suffix(&op));
        let before = snapshot(&conn).await;
        let mut expect = |what: &str, r: &TestResponse, allowed: &[StatusCode]| {
            if !allowed.contains(&r.status) {
                failures.push(format!(
                    "{}: {what}: expected {allowed:?}, got {} {}",
                    op.key, r.status, r.text
                ));
            }
        };
        let probe_body = json!({});
        match op.auth {
            Auth::Platform => {
                // 1. No credential.
                let r = p
                    .h()
                    .request(op.method.clone(), &path, None, Some(probe_body.clone()))
                    .await;
                expect("no credential", &r, &[StatusCode::UNAUTHORIZED]);
                let r = p
                    .h()
                    .request(
                        op.method.clone(),
                        &path,
                        Some("cfmk_live_notacredential_0000"),
                        Some(probe_body.clone()),
                    )
                    .await;
                expect("invalid credential", &r, &[StatusCode::UNAUTHORIZED]);
                // 2. Every scope but the required one(s) — together and one at a time.
                let mut withheld_sets = vec![op.requires.clone()];
                if op.requires.len() > 1 {
                    withheld_sets.extend(op.requires.iter().map(|s| vec![s.clone()]));
                }
                for withheld in withheld_sets {
                    let token = lacking[&withheld].clone();
                    let body = well_formed
                        .get(op.key.as_str())
                        .cloned()
                        .unwrap_or_else(|| probe_body.clone());
                    let r = p
                        .h()
                        .request(op.method.clone(), &path, Some(&token), Some(body))
                        .await;
                    expect(
                        &format!("without {withheld:?}"),
                        &r,
                        &[StatusCode::FORBIDDEN],
                    );
                    if r.status == StatusCode::FORBIDDEN {
                        let code = r.json["error"]["code"].clone();
                        let mut r = r;
                        r.status = if code == "INSUFFICIENT_SCOPE" {
                            StatusCode::FORBIDDEN
                        } else {
                            StatusCode::IM_A_TEAPOT
                        };
                        expect("403 code INSUFFICIENT_SCOPE", &r, &[StatusCode::FORBIDDEN]);
                    }
                }
            }
            Auth::Service => {
                let r = p
                    .h()
                    .request(op.method.clone(), &path, None, Some(probe_body.clone()))
                    .await;
                expect("no service token", &r, &[StatusCode::UNAUTHORIZED]);
                // A TRUSTED service that was not granted this permission.
                let permission = op.requires[0].as_str();
                let peers = p.h().peers.clone();
                let signer = if permission.starts_with("telephony-") {
                    peers.privacy_relay.clone()
                } else {
                    peers.telephony.clone()
                };
                let r = p
                    .as_service(&signer, true, op.method.clone(), &path, probe_body.clone())
                    .await;
                expect(
                    &format!("trusted service without {permission}"),
                    &r,
                    &[StatusCode::FORBIDDEN],
                );
                // An untrusted service.
                let r = p
                    .as_service(
                        &peers.untrusted,
                        true,
                        op.method.clone(),
                        &path,
                        probe_body.clone(),
                    )
                    .await;
                expect(
                    "untrusted service",
                    &r,
                    &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN],
                );
            }
            Auth::Signaling => {
                let r = p
                    .h()
                    .request(op.method.clone(), &path, None, Some(probe_body.clone()))
                    .await;
                expect("no credential", &r, &[StatusCode::UNAUTHORIZED]);
                // A platform API credential (every scope) is never an endpoint credential.
                let r = p
                    .h()
                    .request(
                        op.method.clone(),
                        &path,
                        Some(&p.full),
                        Some(probe_body.clone()),
                    )
                    .await;
                expect("platform credential", &r, &[StatusCode::UNAUTHORIZED]);
                // Another endpoint's signaling credential: hidden resource.
                let other = if op.template.contains("invitation") {
                    &p.caller_signaling
                } else {
                    &p.callee_signaling
                };
                // (a well-formed body: the resource alone decides)
                let (_, version) = a.state(p.ids["call_id"]).await;
                let body = if op.template.contains("invitation") {
                    json!({ "expectedVersion": version })
                } else {
                    json!({})
                };
                let r = p
                    .h()
                    .request(op.method.clone(), &path, Some(other), Some(body))
                    .await;
                expect("another endpoint", &r, &[StatusCode::NOT_FOUND]);
            }
            Auth::ProviderSignature => {
                let body = json!({ "id": "EV_matrix", "event": "room_finished", "room": "x" });
                let r = p
                    .h()
                    .request(op.method.clone(), &path, None, Some(body.clone()))
                    .await;
                expect("no signature", &r, &[StatusCode::UNAUTHORIZED]);
                let wrong = common::fake_media::sign_raw(b"another body");
                let r = p
                    .h()
                    .request_with(
                        op.method.clone(),
                        &path,
                        None,
                        Some(body),
                        &[("authorization", &wrong)],
                    )
                    .await;
                expect("wrong signature", &r, &[StatusCode::UNAUTHORIZED]);
            }
        }
        // 3. Malformed input with full authority (a body that is not a JSON object).
        let bad = json!("not-an-object");
        let invalid = [StatusCode::BAD_REQUEST, StatusCode::UNPROCESSABLE_ENTITY];
        let r = match (op.auth, probe) {
            (_, Probe::HiddenPath) => {
                p.h()
                    .request(
                        op.method.clone(),
                        &p.malformed_path(&op.template),
                        Some(&p.full),
                        None,
                    )
                    .await
            }
            (Auth::Platform, Probe::Query) => {
                p.h()
                    .request(
                        op.method.clone(),
                        &format!("{}?expectedVersion=not-a-number", p.path(&op.template)),
                        Some(&p.full),
                        None,
                    )
                    .await
            }
            (Auth::Platform, Probe::Body) => {
                p.h()
                    .request_with(
                        op.method.clone(),
                        &path,
                        Some(&p.full),
                        Some(bad.clone()),
                        &[("idempotency-key", &Uuid::new_v4().to_string())],
                    )
                    .await
            }
            (Auth::Service, _) => {
                let peers = p.h().peers.clone();
                let permission = op.requires[0].as_str();
                let signer = if permission.starts_with("telephony-") {
                    peers.telephony.clone()
                } else {
                    peers.privacy_relay.clone()
                };
                p.as_service(&signer, true, op.method.clone(), &path, bad.clone())
                    .await
            }
            (Auth::Signaling, _) => {
                let own = if op.template.contains("invitation") {
                    &p.callee_signaling
                } else {
                    &p.caller_signaling
                };
                p.h()
                    .request(op.method.clone(), &path, Some(own), Some(bad.clone()))
                    .await
            }
            (Auth::ProviderSignature, _) => {
                let signature = common::fake_media::sign_raw(bad.to_string().as_bytes());
                p.h()
                    .request_with(
                        op.method.clone(),
                        &path,
                        None,
                        Some(bad.clone()),
                        &[("authorization", &signature)],
                    )
                    .await
            }
        };
        if probe == Probe::HiddenPath {
            expect("malformed id", &r, &[StatusCode::NOT_FOUND]);
        } else {
            expect("malformed input", &r, &invalid);
        }
        // 4. Nothing changed.
        let after = snapshot(&conn).await;
        let diff = changed(&before, &after);
        if !diff.is_empty() {
            failures.push(format!("{}: refused requests changed {diff:?}", op.key));
        }
        checked += 1;
    }
    println!("authorization matrix: {checked} mutable operations checked");
    assert_eq!(checked + UNREACHABLE.len(), CLASSIFIED.len());
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Representative success paths and every kind of 409 (stale version, invalid state,
/// idempotency-key reuse), each leaving the resource as the winner left it.
#[tokio::test]
async fn representative_operations_succeed_and_conflict_with_409() {
    let Some(p) = provision().await else {
        return;
    };
    let full = p.full.clone();
    let send = |method: Method, path: String, body: Option<Value>| {
        let (p, full) = (&p, full.clone());
        async move { p.h().request(method, &path, Some(&full), body).await }
    };
    // Enterprise configuration: versioned update, stale update, stale disable.
    let dept = p.ids["department_id"];
    let path = format!("/v1/enterprise/departments/{dept}");
    let current = send(Method::GET, path.clone(), None).await.json["version"]
        .as_i64()
        .unwrap();
    let r = send(
        Method::PATCH,
        path.clone(),
        Some(json!({ "expectedVersion": current, "name": "Renamed desk" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = send(
        Method::PATCH,
        path.clone(),
        Some(json!({ "expectedVersion": current, "name": "Lost update" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = send(
        Method::DELETE,
        format!("{path}?expectedVersion={current}"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let view = send(Method::GET, path.clone(), None).await.json;
    assert_eq!(view["name"], "Renamed desk");
    assert_eq!(view["status"], "ACTIVE");
    // Webhooks: stale update, then rotation at the current version.
    let hook = p.ids["webhook_id"];
    let hpath = format!("/v1/webhooks/{hook}");
    let r = send(
        Method::PATCH,
        hpath.clone(),
        Some(json!({ "expectedVersion": 99, "status": "DISABLED" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = send(
        Method::POST,
        format!("{hpath}/rotate-secret"),
        Some(json!({ "expectedVersion": 1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Calls: stale version, invalid state, idempotency key reused for another request.
    let call = p.ids["call_id"];
    let a = p.d.a();
    let (_, version) = a.state(call).await;
    let r = send(
        Method::POST,
        format!("/v1/calls/{call}/hold"),
        Some(json!({ "expectedVersion": version })),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "hold while ringing: {}",
        r.text
    );
    let r = send(
        Method::POST,
        format!("/v1/calls/{call}/cancel"),
        Some(json!({ "expectedVersion": version - 1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "stale version: {}", r.text);
    assert_eq!(
        a.state(call).await,
        (calling::calls::CallState::Ringing, version)
    );
    let body = |target: Uuid| {
        json!({ "kind": "VOICE", "initiatorPrincipalId": a.w.initiator.id(),
                "initiatorEndpointId": p.caller_device.endpoint, "targetPrincipalIds": [target] })
    };
    let r = p
        .h()
        .request_with(
            Method::POST,
            "/v1/calls",
            Some(&full),
            Some(body(a.w.recipient.id().as_uuid())),
            &[("idempotency-key", "matrix-key")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let calls = common::scalar_i64(p.h().conn(), "SELECT count(*) FROM calls").await;
    let other = a.principal("matrix-other-target").await;
    let r = p
        .h()
        .request_with(
            Method::POST,
            "/v1/calls",
            Some(&full),
            Some(body(other.id().as_uuid())),
            &[("idempotency-key", "matrix-key")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "IDEMPOTENCY_CONFLICT");
    assert_eq!(
        common::scalar_i64(p.h().conn(), "SELECT count(*) FROM calls").await,
        calls
    );
    // Accept with the current version wins; once the Call is over, every further control
    // command is a state conflict and changes nothing.
    let invitation = p.ids["invitation_id"];
    let (_, version) = a.state(call).await;
    let r = a.accept(call, invitation, &p.callee_device, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = send(
        Method::POST,
        format!("/v1/calls/{call}/cancel"),
        Some(json!({ "expectedVersion": a.state(call).await.1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let terminal = a.state(call).await;
    assert!(terminal.0.is_terminal());
    // (A repeated cancel of the cancelled Call is the idempotent answer, not a conflict;
    // it changes nothing either — checked below.)
    for action in ["hold", "resume", "end"] {
        let r = send(
            Method::POST,
            format!("/v1/calls/{call}/{action}"),
            Some(json!({ "expectedVersion": terminal.1 })),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::CONFLICT,
            "{action} on a terminal call: {}",
            r.text
        );
    }
    let r = send(
        Method::POST,
        format!("/v1/calls/{call}/cancel"),
        Some(json!({ "expectedVersion": terminal.1 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await, terminal);
}

/// Scope × ownership: the owning tenant WITHOUT the scope is refused (403); another tenant
/// WITH the scope cannot see the resource (404, exactly like an unknown id); nothing
/// changes either way.
#[tokio::test]
async fn scope_and_ownership_are_both_required() {
    let Some(p) = provision().await else {
        return;
    };
    let h = p.h();
    let kind = p.d.e.a.w.kind;
    // Tenant B with every scope.
    let b = h.consumer("matrix-b", TenantClassification::External).await;
    for scope in ALL_TEST_SCOPES {
        h.platform
            .grant_application_scope(b.application.id, scope)
            .await
            .unwrap();
    }
    let b_full = credential(h, &b, kind, ALL_TEST_SCOPES).await;
    let b_full = b_full.secret.expose_once().to_string();
    let unknown = Uuid::new_v4();
    let cases: Vec<(Method, String, Value, &str)> = vec![
        (
            Method::PATCH,
            format!("/v1/enterprise/departments/{}", p.ids["department_id"]),
            json!({ "expectedVersion": 1, "name": "Taken" }),
            names::ENTERPRISE_CONFIG_WRITE,
        ),
        (
            Method::POST,
            format!("/v1/calls/{}/end", p.ids["call_id"]),
            json!({ "expectedVersion": 1 }),
            names::CALLS_WRITE,
        ),
        (
            Method::PATCH,
            format!("/v1/webhooks/{}", p.ids["webhook_id"]),
            json!({ "expectedVersion": 1, "status": "DISABLED" }),
            names::WEBHOOKS_MANAGE,
        ),
        (
            Method::POST,
            format!(
                "/v1/enterprise/voicemail/messages/{}/media-access",
                p.ids["message_id"]
            ),
            json!({}),
            names::ENTERPRISE_VOICEMAIL_READ,
        ),
        (
            Method::POST,
            format!(
                "/v1/enterprise/recordings/{}/media-access",
                p.ids["recording_id"]
            ),
            json!({}),
            names::ENTERPRISE_RECORDING_READ,
        ),
    ];
    let conn = h.conn().clone();
    for (method, path, body, scope) in cases {
        let scopes: Vec<&str> = ALL_TEST_SCOPES
            .iter()
            .copied()
            .filter(|s| *s != scope)
            .collect();
        let owner = p.d.credential(&scopes).await;
        let before = snapshot(&conn).await;
        // Owner without the scope.
        let r = h
            .request(
                method.clone(),
                &path,
                Some(owner.secret.expose_once()),
                Some(body.clone()),
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "owner without {scope}: {path}: {}",
            r.text
        );
        // Another tenant with the scope: indistinguishable from an unknown id.
        let r = h
            .request(method.clone(), &path, Some(&b_full), Some(body.clone()))
            .await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "other tenant: {path}: {}",
            r.text
        );
        let unknown_path = {
            let id = path
                .split('/')
                .find(|s| Uuid::parse_str(s).is_ok())
                .unwrap()
                .to_string();
            path.replace(&id, &unknown.to_string())
        };
        let u = h
            .request(
                method.clone(),
                &unknown_path,
                Some(&b_full),
                Some(body.clone()),
            )
            .await;
        assert_eq!(u.status, StatusCode::NOT_FOUND);
        assert_eq!(r.normalized_error(), u.normalized_error(), "{path}");
        assert_eq!(
            changed(&before, &snapshot(&conn).await),
            Vec::<String>::new(),
            "{path}"
        );
    }
}

/// The contract types every request body as a JSON OBJECT. A JSON array must be refused
/// like any other malformed body — it must never be read positionally as the object's
/// fields (`[<version>]` as `{"expectedVersion": <version>}`) and change anything.
#[tokio::test]
async fn request_bodies_must_be_json_objects() {
    let Some(p) = provision().await else {
        return;
    };
    let conn = p.h().conn().clone();
    let a = p.d.a();
    let mut accepted = Vec::new();
    let version_of = |path: String| {
        let p = &p;
        async move {
            p.h()
                .request(Method::GET, &path, Some(&p.full), None)
                .await
                .json["version"]
                .as_i64()
                .unwrap()
        }
    };
    let department = format!("/v1/enterprise/departments/{}", p.ids["department_id"]);
    let webhook = format!("/v1/webhooks/{}", p.ids["webhook_id"]);
    let call = p.ids["call_id"];
    let cases = vec![
        (
            Method::PATCH,
            department.clone(),
            json!([version_of(department.clone()).await]),
        ),
        (
            Method::PATCH,
            webhook.clone(),
            json!([version_of(webhook.clone()).await]),
        ),
        (
            Method::POST,
            format!("/v1/calls/{call}/cancel"),
            json!([a.state(call).await.1]),
        ),
    ];
    for (method, path, body) in cases {
        let before = snapshot(&conn).await;
        let r = p
            .h()
            .request(method.clone(), &path, Some(&p.full), Some(body.clone()))
            .await;
        let diff = changed(&before, &snapshot(&conn).await);
        if r.status != StatusCode::BAD_REQUEST || !diff.is_empty() {
            accepted.push(format!(
                "{method} {path} {body}: {} changed {diff:?}",
                r.status
            ));
        }
    }
    assert!(
        accepted.is_empty(),
        "array bodies were read positionally:\n{}",
        accepted.join("\n")
    );
}

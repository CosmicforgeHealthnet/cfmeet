//! Calling reads through the full chain (real mTLS, signed HQ request) against a fake
//! Calling that records every call: tenant scoping (the request's tenant is the owner's
//! `tenantId`; a non-UUID tenant names nothing), paths, window and cursor mapping, the
//! operator attribution, strict translation and minimization.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const T0: &str = "2026-11-01T10:00:00.123456+00:00";
const CURSOR: &str = "1795000000123456_0b7e7f0e2b9c4c8e9d7a1f2e3d4c5b6a";

/// (owner, path, query, operator) of one owner call.
type Recorded = (Owner, String, Vec<(String, String)>, String);

#[derive(Default)]
struct FakeCalling {
    calls: Mutex<Vec<Recorded>>,
}

fn page(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "nextCursor": next})
}

fn call_doc() -> Value {
    json!({"callId": ID, "applicationId": ID, "environmentId": ID, "callKind": "VOICE", "state": "CONNECTED",
           "origin": "APPLICATION", "participantCount": 2, "terminalReason": null, "failureCategory": null,
           "requestedAt": T0, "answeredAt": T0, "connectedAt": T0, "endedAt": null, "createdAt": T0, "version": 4,
           "sipUri": "sip:+2348030000000@carrier.example", "phoneNumber": "+2348030000000"})
}

#[async_trait]
impl OwnerPort for FakeCalling {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError> {
        self.calls.lock().unwrap().push((
            owner,
            path.to_string(),
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            operator.to_string(),
        ));
        let p = path.trim_start_matches("/internal/v1/operator");
        Ok(match p {
            "/calls" | "/live-calls" => page(vec![call_doc()], json!(CURSOR)),
            x if x.starts_with("/calls/") => {
                let mut d = call_doc();
                d["participants"] = json!([{"principalId": ID, "principalKind": "APP_USER", "role": "INITIATOR",
                    "isHost": true, "joinedAt": T0, "leftAt": null, "displayName": "Ada Lovelace"}]);
                d
            }
            "/abuse-reports" => page(
                vec![
                    json!({"abuseReportId": ID, "applicationId": ID, "environmentId": ID, "callId": ID,
                    "reportingPrincipalId": ID, "reportedPrincipalId": null, "category": "HARASSMENT",
                    "commentCode": null, "status": "OPEN", "createdAt": T0, "reviewedAt": null, "version": 1,
                    "comment": "free text the owner must never send"}),
                ],
                Value::Null,
            ),
            "/blocks" => page(
                vec![
                    json!({"blockRuleId": ID, "applicationId": ID, "environmentId": ID, "blockerKind": "PRINCIPAL",
                    "blockerPrincipalId": ID, "blockedKind": "CONTACT", "blockedPrincipalId": null,
                    "hasContactReference": true, "reasonCode": null, "status": "ACTIVE", "createdAt": T0,
                    "releasedAt": null, "version": 1, "blockedContactReference": ID}),
                ],
                Value::Null,
            ),
            "/limits" => json!({"tenantId": TENANT_ID, "currency": "NGN",
                "tenant": {"requestsPerMinute": 600, "maxConcurrentCommunications": 50,
                    "telecomSpendDailyLimitMinor": 500000, "telecomSpendMonthlyLimitMinor": null, "updatedAt": T0},
                "applications": [{"applicationId": ID, "requestsPerMinute": null, "maxConcurrentCommunications": 5,
                    "telecomSpendDailyLimitMinor": null, "telecomSpendMonthlyLimitMinor": null, "updatedAt": T0}],
                "truncated": false}),
            "/routing-policies" => {
                json!({"environments": [{"environmentId": ID, "applicationId": ID, "revision": 3,
                "updatedAt": T0, "allowedRouteClasses": ["APP_RTC"], "appFirst": true, "publicCallingEnabled": false,
                "callbackRelayEnabled": false, "requireVerifiedContacts": true, "maxFailoverAttempts": 1,
                "complianceRequiredCountries": ["NG"]}], "truncated": false})
            }
            "/enterprise-policies" => {
                json!({"policies": [{"policyId": ID, "environmentId": ID, "applicationId": ID,
                "name": "Default", "isDefault": true, "status": "ACTIVE", "internalCallingEnabled": true,
                "externalPublicCallingAllowed": false, "publicDestinationCountries": [], "forwardingAllowed": true,
                "publicForwardingAllowed": false, "voicemailAllowed": true, "transferAllowed": true,
                "adminCapabilities": ["MANAGE_ROUTING"], "version": 2, "updatedAt": T0, "disabledAt": null}],
                "truncated": true})
            }
            "/spend-policies" => {
                json!({"environments": [{"environmentId": ID, "applicationId": ID, "currency": "USD",
                "enabled": false, "legAllowanceMinor": 100, "maxLegSeconds": 600, "dailyLimitMinor": 10000,
                "monthlyLimitMinor": 200000, "maxConcurrentLegs": 2, "maxOriginationsPerMinute": 5, "revision": 1,
                "updatedAt": T0}], "truncated": false})
            }
            "/recordings" => page(
                vec![
                    json!({"recordingId": ID, "callId": ID, "environmentId": ID, "state": "READY",
                    "captureSource": "MEDIA_SERVER", "durationMs": 61000, "sizeBytes": 1024, "retentionUntil": T0,
                    "createdAt": T0, "deletedAt": null, "objectKey": "recordings/secret/path.ogg"}),
                ],
                Value::Null,
            ),
            "/voicemail" => page(
                vec![
                    json!({"messageId": ID, "callId": null, "mailboxId": ID, "environmentId": ID, "state": "READY",
                    "readState": "UNREAD", "durationMs": 9000, "sizeBytes": 512, "retentionUntil": null,
                    "receivedAt": T0, "deletedAt": null, "objectKey": "voicemail/secret.ogg"}),
                ],
                Value::Null,
            ),
            "/retention-holds" => page(
                vec![
                    json!({"holdId": ID, "scope": "PLATFORM", "dataClass": "*", "resourceId": null,
                    "environmentId": null, "reasonCode": "LEGAL_HOLD", "active": true, "createdAt": T0,
                    "releasedAt": null}),
                ],
                Value::Null,
            ),
            "/incidents" => page(
                vec![
                    json!({"incidentId": ID, "scope": "TENANT", "category": "TELEPHONY_COMMAND_DEAD",
                    "severity": "HIGH", "status": "OPEN", "callId": ID, "resourceType": "CALL", "resourceId": ID,
                    "occurrences": 3, "firstSeenAt": T0, "lastSeenAt": T0, "resolvedAt": null,
                    "restrictedEvidence": {"msisdn": "+2348030000000"}}),
                ],
                Value::Null,
            ),
            "/webhook-keys" => page(
                vec![
                    json!({"keyId": "whk-2026-11", "subscriptions": 4, "activeSubscriptions": 3, "lastUpdatedAt": T0}),
                ],
                Value::Null,
            ),
            "/audit" => page(
                vec![
                    json!({"eventId": ID, "actor": "cfmeet.operator-console", "action": "BLOCK_RELEASED",
                    "targetKind": "BLOCK_RULE", "targetId": ID, "applicationId": ID, "environmentId": ID,
                    "occurredAt": T0, "detail": {"note": "operator free text"}}),
                ],
                Value::Null,
            ),
            _ => return Err(OwnerError::NotFound),
        })
    }
}

async fn harness() -> (Harness, Arc<FakeCalling>) {
    let fake = Arc::new(FakeCalling::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn scoped_to(tenant: &'static str, purpose: &str) -> String {
    format!("market=GLOBAL&organization=cfmeet&zone=production&tenant={tenant}&purpose={purpose}")
}

fn calling(path: &str, extra: &str, permission: &str) -> Call {
    let mut c = call(
        path,
        &format!("{}{extra}", scoped_to(TENANT_ID, "abuse-review")),
        permission,
        &["cfmeet.trust_safety"],
    );
    c.tenant_scopes = vec![TENANT_ID];
    c
}

const WINDOW: &str = "&limit=5&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z";

fn last(fake: &FakeCalling) -> Recorded {
    fake.calls
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an owner call")
}

fn q(r: &Recorded) -> Vec<(&str, &str)> {
    r.2.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[tokio::test]
async fn calling_calls_are_tenant_scoped_and_minimized() {
    let (h, fake) = harness().await;
    let (status, v) = send(
        &h,
        &calling(
            "/calling/calls",
            &format!(
                "&limit=5&cursor={CURSOR}&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z"
            ),
            "calling.calls.read",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let rec = last(&fake);
    assert_eq!(
        (rec.0, rec.1.as_str(), rec.3.as_str()),
        (
            Owner::Calling,
            "/internal/v1/operator/calls",
            "hq:staff-0001"
        )
    );
    assert_eq!(
        q(&rec),
        [
            ("tenantId", TENANT_ID),
            ("limit", "5"),
            ("cursor", CURSOR),
            ("since", "2026-11-01T00:00:00Z"),
            ("until", "2026-11-10T00:00:00Z")
        ]
    );
    let item = &v["result"]["items"][0];
    assert_eq!(item["call_id"], ID);
    assert_eq!(item["created_at"], "2026-11-01T10:00:00Z");
    assert_eq!(v["result"]["next_cursor"], CURSOR);
    let text = v.to_string();
    assert!(
        !text.contains("sip:") && !text.contains("+234"),
        "no SIP URI or telephone number"
    );
}

#[tokio::test]
async fn calling_call_detail_lists_participants_by_id_only() {
    let (h, fake) = harness().await;
    let (status, v) = send(
        &h,
        &calling(&format!("/calling/calls/{ID}"), "", "calling.calls.read"),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let rec = last(&fake);
    assert_eq!(rec.1, format!("/internal/v1/operator/calls/{ID}"));
    assert_eq!(q(&rec), [("tenantId", TENANT_ID)]);
    let p = &v["result"]["participants"][0];
    assert_eq!(p["principal_id"], ID);
    assert!(
        !v.to_string().contains("Ada Lovelace"),
        "display names are never relayed"
    );
    // A malformed call id names nothing and never reaches the owner.
    let before = fake.calls.lock().unwrap().len();
    let (_, bad) = send(
        &h,
        &calling("/calling/calls/not-a-call", "", "calling.calls.read"),
    )
    .await;
    assert_eq!(code(&bad), "RESOURCE_NOT_FOUND");
    assert_eq!(fake.calls.lock().unwrap().len(), before);
}

#[tokio::test]
async fn calling_lists_translate_strictly() {
    let (h, fake) = harness().await;
    for (path, extra, perm, owner_path) in [
        (
            "/calling/live",
            "",
            "calling.live.read",
            "/internal/v1/operator/live-calls",
        ),
        (
            "/calling/abuse-reports",
            "",
            "calling.abuse-reports.read",
            "/internal/v1/operator/abuse-reports",
        ),
        (
            "/calling/retention-holds",
            "",
            "calling.retention-holds.read",
            "/internal/v1/operator/retention-holds",
        ),
        (
            "/calling/incidents",
            WINDOW,
            "calling.incidents.read",
            "/internal/v1/operator/incidents",
        ),
        (
            "/calling/webhook-keys",
            "",
            "calling.webhook-keys.read",
            "/internal/v1/operator/webhook-keys",
        ),
        (
            "/calling/audit",
            WINDOW,
            "calling.audit.read",
            "/internal/v1/operator/audit",
        ),
    ] {
        let (status, v) = send(&h, &calling(path, extra, perm)).await;
        assert_eq!(status, 200, "{path}: {v}");
        assert_eq!(last(&fake).1, owner_path);
        assert_eq!(v["result"]["items"].as_array().unwrap().len(), 1, "{path}");
        let text = v.to_string();
        for leak in [
            "free text",
            "+234",
            "msisdn",
            "restrictedEvidence",
            "detail",
            "operator free text",
        ] {
            assert!(!text.contains(leak), "{path} relayed {leak}");
        }
    }
    // A cursor that is not Calling's form never reaches the owner.
    let before = fake.calls.lock().unwrap().len();
    let (_, bad) = send(
        &h,
        &calling(
            "/calling/abuse-reports",
            "&cursor=abc",
            "calling.abuse-reports.read",
        ),
    )
    .await;
    assert_eq!(code(&bad), "QUERY_INVALID");
    assert_eq!(fake.calls.lock().unwrap().len(), before);
}

#[tokio::test]
async fn calling_blocks_never_name_a_contact() {
    let (h, _) = harness().await;
    let (status, v) = send(&h, &calling("/calling/blocks", "", "calling.blocks.read")).await;
    assert_eq!(status, 200, "{v}");
    let b = &v["result"]["items"][0];
    assert_eq!(b["has_contact_reference"], true);
    assert!(b.get("blocked_contact_reference").is_none());
    assert_eq!(
        v.to_string().matches(ID).count(),
        4,
        "only the expected opaque ids"
    );
}

#[tokio::test]
async fn calling_configuration_reads_are_bounded() {
    let (h, fake) = harness().await;
    let (status, v) = send(&h, &calling("/calling/limits", "", "calling.limits.read")).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        q(&last(&fake)),
        [("tenantId", TENANT_ID)],
        "a singleton read takes the tenant only"
    );
    assert_eq!(v["result"]["currency"], "NGN");
    assert_eq!(
        v["result"]["tenant"]["telecom_spend_daily_limit_minor"],
        500000
    );
    let (_, v) = send(&h, &calling("/calling/routing", "", "calling.routing.read")).await;
    assert_eq!(
        v["result"]["environments"][0]["allowed_route_classes"],
        json!(["APP_RTC"])
    );
    let (_, v) = send(
        &h,
        &calling(
            "/calling/enterprise-features",
            "",
            "calling.enterprise-features.read",
        ),
    )
    .await;
    assert_eq!(v["result"]["truncated"], true);
    assert_eq!(
        v["result"]["policies"][0]["admin_capabilities"],
        json!(["MANAGE_ROUTING"])
    );
    let (status, v) = send(
        &h,
        &calling("/calling/spend-limits", "", "calling.spend-limits.read"),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["environments"][0]["currency"], "USD");
}

#[tokio::test]
async fn calling_media_reads_are_metadata_only() {
    let (h, fake) = harness().await;
    for (path, perm, owner_path) in [
        (
            "/calling/recordings",
            "calling.recordings.read",
            "/internal/v1/operator/recordings",
        ),
        (
            "/calling/voicemail",
            "calling.voicemail.read",
            "/internal/v1/operator/voicemail",
        ),
    ] {
        let (status, v) = send(&h, &calling(path, WINDOW, perm)).await;
        assert_eq!(status, 200, "{path}: {v}");
        assert_eq!(last(&fake).1, owner_path);
        assert!(
            !v.to_string().contains("secret"),
            "{path}: no object key or media URL"
        );
    }
}

#[tokio::test]
async fn a_tenant_that_is_not_a_platform_access_id_names_nothing() {
    let (h, fake) = harness().await;
    // The default harness tenant ("cfmeet-first-party") is enrolled but is not a tenant id.
    let c = call(
        "/calling/live",
        &scoped("abuse-review"),
        "calling.live.read",
        &["cfmeet.trust_safety"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!((status, code(&v)), (404, "RESOURCE_NOT_FOUND"));
    assert!(fake.calls.lock().unwrap().is_empty(), "no owner call");
}

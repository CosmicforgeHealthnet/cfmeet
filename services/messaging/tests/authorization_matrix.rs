//! Part V K remediation — the per-route authorization / IDOR matrix of the Messaging API.
//!
//! EVERY customer operation of `cfmeet.messaging.http-api` (the OpenAPI document is
//! proven elsewhere to list exactly the registered routes; `every_operation_is_in_the_matrix`
//! proves this matrix lists exactly the document's operations) is executed against the
//! real router and a real PostgreSQL database:
//!
//! 1. a fresh tenant fixture is built (MANAGED and PRIVATE_E2EE conversations with real
//!    OpenMLS client devices, a pending MLS commit and Welcome, attachments in object
//!    storage, a legal hold, an export, a report, a client session, a push registration,
//!    an SMS bridge, ...);
//! 2. every APPLICABLE denial case is sent with the SAME path, query and body as the
//!    positive call (so a refusal comes from authorization, never from validation):
//!    missing / invalid / revoked credential, a client token on a customer route,
//!    insufficient scope, another tenant's resource ids (IDOR) with that tenant's own
//!    principal, another tenant's credential acting as our principal, a non-member, a
//!    removed member, a banned member, an inactive principal, a credential restricted to
//!    other principals, a plain member on an administrator operation, another
//!    principal's device, an acting principal on an administration-only operation;
//! 3. each denial must have the expected status and error code, must not echo any of the
//!    fixture's identifiers or content, and must leave EVERY table unchanged (a
//!    fingerprint of all rows; only `messaging_rate_windows` is excluded — a refused
//!    request is still counted against its rate window by design);
//! 4. finally the positive call is made by the authorized principal and must succeed —
//!    proof that the denials above were not validation failures.
//!
//! Cases a route cannot express are skipped with the reason recorded in `Op::skip`, and
//! the per-case outcome is printed as `MATRIX` lines (the evidence table).

mod common;

use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use axum::http::Method;
use cf_messaging_crypto::{
    device::Device,
    history_transfer::{self as ht, TransferContext, TransferKeys},
};
use cf_object_storage::{sha256_hex, MemoryObjectStore, SignedWrite};
use cf_service_auth::DelegatedPlatformContext;
use common::{flows::*, realtime::test_settings, *};
use messaging::{
    attachments::scanner::{AttachmentScanner, ScanOutcome},
    identity::PrincipalStatus,
    realtime::{
        broker::LocalBroker, ephemeral::MemoryPresence, push::LocalTestPush,
        token::ClientTokenKeys, Realtime,
    },
    sms_bridge::{SmsTransport, TransportError, DISCLOSURE_VERSION},
};
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

// ------------------------------------------------------------------ peers in-process

struct CleanScanner;

#[async_trait]
impl AttachmentScanner for CleanScanner {
    fn engine(&self) -> &'static str {
        "matrix-clean"
    }
    async fn scan(&self, _bytes: &[u8]) -> ScanOutcome {
        ScanOutcome::Clean
    }
}

/// A scripted SMS Gateway (the transport is not under test here).
struct Gateway;

#[async_trait]
impl SmsTransport for Gateway {
    async fn call(
        &self,
        _: &DelegatedPlatformContext,
        _: Method,
        path: &str,
        _: Option<&Value>,
    ) -> Result<(u16, Value), TransportError> {
        if path == "/internal/v1/sms-bridge/bindings" {
            return Ok((
                201,
                json!({ "bindingId": Uuid::new_v4(), "remote": { "masked": "+*******3456" }, "status": "ACTIVE" }),
            ));
        }
        if path.ends_with("/close") {
            return Ok((200, json!({ "status": "CLOSED" })));
        }
        if path == "/internal/v1/sms-bridge/messages" {
            return Ok((
                202,
                json!({ "smsMessageId": Uuid::new_v4(), "status": "AUTHORIZED", "segmentCount": 1 }),
            ));
        }
        Ok((200, json!({ "status": "DELIVERED", "segmentCount": 1 })))
    }
}

async fn harness() -> Option<(Harness, Arc<MemoryObjectStore>)> {
    let store = Arc::new(MemoryObjectStore::new());
    let s2 = store.clone();
    let realtime = Arc::new(Realtime::new(
        test_settings(),
        Some(ClientTokenKeys::generate()),
        Arc::new(LocalBroker::default()),
        Arc::new(MemoryPresence::default()),
        Arc::new(LocalTestPush::default()),
    ));
    let h = Harness::build(move |s| {
        s.storage = s2;
        s.scanner = Arc::new(CleanScanner);
        s.sms = Arc::new(Gateway);
        s.realtime = realtime;
    })
    .await?;
    Some((h, store))
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89";

fn png(n: usize) -> Vec<u8> {
    let mut b = PNG.to_vec();
    b.extend((0..n).map(|i| (i % 251) as u8));
    b
}

fn write_of(upload: &Value) -> SignedWrite {
    SignedWrite {
        method: "PUT",
        url: upload["url"].as_str().unwrap().to_string(),
        headers: upload["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["name"].as_str().unwrap().to_string(),
                    h["value"].as_str().unwrap().to_string(),
                )
            })
            .collect(),
        expires_at: chrono::DateTime::parse_from_rfc3339(upload["expiresAt"].as_str().unwrap())
            .unwrap()
            .into(),
    }
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str()
        .unwrap_or_else(|| panic!("{k} missing in {v}"))
        .to_string()
}

fn ok(r: &Response, what: &str) {
    assert!(
        r.status.is_success(),
        "fixture {what}: {} {}",
        r.status,
        r.text
    );
}

// ------------------------------------------------------------------------- fixture

/// One tenant (A) with every resource the matrix touches, plus another tenant (B).
struct Fx {
    a: Customer,
    b: Customer,
    /// B's own principal.
    b_actor: Uuid,
    owner: Member,
    member: Member,
    /// A second plain MEMBER (target of administrator operations).
    member2: Uuid,
    /// ACTIVE principal of A's environment, never a member.
    outsider: Uuid,
    removed: Uuid,
    banned: Uuid,
    blocked: Uuid,
    /// Fresh principal with a pending device registration challenge.
    newbie: Uuid,
    register_body: Value,
    conv: String,
    msg: String,
    member_msg: String,
    bridged_msg: String,
    canary: String,
    attachment: String,
    pending_attachment: String,
    /// E2EE conversation whose add-member commit is built but not yet posted.
    e2ee_a: String,
    commit_body: Value,
    /// E2EE conversation with a posted commit and a PENDING Welcome for `member`.
    e2ee_b: String,
    welcome: String,
    hold: String,
    export: String,
    deletion: String,
    report: String,
    session: String,
    bridge: String,
    /// The conversation the fixture bridge belongs to.
    sms_conv: String,
    /// A second TELEPHONE_ENDPOINT party (not yet bridged).
    party2: Uuid,
    policy_version: i64,
    conv_version: i64,
    /// History transfers of the owner's devices (ADR 0052 AD-07), one per operation.
    transfer_create_body: Value,
    transfer_requested: String,
    transfer_seal_body: Value,
    transfer_download: String,
    transfer_complete: String,
    transfer_cancel: String,
}

impl Fx {
    /// Every identifier / secret of tenant A a denial must never echo.
    fn secrets(&self) -> Vec<String> {
        let mut v = vec![
            self.conv.clone(),
            self.msg.clone(),
            self.member_msg.clone(),
            self.canary.clone(),
            self.attachment.clone(),
            self.e2ee_a.clone(),
            self.e2ee_b.clone(),
            self.hold.clone(),
            self.export.clone(),
            self.report.clone(),
            self.session.clone(),
            self.bridge.clone(),
            self.transfer_requested.clone(),
            self.transfer_download.clone(),
            self.transfer_complete.clone(),
            self.transfer_cancel.clone(),
            self.owner.device_id().to_string(),
            self.member.principal.to_string(),
        ];
        v.retain(|x| !x.is_empty());
        v
    }
}

/// A signed history-transfer request from `target` for history held by `source`.
fn transfer_request(
    h: &Harness,
    a: &Customer,
    principal: Uuid,
    source: &Device,
    target: &Device,
) -> (Value, TransferContext, TransferKeys) {
    let id = Uuid::new_v4();
    let tc = TransferContext {
        transfer_id: id,
        tenant_id: a.context.scope.tenant_id,
        application_id: a.context.scope.application_id,
        environment_id: a.context.scope.environment_id,
        principal_id: principal,
        source_device_id: source.device_id(),
        target_device_id: target.device_id(),
    };
    let keys = TransferKeys::generate().unwrap();
    let exp = h.now().timestamp() + 1800;
    let sig = ht::sign_request(target, &tc, &keys, exp).unwrap();
    let body = json!({ "transferId": id, "sourceDeviceId": source.device_id(), "targetDeviceId": target.device_id(),
        "transferPublicKey": b64(keys.public_key()),
        "expiresAt": chrono::DateTime::from_timestamp(exp, 0).unwrap().to_rfc3339(),
        "requestSignature": b64(&sig) });
    (body, tc, keys)
}

/// A MANAGED group: owner OWNER, `members` MEMBER, `removed` REMOVED, `banned` BANNED.
async fn shaped_conversation(
    h: &Harness,
    a: &Customer,
    owner: Uuid,
    members: &[Uuid],
    removed: Uuid,
    banned: Uuid,
) -> String {
    let r = h
        .create_conversation(
            a,
            owner,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    ok(&r, "managed conversation");
    let conv = s(&r.json, "conversationId");
    for p in members.iter().copied().chain([removed, banned]) {
        let r = h
            .post(
                a,
                Some(owner),
                &format!("/v1/conversations/{conv}/members"),
                json!({ "principalId": p }),
            )
            .await;
        ok(&r, "add member");
    }
    let r = h
        .send(
            Method::DELETE,
            Some(&a.token),
            Some(owner),
            &format!("/v1/conversations/{conv}/members/{removed}"),
            None,
            &[],
        )
        .await;
    ok(&r, "remove member");
    let r = h
        .post(
            a,
            Some(owner),
            &format!("/v1/conversations/{conv}/bans"),
            json!({ "principalId": banned, "reason": "SPAM" }),
        )
        .await;
    ok(&r, "ban");
    conv
}

async fn fixture(h: &Harness, store: &MemoryObjectStore) -> Fx {
    let a = h.customer(ALL_SCOPES);
    let b = h.customer(ALL_SCOPES);
    let b_actor = h.principal(&b);
    h.set_policy(
        &a,
        json!({
            "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED", "exportsEnabled": true } },
            "attachments": { "enabled": true, "managedScanRequired": false }
        }),
    )
    .await;
    let mut owner = h.member(&a, 3).await;
    let mut member = h.member(&a, 4).await;
    let outsider = h.principal(&a);
    let member2 = h.principal(&a);
    let removed = h.principal(&a);
    let banned = h.principal(&a);
    let blocked = h.principal(&a);
    let canary = format!("canary-{}", Uuid::new_v4().simple());

    let conv = shaped_conversation(
        h,
        &a,
        owner.principal,
        &[member.principal, member2],
        removed,
        banned,
    )
    .await;
    let sms_conv =
        shaped_conversation(h, &a, owner.principal, &[member.principal], removed, banned).await;
    let conv_v = json!({ "conversationId": conv });
    let r = h.send_managed(&a, &conv_v, owner.principal, &canary).await;
    ok(&r, "owner message");
    let msg = s(&r.json, "messageId");
    let r = h
        .send_managed(&a, &conv_v, member.principal, "member says hi")
        .await;
    ok(&r, "member message");
    let member_msg = s(&r.json, "messageId");
    for path in [
        format!("/v1/conversations/{conv}/messages/{msg}/reactions/:wave:"),
        format!("/v1/conversations/{conv}/pins/{msg}"),
    ] {
        let r = h
            .send(
                Method::PUT,
                Some(&a.token),
                Some(owner.principal),
                &path,
                Some(json!({})),
                &[],
            )
            .await;
        ok(&r, "reaction / pin");
    }
    let r = h
        .post(
            &a,
            Some(owner.principal),
            "/v1/messaging/blocks",
            json!({ "principalId": blocked }),
        )
        .await;
    ok(&r, "block");

    // Attachments: one READY, one uploaded but not finalized.
    let mut attachment_ids = Vec::new();
    for finalize in [true, false] {
        // The finalized one is a real image (it gets a server thumbnail).
        let bytes = if finalize {
            let mut b = Vec::new();
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                32,
                32,
                image::Rgb([1, 2, 3]),
            ))
            .write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png)
            .unwrap();
            b
        } else {
            png(256)
        };
        let r = h
            .post(
                &a,
                Some(owner.principal),
                &format!("/v1/conversations/{conv}/attachments"),
                json!({ "kind": "IMAGE", "size": bytes.len(), "sha256": sha256_hex(&bytes),
                        "mediaType": "image/png", "fileName": "a.png" }),
            )
            .await;
        ok(&r, "attachment intent");
        store
            .upload_signed(&write_of(&r.json["upload"]), h.now(), bytes)
            .unwrap();
        let id = s(&r.json, "attachmentId");
        if finalize {
            let f = h
                .post(
                    &a,
                    Some(owner.principal),
                    &format!("/v1/conversations/{conv}/attachments/{id}/finalize"),
                    json!({}),
                )
                .await;
            ok(&f, "finalize");
            assert_eq!(f.json["state"], "READY", "{}", f.text);
        }
        attachment_ids.push(id);
    }
    // Server-side derivatives (the finalized MANAGED image's thumbnail).
    let report = messaging::attachments::lifecycle::run_once(&h.state)
        .await
        .unwrap();
    assert_eq!(report.derivatives_ready, 1, "{report:?}");

    // SMS bridge (TELEPHONE_ENDPOINT party) and a bridged message.
    let party = h.principal_of_kind(&a, "TELEPHONE_ENDPOINT");
    let r = h
        .post(
            &a,
            Some(owner.principal),
            &format!("/v1/conversations/{sms_conv}/sms-bridges"),
            json!({ "numberResourceId": Uuid::new_v4(), "to": "+15550123456", "externalPrincipalId": party,
                    "disclosureVersion": DISCLOSURE_VERSION, "smsDisclosureAcknowledged": true }),
        )
        .await;
    ok(&r, "sms bridge");
    let bridge = s(&r.json, "bridgeId");
    let r = h
        .send_managed(
            &a,
            &json!({ "conversationId": sms_conv }),
            owner.principal,
            "bridged hello",
        )
        .await;
    ok(&r, "bridged message");
    let bridged_msg = s(&r.json, "messageId");

    // E2EE conversation A: group created, add commit built, NOT posted.
    let mut e2ee = Vec::new();
    for _ in 0..2 {
        let r = h
            .create_conversation(
                &a,
                owner.principal,
                json!({ "contextClass": "GROUP", "securityProfile": "PRIVATE_E2EE" }),
            )
            .await;
        ok(&r, "e2ee conversation");
        let id = s(&r.json, "conversationId");
        let r2 = h
            .post(
                &a,
                Some(owner.principal),
                &format!("/v1/conversations/{id}/members"),
                json!({ "principalId": member.principal }),
            )
            .await;
        ok(&r2, "e2ee member");
        e2ee.push(r.json);
    }
    let mut commits = Vec::new();
    for conv in &e2ee {
        let cid = s(conv, "conversationId");
        let gid = unb64(&conv["encryption"]["mlsGroupId"]);
        let mut group = owner.device.create_group(&gid).unwrap();
        let claim = h
            .post(
                &a,
                Some(owner.principal),
                &format!("/v1/conversations/{cid}/mls/key-package-claims"),
                json!({ "principalIds": [member.principal] }),
            )
            .await;
        ok(&claim, "claim");
        let packages: Vec<Vec<u8>> = claim.json["keyPackages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| unb64(&k["keyPackage"]))
            .collect();
        let out = owner.device.add_devices(&mut group, &packages).unwrap();
        commits.push(json!({
            "clientMessageId": Uuid::new_v4(),
            "deviceId": owner.device_id(),
            "epoch": out.epoch,
            "commit": b64(&out.commit),
            "removedDeviceIds": [],
            "welcome": b64(out.welcome.as_deref().unwrap()),
            "welcomeDeviceIds": [member.device_id()],
        }));
    }
    let e2ee_a = s(&e2ee[0], "conversationId");
    let e2ee_b = s(&e2ee[1], "conversationId");
    let r = h
        .post(
            &a,
            Some(owner.principal),
            &format!("/v1/conversations/{e2ee_b}/mls/commits"),
            commits[1].clone(),
        )
        .await;
    ok(&r, "e2ee_b commit");
    let w = h
        .get(
            &a,
            Some(member.principal),
            &format!("/v1/messaging/devices/{}/welcomes", member.device_id()),
        )
        .await;
    ok(&w, "welcomes");
    let welcome = w.json["welcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["conversationId"] == json!(e2ee_b))
        .map(|x| s(x, "welcomeId"))
        .expect("pending welcome");
    owner.cursor = 0;
    member.cursor = 0;

    // Compliance / moderation records.
    let r = h
        .post(
            &a,
            None,
            "/v1/messaging/legal-holds",
            json!({ "scopeKind": "ENVIRONMENT", "reasonCategory": "LITIGATION" }),
        )
        .await;
    ok(&r, "legal hold");
    let hold = s(&r.json, "holdId");
    let r = h
        .post(
            &a,
            None,
            "/v1/messaging/exports",
            json!({ "conversationId": conv }),
        )
        .await;
    ok(&r, "export");
    let export = s(&r.json, "exportId");
    messaging::compliance::run_once(&h.state).await.unwrap();
    let r = h
        .post(
            &a,
            None,
            "/v1/messaging/deletion-requests",
            json!({ "kind": "PRINCIPAL_ERASURE", "principalId": h.principal(&a) }),
        )
        .await;
    ok(&r, "deletion request");
    let deletion = s(&r.json, "requestId");
    let r = h
        .post(
            &a,
            Some(member.principal),
            &format!("/v1/conversations/{conv}/reports"),
            json!({ "category": "SPAM" }),
        )
        .await;
    ok(&r, "report");
    let report = s(&r.json, "reportId");

    // A client session of the owner's device and a push registration.
    let r = h
        .post(
            &a,
            Some(owner.principal),
            "/v1/messaging/client-tokens",
            json!({ "deviceId": owner.device_id() }),
        )
        .await;
    ok(&r, "client token");
    let session = s(&r.json, "sessionId");
    let r = h
        .send(
            Method::PUT,
            Some(&a.token),
            Some(owner.principal),
            &format!("/v1/messaging/devices/{}/push-registration", owner.device_id()),
            Some(json!({ "platform": "LOCAL_TEST", "handle": format!("handle-{}", Uuid::new_v4().simple()) })),
            &[],
        )
        .await;
    ok(&r, "push registration");

    // A pending device registration for a fresh principal.
    let newbie = h.principal(&a);
    let ch = h.challenge(&a, newbie).await;
    ok(&ch, "challenge");
    let device = Device::generate(uuid(&ch.json["deviceId"])).unwrap();
    let nonce: [u8; 32] = hex::decode(s(&ch.json, "nonce"))
        .unwrap()
        .try_into()
        .unwrap();
    let kp = device
        .registration_key_package(uuid(&ch.json["challengeId"]), &nonce)
        .unwrap();
    let register_body = json!({ "challengeId": ch.json["challengeId"], "keyPackage": b64(&kp), "displayLabel": "matrix" });

    let pv = h.get(&a, None, "/v1/messaging/policy").await;
    ok(&pv, "policy");
    let policy_version = pv.json["version"].as_i64().unwrap();
    let cv = h
        .get(
            &a,
            Some(owner.principal),
            &format!("/v1/conversations/{conv}"),
        )
        .await;
    ok(&cv, "conversation");
    let conv_version = cv.json["version"].as_i64().unwrap();
    let party2 = h.principal_of_kind(&a, "TELEPHONE_ENDPOINT");

    // History transfers between the owner's devices: one open transfer per target device.
    let mut targets = Vec::new();
    for _ in 0..5 {
        targets.push(h.active_device(&a, owner.principal).await);
    }
    let transfer = |i: usize, seal: bool| {
        let (body, tc, keys) = transfer_request(h, &a, owner.principal, &owner.device, &targets[i]);
        let sealed =
            ht::seal_history(&owner.device, &tc, keys.public_key(), b"matrix history").unwrap();
        (
            body,
            json!({ "sealedHistory": b64(&sealed) }),
            seal,
            tc.transfer_id.to_string(),
        )
    };
    let mut transfer_ids = Vec::new();
    let mut seal_body = Value::Null;
    for (i, seal) in [(0, false), (1, true), (2, true), (3, false)] {
        let (body, sealed, seal, id) = transfer(i, seal);
        let r = h
            .post(
                &a,
                Some(owner.principal),
                "/v1/messaging/history-transfers",
                body,
            )
            .await;
        ok(&r, "history transfer");
        if seal {
            let r = h
                .put(
                    &a,
                    Some(owner.principal),
                    &format!("/v1/messaging/history-transfers/{id}/sealed-history"),
                    sealed,
                )
                .await;
            ok(&r, "sealed history");
        } else if i == 0 {
            seal_body = sealed;
        }
        transfer_ids.push(id);
    }
    let (transfer_create_body, _, _, _) = transfer(4, false);
    Fx {
        transfer_create_body,
        transfer_requested: transfer_ids[0].clone(),
        transfer_seal_body: seal_body,
        transfer_download: transfer_ids[1].clone(),
        transfer_complete: transfer_ids[2].clone(),
        transfer_cancel: transfer_ids[3].clone(),
        party2,
        sms_conv,
        policy_version,
        conv_version,
        a,
        b,
        b_actor,
        owner,
        member,
        member2,
        outsider,
        removed,
        banned,
        blocked,
        newbie,
        register_body,
        conv,
        msg,
        member_msg,
        bridged_msg,
        canary,
        attachment: attachment_ids[0].clone(),
        pending_attachment: attachment_ids[1].clone(),
        e2ee_a,
        commit_body: commits[0].clone(),
        e2ee_b,
        welcome,
        hold,
        export,
        deletion,
        report,
        session,
        bridge,
    }
}

// ------------------------------------------------------------------------ the table

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Acting {
    /// The operation acts for a principal (`CF-Acting-Principal` required).
    Required,
    /// Application-level operation: the header is never read.
    Ignored,
    /// Administration-only: an acting principal is refused.
    Forbidden,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Res {
    /// Environment-level: only the environment scopes it.
    Env,
    /// A conversation resource: membership required (`admin`: OWNER / ADMIN only).
    Conversation { admin: bool },
    /// A device resource of the acting principal.
    Device,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
enum Case {
    NoCredential,
    InvalidCredential,
    RevokedCredential,
    ClientTokenOnCustomerRoute,
    InsufficientScope,
    CrossTenantIds,
    CrossTenantActingAsOurPrincipal,
    NonMember,
    RemovedMember,
    BannedMember,
    InactivePrincipal,
    RestrictedCredential,
    PlainMemberOnAdminOperation,
    OtherPrincipalsDevice,
    ActingOnAdministrationOnly,
}

struct Req {
    path: String,
    actor: Option<Uuid>,
    body: Option<Value>,
    headers: Vec<(&'static str, String)>,
}

struct Op {
    method: Method,
    /// OpenAPI path template (the coverage key).
    template: &'static str,
    /// Scopes the operation needs (a credential missing them is refused).
    scopes: &'static [&'static str],
    acting: Acting,
    res: Res,
    /// The positive call (path, actor, body).
    req: fn(&Fx) -> Req,
    /// Expected positive status.
    ok: u16,
    /// Cases that do not apply, with the reason (recorded in the evidence).
    skip: &'static [(Case, &'static str)],
    /// Case-specific expected (status, code) where it differs from the default rule.
    expect: &'static [(Case, u16, &'static str)],
}

fn r(path: String, actor: Option<Uuid>, body: Option<Value>) -> Req {
    Req {
        path,
        actor,
        body,
        headers: vec![],
    }
}

const SEND: &[&str] = &["messages.send"];
const READ: &[&str] = &["messages.read"];
const MANAGE: &[&str] = &["conversations.manage"];
const CREATE: &[&str] = &["conversations.create"];
const AW: &[&str] = &["attachments.write"];
const AR: &[&str] = &["attachments.read"];
const MOD: &[&str] = &["moderation.manage"];
const EXPORT: &[&str] = &["messaging.export"];
const USAGE: &[&str] = &["usage.read"];

const CONV: Res = Res::Conversation { admin: false };
const ADMIN: Res = Res::Conversation { admin: true };
const AM: Acting = Acting::Required;
const AI: Acting = Acting::Ignored;
const AF: Acting = Acting::Forbidden;

const NO_SKIP: &[(Case, &str)] = &[];
const NO_EXPECT: &[(Case, u16, &str)] = &[];

fn ops() -> Vec<Op> {
    use Method as M;
    vec![
        // ---------------------------------------------------------------- devices
        Op { method: M::POST, template: "/v1/messaging/devices/registration-challenges", scopes: SEND, acting: AM, res: Res::Env,
             req: |f| r("/v1/messaging/devices/registration-challenges".into(), Some(f.owner.principal), Some(json!({}))),
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/devices", scopes: SEND, acting: AM, res: Res::Env,
             req: |f| r("/v1/messaging/devices".into(), Some(f.newbie), Some(f.register_body.clone())),
             ok: 201, skip: NO_SKIP,
             // The challenge is bound to (environment, credential, principal): it IS the
             // authorization of a registration; another binding never matches it.
             expect: &[(Case::CrossTenantIds, 409, "CHALLENGE_INVALID"),
                       (Case::RestrictedCredential, 403, "PRINCIPAL_NOT_AUTHORIZED")] },
        Op { method: M::POST, template: "/v1/messaging/devices/{deviceId}/key-packages", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/devices/{}/key-packages", f.owner.device_id()), Some(f.owner.principal),
                        Some(json!({ "keyPackages": [b64(&f.owner.device.key_package().unwrap())] }))),
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/devices/{deviceId}/revoke", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/devices/{}/revoke", f.owner.device_id()), Some(f.owner.principal),
                        Some(json!({ "reason": "LOST_OR_STOLEN" }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/principals/{principalId}/devices", scopes: READ, acting: AM, res: Res::Env,
             req: |f| r(format!("/v1/messaging/principals/{}/devices", f.member.principal), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP,
             // Another environment's principal is unknown in this one.
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::GET, template: "/v1/messaging/key-transparency/proofs", scopes: READ, acting: AM, res: Res::Env,
             req: |f| r(format!("/v1/messaging/key-transparency/proofs?deviceId={}", f.member.device_id()), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP,
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        // ------------------------------------------------ history transfers (AD-07)
        Op { method: M::POST, template: "/v1/messaging/history-transfers", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r("/v1/messaging/history-transfers".into(), Some(f.owner.principal), Some(f.transfer_create_body.clone())),
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/history-transfers", scopes: READ, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/history-transfers?deviceId={}", f.owner.device_id()), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/history-transfers/{transferId}", scopes: READ, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/history-transfers/{}", f.transfer_requested), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PUT, template: "/v1/messaging/history-transfers/{transferId}/sealed-history", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/history-transfers/{}/sealed-history", f.transfer_requested), Some(f.owner.principal),
                        Some(f.transfer_seal_body.clone())),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/history-transfers/{transferId}/sealed-history", scopes: READ, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/history-transfers/{}/sealed-history", f.transfer_download), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/history-transfers/{transferId}/complete", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/history-transfers/{}/complete", f.transfer_complete), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/history-transfers/{transferId}/cancel", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/history-transfers/{}/cancel", f.transfer_cancel), Some(f.owner.principal), Some(json!({ "reason": "MATRIX" }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // ---------------------------------------------------------- conversations
        Op { method: M::POST, template: "/v1/conversations", scopes: CREATE, acting: AM, res: Res::Env,
             req: |f| Req { path: "/v1/conversations".into(), actor: Some(f.owner.principal),
                            body: Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
                            headers: vec![("idempotency-key", idem())] },
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations", scopes: READ, acting: AM, res: Res::Env,
             req: |f| r("/v1/conversations".into(), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PATCH, template: "/v1/conversations/{conversationId}", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}", f.conv), Some(f.owner.principal),
                        Some(json!({ "expectedVersion": f.conv_version, "lifecycleState": "LOCKED" }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/policy", scopes: MANAGE, acting: AI, res: Res::Env,
             req: |_| r("/v1/messaging/policy".into(), None, None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PUT, template: "/v1/messaging/policy", scopes: MANAGE, acting: AI, res: Res::Env,
             req: |f| r("/v1/messaging/policy".into(), None,
                        Some(json!({ "expectedVersion": f.policy_version, "policy": { "contexts": { "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } } }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // ------------------------------------------------------------- membership
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/members", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/members", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/members", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/members", f.conv), Some(f.owner.principal),
                        Some(json!({ "principalId": f.outsider }))),
             ok: 201, skip: NO_SKIP,
             // The added principal is resolved in the CALLER's environment first.
             expect: &[(Case::CrossTenantIds, 403, "PRINCIPAL_NOT_AUTHORIZED")] },
        Op { method: M::DELETE, template: "/v1/conversations/{conversationId}/members/{principalId}", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/members/{}", f.conv, f.member2), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PATCH, template: "/v1/conversations/{conversationId}/members/{principalId}", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/members/{}", f.conv, f.member2), Some(f.owner.principal),
                        Some(json!({ "role": "ADMIN" }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/bans", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/bans", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/bans", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/bans", f.conv), Some(f.owner.principal),
                        Some(json!({ "principalId": f.member2, "reason": "SPAM" }))),
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::DELETE, template: "/v1/conversations/{conversationId}/bans/{principalId}", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/bans/{}", f.conv, f.banned), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // --------------------------------------------------------------- messages
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/messages", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/messages", scopes: SEND, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages", f.conv), Some(f.owner.principal),
                        Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "matrix send" }))),
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/messages/{messageId}", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages/{}", f.conv, f.msg), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PATCH, template: "/v1/conversations/{conversationId}/messages/{messageId}", scopes: SEND, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages/{}", f.conv, f.msg), Some(f.owner.principal),
                        Some(json!({ "text": "edited by the sender" }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::DELETE, template: "/v1/conversations/{conversationId}/messages/{messageId}", scopes: SEND, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages/{}", f.conv, f.msg), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PUT, template: "/v1/conversations/{conversationId}/read-cursor", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/read-cursor", f.conv), Some(f.owner.principal), Some(json!({ "sequence": 1 }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/events", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/events", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // -------------------------------------------------------------------- MLS
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/mls/commits", scopes: SEND, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/mls/commits", f.e2ee_a), Some(f.owner.principal), Some(f.commit_body.clone())),
             ok: 201, skip: &[(Case::RemovedMember, "the E2EE fixture conversation has no removed member (covered on the MANAGED routes and in tests/messaging.rs)"),
                              (Case::BannedMember, "the E2EE fixture conversation has no banned member (covered on the MANAGED routes)")],
             expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/mls/state", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/mls/state", f.e2ee_b), Some(f.owner.principal), None),
             ok: 200, skip: &[(Case::RemovedMember, "E2EE fixture: see mls/commits"), (Case::BannedMember, "E2EE fixture: see mls/commits")],
             expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/mls/key-package-claims", scopes: SEND, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/mls/key-package-claims", f.e2ee_b), Some(f.owner.principal),
                        Some(json!({ "principalIds": [f.member.principal] }))),
             ok: 200, skip: &[(Case::RemovedMember, "E2EE fixture: see mls/commits"), (Case::BannedMember, "E2EE fixture: see mls/commits")],
             expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/devices/{deviceId}/welcomes", scopes: READ, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/devices/{}/welcomes", f.member.device_id()), Some(f.member.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/devices/{deviceId}/welcomes/{welcomeId}/consume", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/devices/{}/welcomes/{}/consume", f.member.device_id(), f.welcome), Some(f.member.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // ----------------------------------------------------------------- blocks
        Op { method: M::GET, template: "/v1/messaging/blocks", scopes: READ, acting: AM, res: Res::Env,
             req: |f| r("/v1/messaging/blocks".into(), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/blocks", scopes: SEND, acting: AM, res: Res::Env,
             req: |f| r("/v1/messaging/blocks".into(), Some(f.owner.principal), Some(json!({ "principalId": f.outsider }))),
             ok: 201, skip: NO_SKIP,
             expect: &[(Case::CrossTenantIds, 403, "PRINCIPAL_NOT_AUTHORIZED")] },
        Op { method: M::DELETE, template: "/v1/messaging/blocks/{principalId}", scopes: SEND, acting: AM, res: Res::Env,
             req: |f| r(format!("/v1/messaging/blocks/{}", f.blocked), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP,
             // Only the caller's own block rows exist for it.
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        // --------------------------------------------------------------- realtime
        Op { method: M::POST, template: "/v1/messaging/client-tokens", scopes: READ, acting: AM, res: Res::Device,
             req: |f| r("/v1/messaging/client-tokens".into(), Some(f.owner.principal), Some(json!({ "deviceId": f.owner.device_id() }))),
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/client-sessions/{sessionId}/revoke", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/client-sessions/{}/revoke", f.session), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/client-sessions/revoke-all", scopes: SEND, acting: AM, res: Res::Env,
             req: |f| r("/v1/messaging/client-sessions/revoke-all".into(), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/presence/settings", scopes: READ, acting: AM, res: Res::Env,
             req: |f| r("/v1/messaging/presence/settings".into(), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PUT, template: "/v1/messaging/presence/settings", scopes: SEND, acting: AM, res: Res::Env,
             req: |f| r("/v1/messaging/presence/settings".into(), Some(f.owner.principal), Some(json!({ "visibility": "HIDDEN" }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PUT, template: "/v1/messaging/devices/{deviceId}/push-registration", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/devices/{}/push-registration", f.owner.device_id()), Some(f.owner.principal),
                        Some(json!({ "platform": "LOCAL_TEST", "handle": "matrix-handle" }))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::DELETE, template: "/v1/messaging/devices/{deviceId}/push-registration", scopes: SEND, acting: AM, res: Res::Device,
             req: |f| r(format!("/v1/messaging/devices/{}/push-registration", f.owner.device_id()), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // ------------------------------------------------------------ attachments
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/attachments", scopes: AW, acting: AM, res: CONV,
             req: |f| { let b = png(64);
                        r(format!("/v1/conversations/{}/attachments", f.conv), Some(f.owner.principal),
                          Some(json!({ "kind": "IMAGE", "size": b.len(), "sha256": sha256_hex(&b), "mediaType": "image/png", "fileName": "m.png" }))) },
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/attachments/{attachmentId}", scopes: AR, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/attachments/{}", f.conv, f.attachment), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/attachments/{attachmentId}/finalize", scopes: AW, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/attachments/{}/finalize", f.conv, f.pending_attachment), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/attachments/{attachmentId}/download-grants", scopes: AR, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/attachments/{}/download-grants", f.conv, f.attachment), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/attachments/{attachmentId}/thumbnail-grants", scopes: AR, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/attachments/{}/thumbnail-grants", f.conv, f.attachment), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/attachments/{attachmentId}/content", scopes: AR, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/attachments/{}/content", f.conv, f.attachment), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // ------------------------------------------------------------- compliance
        Op { method: M::GET, template: "/v1/messaging/legal-holds", scopes: MANAGE, acting: AF, res: Res::Env,
             req: |_| r("/v1/messaging/legal-holds".into(), None, None), ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/legal-holds", scopes: MANAGE, acting: AF, res: Res::Env,
             req: |f| r("/v1/messaging/legal-holds".into(), None,
                        Some(json!({ "scopeKind": "CONVERSATION", "conversationId": f.conv, "reasonCategory": "LITIGATION" }))),
             ok: 201, skip: NO_SKIP,
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::GET, template: "/v1/messaging/legal-holds/{holdId}", scopes: MANAGE, acting: AF, res: Res::Env,
             req: |f| r(format!("/v1/messaging/legal-holds/{}", f.hold), None, None), ok: 200, skip: NO_SKIP,
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::POST, template: "/v1/messaging/legal-holds/{holdId}/release", scopes: MANAGE, acting: AF, res: Res::Env,
             req: |f| r(format!("/v1/messaging/legal-holds/{}/release", f.hold), None, Some(json!({ "reason": "CASE_CLOSED" }))),
             ok: 200, skip: NO_SKIP, expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::POST, template: "/v1/messaging/exports", scopes: EXPORT, acting: AF, res: Res::Env,
             req: |f| r("/v1/messaging/exports".into(), None, Some(json!({ "conversationId": f.conv }))),
             ok: 201, skip: NO_SKIP, expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::GET, template: "/v1/messaging/exports/{exportId}", scopes: EXPORT, acting: AF, res: Res::Env,
             req: |f| r(format!("/v1/messaging/exports/{}", f.export), None, None), ok: 200, skip: NO_SKIP,
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::POST, template: "/v1/messaging/exports/{exportId}/download-grants", scopes: EXPORT, acting: AF, res: Res::Env,
             req: |f| r(format!("/v1/messaging/exports/{}/download-grants", f.export), None, Some(json!({}))), ok: 200, skip: NO_SKIP,
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::POST, template: "/v1/messaging/deletion-requests", scopes: MANAGE, acting: AF, res: Res::Env,
             req: |f| r("/v1/messaging/deletion-requests".into(), None, Some(json!({ "kind": "CONVERSATION_PURGE", "conversationId": f.conv }))),
             ok: 202, skip: NO_SKIP, expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::GET, template: "/v1/messaging/deletion-requests/{requestId}", scopes: MANAGE, acting: AF, res: Res::Env,
             req: |f| r(format!("/v1/messaging/deletion-requests/{}", f.deletion), None, None), ok: 200, skip: NO_SKIP,
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::GET, template: "/v1/messaging/audit", scopes: MANAGE, acting: AF, res: Res::Env,
             req: |_| r("/v1/messaging/audit".into(), None, None), ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // ----------------------------------------------------------------- search
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/search", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/search?q=hi", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/search", scopes: READ, acting: AM, res: Res::Env,
             req: |f| r(format!("/v1/messaging/search?q={}", f.canary), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // ------------------------------------------------------------- moderation
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/reports", scopes: SEND, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/reports", f.conv), Some(f.owner.principal), Some(json!({ "category": "SPAM" }))),
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/reports", scopes: MOD, acting: AF, res: Res::Env,
             req: |_| r("/v1/messaging/reports".into(), None, None), ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/messaging/reports/{reportId}", scopes: MOD, acting: AF, res: Res::Env,
             req: |f| r(format!("/v1/messaging/reports/{}", f.report), None, None), ok: 200, skip: NO_SKIP,
             expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        Op { method: M::GET, template: "/v1/messaging/moderation/actions", scopes: MOD, acting: AF, res: Res::Env,
             req: |_| r("/v1/messaging/moderation/actions".into(), None, None), ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/messaging/moderation/actions", scopes: MOD, acting: AF, res: Res::Env,
             req: |f| r("/v1/messaging/moderation/actions".into(), None,
                        Some(json!({ "kind": "RESTRICT_PRINCIPAL", "principalId": f.member.principal, "reason": "SPAM_WAVE", "durationSeconds": 600, "conversationId": f.conv }))),
             ok: 201, skip: NO_SKIP, expect: &[(Case::CrossTenantIds, 404, "NOT_FOUND")] },
        // -------------------------------------------------------------- SMS bridge
        Op { method: M::GET, template: "/v1/messaging/sms-bridge/disclosure", scopes: READ, acting: AI, res: Res::Env,
             req: |_| r("/v1/messaging/sms-bridge/disclosure".into(), None, None), ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/sms-bridges", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/sms-bridges", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/sms-bridges", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/sms-bridges", f.conv), Some(f.owner.principal),
                        Some(json!({ "numberResourceId": Uuid::new_v4(), "to": "+15550199999", "externalPrincipalId": f.party2,
                                     "disclosureVersion": DISCLOSURE_VERSION, "smsDisclosureAcknowledged": true }))),
             ok: 201, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::POST, template: "/v1/conversations/{conversationId}/sms-bridges/{bridgeId}/close", scopes: MANAGE, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/sms-bridges/{}/close", f.sms_conv, f.bridge), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/messages/{messageId}/sms-transport", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages/{}/sms-transport", f.sms_conv, f.bridged_msg), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // -------------------------------------------------------------- engagement
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/messages/{messageId}/reactions", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages/{}/reactions", f.conv, f.msg), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PUT, template: "/v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}", scopes: SEND, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages/{}/reactions/:thumbsup:", f.conv, f.member_msg), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::DELETE, template: "/v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}", scopes: SEND, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/messages/{}/reactions/:wave:", f.conv, f.msg), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/pins", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/pins", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::PUT, template: "/v1/conversations/{conversationId}/pins/{messageId}", scopes: SEND, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/pins/{}", f.conv, f.member_msg), Some(f.owner.principal), Some(json!({}))),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::DELETE, template: "/v1/conversations/{conversationId}/pins/{messageId}", scopes: SEND, acting: AM, res: ADMIN,
             req: |f| r(format!("/v1/conversations/{}/pins/{}", f.conv, f.msg), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        Op { method: M::GET, template: "/v1/conversations/{conversationId}/unread", scopes: READ, acting: AM, res: CONV,
             req: |f| r(format!("/v1/conversations/{}/unread", f.conv), Some(f.owner.principal), None),
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
        // ------------------------------------------------------------------- usage
        Op { method: M::GET, template: "/v1/messaging/usage", scopes: USAGE, acting: AI, res: Res::Env,
             req: |_| { let t = |d: i64| (chrono::Utc::now() + chrono::Duration::days(d)).format("%Y-%m-%dT00:00:00Z").to_string();
                        r(format!("/v1/messaging/usage?from={}&to={}", t(-1), t(1)), None, None) },
             ok: 200, skip: NO_SKIP, expect: NO_EXPECT },
    ]
}

/// Operations certified by dedicated tests instead of this table (and why).
const ELSEWHERE: &[(&str, &str, &str)] = &[
    ("GET", "/health", "public probe, no data"),
    ("GET", "/ready", "public probe, no data"),
    ("GET", "/openapi.json", "public contract"),
    ("POST", "/v1/realtime/tickets", "device-direct client-token route: `realtime_routes_refuse_customer_credentials_and_missing_tokens` below; tests/realtime.rs"),
    ("GET", "/v1/realtime", "WebSocket upgrade with a single-use ticket: `realtime_routes_refuse_customer_credentials_and_missing_tokens` below; tests/realtime.rs"),
];

// ---------------------------------------------------------------------- execution

/// A fingerprint of every row of every table (rate windows excluded, see module docs).
async fn fingerprint(h: &Harness) -> Vec<(String, String)> {
    let tables = h
        .conn()
        .query_all(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' \
             AND table_type = 'BASE TABLE' AND table_name <> 'messaging_rate_windows' ORDER BY 1"
                .to_string(),
        ))
        .await
        .unwrap();
    let mut out = Vec::new();
    for t in tables {
        let t: String = t.try_get_by_index(0).unwrap();
        let r = h
            .conn()
            .query_one(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT count(*)::text || ':' || coalesce(md5(string_agg(x::text, '|' ORDER BY x::text)), '') \
                     FROM (SELECT to_jsonb(t) AS x FROM {t} t) s"
                ),
            ))
            .await
            .unwrap()
            .unwrap();
        out.push((t, r.try_get_by_index::<String>(0).unwrap()));
    }
    out
}

async fn call(
    h: &Harness,
    op: &Op,
    req: &Req,
    token: Option<&str>,
    actor: Option<Uuid>,
    extra: &[(&str, &str)],
) -> Response {
    let mut headers: Vec<(&str, &str)> =
        req.headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
    headers.extend_from_slice(extra);
    h.send(
        op.method.clone(),
        token,
        actor,
        &req.path,
        req.body.clone(),
        &headers,
    )
    .await
}

fn default_expect(op: &Op, case: Case) -> (u16, &'static str) {
    match case {
        Case::NoCredential | Case::InvalidCredential | Case::RevokedCredential => {
            (401, "AUTHENTICATION_FAILED")
        }
        Case::ClientTokenOnCustomerRoute => (401, "CLIENT_TOKENS_NOT_ENABLED"),
        Case::InsufficientScope => (403, "INSUFFICIENT_SCOPE"),
        Case::CrossTenantIds => (404, "NOT_FOUND"),
        Case::CrossTenantActingAsOurPrincipal
        | Case::InactivePrincipal
        | Case::RestrictedCredential
        | Case::PlainMemberOnAdminOperation
        | Case::ActingOnAdministrationOnly => (403, "PRINCIPAL_NOT_AUTHORIZED"),
        Case::NonMember
        | Case::RemovedMember
        | Case::BannedMember
        | Case::OtherPrincipalsDevice => {
            let _ = op;
            (404, "NOT_FOUND")
        }
    }
}

/// The cases that apply to `op`.
fn cases(op: &Op) -> Vec<Case> {
    let mut v = vec![
        Case::NoCredential,
        Case::InvalidCredential,
        Case::RevokedCredential,
        Case::ClientTokenOnCustomerRoute,
        Case::InsufficientScope,
    ];
    match op.acting {
        Acting::Required => v.extend([
            Case::CrossTenantActingAsOurPrincipal,
            Case::InactivePrincipal,
            Case::RestrictedCredential,
        ]),
        Acting::Forbidden => v.push(Case::ActingOnAdministrationOnly),
        Acting::Ignored => {}
    }
    match op.res {
        Res::Conversation { admin } => {
            v.extend([Case::NonMember, Case::RemovedMember, Case::BannedMember]);
            if admin {
                v.push(Case::PlainMemberOnAdminOperation);
            }
        }
        Res::Device => v.push(Case::OtherPrincipalsDevice),
        Res::Env => {}
    }
    // IDOR: another tenant with ITS OWN credential and principal and OUR ids. For
    // environment-level operations without our ids in the request it is the
    // isolation case (B must not see A's data) and is checked separately.
    v.push(Case::CrossTenantIds);
    v.retain(|c| !op.skip.iter().any(|(s, _)| s == c));
    v
}

async fn run_op(h: &Harness, store: &MemoryObjectStore, op: &Op) -> Vec<String> {
    let f = fixture(h, store).await;
    let req = (op.req)(&f);
    let owner_actor = req.actor;
    let secrets = f.secrets();
    let mut evidence = Vec::new();
    let before = fingerprint(h).await;
    // Environment-level cross-tenant calls without our ids may legitimately write to B's
    // OWN environment (e.g. B registers its own device challenge): they run last, after
    // the no-side-effect fingerprint of the real denials.
    let (isolation, denials): (Vec<Case>, Vec<Case>) = cases(op).into_iter().partition(|c| {
        *c == Case::CrossTenantIds
            && op.res == Res::Env
            && !op.expect.iter().any(|(e, _, _)| e == c)
    });
    let mut after: Option<Vec<(String, String)>> = None;
    for case in denials.into_iter().chain(isolation) {
        if after.is_none()
            && case == Case::CrossTenantIds
            && op.res == Res::Env
            && !op.expect.iter().any(|(e, _, _)| *e == case)
        {
            after = Some(fingerprint(h).await);
        }
        let (token, actor, extra): (Option<String>, Option<Uuid>, Vec<(&str, String)>) = match case
        {
            Case::NoCredential => (None, owner_actor, vec![]),
            Case::InvalidCredential => (
                Some(format!("cfmk_test_{}", "x".repeat(64))),
                owner_actor,
                vec![],
            ),
            Case::RevokedCredential => {
                let c = h.credential(&f.a, ALL_SCOPES);
                h.authority.tokens.lock().unwrap().remove(&c.token);
                (Some(c.token), owner_actor, vec![])
            }
            Case::ClientTokenOnCustomerRoute => (
                None,
                owner_actor,
                vec![("authorization", "CF-Client-Token abc.def.ghi".into())],
            ),
            Case::InsufficientScope => {
                let rest: Vec<&str> = ALL_SCOPES
                    .iter()
                    .copied()
                    .filter(|s| !op.scopes.contains(s))
                    .collect();
                (Some(h.credential(&f.a, &rest).token), owner_actor, vec![])
            }
            Case::CrossTenantIds => (
                Some(f.b.token.clone()),
                owner_actor.map(|_| f.b_actor),
                vec![],
            ),
            Case::CrossTenantActingAsOurPrincipal => (Some(f.b.token.clone()), owner_actor, vec![]),
            Case::NonMember => (Some(f.a.token.clone()), Some(f.outsider), vec![]),
            Case::RemovedMember => (Some(f.a.token.clone()), Some(f.removed), vec![]),
            Case::BannedMember => (Some(f.a.token.clone()), Some(f.banned), vec![]),
            Case::InactivePrincipal => (Some(f.a.token.clone()), owner_actor, vec![]),
            Case::RestrictedCredential => (
                Some(h.restricted(&f.a, &[f.outsider]).token),
                owner_actor,
                vec![],
            ),
            Case::PlainMemberOnAdminOperation => {
                (Some(f.a.token.clone()), Some(f.member.principal), vec![])
            }
            Case::OtherPrincipalsDevice => (Some(f.a.token.clone()), Some(f.outsider), vec![]),
            Case::ActingOnAdministrationOnly => {
                (Some(f.a.token.clone()), Some(f.owner.principal), vec![])
            }
        };
        if case == Case::InactivePrincipal {
            h.set_principal(&f.a, owner_actor.unwrap(), PrincipalStatus::Inactive);
        }
        let extra_ref: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let resp = call(h, op, &req, token.as_deref(), actor, &extra_ref).await;
        if case == Case::InactivePrincipal {
            h.set_principal(
                &f.a,
                owner_actor.unwrap(),
                PrincipalStatus::Active {
                    kind: "EXTERNAL_APPLICATION_PRINCIPAL".into(),
                },
            );
        }
        let isolation_only = case == Case::CrossTenantIds
            && op.res == Res::Env
            && !op.expect.iter().any(|(c, _, _)| *c == case);
        if isolation_only {
            // B's own environment-level view: whatever it returns, none of A's data.
            for secret in &secrets {
                assert!(
                    !resp.text.contains(secret.as_str()),
                    "{} {} {case:?}: leaked {secret}: {}",
                    op.method,
                    op.template,
                    resp.text
                );
            }
            evidence.push(format!("{case:?}=ISOLATED({})", resp.status.as_u16()));
            continue;
        }
        let (status, code) = op
            .expect
            .iter()
            .find(|(c, _, _)| *c == case)
            .map(|(_, s, c)| (*s, *c))
            .unwrap_or_else(|| default_expect(op, case));
        assert_eq!(
            (resp.status.as_u16(), resp.code()),
            (status, code),
            "{} {} {case:?}: {}",
            op.method,
            op.template,
            resp.text
        );
        for secret in &secrets {
            assert!(
                !resp.text.contains(secret.as_str()),
                "{} {} {case:?}: leaked {secret}: {}",
                op.method,
                op.template,
                resp.text
            );
        }
        evidence.push(format!("{case:?}={status}"));
    }
    let after = match after {
        Some(a) => a,
        None => fingerprint(h).await,
    };
    let changed: Vec<&String> = before
        .iter()
        .zip(&after)
        .filter(|(a, b)| a != b)
        .map(|(a, _)| &a.0)
        .collect();
    assert!(
        changed.is_empty(),
        "{} {}: denied requests changed {changed:?}",
        op.method,
        op.template
    );
    let resp = call(h, op, &req, Some(&f.a.token), owner_actor, &[]).await;
    assert_eq!(
        resp.status.as_u16(),
        op.ok,
        "{} {} positive: {}",
        op.method,
        op.template,
        resp.text
    );
    evidence
}

#[tokio::test]
async fn every_customer_operation_enforces_authorization_and_tenant_isolation() {
    let Some((h, store)) = harness().await else {
        return;
    };
    let ops = ops();
    let mut total = 0;
    for op in &ops {
        let evidence = run_op(&h, &store, op).await;
        total += evidence.len();
        println!(
            "MATRIX {} {} scopes={:?} acting={:?} resource={:?} positive={} denials=[{}] skipped=[{}]",
            op.method,
            op.template,
            op.scopes,
            op.acting,
            op.res,
            op.ok,
            evidence.join(","),
            op.skip.iter().map(|(c, why)| format!("{c:?}: {why}")).collect::<Vec<_>>().join("; ")
        );
    }
    println!("MATRIX TOTAL operations={} denial_cases={total}", ops.len());
}

/// The matrix lists exactly the customer operations of the published contract.
#[test]
fn every_operation_is_in_the_matrix() {
    let doc: Value = serde_json::from_str(include_str!(
        "../../../packages/contracts/messaging/http/v1.openapi.json"
    ))
    .unwrap();
    let mut published = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, _) in item.as_object().unwrap() {
            if ["get", "post", "put", "patch", "delete"].contains(&method.as_str()) {
                published.insert((method.to_uppercase(), path.clone()));
            }
        }
    }
    let mut covered: BTreeSet<(String, String)> = ops()
        .iter()
        .map(|o| (o.method.to_string(), o.template.to_string()))
        .collect();
    let in_table = covered.len();
    assert_eq!(in_table, ops().len(), "an operation is listed twice");
    covered.extend(
        ELSEWHERE
            .iter()
            .map(|(m, p, _)| (m.to_string(), p.to_string())),
    );
    // Every positive status of the matrix is a status the contract documents.
    for op in ops() {
        let responses = &doc["paths"][op.template][op.method.as_str().to_lowercase()]["responses"];
        assert!(
            responses.get(op.ok.to_string()).is_some(),
            "{} {} answers {} but the contract documents {:?}",
            op.method,
            op.template,
            op.ok,
            responses.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }
    let missing: Vec<_> = published.difference(&covered).collect();
    let extra: Vec<_> = covered.difference(&published).collect();
    assert!(
        missing.is_empty(),
        "operations without authorization evidence: {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "matrix rows for unpublished operations: {extra:?}"
    );
}

/// The device-direct realtime routes never accept a customer credential and refuse
/// requests without their own short-lived credentials.
#[tokio::test]
async fn realtime_routes_refuse_customer_credentials_and_missing_tokens() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.customer(ALL_SCOPES);
    let m = h.member(&c, 0).await;
    // Tickets: no token, a customer bearer credential, a forged client token.
    for auth in [
        None,
        Some(format!("Bearer {}", c.token)),
        Some("CF-Client-Token forged.token.value".into()),
    ] {
        let headers: Vec<(&str, &str)> =
            auth.iter().map(|a| ("authorization", a.as_str())).collect();
        let r = h
            .send(
                Method::POST,
                None,
                None,
                "/v1/realtime/tickets",
                None,
                &headers,
            )
            .await;
        assert_eq!(r.status.as_u16(), 401, "{auth:?}: {}", r.text);
    }
    // A valid token of a revoked session is refused.
    let t = h
        .post(
            &c,
            Some(m.principal),
            "/v1/messaging/client-tokens",
            json!({ "deviceId": m.device_id() }),
        )
        .await;
    assert_eq!(t.status.as_u16(), 201, "{}", t.text);
    let token = t.json["token"].as_str().unwrap().to_string();
    let ok_ticket = h
        .send(
            Method::POST,
            None,
            None,
            "/v1/realtime/tickets",
            None,
            &[("authorization", &format!("CF-Client-Token {token}"))],
        )
        .await;
    assert_eq!(ok_ticket.status.as_u16(), 201, "{}", ok_ticket.text);
    let rv = h
        .post(
            &c,
            Some(m.principal),
            &format!(
                "/v1/messaging/client-sessions/{}/revoke",
                s(&t.json, "sessionId")
            ),
            json!({}),
        )
        .await;
    assert_eq!(rv.status.as_u16(), 200, "{}", rv.text);
    let r = h
        .send(
            Method::POST,
            None,
            None,
            "/v1/realtime/tickets",
            None,
            &[("authorization", &format!("CF-Client-Token {token}"))],
        )
        .await;
    assert_eq!(r.status.as_u16(), 401, "{}", r.text);
    // WebSocket upgrade: a customer credential is not a ticket.
    for headers in [
        vec![("authorization".to_string(), format!("Bearer {}", c.token))],
        vec![(
            "sec-websocket-protocol".to_string(),
            "cfmeet.messaging.v1".to_string(),
        )],
        vec![(
            "sec-websocket-protocol".to_string(),
            "cfmeet.messaging.v1, cfmeet.ticket.0000".to_string(),
        )],
    ] {
        let hs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let r = h
            .send(Method::GET, None, None, "/v1/realtime", None, &hs)
            .await;
        assert!(
            matches!(r.status.as_u16(), 400 | 401 | 426),
            "{headers:?}: {} {}",
            r.status,
            r.text
        );
        assert_ne!(r.status.as_u16(), 101);
    }
}

/// Internal (service-to-service) operations: authentication, permission and delegated
/// scope are enforced before anything else.
#[tokio::test]
async fn internal_operations_enforce_service_identity_permission_and_delegated_scope() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let fp = h.first_party(true);
    let owner = h.principal(&fp);
    let peers = peers();
    let doc: Value = serde_json::from_str(include_str!(
        "../../../packages/contracts/messaging/internal/v1.openapi.json"
    ))
    .unwrap();
    let cid = Uuid::new_v4();
    let pid = Uuid::new_v4();
    let mid = Uuid::new_v4();
    // (method, path, the one peer allowed, a well-formed body)
    let table: Vec<(Method, String, &str, Value)> = vec![
        (
            Method::POST,
            "/internal/v1/meeting-conversations".into(),
            "meeting",
            json!({ "meetingRef": "conference:matrix", "ownerPrincipalId": owner, "securityProfile": "MANAGED", "managedDisclosureAcknowledged": true }),
        ),
        (
            Method::PUT,
            format!("/internal/v1/meeting-conversations/{cid}/members/{pid}"),
            "meeting",
            json!({ "state": "REMOVED", "sourceVersion": 1 }),
        ),
        (
            Method::POST,
            format!("/internal/v1/meeting-conversations/{cid}/lifecycle"),
            "meeting",
            json!({ "lifecycleState": "CLOSED", "sourceVersion": 1 }),
        ),
        (
            Method::POST,
            format!("/internal/v1/meeting-conversations/{cid}/legacy-imports"),
            "meeting",
            json!({ "legacySource": "matrix.legacy", "messages": [{ "legacyMessageId": "1", "senderPrincipalId": owner, "sentAt": "2026-01-01T00:00:00Z", "text": "x" }] }),
        ),
        (
            Method::POST,
            format!("/internal/v1/meeting-conversations/{cid}/messages/{mid}/moderation"),
            "meeting",
            json!({ "action": "REMOVE", "reason": "MATRIX" }),
        ),
        (
            Method::POST,
            "/internal/v1/context-authorities".into(),
            "privacy_relay",
            json!({ "namespace": "privacy-relay", "reference": "ctx:matrix", "state": "REVOKED", "sourceVersion": 1 }),
        ),
        (
            Method::POST,
            "/internal/v1/sms-bridge/inbound".into(),
            "sms_gateway",
            json!({ "bridgeRef": Uuid::new_v4(), "smsMessageId": Uuid::new_v4(), "text": "x", "segmentCount": 1, "receivedAt": "2026-01-01T00:00:00Z" }),
        ),
    ];
    // Coverage: every internal operation of the contract is in the table.
    let mut published = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, _) in item.as_object().unwrap() {
            if ["get", "post", "put", "patch", "delete"].contains(&method.as_str()) {
                published.insert((method.to_uppercase(), path.clone()));
            }
        }
    }
    let covered: BTreeSet<(String, String)> = table
        .iter()
        .map(|(m, p, _, _)| {
            let t = p
                .replace(&cid.to_string(), "{conversationId}")
                .replace(&pid.to_string(), "{principalId}")
                .replace(&mid.to_string(), "{messageId}");
            (m.to_string(), t)
        })
        .collect();
    assert_eq!(published, covered, "internal operations without evidence");
    let delegated = fp.context.scope.delegated();
    let mut forged = delegated.clone();
    forged = cf_service_auth::DelegatedPlatformContext::new(
        Uuid::new_v4(),
        forged.application_id(),
        Uuid::new_v4(),
        forged.application_environment_kind(),
        forged.initiating_service_account_id(),
    );
    for (method, path, allowed, body) in &table {
        let before = fingerprint(&h).await;
        // No service token at all.
        let r = h
            .send(method.clone(), None, None, path, Some(body.clone()), &[])
            .await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (401, "INTERNAL_AUTHENTICATION_FAILED"),
            "{path}: {}",
            r.text
        );
        // A customer credential is not a service identity.
        let r = h
            .send(
                method.clone(),
                Some(&fp.token),
                Some(owner),
                path,
                Some(body.clone()),
                &[],
            )
            .await;
        assert_eq!(
            r.status.as_u16(),
            401,
            "{path} customer credential: {}",
            r.text
        );
        // Untrusted issuer and a known name with an unlisted key.
        for signer in [&h.untrusted, &peers.rogue_meeting] {
            let r = h
                .call_internal(
                    signer,
                    method.clone(),
                    path,
                    Some(body.clone()),
                    Some(delegated.clone()),
                    &[],
                )
                .await;
            assert_eq!(
                (r.status.as_u16(), r.code()),
                (401, "INTERNAL_AUTHENTICATION_FAILED"),
                "{path}: {}",
                r.text
            );
        }
        // Trusted peers WITHOUT this operation's permission.
        for (name, signer) in [
            ("meeting", &peers.meeting),
            ("privacy_relay", &peers.privacy_relay),
            ("sms_gateway", &peers.sms_gateway),
        ] {
            if name == *allowed {
                continue;
            }
            let r = h
                .call_internal(
                    signer,
                    method.clone(),
                    path,
                    Some(body.clone()),
                    Some(delegated.clone()),
                    &[],
                )
                .await;
            assert_eq!(
                (r.status.as_u16(), r.code()),
                (403, "INTERNAL_PERMISSION_DENIED"),
                "{path} as {name}: {}",
                r.text
            );
        }
        let allowed_signer = match *allowed {
            "meeting" => &peers.meeting,
            "privacy_relay" => &peers.privacy_relay,
            _ => &peers.sms_gateway,
        };
        // The right peer without a delegated scope, and with a forged one.
        let r = h
            .call_internal(
                allowed_signer,
                method.clone(),
                path,
                Some(body.clone()),
                None,
                &[],
            )
            .await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (403, "DELEGATED_CONTEXT_REQUIRED"),
            "{path}: {}",
            r.text
        );
        let r = h
            .call_internal(
                allowed_signer,
                method.clone(),
                path,
                Some(body.clone()),
                Some(forged.clone()),
                &[],
            )
            .await;
        assert_eq!(
            (r.status.as_u16(), r.code()),
            (403, "DELEGATED_CONTEXT_INVALID"),
            "{path}: {}",
            r.text
        );
        assert_eq!(
            before,
            fingerprint(&h).await,
            "{path}: refused internal calls changed state"
        );
        println!("MATRIX-INTERNAL {method} {path} allowed={allowed} denials=[no-token,customer-credential,untrusted,rogue-key,wrong-peer,no-delegated,forged-delegated]");
    }
    // The delegated customer surface: the acting header must match the signed initiator.
    let other = h.principal(&fp);
    let r = h
        .call_internal(
            &peers.meeting,
            Method::GET,
            "/internal/v1/delegated/v1/conversations",
            None,
            Some(fp.context.scope.delegated().with_initiating_principal(
                owner,
                cf_service_auth::DelegatedPrincipalKind::ExternalApplicationPrincipal,
            )),
            &[("cf-acting-principal", &other.to_string())],
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), r.code()),
        (403, "PRINCIPAL_NOT_AUTHORIZED"),
        "{}",
        r.text
    );
}

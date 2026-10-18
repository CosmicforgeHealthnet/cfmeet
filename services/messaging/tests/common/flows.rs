//! Shared end-to-end flows: conversations, real MLS groups of real client devices,
//! encrypted sends and receives through the REAL API (no fake delivery, no server-side
//! decryption — the test devices decrypt locally).
#![allow(dead_code)]

use axum::http::{Method, StatusCode};
use cf_messaging_crypto::{
    binding::{self, ApplicationMessageBinding},
    device::Device,
    group::{ClientGroup, Processed},
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

pub const ALL_SCOPES: &[&str] = &[
    "messages.send",
    "messages.read",
    "conversations.create",
    "conversations.manage",
    "attachments.write",
    "attachments.read",
    "moderation.manage",
    "messaging.export",
];

/// One principal with one real client device and its view of the MLS group.
pub struct Member {
    pub principal: Uuid,
    pub device: Device,
    pub group: Option<ClientGroup>,
    /// Last message sequence this device processed.
    pub cursor: i64,
}

impl Member {
    pub fn device_id(&self) -> Uuid {
        self.device.device_id()
    }
}

pub fn idem() -> String {
    format!("idem-{}", Uuid::new_v4().simple())
}

impl Harness {
    pub async fn create_conversation(&self, c: &Customer, acting: Uuid, body: Value) -> Response {
        self.send(
            Method::POST,
            Some(&c.token),
            Some(acting),
            "/v1/conversations",
            Some(body),
            &[("idempotency-key", &idem())],
        )
        .await
    }

    pub async fn put(
        &self,
        c: &Customer,
        acting: Option<Uuid>,
        path: &str,
        body: Value,
    ) -> Response {
        self.send(Method::PUT, Some(&c.token), acting, path, Some(body), &[])
            .await
    }

    pub async fn patch(
        &self,
        c: &Customer,
        acting: Option<Uuid>,
        path: &str,
        body: Value,
    ) -> Response {
        self.send(Method::PATCH, Some(&c.token), acting, path, Some(body), &[])
            .await
    }

    pub async fn delete(&self, c: &Customer, acting: Option<Uuid>, path: &str) -> Response {
        self.send(Method::DELETE, Some(&c.token), acting, path, None, &[])
            .await
    }

    /// Sets the application policy (reads the current version first).
    pub async fn set_policy(&self, c: &Customer, policy: Value) {
        let v = self.get(c, None, "/v1/messaging/policy").await;
        assert_eq!(v.status, StatusCode::OK, "{}", v.text);
        let r = self
            .put(
                c,
                None,
                "/v1/messaging/policy",
                json!({ "expectedVersion": v.json["version"], "policy": policy }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }

    /// A principal with one ACTIVE device that has published `packages` KeyPackages.
    pub async fn member(&self, c: &Customer, packages: usize) -> Member {
        let principal = self.principal(c);
        let device = self.active_device(c, principal).await;
        if packages > 0 {
            let kps: Vec<Vec<u8>> = (0..packages)
                .map(|_| device.key_package().unwrap())
                .collect();
            let r = self.publish(c, principal, device.device_id(), &kps).await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        }
        Member {
            principal,
            device,
            group: None,
            cursor: 0,
        }
    }

    /// The creator builds the MLS group for `conversation` with its server-assigned group
    /// id and adds every other member's devices through the Delivery Service; the others
    /// join from their Welcomes. Returns the accepted commit's response.
    pub async fn establish_group(
        &self,
        c: &Customer,
        conversation: &Value,
        creator: &mut Member,
        others: &mut [&mut Member],
    ) {
        let cid = conversation["conversationId"].as_str().unwrap();
        let gid = unb64(&conversation["encryption"]["mlsGroupId"]);
        let mut group = creator.device.create_group(&gid).unwrap();
        if !others.is_empty() {
            let claim = self
                .post(
                    c,
                    Some(creator.principal),
                    &format!("/v1/conversations/{cid}/mls/key-package-claims"),
                    json!({ "principalIds": others.iter().map(|m| m.principal).collect::<Vec<_>>() }),
                )
                .await;
            assert_eq!(claim.status, StatusCode::OK, "{}", claim.text);
            let packages: Vec<Vec<u8>> = claim.json["keyPackages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| unb64(&k["keyPackage"]))
                .collect();
            assert_eq!(packages.len(), others.len());
            let out = creator.device.add_devices(&mut group, &packages).unwrap();
            let r = self
                .commit(
                    c,
                    cid,
                    creator,
                    &out.commit,
                    out.epoch,
                    out.welcome.as_deref(),
                    &others.iter().map(|m| m.device_id()).collect::<Vec<_>>(),
                    &[],
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
            creator.cursor = r.json["sequence"].as_i64().unwrap();
            for m in others.iter_mut() {
                let w = self
                    .get(
                        c,
                        Some(m.principal),
                        &format!("/v1/messaging/devices/{}/welcomes", m.device_id()),
                    )
                    .await;
                assert_eq!(w.status, StatusCode::OK, "{}", w.text);
                let welcome = w.json["welcomes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|x| x["conversationId"] == conversation["conversationId"])
                    .expect("a welcome for the conversation")
                    .clone();
                m.group = Some(m.device.join(&unb64(&welcome["welcome"])).unwrap());
                m.cursor = creator.cursor;
                let consumed = self
                    .post(
                        c,
                        Some(m.principal),
                        &format!(
                            "/v1/messaging/devices/{}/welcomes/{}/consume",
                            m.device_id(),
                            welcome["welcomeId"].as_str().unwrap()
                        ),
                        json!({}),
                    )
                    .await;
                assert_eq!(consumed.status, StatusCode::OK, "{}", consumed.text);
            }
        }
        creator.group = Some(group);
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn commit(
        &self,
        c: &Customer,
        cid: &str,
        by: &Member,
        commit: &[u8],
        epoch: u64,
        welcome: Option<&[u8]>,
        welcomed: &[Uuid],
        removed: &[Uuid],
    ) -> Response {
        let mut body = json!({
            "clientMessageId": Uuid::new_v4(),
            "deviceId": by.device_id(),
            "epoch": epoch,
            "commit": b64(commit),
            "removedDeviceIds": removed,
        });
        if let Some(w) = welcome {
            body["welcome"] = json!(b64(w));
            body["welcomeDeviceIds"] = json!(welcomed);
        }
        self.post(
            c,
            Some(by.principal),
            &format!("/v1/conversations/{cid}/mls/commits"),
            body,
        )
        .await
    }

    /// A DIRECT PRIVATE_E2EE conversation with an established two-device MLS group.
    pub async fn e2ee_direct(&self, c: &Customer) -> (Value, Member, Member) {
        let mut a = self.member(c, 1).await;
        let mut b = self.member(c, 1).await;
        let r = self
            .create_conversation(
                c,
                a.principal,
                json!({ "contextClass": "DIRECT", "counterpartPrincipalId": b.principal }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let conv = r.json;
        self.establish_group(c, &conv, &mut a, &mut [&mut b]).await;
        (conv, a, b)
    }

    /// A GROUP PRIVATE_E2EE conversation of `n` members (the first is the owner).
    pub async fn e2ee_group(&self, c: &Customer, n: usize) -> (Value, Vec<Member>) {
        let mut members: Vec<Member> = Vec::new();
        for _ in 0..n {
            members.push(self.member(c, 2).await);
        }
        let r = self
            .create_conversation(c, members[0].principal, json!({ "contextClass": "GROUP" }))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let conv = r.json;
        let cid = conv["conversationId"].as_str().unwrap().to_string();
        for m in &members[1..] {
            let r = self
                .post(
                    c,
                    Some(members[0].principal),
                    &format!("/v1/conversations/{cid}/members"),
                    json!({ "principalId": m.principal }),
                )
                .await;
            assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        }
        let (first, rest) = members.split_at_mut(1);
        let mut others: Vec<&mut Member> = rest.iter_mut().collect();
        self.establish_group(c, &conv, &mut first[0], &mut others)
            .await;
        (conv, members)
    }

    /// Encrypts `text` on `m`'s device under the CF Meet AAD and sends it.
    pub async fn send_e2ee(
        &self,
        c: &Customer,
        conv: &Value,
        m: &mut Member,
        text: &str,
    ) -> Response {
        let cid = conv["conversationId"].as_str().unwrap();
        let client_message_id = Uuid::new_v4();
        let ciphertext = self.encrypt_for(c, conv, m, client_message_id, text);
        self.post(
            c,
            Some(m.principal),
            &format!("/v1/conversations/{cid}/messages"),
            json!({
                "clientMessageId": client_message_id,
                "deviceId": m.device_id(),
                "epoch": m.group.as_ref().unwrap().epoch(),
                "ciphertext": b64(&ciphertext),
            }),
        )
        .await
    }

    pub fn encrypt_for(
        &self,
        c: &Customer,
        conv: &Value,
        m: &mut Member,
        client_message_id: Uuid,
        text: &str,
    ) -> Vec<u8> {
        let aad = binding::application_message_aad(&ApplicationMessageBinding {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
            conversation_id: uuid(&conv["conversationId"]),
            client_message_id,
            sender_device_id: m.device_id(),
        });
        let group = m.group.as_mut().unwrap();
        m.device.encrypt(group, &aad, text.as_bytes()).unwrap()
    }

    /// Fetches every message after the member's cursor and processes it on the device:
    /// commits are merged, application messages decrypted LOCALLY. Returns
    /// `(sequence, plaintext)` of the decrypted ones.
    pub async fn receive(&self, c: &Customer, conv: &Value, m: &mut Member) -> Vec<(i64, String)> {
        let cid = conv["conversationId"].as_str().unwrap();
        let r = self
            .get(
                c,
                Some(m.principal),
                &format!(
                    "/v1/conversations/{cid}/messages?afterSequence={}",
                    m.cursor
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        let mut out = Vec::new();
        for msg in r.json["messages"].as_array().unwrap() {
            let seq = msg["sequence"].as_i64().unwrap();
            m.cursor = seq;
            if msg["lifecycle"] != "ACTIVE" {
                continue;
            }
            let own = msg["senderDeviceId"] == json!(m.device_id());
            let group = m.group.as_mut().unwrap();
            if own || msg["e2ee"]["epoch"].as_u64().unwrap() < group.epoch() {
                continue;
            }
            let bytes = unb64(&msg["e2ee"]["ciphertext"]);
            match m.device.process(group, &bytes) {
                Ok(Processed::Application { plaintext, aad }) => {
                    let parsed = binding::parse_application_message_aad(&aad).expect("CF Meet AAD");
                    assert_eq!(parsed.client_message_id, uuid(&msg["clientMessageId"]));
                    assert_eq!(parsed.sender_device_id, uuid(&msg["senderDeviceId"]));
                    out.push((seq, String::from_utf8(plaintext).unwrap()));
                }
                Ok(_) => {}
                Err(e) => panic!("device failed to process sequence {seq}: {e:?}"),
            }
        }
        out
    }

    /// Lets MANAGED conversations of `class` exist (default MANAGED).
    pub async fn allow_managed(&self, c: &Customer, class: &str) {
        self.set_policy(
            c,
            json!({ "contexts": { class: { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } }),
        )
        .await;
    }

    /// A MANAGED GROUP conversation of `n` principals (the first owns it).
    pub async fn managed_group(&self, c: &Customer, n: usize) -> (Value, Vec<Uuid>) {
        self.allow_managed(c, "GROUP").await;
        let people: Vec<Uuid> = (0..n).map(|_| self.principal(c)).collect();
        let r = self
            .create_conversation(
                c,
                people[0],
                json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let cid = r.json["conversationId"].as_str().unwrap().to_string();
        for p in &people[1..] {
            let a = self
                .post(
                    c,
                    Some(people[0]),
                    &format!("/v1/conversations/{cid}/members"),
                    json!({ "principalId": p }),
                )
                .await;
            assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
        }
        (r.json, people)
    }

    pub async fn send_managed(
        &self,
        c: &Customer,
        conv: &Value,
        from: Uuid,
        text: &str,
    ) -> Response {
        let cid = conv["conversationId"].as_str().unwrap();
        self.post(
            c,
            Some(from),
            &format!("/v1/conversations/{cid}/messages"),
            json!({ "clientMessageId": Uuid::new_v4(), "text": text }),
        )
        .await
    }
}

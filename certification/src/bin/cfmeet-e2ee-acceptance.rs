//! Production multi-device personal E2EE acceptance: REAL OpenMLS client devices and real
//! Ed25519 account-device keys, driven only through the public CF Meet API.
//!
//! Account A links a primary mobile and two web browsers (each browser is trusted by its own
//! fresh sign-in, ADR 0067; a third browser signs in and removes itself); account B has one
//! web browser. Every device decrypts locally.
//! A's mobile is then replaced by a new phone: the old phone's sign-in, Hub device and
//! Messaging device are revoked, a web device commits its MLS removal, the old phone cannot
//! decrypt anything after the rekey, both web devices keep working, and the new phone joins
//! with fresh keys and reads future messages only. Then, WhatsApp-style, with every other
//! device offline: B's new browser reads what A sent after it was registered (A's device
//! added it from its published KeyPackage before sending), and another new browser of B
//! with NO KeyPackage joins the chat by itself (MLS external commit from the GroupInfo the
//! last committer published) and writes at once; A merges that join only after verifying
//! the device against the directory.
//!
//! Inputs: `CFMEET_BASE_URL` (for example https://api.cfmeet.com) and `CFMEET_E2EE_ACCOUNTS`
//! (a JSON file of two fresh accounts). Sign-in is passwordless and every device signs in
//! separately: each account is `{"email", "refreshTokens": [...]}` — refresh tokens of
//! sessions the operator opened just before the run (production; one per device, used in
//! order) — and/or `{"email", "code"}` for a fixed-code test account (`AUTH_TEST_ACCOUNTS`,
//! the certification stack).
//! Output: one line `E2EE_ACCEPTANCE_REPORT {json}` (no tokens, codes or keys; the
//! random test phrases are listed so the operator can prove the server never stored them).
use std::collections::BTreeMap;

use anyhow::{bail, Context};
use base64::Engine;
use cf_messaging_crypto::{
    binding::{application_message_aad, ApplicationMessageBinding},
    device::Device,
    group::{ClientGroup, Processed},
};
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use reqwest::Method;
use serde_json::{json, Value};
use sha2::Digest;
use uuid::Uuid;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

struct Api {
    http: reqwest::Client,
    base: String,
}

struct Resp {
    status: u16,
    body: Value,
}

impl Resp {
    fn code(&self) -> Option<&str> {
        self.body["error"]["code"].as_str()
    }
}

impl Api {
    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> anyhow::Result<Resp> {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("Idempotency-Key", Uuid::new_v4().to_string());
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        if let Some(b) = body {
            r = r.json(&b);
        }
        let response = r.send().await?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        Ok(Resp {
            status,
            body: serde_json::from_str(&text).unwrap_or(Value::Null),
        })
    }
    async fn ok(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        expect: &[u16],
        what: &str,
    ) -> anyhow::Result<Value> {
        let r = self.call(method, path, token, body).await?;
        if !expect.contains(&r.status) {
            bail!(
                "{what}: expected {expect:?}, got {} {:?}",
                r.status,
                r.code()
            );
        }
        Ok(r.body)
    }
}

/// One sign-in on one installation (browser profile or mobile app), with its device key.
struct Signin {
    name: &'static str,
    class: &'static str,
    platform: &'static str,
    installation: String,
    key: SigningKey,
    access: String,
    session: Uuid,
    device: Option<Uuid>,
}

impl Signin {
    fn thumbprint(&self) -> String {
        hex::encode(sha2::Sha256::digest(self.key.verifying_key().to_bytes()))
    }
    fn proof(&self, purpose: &str, context: &str) -> Value {
        let mut nonce = [0u8; 18];
        rand::thread_rng().fill_bytes(&mut nonce);
        let nonce = B64.encode(nonce);
        let issued_at = chrono::Utc::now().timestamp();
        let statement = format!(
            "cfmeet-account-device-proof:v1\n{purpose}\n{}\n{context}\n{issued_at}\n{nonce}",
            self.thumbprint()
        );
        json!({"issuedAt": issued_at, "nonce": nonce, "signature": B64.encode(self.key.sign(statement.as_bytes()).to_bytes())})
    }
}

/// One Messaging E2EE device of a sign-in and its view of the MLS group.
struct Mls {
    device: Device,
    group: Option<ClientGroup>,
    cursor: i64,
}

/// How an account's devices sign in (passwordless): pre-opened sessions first, then the
/// fixed code of a test account.
#[derive(Clone, Default)]
struct Credential {
    code: Option<String>,
    sessions: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
}

impl Credential {
    fn parse(a: &Value) -> Option<Self> {
        let sessions: std::collections::VecDeque<String> = a["refreshTokens"]
            .as_array()
            .map(|v| {
                v.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let code = a["code"].as_str().map(str::to_string);
        (code.is_some() || !sessions.is_empty()).then(|| Self {
            code,
            sessions: std::sync::Arc::new(std::sync::Mutex::new(sessions)),
        })
    }
}

struct Account {
    email: String,
    credential: Credential,
    principal: Uuid,
    cfmeet_id: String,
    scope: (Uuid, Uuid, Uuid),
}

struct Journey {
    api: Api,
    checks: BTreeMap<String, &'static str>,
    phrases: Vec<String>,
    conversation: Option<String>,
}

fn uuid_of(v: &Value) -> anyhow::Result<Uuid> {
    Ok(Uuid::parse_str(v.as_str().context("uuid field")?)?)
}

impl Journey {
    fn pass(&mut self, name: &str) {
        self.checks.insert(name.to_string(), "PASS");
        eprintln!("PASS {name}");
    }

    async fn sign_in(
        &self,
        a: &Account,
        name: &'static str,
        class: &'static str,
        platform: &'static str,
    ) -> anyhow::Result<Signin> {
        let session = a.credential.sessions.lock().unwrap().pop_front();
        let login = match (session, &a.credential.code) {
            // A session the operator opened for this device: its refresh token is
            // exchanged once (rotated) for this device's tokens.
            (Some(refresh_token), _) => {
                self.api
                    .ok(
                        Method::POST,
                        "/api/v1/auth/refresh",
                        None,
                        Some(json!({ "refresh_token": refresh_token })),
                        &[200],
                        "sign in",
                    )
                    .await?
            }
            (None, Some(code)) => {
                self.api
                    .ok(
                        Method::POST,
                        "/api/v1/auth/otp/verify",
                        None,
                        Some(json!({"email": a.email, "code": code})),
                        &[200],
                        "sign in",
                    )
                    .await?
            }
            (None, None) => bail!("no sign-in left for an account (provide more refreshTokens)"),
        };
        let access = login["access_token"]
            .as_str()
            .context("access token")?
            .to_string();
        let claims: Value =
            serde_json::from_slice(&B64.decode(access.split('.').nth(1).context("jwt")?)?)?;
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        Ok(Signin {
            name,
            class,
            platform,
            installation: format!("accept-{}", Uuid::new_v4().simple()),
            key: SigningKey::from_bytes(&seed),
            access,
            session: uuid_of(&claims["sid"])?,
            device: None,
        })
    }

    async fn declare(&self, s: &mut Signin) -> anyhow::Result<Value> {
        let body = json!({
            "installationId": s.installation, "deviceClass": s.class, "platform": s.platform,
            "label": format!("acceptance {}", s.name),
            "publicKey": B64.encode(s.key.verifying_key().to_bytes()),
            "proof": s.proof("DECLARE", &s.session.to_string()),
        });
        let v = self
            .api
            .ok(
                Method::POST,
                "/api/v2/me/devices/current",
                Some(&s.access),
                Some(body),
                &[200],
                "declare device",
            )
            .await?;
        s.device = Some(uuid_of(&v["device"]["deviceId"])?);
        Ok(v["device"].clone())
    }

    /// `new` asks to be linked, `approver` (trusted) enters the comparison code and signs.
    async fn link(&self, new: &Signin, approver: &Signin) -> anyhow::Result<Value> {
        let link = self
            .api
            .ok(
                Method::POST,
                "/api/v2/me/device-links",
                Some(&new.access),
                Some(json!({"proof": new.proof("LINK_REQUEST", &new.device.unwrap().to_string())})),
                &[201],
                "link request",
            )
            .await?;
        let id = link["linkId"].as_str().context("link id")?.to_string();
        let listed = self
            .api
            .ok(
                Method::GET,
                "/api/v2/me/device-links",
                Some(&approver.access),
                None,
                &[200],
                "pending links",
            )
            .await?;
        let pending = listed["links"]
            .as_array()
            .context("links")?
            .iter()
            .find(|l| l["linkId"] == json!(id))
            .context("pending link listed")?;
        if pending.get("comparisonCode").is_some() {
            bail!("the comparison code is listed to another device");
        }
        let wrong = self
            .api
            .call(Method::POST, &format!("/api/v2/me/device-links/{id}/approve"), Some(&approver.access),
                Some(json!({"comparisonCode": "000000-wrong", "proof": approver.proof("LINK_APPROVE", &format!("{id}|{}", new.thumbprint()))})))
            .await?;
        if wrong.status != 400 {
            bail!("a wrong comparison code was not refused: {}", wrong.status);
        }
        self.api
            .ok(Method::POST, &format!("/api/v2/me/device-links/{id}/approve"), Some(&approver.access),
                Some(json!({"comparisonCode": link["comparisonCode"], "proof": approver.proof("LINK_APPROVE", &format!("{id}|{}", new.thumbprint()))})),
                &[200], "approve link")
            .await?;
        self.api
            .ok(
                Method::POST,
                &format!("/api/v2/me/device-links/{id}/complete"),
                Some(&new.access),
                Some(json!({"proof": new.proof("LINK_COMPLETE", &id)})),
                &[200],
                "complete link",
            )
            .await
    }

    async fn policy(&self, s: &Signin) -> anyhow::Result<Value> {
        self.api
            .ok(
                Method::GET,
                "/api/v2/me/device-policy",
                Some(&s.access),
                None,
                &[200],
                "device policy",
            )
            .await
    }

    async fn relay(
        &self,
        s: &Signin,
        method: Method,
        path: &str,
        body: Option<Value>,
        expect: &[u16],
        what: &str,
    ) -> anyhow::Result<Value> {
        self.api
            .ok(
                method,
                &format!("/api/v2/messaging/relay/v1/{path}"),
                Some(&s.access),
                body,
                expect,
                what,
            )
            .await
    }

    /// Enroll a Messaging E2EE device for this sign-in, publish `packages` KeyPackages and link
    /// it in the Hub (so ending the sign-in revokes it).
    async fn enroll(&self, s: &Signin, packages: usize) -> anyhow::Result<Mls> {
        let ch = self
            .api
            .ok(
                Method::POST,
                "/api/v2/messaging/devices/registration-challenges",
                Some(&s.access),
                Some(json!({})),
                &[201],
                "E2EE challenge",
            )
            .await?;
        let device = Device::generate(uuid_of(&ch["deviceId"])?)?;
        let nonce: [u8; 32] = hex::decode(ch["nonce"].as_str().context("nonce")?)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("nonce length"))?;
        let kp = device.registration_key_package(uuid_of(&ch["challengeId"])?, &nonce)?;
        self.api
            .ok(
                Method::POST,
                "/api/v2/messaging/devices",
                Some(&s.access),
                Some(json!({"challengeId": ch["challengeId"], "keyPackage": B64.encode(kp)})),
                &[201],
                "E2EE device registration",
            )
            .await?;
        if packages > 0 {
            let kps: Vec<String> = (0..packages)
                .map(|_| device.key_package().map(|k| B64.encode(k)))
                .collect::<Result<_, _>>()?;
            self.api
                .ok(
                    Method::POST,
                    &format!(
                        "/api/v2/messaging/devices/{}/key-packages",
                        device.device_id()
                    ),
                    Some(&s.access),
                    Some(json!({"keyPackages": kps})),
                    &[201],
                    "publish KeyPackages",
                )
                .await?;
        }
        self.api
            .ok(Method::POST, "/api/v2/communications/devices/link", Some(&s.access),
                Some(json!({"platform": s.platform, "label": format!("acceptance {}", s.name), "chatDeviceId": device.device_id()})),
                &[200, 201], "Hub device link")
            .await?;
        Ok(Mls {
            device,
            group: None,
            cursor: 0,
        })
    }

    async fn account(
        &self,
        email: &str,
        credential: &Credential,
        first: &mut Option<Signin>,
    ) -> anyhow::Result<Account> {
        let mut s = self
            .sign_in(
                &Account {
                    email: email.into(),
                    credential: credential.clone(),
                    principal: Uuid::nil(),
                    cfmeet_id: String::new(),
                    scope: (Uuid::nil(), Uuid::nil(), Uuid::nil()),
                },
                "web-1",
                "WEB",
                "WEB",
            )
            .await?;
        let d = self.declare(&mut s).await?;
        if d["trust"] != "TRUSTED" || d["trustedVia"] != "BOOTSTRAP" {
            bail!("the first browser of a fresh account is not the bootstrap trusted device");
        }
        let me = self
            .api
            .ok(
                Method::GET,
                "/api/v2/messaging/me",
                Some(&s.access),
                None,
                &[200],
                "messaging identity",
            )
            .await?;
        let profile = self
            .api
            .ok(
                Method::GET,
                "/api/v2/communications/me",
                Some(&s.access),
                None,
                &[200],
                "communications profile",
            )
            .await?;
        let account = Account {
            email: email.into(),
            credential: credential.clone(),
            principal: uuid_of(&me["principalId"])?,
            cfmeet_id: profile["cfMeetId"].as_str().context("cfMeetId")?.into(),
            scope: (
                uuid_of(&me["tenantId"])?,
                uuid_of(&me["applicationId"])?,
                uuid_of(&me["environmentId"])?,
            ),
        };
        *first = Some(s);
        Ok(account)
    }

    fn aad(
        a: &Account,
        conversation: &str,
        client_message_id: Uuid,
        sender: Uuid,
    ) -> anyhow::Result<Vec<u8>> {
        Ok(application_message_aad(&ApplicationMessageBinding {
            tenant_id: a.scope.0,
            application_id: a.scope.1,
            environment_id: a.scope.2,
            conversation_id: Uuid::parse_str(conversation)?,
            client_message_id,
            sender_device_id: sender,
        }))
    }

    async fn send(
        &mut self,
        a: &Account,
        s: &Signin,
        m: &mut Mls,
        text: &str,
    ) -> anyhow::Result<(Value, Vec<u8>)> {
        let cid = self.conversation.clone().context("conversation")?;
        let id = Uuid::new_v4();
        let aad = Self::aad(a, &cid, id, m.device.device_id())?;
        let group = m.group.as_mut().context("group")?;
        let ciphertext = m.device.encrypt(group, &aad, text.as_bytes())?;
        let payload = json!({"clientMessageId": id, "deviceId": m.device.device_id(), "epoch": group.epoch(), "ciphertext": B64.encode(&ciphertext)});
        let sent = self
            .relay(
                s,
                Method::POST,
                &format!("conversations/{cid}/messages"),
                Some(payload.clone()),
                &[201],
                "send encrypted",
            )
            .await?;
        let again = self
            .relay(
                s,
                Method::POST,
                &format!("conversations/{cid}/messages"),
                Some(payload),
                &[200, 201],
                "retry send",
            )
            .await?;
        if again["messageId"] != sent["messageId"] {
            bail!("a retried delivery created a second message");
        }
        m.cursor = m.cursor.max(sent["sequence"].as_i64().unwrap_or(0));
        self.phrases.push(text.to_string());
        Ok((sent, ciphertext))
    }

    /// Fetch and process everything after the device's cursor (commits are merged, application
    /// messages decrypted LOCALLY). Returns the decrypted plaintexts in order.
    async fn receive(&self, s: &Signin, m: &mut Mls) -> anyhow::Result<Vec<String>> {
        let cid = self.conversation.clone().context("conversation")?;
        let list = self
            .relay(
                s,
                Method::GET,
                &format!(
                    "conversations/{cid}/messages?afterSequence={}&limit=100",
                    m.cursor
                ),
                None,
                &[200],
                "fetch messages",
            )
            .await?;
        let mut out = vec![];
        for msg in list["messages"].as_array().context("messages")? {
            m.cursor = msg["sequence"].as_i64().context("sequence")?;
            if msg["lifecycle"] != "ACTIVE" || msg["senderDeviceId"] == json!(m.device.device_id())
            {
                continue;
            }
            let group = m.group.as_mut().context("group")?;
            if msg["e2ee"]["epoch"].as_u64().context("epoch")? < group.epoch() {
                continue;
            }
            let bytes = B64.decode(msg["e2ee"]["ciphertext"].as_str().context("ciphertext")?)?;
            if let Processed::Application { plaintext, .. } = m.device.process(group, &bytes)? {
                out.push(String::from_utf8(plaintext)?);
            }
        }
        Ok(out)
    }

    /// `by` adds the devices of `targets` (claimed per principal) with one commit; each target
    /// joins from its Welcome.
    async fn add(
        &self,
        by_s: &Signin,
        by: &mut Mls,
        principals: &[Uuid],
        targets: &mut [(&Signin, &mut Mls)],
    ) -> anyhow::Result<()> {
        let cid = self.conversation.clone().context("conversation")?;
        let claim = self
            .relay(
                by_s,
                Method::POST,
                &format!("conversations/{cid}/mls/key-package-claims"),
                Some(json!({"principalIds": principals})),
                &[200],
                "claim KeyPackages",
            )
            .await?;
        let mut packages = vec![];
        let mut ids = vec![];
        for (_, t) in targets.iter() {
            let k = claim["keyPackages"]
                .as_array()
                .context("packages")?
                .iter()
                .find(|k| k["deviceId"] == json!(t.device.device_id()))
                .context("a KeyPackage per target device")?;
            packages.push(B64.decode(k["keyPackage"].as_str().context("kp")?)?);
            ids.push(t.device.device_id());
        }
        if claim["keyPackages"].as_array().map(Vec::len) != Some(targets.len()) {
            bail!("the claim returned packages for unexpected devices");
        }
        let group = by.group.as_mut().context("group")?;
        let out = by.device.prepare_add_devices(group, &packages)?;
        // The GroupInfo of the epoch the commit creates, published with it so a member's new
        // device can join by itself. (This journey stops on any refusal, so the commit is
        // merged locally first, and the GroupInfo exported from the merged group.)
        by.device.accept_pending_commit(group)?;
        let info = by.device.export_group_info(group)?;
        let r = self
            .relay(by_s, Method::POST, &format!("conversations/{cid}/mls/commits"), Some(json!({
                "clientMessageId": Uuid::new_v4(), "deviceId": by.device.device_id(), "epoch": out.epoch,
                "commit": B64.encode(&out.commit), "welcome": B64.encode(out.welcome.as_ref().context("welcome")?),
                "welcomeDeviceIds": ids, "removedDeviceIds": [], "groupInfo": B64.encode(&info)})), &[201], "add commit")
            .await?;
        by.cursor = r["sequence"].as_i64().context("sequence")?;
        for (s, t) in targets.iter_mut() {
            let w = self
                .relay(
                    s,
                    Method::GET,
                    &format!("messaging/devices/{}/welcomes", t.device.device_id()),
                    None,
                    &[200],
                    "welcomes",
                )
                .await?;
            let welcome = w["welcomes"]
                .as_array()
                .context("welcomes")?
                .iter()
                .find(|x| x["conversationId"] == json!(cid))
                .context("own welcome")?
                .clone();
            t.group = Some(
                t.device
                    .join(&B64.decode(welcome["welcome"].as_str().context("bytes")?)?)?,
            );
            t.cursor = by.cursor;
            self.relay(
                s,
                Method::POST,
                &format!(
                    "messaging/devices/{}/welcomes/{}/consume",
                    t.device.device_id(),
                    welcome["welcomeId"].as_str().unwrap()
                ),
                Some(json!({})),
                &[200],
                "consume welcome",
            )
            .await?;
        }
        Ok(())
    }

    async fn run(&mut self, accounts: &[(String, Credential)]) -> anyhow::Result<()> {
        // ---- Devices: web-only bootstrap, browsers trusted by sign-in, first mobile.
        let (mut a_web1, mut b_web) = (None, None);
        let a = self
            .account(&accounts[0].0, &accounts[0].1, &mut a_web1)
            .await?;
        let b = self
            .account(&accounts[1].0, &accounts[1].1, &mut b_web)
            .await?;
        let (a_web1, b_web) = (a_web1.unwrap(), b_web.unwrap());
        if self.policy(&a_web1).await?["accountState"] != "WEB_ONLY" {
            bail!("a web-only account is not WEB_ONLY");
        }
        self.pass("web_only_account_bootstraps_a_trusted_browser_without_phone");
        let mut a_web2 = self.sign_in(&a, "web-2", "WEB", "WEB").await?;
        if self.declare(&mut a_web2).await?["trust"] != "TRUSTED" {
            bail!("a freshly signed-in browser was not trusted (ADR 0067: no verification step)");
        }
        self.pass("second_browser_trusted_by_its_own_sign_in");
        let mut a_mobile = self.sign_in(&a, "mobile-a", "MOBILE", "ANDROID").await?;
        if self.declare(&mut a_mobile).await?["state"] != "REPLACEMENT_PENDING" {
            bail!("a new mobile was not held for approval");
        }
        let r = self
            .api
            .call(
                Method::GET,
                "/api/v2/messaging/me",
                Some(&a_mobile.access),
                None,
            )
            .await?;
        if (r.status, r.code()) != (403, Some("DEVICE_REPLACEMENT_PENDING")) {
            bail!("a pending mobile had authority: {}", r.status);
        }
        let done = self.link(&a_mobile, &a_web1).await?;
        if done["device"]["primary"] != true
            || self.policy(&a_web2).await?["accountState"] != "MOBILE_PRIMARY"
        {
            bail!("the approved mobile did not become the primary mobile");
        }
        self.pass("first_mobile_becomes_primary_after_approval_and_web_stays_trusted");
        let mut a_web3 = self.sign_in(&a, "web-3", "WEB", "WEB").await?;
        if self.declare(&mut a_web3).await?["trust"] != "TRUSTED" {
            bail!("a freshly signed-in browser was not trusted (ADR 0067: no verification step)");
        }
        self.api
            .ok(
                Method::POST,
                &format!("/api/v2/me/devices/{}/revoke", a_web3.device.unwrap()),
                Some(&a_web3.access),
                Some(json!({})),
                &[200],
                "self revoke",
            )
            .await?;
        self.pass("browser_trusted_by_sign_in_removes_itself");

        // ---- E2EE devices, one per sign-in; contacts; a DIRECT PRIVATE_E2EE conversation.
        let mut m_web1 = self.enroll(&a_web1, 0).await?;
        let mut m_web2 = self.enroll(&a_web2, 1).await?;
        let mut m_mobile = self.enroll(&a_mobile, 1).await?;
        let mut m_b = self.enroll(&b_web, 1).await?;
        let req = self
            .api
            .ok(
                Method::POST,
                "/api/v2/communications/contact-requests",
                Some(&a_web1.access),
                Some(json!({"cfMeetId": b.cfmeet_id})),
                &[200],
                "contact request",
            )
            .await?;
        self.api
            .ok(
                Method::POST,
                &format!(
                    "/api/v2/communications/contact-requests/{}/accept",
                    req["requestId"].as_str().context("request")?
                ),
                Some(&b_web.access),
                Some(json!({"expectedVersion": req["version"]})),
                &[200],
                "accept contact",
            )
            .await?;
        let conv = self
            .relay(&a_web1, Method::POST, "conversations", Some(json!({"contextClass": "DIRECT", "securityProfile": "PRIVATE_E2EE", "counterpartCfMeetId": b.cfmeet_id})), &[200, 201], "create conversation")
            .await?;
        if conv["securityProfile"] != "PRIVATE_E2EE" {
            bail!("the conversation is not PRIVATE_E2EE");
        }
        self.conversation = Some(
            conv["conversationId"]
                .as_str()
                .context("conversation id")?
                .to_string(),
        );
        m_web1.group = Some(
            m_web1.device.create_group(
                &B64.decode(
                    conv["encryption"]["mlsGroupId"]
                        .as_str()
                        .context("group id")?,
                )?,
            )?,
        );
        self.add(
            &a_web1,
            &mut m_web1,
            &[a.principal, b.principal],
            &mut [
                (&a_web2, &mut m_web2),
                (&a_mobile, &mut m_mobile),
                (&b_web, &mut m_b),
            ],
        )
        .await?;
        let mut leaves = m_web1.group.as_ref().unwrap().member_devices();
        leaves.sort();
        let mut want = vec![
            m_web1.device.device_id(),
            m_web2.device.device_id(),
            m_mobile.device.device_id(),
            m_b.device.device_id(),
        ];
        want.sort();
        if leaves != want {
            bail!("the MLS group does not hold exactly one leaf per authorized device");
        }
        self.pass("one_mls_leaf_per_authorized_device_of_both_accounts");

        let tag = Uuid::new_v4().simple().to_string();
        let first = format!("acceptance {tag} from B to every device of A");
        let (_, first_ct) = self.send(&b, &b_web, &mut m_b, &first).await?;
        for (s, m) in [
            (&a_web1, &mut m_web1),
            (&a_web2, &mut m_web2),
            (&a_mobile, &mut m_mobile),
        ] {
            if self.receive(s, m).await?.last() != Some(&first) {
                bail!("{} did not decrypt B's message", s.name);
            }
        }
        self.pass("web_and_mobile_devices_of_one_account_decrypt_the_same_message_locally");
        let from_web = format!("acceptance {tag} from web-2");
        self.send(&a, &a_web2, &mut m_web2, &from_web).await?;
        for (s, m) in [
            (&b_web, &mut m_b),
            (&a_web1, &mut m_web1),
            (&a_mobile, &mut m_mobile),
        ] {
            if self.receive(s, m).await?.last() != Some(&from_web) {
                bail!("{} did not decrypt web-2's message", s.name);
            }
        }
        self.pass("web_to_mobile_and_cross_web_sync_of_encrypted_messages");
        let from_mobile = format!("acceptance {tag} from mobile-a");
        self.send(&a, &a_mobile, &mut m_mobile, &from_mobile)
            .await?;
        for (s, m) in [
            (&a_web1, &mut m_web1),
            (&a_web2, &mut m_web2),
            (&b_web, &mut m_b),
        ] {
            if self.receive(s, m).await?.last() != Some(&from_mobile) {
                bail!("{} did not decrypt mobile-a's message", s.name);
            }
        }
        self.pass("mobile_to_web_encrypted_messages_and_idempotent_retries");

        // ---- Mobile replacement: approved by a web device, old phone revoked everywhere.
        let mut a_mobile2 = self.sign_in(&a, "mobile-b", "MOBILE", "IOS").await?;
        self.declare(&mut a_mobile2).await?;
        let replaced = self.link(&a_mobile2, &a_web2).await?;
        if replaced["replacedDeviceId"] != json!(a_mobile.device.unwrap()) {
            bail!("the new phone did not replace the previous primary mobile");
        }
        if self
            .api
            .call(
                Method::GET,
                "/api/v2/messaging/me",
                Some(&a_mobile.access),
                None,
            )
            .await?
            .status
            != 401
        {
            bail!("the replaced phone kept authority");
        }
        for s in [&a_web1, &a_web2] {
            if self.policy(s).await?["accountState"] != "MOBILE_PRIMARY" {
                bail!("{} lost its sign-in on mobile replacement", s.name);
            }
        }
        self.pass("new_mobile_replaces_old_and_web_devices_stay_connected");
        let cid = self.conversation.clone().unwrap();
        let old_device = m_mobile.device.device_id();
        let mut removed = false;
        for _ in 0..120 {
            let st = self
                .relay(
                    &a_web1,
                    Method::GET,
                    &format!("conversations/{cid}/mls/state"),
                    None,
                    &[200],
                    "mls state",
                )
                .await?;
            if st["pendingRemovals"]
                .as_array()
                .is_some_and(|p| p.iter().any(|r| r["deviceId"] == json!(old_device)))
            {
                removed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        if !removed {
            bail!("the replaced phone's Messaging device was not revoked (no pending MLS removal)");
        }
        let hub = self
            .api
            .ok(
                Method::GET,
                "/api/v2/communications/devices",
                Some(&a_web1.access),
                None,
                &[200],
                "hub devices",
            )
            .await?;
        let gone = hub["data"]
            .as_array()
            .context("devices")?
            .iter()
            .any(|d| d["chatDeviceId"] == json!(old_device) && d["state"] == "REVOKED");
        if !gone {
            bail!("the Hub still lists the replaced phone as active");
        }
        self.pass("replaced_phone_hub_and_messaging_devices_revoked_by_fan_out");
        // A genuine MLS application message is held until a member commits the removal.
        let early_id = Uuid::new_v4();
        let early_aad = Self::aad(&a, &cid, early_id, m_web2.device.device_id())?;
        let early_group = m_web2.group.as_mut().unwrap();
        let early = m_web2
            .device
            .encrypt(early_group, &early_aad, b"held until the rekey")?;
        let blocked = self
            .relay(&a_web2, Method::POST, &format!("conversations/{cid}/messages"), Some(json!({"clientMessageId": early_id, "deviceId": m_web2.device.device_id(), "epoch": early_group.epoch(), "ciphertext": B64.encode(&early)})), &[409], "send before rekey")
            .await?;
        if blocked["error"]["code"] != "MLS_REKEY_REQUIRED" {
            bail!("sends were not held until the rekey");
        }
        let group = m_web1.group.as_mut().unwrap();
        let epoch = group.epoch();
        let out = m_web1.device.prepare_remove_devices(group, &[old_device])?;
        let r = self
            .relay(&a_web1, Method::POST, &format!("conversations/{cid}/mls/commits"), Some(json!({"clientMessageId": Uuid::new_v4(), "deviceId": m_web1.device.device_id(), "epoch": out.epoch, "commit": B64.encode(&out.commit), "removedDeviceIds": [old_device]})), &[201], "removal commit")
            .await?;
        m_web1
            .device
            .accept_pending_commit(m_web1.group.as_mut().unwrap())?;
        m_web1.cursor = r["sequence"].as_i64().unwrap_or(m_web1.cursor);
        for (s, m) in [(&a_web2, &mut m_web2), (&b_web, &mut m_b)] {
            self.receive(s, m).await?;
            if m.group.as_ref().unwrap().epoch() != epoch + 1
                || m.group
                    .as_ref()
                    .unwrap()
                    .member_devices()
                    .contains(&old_device)
            {
                bail!("{} did not process the removal commit", s.name);
            }
        }
        self.pass("web_device_commits_the_mls_removal_and_the_epoch_advances");
        let after = format!("acceptance {tag} after the replacement");
        let (_, after_ct) = self.send(&b, &b_web, &mut m_b, &after).await?;
        if m_mobile
            .device
            .process(m_mobile.group.as_mut().unwrap(), &after_ct)
            .is_ok()
        {
            bail!("the removed phone decrypted a message after the rekey");
        }
        for (s, m) in [(&a_web1, &mut m_web1), (&a_web2, &mut m_web2)] {
            if self.receive(s, m).await?.last() != Some(&after) {
                bail!("{} did not decrypt after the replacement", s.name);
            }
        }
        self.pass("removed_phone_cannot_decrypt_after_rekey_web_devices_continue");

        // ---- The new phone: fresh keys, future messages only.
        let mut m_mobile2 = self.enroll(&a_mobile2, 1).await?;
        if m_mobile2.device.signature_public_key() == m_mobile.device.signature_public_key() {
            bail!("the new phone reused the old phone's key");
        }
        self.add(
            &a_web1,
            &mut m_web1,
            &[a.principal],
            &mut [(&a_mobile2, &mut m_mobile2)],
        )
        .await?;
        for (s, m) in [(&a_web2, &mut m_web2), (&b_web, &mut m_b)] {
            self.receive(s, m).await?;
        }
        if m_mobile2
            .device
            .process(m_mobile2.group.as_mut().unwrap(), &first_ct)
            .is_ok()
        {
            bail!("the new phone decrypted history without an explicit transfer");
        }
        let welcome = format!("acceptance {tag} to the new phone");
        self.send(&b, &b_web, &mut m_b, &welcome).await?;
        if self.receive(&a_mobile2, &mut m_mobile2).await?.last() != Some(&welcome) {
            bail!("the new phone did not decrypt a new message");
        }
        let reply = format!("acceptance {tag} from the new phone");
        self.send(&a, &a_mobile2, &mut m_mobile2, &reply).await?;
        if self.receive(&b_web, &mut m_b).await?.last() != Some(&reply) {
            bail!("B did not decrypt the new phone's message");
        }
        self.pass("new_phone_fresh_keys_future_messages_only_and_replies");

        // ---- WhatsApp: a recipient's new device, offline when the message is sent.
        // B signs in on a second browser that publishes one KeyPackage and goes offline. A's
        // phone adds every new device of the members before it sends (from the published
        // KeyPackage, nobody else online), then goes offline too; B's browser opens later
        // and reads it.
        let mut b_web2 = self.sign_in(&b, "web-2b", "WEB", "WEB").await?;
        // ADR 0067: a browser is trusted by its own sign-in (no link step).
        if self.declare(&mut b_web2).await?["trust"] != "TRUSTED" {
            bail!("B's new browser was not trusted by its sign-in");
        }
        let mut m_b2 = self.enroll(&b_web2, 1).await?;
        for (s, m) in [
            (&a_web1, &mut m_web1),
            (&a_web2, &mut m_web2),
            (&b_web, &mut m_b),
        ] {
            self.receive(s, m).await?;
        }
        self.add(
            &a_mobile2,
            &mut m_mobile2,
            &[b.principal],
            &mut [(&b_web2, &mut m_b2)],
        )
        .await?;
        let offline = format!("acceptance {tag} to B's offline browser");
        self.send(&a, &a_mobile2, &mut m_mobile2, &offline).await?;
        if self.receive(&b_web2, &mut m_b2).await?.last() != Some(&offline) {
            bail!("B's new browser did not read a message sent while it was offline");
        }
        self.pass("sender_adds_an_offline_recipients_new_device_from_its_published_keys");

        // A third browser of B with NO KeyPackage: nobody could add it, and nobody else is
        // online. It joins by itself from the GroupInfo the last committer published.
        let mut b_web3 = self.sign_in(&b, "web-3b", "WEB", "WEB").await?;
        // ADR 0067: a browser is trusted by its own sign-in (no link step).
        if self.declare(&mut b_web3).await?["trust"] != "TRUSTED" {
            bail!("B's new browser was not trusted by its sign-in");
        }
        let mut m_b3 = self.enroll(&b_web3, 0).await?;
        let st = self
            .relay(
                &b_web3,
                Method::GET,
                &format!("conversations/{cid}/mls/state"),
                None,
                &[200],
                "mls state",
            )
            .await?;
        if st["groupInfoEpoch"] != st["mlsEpoch"] {
            bail!("the current epoch has no published GroupInfo: {st}");
        }
        let info = B64.decode(st["groupInfo"].as_str().context("groupInfo")?)?;
        let (g3, join) = m_b3.device.join_by_external_commit(&info)?;
        let next = m_b3.device.export_group_info(&g3)?;
        let joined = self
            .relay(&b_web3, Method::POST, &format!("conversations/{cid}/mls/commits"), Some(json!({
                "clientMessageId": Uuid::new_v4(), "deviceId": m_b3.device.device_id(), "epoch": join.epoch,
                "commit": B64.encode(&join.commit), "groupInfo": B64.encode(&next)})), &[201], "external join")
            .await?;
        m_b3.cursor = joined["sequence"].as_i64().context("sequence")?;
        m_b3.group = Some(g3);
        let by_itself = format!("acceptance {tag} from B's self-joined browser");
        self.send(&b, &b_web3, &mut m_b3, &by_itself).await?;
        // A comes back: the join is merged only for B's registered device with its key.
        let dir = self
            .relay(
                &a_web1,
                Method::GET,
                &format!("messaging/principals/{}/devices", b.principal),
                None,
                &[200],
                "B's devices",
            )
            .await?;
        let registered: Vec<(String, Vec<u8>)> = dir["devices"]
            .as_array()
            .context("devices")?
            .iter()
            .filter_map(|d| {
                Some((
                    d["deviceId"].as_str()?.to_string(),
                    B64.decode(d["signaturePublicKey"].as_str()?).ok()?,
                ))
            })
            .collect();
        let list = self
            .relay(
                &a_web1,
                Method::GET,
                &format!(
                    "conversations/{cid}/messages?afterSequence={}&limit=100",
                    m_web1.cursor
                ),
                None,
                &[200],
                "fetch messages",
            )
            .await?;
        let mut read = vec![];
        let mut merged_join = false;
        for msg in list["messages"].as_array().context("messages")? {
            m_web1.cursor = msg["sequence"].as_i64().context("sequence")?;
            if msg["lifecycle"] != "ACTIVE"
                || msg["senderDeviceId"] == json!(m_web1.device.device_id())
            {
                continue;
            }
            let group = m_web1.group.as_mut().context("group")?;
            if msg["e2ee"]["epoch"].as_u64().context("epoch")? < group.epoch() {
                continue;
            }
            let sender = msg["senderDeviceId"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let bytes = B64.decode(msg["e2ee"]["ciphertext"].as_str().context("ciphertext")?)?;
            match m_web1.device.process_with_joiners(group, &bytes, |d, k| {
                d.to_string() == sender
                    && registered.iter().any(|(id, key)| *id == sender && key == k)
            })? {
                Processed::Application { plaintext, .. } => {
                    read.push(String::from_utf8(plaintext)?)
                }
                Processed::ExternalJoin { device_id, .. } => {
                    merged_join |= device_id == m_b3.device.device_id()
                }
                _ => {}
            }
        }
        if !merged_join || read.last() != Some(&by_itself) {
            bail!("A did not merge B's verified self-join or read its message");
        }
        self.pass("recipients_new_device_joins_by_itself_with_nobody_online_and_is_verified");

        // ---- Cleanup: sign every device of both accounts out (devices revoked).
        for s in [&a_web1, &b_web] {
            self.api
                .ok(
                    Method::POST,
                    "/api/v2/me/sessions/revoke-all",
                    Some(&s.access),
                    Some(json!({})),
                    &[200],
                    "sign out everywhere",
                )
                .await?;
        }
        if self.policy(&a_mobile2).await.is_ok() {
            bail!("sign-out everywhere left a device signed in");
        }
        self.pass("sign_out_everywhere_revokes_all_devices");
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let base = std::env::var("CFMEET_BASE_URL").unwrap_or_else(|_| "https://api.cfmeet.com".into());
    let accounts: Vec<(String, Credential)> = std::env::var("CFMEET_E2EE_ACCOUNTS")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        .map(|v| {
            v.iter()
                .filter_map(|a| Some((a["email"].as_str()?.to_string(), Credential::parse(a)?)))
                .collect()
        })
        .unwrap_or_default();
    let mut journey = Journey {
        api: Api {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("http client"),
            base,
        },
        checks: BTreeMap::new(),
        phrases: vec![],
        conversation: None,
    };
    let result = if accounts.len() == 2 {
        journey.run(&accounts).await
    } else {
        Err(anyhow::anyhow!(
            "CFMEET_E2EE_ACCOUNTS must list two accounts"
        ))
    };
    let failure = result.as_ref().err().map(|e| e.to_string());
    let report = json!({
        "verdict": if failure.is_none() { "PASS" } else { "FAIL" },
        "checks": journey.checks,
        "failure": failure,
        "conversationId": journey.conversation,
        "phrases": journey.phrases,
        "client": "OpenMLS 0.9.1 via cf-messaging-crypto (real client devices; keys never leave this process)",
    });
    println!("E2EE_ACCEPTANCE_REPORT {report}");
    std::process::exit(if failure.is_none() { 0 } else { 1 });
}

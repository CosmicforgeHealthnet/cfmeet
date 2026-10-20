//! wasm-bindgen bindings over the CF Meet Messaging client crypto core, so the REAL Rust
//! client code (OpenMLS 0.9.1, the attachment format, history transfer) runs inside a
//! browser. Each `WebDevice` lives in one Web Worker: its private keys never cross the
//! worker boundary, only MLS messages / ciphertext bytes do.
//!
//! Thin by design: no protocol logic here, only argument conversion.

use cf_messaging_crypto::{
    attachment::{self, AttachmentDescriptor},
    binding::{self, ApplicationMessageBinding},
    device::Device,
    group::{ClientGroup, Processed},
    history_transfer::{self, TransferContext, TransferKeys},
    key_package::{validate_key_package, LifetimePolicy},
};
use uuid::Uuid;
use wasm_bindgen::prelude::*;

fn id(s: &str) -> Result<Uuid, JsError> {
    Uuid::parse_str(s).map_err(|_| JsError::new("BAD_UUID"))
}

fn err<E: std::fmt::Debug>(e: E) -> JsError {
    JsError::new(&format!("{e:?}"))
}

/// The CF Meet AAD of one application message.
#[wasm_bindgen]
pub fn message_aad(
    tenant: &str,
    application: &str,
    environment: &str,
    conversation: &str,
    client_message: &str,
    sender_device: &str,
) -> Result<Vec<u8>, JsError> {
    Ok(binding::application_message_aad(
        &ApplicationMessageBinding {
            tenant_id: id(tenant)?,
            application_id: id(application)?,
            environment_id: id(environment)?,
            conversation_id: id(conversation)?,
            client_message_id: id(client_message)?,
            sender_device_id: id(sender_device)?,
        },
    ))
}

/// The checks Messaging runs when a device registers (services/messaging/src/devices):
/// KeyPackage validation under the default lifetime policy at `now` (Unix seconds), the
/// credential's device id, and the challenge binding in `application_id`. Returns the
/// device id, or the stable refusal code.
#[wasm_bindgen]
pub fn server_check_registration(
    encoded: &[u8],
    now: u64,
    challenge_id: &str,
    nonce: &[u8],
    device_id: &str,
) -> Result<String, JsError> {
    let kp = validate_key_package(encoded, now, &LifetimePolicy::default())
        .map_err(|e| JsError::new(e.code()))?;
    let nonce: [u8; binding::NONCE_LEN] =
        nonce.try_into().map_err(|_| JsError::new("BAD_NONCE"))?;
    let bound = kp
        .application_id
        .as_deref()
        .and_then(binding::parse_registration_binding)
        == Some((id(challenge_id)?, nonce));
    if kp.device_id != id(device_id)? || !bound {
        return Err(JsError::new("REGISTRATION_BINDING_MISMATCH"));
    }
    Ok(kp.device_id.to_string())
}

/// Whether `aad` is a canonical CF Meet application-message AAD.
#[wasm_bindgen]
pub fn aad_is_canonical(aad: &[u8]) -> bool {
    binding::parse_application_message_aad(aad).is_some()
}

#[wasm_bindgen]
pub struct CommitResult {
    commit: Vec<u8>,
    welcome: Option<Vec<u8>>,
    epoch: u64,
}

#[wasm_bindgen]
impl CommitResult {
    #[wasm_bindgen(getter)]
    pub fn commit(&self) -> Vec<u8> {
        self.commit.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn welcome(&self) -> Option<Vec<u8>> {
        self.welcome.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[wasm_bindgen]
pub struct ProcessResult {
    kind: String,
    aad: Vec<u8>,
    plaintext: Vec<u8>,
    epoch: u64,
}

#[wasm_bindgen]
impl ProcessResult {
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn aad(&self) -> Vec<u8> {
        self.aad.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn plaintext(&self) -> Vec<u8> {
        self.plaintext.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[wasm_bindgen]
pub struct EncryptedFile {
    ciphertext: Vec<u8>,
    descriptor: Vec<u8>,
    sha256: String,
}

#[wasm_bindgen]
impl EncryptedFile {
    #[wasm_bindgen(getter)]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }
    /// The descriptor (content key included): travels INSIDE an MLS application message.
    #[wasm_bindgen(getter)]
    pub fn descriptor(&self) -> Vec<u8> {
        self.descriptor.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn sha256(&self) -> String {
        self.sha256.clone()
    }
}

#[wasm_bindgen]
pub fn encrypt_file(
    plaintext: &[u8],
    media_type: &str,
    file_name: &str,
    attachment_id: &str,
) -> Result<EncryptedFile, JsError> {
    let enc = attachment::encrypt_attachment(plaintext, media_type, file_name).map_err(err)?;
    let descriptor = enc.descriptor.for_attachment(id(attachment_id)?);
    Ok(EncryptedFile {
        sha256: descriptor.ciphertext_sha256.clone(),
        descriptor: descriptor.to_bytes(),
        ciphertext: enc.ciphertext,
    })
}

#[wasm_bindgen]
pub fn decrypt_file(descriptor: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, JsError> {
    let d = AttachmentDescriptor::from_bytes(descriptor).map_err(err)?;
    attachment::decrypt_attachment(&d, ciphertext).map_err(err)
}

fn transfer_context(ids: &[String]) -> Result<TransferContext, JsError> {
    if ids.len() != 7 {
        return Err(JsError::new("BAD_CONTEXT"));
    }
    Ok(TransferContext {
        transfer_id: id(&ids[0])?,
        tenant_id: id(&ids[1])?,
        application_id: id(&ids[2])?,
        environment_id: id(&ids[3])?,
        principal_id: id(&ids[4])?,
        source_device_id: id(&ids[5])?,
        target_device_id: id(&ids[6])?,
    })
}

/// Server-side check of a transfer request (what Messaging runs; here for parity).
#[wasm_bindgen]
pub fn verify_transfer_request(
    target_device_key: &[u8],
    ctx: Vec<String>,
    transfer_key: &[u8],
    expires_at: i64,
    signature: &[u8],
) -> Result<bool, JsError> {
    Ok(history_transfer::verify_request(
        target_device_key,
        &transfer_context(&ctx)?,
        transfer_key,
        expires_at,
        signature,
    ))
}

#[wasm_bindgen]
pub fn comparison_code(
    ctx: Vec<String>,
    transfer_key: &[u8],
    source_key: &[u8],
    target_key: &[u8],
) -> Result<String, JsError> {
    Ok(history_transfer::comparison_code(
        &transfer_context(&ctx)?,
        transfer_key,
        source_key,
        target_key,
    ))
}

/// One device: its MLS signature key, KeyPackage private keys, groups and (while a
/// history transfer is pending) its single-use transfer key, all held in this worker.
#[wasm_bindgen]
pub struct WebDevice {
    device: Device,
    groups: Vec<(Vec<u8>, ClientGroup)>,
    transfer: Option<TransferKeys>,
}

fn find<'a>(
    groups: &'a mut [(Vec<u8>, ClientGroup)],
    group_id: &[u8],
) -> Result<&'a mut ClientGroup, JsError> {
    groups
        .iter_mut()
        .find(|(g, _)| g == group_id)
        .map(|(_, g)| g)
        .ok_or_else(|| JsError::new("UNKNOWN_GROUP"))
}

fn commit(c: cf_messaging_crypto::group::CommitOutput) -> CommitResult {
    CommitResult {
        commit: c.commit,
        welcome: c.welcome,
        epoch: c.epoch,
    }
}

#[wasm_bindgen]
impl WebDevice {
    /// Generates the device keys in the browser (Web Crypto randomness).
    #[wasm_bindgen(constructor)]
    pub fn new(device_id: &str) -> Result<WebDevice, JsError> {
        Ok(WebDevice {
            device: Device::generate(id(device_id)?).map_err(err)?,
            groups: Vec::new(),
            transfer: None,
        })
    }

    #[wasm_bindgen(getter, js_name = deviceId)]
    pub fn device_id(&self) -> String {
        self.device.device_id().to_string()
    }

    #[wasm_bindgen(getter, js_name = signaturePublicKey)]
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.device.signature_public_key()
    }

    #[wasm_bindgen(js_name = keyPackage)]
    pub fn key_package(&self) -> Result<Vec<u8>, JsError> {
        self.device.key_package().map_err(err)
    }

    /// The registration KeyPackage answering a server challenge (device binding).
    #[wasm_bindgen(js_name = registrationKeyPackage)]
    pub fn registration_key_package(
        &self,
        challenge_id: &str,
        nonce: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let nonce: [u8; binding::NONCE_LEN] =
            nonce.try_into().map_err(|_| JsError::new("BAD_NONCE"))?;
        self.device
            .registration_key_package(id(challenge_id)?, &nonce)
            .map_err(err)
    }

    #[wasm_bindgen(js_name = createGroup)]
    pub fn create_group(&mut self, group_id: &[u8]) -> Result<u64, JsError> {
        let g = self.device.create_group(group_id).map_err(err)?;
        let e = g.epoch();
        self.groups.push((group_id.to_vec(), g));
        Ok(e)
    }

    /// Joins from a Welcome; returns the joined epoch.
    pub fn join(&mut self, welcome: &[u8]) -> Result<u64, JsError> {
        let g = self.device.join(welcome).map_err(err)?;
        let e = g.epoch();
        let gid = g.group_id();
        self.groups.retain(|(id, _)| id != &gid);
        self.groups.push((gid, g));
        Ok(e)
    }

    /// Commits adding the device of one KeyPackage (validated by OpenMLS: signature,
    /// suite, lifetime) and merges it locally.
    #[wasm_bindgen(js_name = addDevice)]
    pub fn add_device(
        &mut self,
        group_id: &[u8],
        key_package: &[u8],
    ) -> Result<CommitResult, JsError> {
        let g = find(&mut self.groups, group_id)?;
        self.device
            .add_devices(g, &[key_package.to_vec()])
            .map(commit)
            .map_err(err)
    }

    /// Commits removing every leaf of `device_id` and merges it locally.
    #[wasm_bindgen(js_name = removeDevice)]
    pub fn remove_device(
        &mut self,
        group_id: &[u8],
        device_id: &str,
    ) -> Result<CommitResult, JsError> {
        let d = id(device_id)?;
        let g = find(&mut self.groups, group_id)?;
        self.device.remove_devices(g, &[d]).map(commit).map_err(err)
    }

    pub fn encrypt(
        &mut self,
        group_id: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let g = find(&mut self.groups, group_id)?;
        self.device.encrypt(g, aad, plaintext).map_err(err)
    }

    /// Decrypts an application message or verifies and merges a commit.
    pub fn process(&mut self, group_id: &[u8], message: &[u8]) -> Result<ProcessResult, JsError> {
        let g = find(&mut self.groups, group_id)?;
        let p = self.device.process(g, message).map_err(err)?;
        let epoch = g.epoch();
        Ok(match p {
            Processed::Application { aad, plaintext } => ProcessResult {
                kind: "APPLICATION".into(),
                aad,
                plaintext,
                epoch,
            },
            Processed::CommitMerged { epoch } => ProcessResult {
                kind: "COMMIT".into(),
                aad: vec![],
                plaintext: vec![],
                epoch,
            },
            Processed::Proposal => ProcessResult {
                kind: "PROPOSAL".into(),
                aad: vec![],
                plaintext: vec![],
                epoch,
            },
        })
    }

    pub fn epoch(&mut self, group_id: &[u8]) -> Result<u64, JsError> {
        Ok(find(&mut self.groups, group_id)?.epoch())
    }

    /// The group's member device ids, as the MLS tree records them.
    pub fn members(&mut self, group_id: &[u8]) -> Result<Vec<String>, JsError> {
        Ok(find(&mut self.groups, group_id)?
            .member_devices()
            .iter()
            .map(|d| d.to_string())
            .collect())
    }

    /// TARGET of a history transfer: makes the single-use transfer key (kept here) and
    /// returns its public half.
    #[wasm_bindgen(js_name = beginTransfer)]
    pub fn begin_transfer(&mut self) -> Result<Vec<u8>, JsError> {
        let keys = TransferKeys::generate().map_err(err)?;
        let public = keys.public_key().to_vec();
        self.transfer = Some(keys);
        Ok(public)
    }

    /// TARGET: signs the transfer request with the device key.
    #[wasm_bindgen(js_name = signTransferRequest)]
    pub fn sign_transfer_request(
        &self,
        ctx: Vec<String>,
        expires_at: i64,
    ) -> Result<Vec<u8>, JsError> {
        let keys = self
            .transfer
            .as_ref()
            .ok_or_else(|| JsError::new("NO_TRANSFER"))?;
        history_transfer::sign_request(&self.device, &transfer_context(&ctx)?, keys, expires_at)
            .map_err(err)
    }

    /// SOURCE: seals the history archive to the target's transfer key and signs it.
    #[wasm_bindgen(js_name = sealHistory)]
    pub fn seal_history(
        &self,
        ctx: Vec<String>,
        transfer_key: &[u8],
        history: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        history_transfer::seal_history(
            &self.device,
            &transfer_context(&ctx)?,
            transfer_key,
            history,
        )
        .map_err(err)
    }

    /// TARGET: verifies the source device signature and opens the archive. The transfer
    /// key is consumed (single use) whether or not opening succeeds.
    #[wasm_bindgen(js_name = openHistory)]
    pub fn open_history(
        &mut self,
        source_key: &[u8],
        ctx: Vec<String>,
        sealed: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let keys = self
            .transfer
            .take()
            .ok_or_else(|| JsError::new("NO_TRANSFER"))?;
        history_transfer::open_history(&keys, source_key, &transfer_context(&ctx)?, sealed)
            .map_err(err)
    }
}

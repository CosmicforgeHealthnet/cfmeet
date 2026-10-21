//! wasm-bindgen bindings of the CF Meet Messaging client crypto core, shipped with the web
//! app (`apps/web/public/messaging-crypto/`). Each [`WebDevice`] lives in ONE dedicated
//! Web Worker: its private keys never cross the worker boundary — only public keys, MLS
//! messages, ciphertext and (for display) decrypted plaintext do. The worker stores the
//! device's exported state only encrypted under a non-extractable Web Crypto key.
//!
//! Thin by design: no protocol logic, only argument conversion and fail-closed checks
//! (stable error codes, never key material, in every error).

use cf_messaging_crypto::{
    attachment::{self, AttachmentDescriptor},
    binding::{self, ApplicationMessageBinding},
    device::Device,
    group::{ClientGroup, Processed},
    history_transfer::{self, TransferContext, TransferKeys},
    key_package::{validate_key_package, LifetimePolicy},
    transparency,
};
use uuid::Uuid;
use wasm_bindgen::prelude::*;

fn id(s: &str) -> Result<Uuid, JsError> {
    Uuid::parse_str(s).map_err(|_| JsError::new("BAD_UUID"))
}

fn code(c: &str) -> JsError {
    JsError::new(c)
}

/// The CF Meet AAD of one application message (binds tenant, application, environment,
/// conversation, client message id and sending device).
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

/// What a published KeyPackage says about its device (after full validation: signature,
/// cipher suite, lifetime at `now` Unix seconds).
#[wasm_bindgen]
pub struct KeyPackageInfo {
    device_id: String,
    signature_key: Vec<u8>,
}

#[wasm_bindgen]
impl KeyPackageInfo {
    #[wasm_bindgen(getter, js_name = deviceId)]
    pub fn device_id(&self) -> String {
        self.device_id.clone()
    }
    #[wasm_bindgen(getter, js_name = signatureKey)]
    pub fn signature_key(&self) -> Vec<u8> {
        self.signature_key.clone()
    }
}

#[wasm_bindgen]
pub fn inspect_key_package(encoded: &[u8], now: u64) -> Result<KeyPackageInfo, JsError> {
    let kp = validate_key_package(encoded, now, &LifetimePolicy::default())
        .map_err(|e| code(e.code()))?;
    Ok(KeyPackageInfo {
        device_id: kp.device_id.to_string(),
        signature_key: kp.signature_key,
    })
}

fn hash32(b: &[u8]) -> Result<transparency::Hash, JsError> {
    b.try_into().map_err(|_| code("KT_BAD_HASH"))
}

/// Verifies a key-transparency tree head signed by the pinned log key (RFC 9162 heads,
/// Part V G.5). `false`: forged, substituted log or wrong key.
#[wasm_bindgen]
pub fn verify_tree_head(
    log_public_key: &[u8],
    log_id: &str,
    tree_size: u64,
    root_hash: &[u8],
    timestamp_micros: i64,
    signature: &[u8],
) -> Result<bool, JsError> {
    let input = transparency::tree_head_signing_input(
        log_id,
        tree_size,
        &hash32(root_hash)?,
        timestamp_micros,
    );
    Ok(transparency::verify_tree_head_signature(
        log_public_key,
        &input,
        signature,
    ))
}

/// Verifies that `leaf_data` is leaf `index` of the tree of `size` with `root`.
/// `proof` is the concatenation of the 32-byte proof hashes.
#[wasm_bindgen]
pub fn verify_inclusion(
    index: u64,
    size: u64,
    leaf_data: &[u8],
    proof: &[u8],
    root: &[u8],
) -> Result<bool, JsError> {
    if !proof.len().is_multiple_of(32) {
        return Err(code("KT_BAD_PROOF"));
    }
    let proof: Vec<transparency::Hash> = proof.chunks(32).map(hash32).collect::<Result<_, _>>()?;
    Ok(transparency::verify_inclusion(
        index,
        size,
        &transparency::leaf_hash(leaf_data),
        &proof,
        &hash32(root)?,
    ))
}

/// Verifies that the tree of `new_size` / `new_root` extends the one this device saw
/// (no rewritten or rolled-back history).
#[wasm_bindgen]
pub fn verify_consistency(
    old_size: u64,
    new_size: u64,
    old_root: &[u8],
    new_root: &[u8],
    proof: &[u8],
) -> Result<bool, JsError> {
    if !proof.len().is_multiple_of(32) {
        return Err(code("KT_BAD_PROOF"));
    }
    let proof: Vec<transparency::Hash> = proof.chunks(32).map(hash32).collect::<Result<_, _>>()?;
    Ok(transparency::verify_consistency(
        old_size,
        new_size,
        &hash32(old_root)?,
        &hash32(new_root)?,
        &proof,
    ))
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
    /// The epoch the commit was created in (what the Delivery Service orders on).
    #[wasm_bindgen(getter)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
}

#[wasm_bindgen]
pub struct ProcessResult {
    kind: String,
    plaintext: Vec<u8>,
    epoch: u64,
}

#[wasm_bindgen]
impl ProcessResult {
    /// `APPLICATION`, `COMMIT` or `PROPOSAL`.
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> String {
        self.kind.clone()
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

/// A file encrypted for upload. Its descriptor (content key, file name, media type) stays
/// inside this object until it is bound to the attachment id the server assigned
/// ([`EncryptedFile::descriptor_for`]) and travels inside an MLS message.
#[wasm_bindgen]
pub struct EncryptedFile {
    ciphertext: Vec<u8>,
    descriptor: AttachmentDescriptor,
}

#[wasm_bindgen]
impl EncryptedFile {
    #[wasm_bindgen(getter)]
    pub fn ciphertext(&self) -> Vec<u8> {
        self.ciphertext.clone()
    }
    /// Lower-case hex SHA-256 of the ciphertext (what the upload intent declares).
    #[wasm_bindgen(getter)]
    pub fn sha256(&self) -> String {
        self.descriptor.ciphertext_sha256.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u64 {
        self.descriptor.ciphertext_size
    }
    /// The descriptor bound to the server-assigned attachment id (SECRET: it carries the
    /// content key; it is only ever placed inside an MLS application message).
    #[wasm_bindgen(js_name = descriptorFor)]
    pub fn descriptor_for(&self, attachment_id: &str) -> Result<Vec<u8>, JsError> {
        Ok(self
            .descriptor
            .clone()
            .for_attachment(id(attachment_id)?)
            .to_bytes())
    }
}

/// Encrypts a file for upload (client-side; the server stores ciphertext only).
#[wasm_bindgen]
pub fn encrypt_file(
    plaintext: &[u8],
    media_type: &str,
    file_name: &str,
) -> Result<EncryptedFile, JsError> {
    let enc = attachment::encrypt_attachment(plaintext, media_type, file_name)
        .map_err(|_| code("ATTACHMENT_ENCRYPT_FAILED"))?;
    Ok(EncryptedFile {
        descriptor: enc.descriptor,
        ciphertext: enc.ciphertext,
    })
}

#[wasm_bindgen]
pub struct DecryptedFile {
    plaintext: Vec<u8>,
    media_type: String,
    file_name: String,
}

#[wasm_bindgen]
impl DecryptedFile {
    #[wasm_bindgen(getter)]
    pub fn plaintext(&self) -> Vec<u8> {
        self.plaintext.clone()
    }
    #[wasm_bindgen(getter, js_name = mediaType)]
    pub fn media_type(&self) -> String {
        self.media_type.clone()
    }
    #[wasm_bindgen(getter, js_name = fileName)]
    pub fn file_name(&self) -> String {
        self.file_name.clone()
    }
}

/// Decrypts a downloaded attachment. Refuses a ciphertext that does not match the
/// descriptor's digest or authentication, and a descriptor of another attachment.
#[wasm_bindgen]
pub fn decrypt_file(
    descriptor: &[u8],
    attachment_id: &str,
    ciphertext: &[u8],
) -> Result<DecryptedFile, JsError> {
    let d = AttachmentDescriptor::from_bytes(descriptor).map_err(|_| code("BAD_DESCRIPTOR"))?;
    if d.attachment_id != id(attachment_id)? {
        return Err(code("DESCRIPTOR_ATTACHMENT_MISMATCH"));
    }
    let plaintext =
        attachment::decrypt_attachment(&d, ciphertext).map_err(|_| code("ATTACHMENT_TAMPERED"))?;
    Ok(DecryptedFile {
        plaintext,
        media_type: d.media_type.clone(),
        file_name: d.file_name.clone(),
    })
}

/// The protected presentation metadata of a descriptor (shown before download).
#[wasm_bindgen]
pub struct DescriptorInfo {
    media_type: String,
    file_name: String,
    size: u64,
}

#[wasm_bindgen]
impl DescriptorInfo {
    #[wasm_bindgen(getter, js_name = mediaType)]
    pub fn media_type(&self) -> String {
        self.media_type.clone()
    }
    #[wasm_bindgen(getter, js_name = fileName)]
    pub fn file_name(&self) -> String {
        self.file_name.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn size(&self) -> u64 {
        self.size
    }
}

#[wasm_bindgen]
pub fn descriptor_info(descriptor: &[u8]) -> Result<DescriptorInfo, JsError> {
    let d = AttachmentDescriptor::from_bytes(descriptor).map_err(|_| code("BAD_DESCRIPTOR"))?;
    Ok(DescriptorInfo {
        media_type: d.media_type.clone(),
        file_name: d.file_name.clone(),
        size: d.plaintext_size,
    })
}

/// Whether `bytes` is an attachment descriptor (an encrypted attachment message).
#[wasm_bindgen]
pub fn is_descriptor(bytes: &[u8]) -> bool {
    AttachmentDescriptor::from_bytes(bytes).is_ok()
}

fn transfer_context(ids: &[String]) -> Result<TransferContext, JsError> {
    if ids.len() != 7 {
        return Err(code("BAD_CONTEXT"));
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

/// SOURCE: checks the target device signed the transfer request with its registered key.
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

/// The short code both devices show; the user confirms they match.
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

/// Splits `[u32 len][bytes]...` (several KeyPackages in one argument).
fn unpack(packed: &[u8]) -> Result<Vec<Vec<u8>>, JsError> {
    let mut out = Vec::new();
    let mut rest = packed;
    while !rest.is_empty() {
        if rest.len() < 4 {
            return Err(code("BAD_PACKING"));
        }
        let n = u32::from_be_bytes(rest[..4].try_into().expect("4 bytes")) as usize;
        rest = &rest[4..];
        if rest.len() < n || out.len() >= 1_000 {
            return Err(code("BAD_PACKING"));
        }
        out.push(rest[..n].to_vec());
        rest = &rest[n..];
    }
    Ok(out)
}

/// One device: its MLS signature key, KeyPackage private keys, groups and (while a
/// history transfer is pending) its single-use transfer key, all inside this worker.
#[wasm_bindgen]
pub struct WebDevice {
    device: Device,
    groups: Vec<(Vec<u8>, ClientGroup)>,
    transfer: Option<TransferKeys>,
}

/// The device's state of one group: in memory, or read back from the device's own
/// storage on first use (an imported device).
fn group_of<'a>(
    device: &Device,
    groups: &'a mut Vec<(Vec<u8>, ClientGroup)>,
    group_id: &[u8],
) -> Result<&'a mut ClientGroup, JsError> {
    if let Some(i) = groups.iter().position(|(g, _)| g == group_id) {
        return Ok(&mut groups[i].1);
    }
    let g = device
        .load_group(group_id)
        .map_err(|_| code("GROUP_STATE_INVALID"))?
        .ok_or_else(|| code("UNKNOWN_GROUP"))?;
    groups.push((group_id.to_vec(), g));
    Ok(&mut groups.last_mut().expect("just pushed").1)
}

fn commit_result(c: cf_messaging_crypto::group::CommitOutput) -> CommitResult {
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
            device: Device::generate(id(device_id)?).map_err(|_| code("KEY_GENERATION_FAILED"))?,
            groups: Vec::new(),
            transfer: None,
        })
    }

    /// Restores a device from [`WebDevice::export_state`] (decrypted by the worker).
    #[wasm_bindgen(js_name = fromState)]
    pub fn from_state(device_id: &str, state: &[u8]) -> Result<WebDevice, JsError> {
        Ok(WebDevice {
            device: Device::import_state(id(device_id)?, state)
                .map_err(|_| code("DEVICE_STATE_INVALID"))?,
            groups: Vec::new(),
            transfer: None,
        })
    }

    /// The complete secret state (keys and every group). The worker encrypts it before
    /// it leaves memory and never posts it out of the worker.
    #[wasm_bindgen(js_name = exportState)]
    pub fn export_state(&self) -> Vec<u8> {
        self.device.export_state().to_vec()
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
        self.device
            .key_package()
            .map_err(|_| code("KEY_PACKAGE_FAILED"))
    }

    /// The registration KeyPackage answering a server challenge (device binding).
    #[wasm_bindgen(js_name = registrationKeyPackage)]
    pub fn registration_key_package(
        &self,
        challenge_id: &str,
        nonce: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let nonce: [u8; binding::NONCE_LEN] = nonce.try_into().map_err(|_| code("BAD_NONCE"))?;
        self.device
            .registration_key_package(id(challenge_id)?, &nonce)
            .map_err(|_| code("KEY_PACKAGE_FAILED"))
    }

    #[wasm_bindgen(js_name = hasGroup)]
    pub fn has_group(&mut self, group_id: &[u8]) -> bool {
        group_of(&self.device, &mut self.groups, group_id).is_ok()
    }

    #[wasm_bindgen(js_name = createGroup)]
    pub fn create_group(&mut self, group_id: &[u8]) -> Result<u64, JsError> {
        if self.has_group(group_id) {
            return Err(code("GROUP_EXISTS"));
        }
        let g = self
            .device
            .create_group(group_id)
            .map_err(|_| code("GROUP_CREATE_REFUSED"))?;
        let e = g.epoch();
        self.groups.push((group_id.to_vec(), g));
        Ok(e)
    }

    /// Joins from a Welcome; refuses a Welcome of another group than `group_id`.
    pub fn join(&mut self, group_id: &[u8], welcome: &[u8]) -> Result<u64, JsError> {
        let g = self
            .device
            .join(welcome)
            .map_err(|_| code("WELCOME_REFUSED"))?;
        if g.group_id() != group_id {
            return Err(code("WELCOME_GROUP_MISMATCH"));
        }
        let e = g.epoch();
        self.groups.retain(|(gid, _)| gid != group_id);
        self.groups.push((group_id.to_vec(), g));
        Ok(e)
    }

    /// Commits adding the devices of the packed KeyPackages (`[u32 len][bytes]...`, each
    /// validated by OpenMLS: signature, suite, lifetime) and merges it locally.
    #[wasm_bindgen(js_name = addDevices)]
    pub fn add_devices(&mut self, group_id: &[u8], packed: &[u8]) -> Result<CommitResult, JsError> {
        let packages = unpack(packed)?;
        if packages.is_empty() {
            return Err(code("NO_KEY_PACKAGES"));
        }
        let g = group_of(&self.device, &mut self.groups, group_id)?;
        self.device
            .add_devices(g, &packages)
            .map(commit_result)
            .map_err(|_| code("ADD_REFUSED"))
    }

    /// Commits removing every leaf of `device_ids` and merges it locally.
    #[wasm_bindgen(js_name = removeDevices)]
    pub fn remove_devices(
        &mut self,
        group_id: &[u8],
        device_ids: Vec<String>,
    ) -> Result<CommitResult, JsError> {
        let ids = device_ids
            .iter()
            .map(|d| id(d))
            .collect::<Result<Vec<_>, _>>()?;
        let g = group_of(&self.device, &mut self.groups, group_id)?;
        self.device
            .remove_devices(g, &ids)
            .map(commit_result)
            .map_err(|_| code("REMOVE_REFUSED"))
    }

    pub fn encrypt(
        &mut self,
        group_id: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        let g = group_of(&self.device, &mut self.groups, group_id)?;
        self.device
            .encrypt(g, aad, plaintext)
            .map_err(|_| code("ENCRYPT_REFUSED"))
    }

    /// Decrypts an application message (only when MLS authenticated exactly
    /// `expected_aad` with it: the CF Meet binding of tenant, application, environment,
    /// conversation, client message id and sending device) or verifies and merges a
    /// commit. A mismatched AAD, a tampered or replayed message is refused.
    pub fn process(
        &mut self,
        group_id: &[u8],
        message: &[u8],
        expected_aad: Option<Vec<u8>>,
    ) -> Result<ProcessResult, JsError> {
        let g = group_of(&self.device, &mut self.groups, group_id)?;
        let p = self
            .device
            .process(g, message)
            .map_err(|_| code("MESSAGE_REFUSED"))?;
        let epoch = g.epoch();
        Ok(match p {
            Processed::Application { aad, plaintext } => {
                if expected_aad.as_deref() != Some(aad.as_slice()) {
                    return Err(code("AAD_MISMATCH"));
                }
                ProcessResult {
                    kind: "APPLICATION".into(),
                    plaintext,
                    epoch,
                }
            }
            Processed::CommitMerged { epoch } => ProcessResult {
                kind: "COMMIT".into(),
                plaintext: vec![],
                epoch,
            },
            Processed::Proposal => ProcessResult {
                kind: "PROPOSAL".into(),
                plaintext: vec![],
                epoch,
            },
        })
    }

    pub fn epoch(&mut self, group_id: &[u8]) -> Result<u64, JsError> {
        Ok(group_of(&self.device, &mut self.groups, group_id)?.epoch())
    }

    /// The group's member device ids, as the MLS tree records them.
    pub fn members(&mut self, group_id: &[u8]) -> Result<Vec<String>, JsError> {
        Ok(group_of(&self.device, &mut self.groups, group_id)?
            .member_devices()
            .iter()
            .map(|d| d.to_string())
            .collect())
    }

    /// TARGET of a history transfer: makes the single-use transfer key (kept here) and
    /// returns its public half.
    #[wasm_bindgen(js_name = beginTransfer)]
    pub fn begin_transfer(&mut self) -> Result<Vec<u8>, JsError> {
        let keys = TransferKeys::generate().map_err(|_| code("KEY_GENERATION_FAILED"))?;
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
        let keys = self.transfer.as_ref().ok_or_else(|| code("NO_TRANSFER"))?;
        history_transfer::sign_request(&self.device, &transfer_context(&ctx)?, keys, expires_at)
            .map_err(|_| code("TRANSFER_SIGN_FAILED"))
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
        .map_err(|_| code("TRANSFER_SEAL_FAILED"))
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
        let keys = self.transfer.take().ok_or_else(|| code("NO_TRANSFER"))?;
        history_transfer::open_history(&keys, source_key, &transfer_context(&ctx)?, sealed).map_err(
            |e| {
                code(match e {
                    history_transfer::TransferError::BadSignature => "TRANSFER_BAD_SIGNATURE",
                    history_transfer::TransferError::Malformed => "TRANSFER_MALFORMED",
                    _ => "TRANSFER_REFUSED",
                })
            },
        )
    }
}

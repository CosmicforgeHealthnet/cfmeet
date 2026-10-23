//! C ABI of the CF Meet Messaging client crypto core, loaded by the Dart SDK
//! (`sdks/dart/lib/src/crypto/`, `dart:ffi`) and linkable into native mobile apps.
//!
//! It mirrors, function for function, the wasm-bindgen surface of `../wasm/src/lib.rs`
//! and adds nothing to it: no protocol logic, only argument conversion and fail-closed
//! checks. Private keys and MLS secrets never leave the native side, except as the one
//! opaque device state the core exports ([`cfm_device_export_state`], exactly as the wasm
//! binding's `exportState`), which the caller must only store encrypted under a
//! platform-protected key. The C header is `include/cf_messaging_crypto.h`.
//!
//! # ABI rules
//!
//! * Every function returns a [`CfmStatus`] (`int32_t`): `CFM_OK` (0) or an error class.
//!   On error, [`cfm_last_error`] returns the stable error code (the same strings as the
//!   wasm binding: `BAD_UUID`, `MESSAGE_REFUSED`, `AAD_MISMATCH`, ...). Error codes never
//!   contain key material, plaintext or caller input. The last error is thread-local:
//!   read it on the thread that made the failing call, before the next call.
//! * Byte / string inputs are `(const uint8_t *ptr, size_t len)` pairs; `ptr` may be NULL
//!   only when `len` is 0. Strings are UTF-8 (not NUL-terminated).
//! * Byte outputs are [`CfmBuffer`]s owned by the caller and released with
//!   [`cfm_buffer_free`], which zeroizes them first. On error no output is written.
//! * Stateful objects ([`CfmDevice`], [`CfmEncryptedFile`]) are opaque handles released
//!   with their `*_free` function. A handle is internally locked: concurrent calls on one
//!   handle are serialized. After an internal panic the handle is poisoned and every later
//!   call on it fails closed with `CFM_PANIC` / `HANDLE_POISONED`.
//! * No panic crosses the ABI: every function runs under `catch_unwind`.

#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::ffi::{c_char, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use cf_messaging_crypto::{
    attachment::{self, AttachmentDescriptor},
    binding::{self, ApplicationMessageBinding},
    device::Device,
    group::{ClientGroup, CommitOutput, Processed},
    history_transfer::{self, TransferContext, TransferKeys},
    key_package::{validate_key_package, LifetimePolicy},
    transparency,
};
use uuid::Uuid;
use zeroize::Zeroize;

/// Version of this C ABI. Bumped on every incompatible change; the Dart binding refuses a
/// library of another version.
pub const CFM_ABI_VERSION: u32 = 1;

/// Status of every call.
pub type CfmStatus = i32;
/// Success.
pub const CFM_OK: CfmStatus = 0;
/// Refused by the crypto core or by a fail-closed check (code in [`cfm_last_error`]).
pub const CFM_ERROR: CfmStatus = 1;
/// A NULL pointer, an impossible length or a NULL handle (`INVALID_ARGUMENT`).
pub const CFM_INVALID_ARGUMENT: CfmStatus = 2;
/// An internal panic was caught (`INTERNAL_PANIC`), or the handle was poisoned by one.
pub const CFM_PANIC: CfmStatus = 3;

/// `kind` of a [`CfmProcessResult`].
pub const CFM_PROCESSED_APPLICATION: u32 = 1;
pub const CFM_PROCESSED_COMMIT: u32 = 2;
pub const CFM_PROCESSED_PROPOSAL: u32 = 3;

/// A byte buffer allocated by this library. Release with [`cfm_buffer_free`].
/// An empty buffer is `{ NULL, 0 }`.
#[repr(C)]
#[derive(Debug)]
pub struct CfmBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}

/// A borrowed input slice (for list arguments: device ids, transfer context ids).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CfmBytes {
    pub ptr: *const u8,
    pub len: usize,
}

/// Output of `cfm_device_add_devices` / `cfm_device_remove_devices`.
#[repr(C)]
#[derive(Debug)]
pub struct CfmCommitResult {
    pub commit: CfmBuffer,
    /// Empty when `has_welcome` is 0.
    pub welcome: CfmBuffer,
    pub has_welcome: u8,
    /// The epoch the commit was created in (what the Delivery Service orders on).
    pub epoch: u64,
}

/// Output of `cfm_device_process`.
#[repr(C)]
#[derive(Debug)]
pub struct CfmProcessResult {
    /// `CFM_PROCESSED_APPLICATION`, `_COMMIT` or `_PROPOSAL`.
    pub kind: u32,
    /// The decrypted application message (empty for a commit or a proposal).
    pub plaintext: CfmBuffer,
    pub epoch: u64,
}

// ---------------------------------------------------------------------------------------
// Errors, panics and the last-error slot
// ---------------------------------------------------------------------------------------

/// A failure: its status class and stable code (a static string: never secret data).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Failure(CfmStatus, &'static str);

fn code(c: &'static str) -> Failure {
    Failure(CFM_ERROR, c)
}

const INVALID: Failure = Failure(CFM_INVALID_ARGUMENT, "INVALID_ARGUMENT");
const POISONED: Failure = Failure(CFM_PANIC, "HANDLE_POISONED");

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_last_error(c: &str) {
    // Codes are static ASCII identifiers: never contain NUL.
    let s = CString::new(c).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = s);
}

/// Runs `f` with panics caught; records the error code on failure.
fn guard(f: impl FnOnce() -> Result<(), Failure>) -> CfmStatus {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => {
            set_last_error("");
            CFM_OK
        }
        Ok(Err(Failure(status, c))) => {
            set_last_error(c);
            status
        }
        Err(_) => {
            // The panic payload is deliberately not reported (it could quote data).
            set_last_error("INTERNAL_PANIC");
            CFM_PANIC
        }
    }
}

/// The stable error code of the last failed call on this thread (empty after a success).
/// The pointer is valid until the next call into this library on the same thread.
#[no_mangle]
pub extern "C" fn cfm_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

/// The C ABI version ([`CFM_ABI_VERSION`]).
#[no_mangle]
pub extern "C" fn cfm_abi_version() -> u32 {
    CFM_ABI_VERSION
}

// ---------------------------------------------------------------------------------------
// Argument conversion
// ---------------------------------------------------------------------------------------

/// Borrows `(ptr, len)` as a slice. NULL is allowed only with `len == 0`.
///
/// # Safety
/// A non-NULL `ptr` must point to `len` readable bytes that outlive the call.
unsafe fn slice<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], Failure> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() || len > isize::MAX as usize {
        return Err(INVALID);
    }
    // SAFETY: non-NULL and, per the caller contract, valid for `len` bytes.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// # Safety
/// As [`slice`].
unsafe fn string<'a>(ptr: *const u8, len: usize) -> Result<&'a str, Failure> {
    // SAFETY: forwarded caller contract.
    std::str::from_utf8(unsafe { slice(ptr, len) }?).map_err(|_| code("BAD_UTF8"))
}

/// # Safety
/// As [`slice`].
unsafe fn uuid(ptr: *const u8, len: usize) -> Result<Uuid, Failure> {
    // SAFETY: forwarded caller contract.
    Uuid::parse_str(unsafe { string(ptr, len) }?).map_err(|_| code("BAD_UUID"))
}

/// Borrows a list of `count` [`CfmBytes`] as UTF-8 strings.
///
/// # Safety
/// `items` must point to `count` readable [`CfmBytes`], each satisfying [`slice`].
unsafe fn strings<'a>(items: *const CfmBytes, count: usize) -> Result<Vec<&'a str>, Failure> {
    // SAFETY: forwarded caller contract (an array of `count` CfmBytes).
    let items = unsafe { typed_slice(items, count) }?;
    items
        .iter()
        // SAFETY: each element satisfies the `slice` contract.
        .map(|b| unsafe { string(b.ptr, b.len) })
        .collect()
}

/// # Safety
/// A non-NULL `ptr` must point to `count` readable `T`.
unsafe fn typed_slice<'a, T>(ptr: *const T, count: usize) -> Result<&'a [T], Failure> {
    if count == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() || count > (isize::MAX as usize) / std::mem::size_of::<T>().max(1) {
        return Err(INVALID);
    }
    // SAFETY: non-NULL and, per the caller contract, valid for `count` elements.
    Ok(unsafe { std::slice::from_raw_parts(ptr, count) })
}

/// Writes `value` to a caller-provided out pointer.
///
/// # Safety
/// A non-NULL `out` must be valid for a write of `T`.
unsafe fn write<T>(out: *mut T, value: T) -> Result<(), Failure> {
    if out.is_null() {
        return Err(INVALID);
    }
    // SAFETY: non-NULL, valid for writes per the caller contract; the previous contents
    // (uninitialised or a caller-owned value) are not dropped.
    unsafe { out.write(value) };
    Ok(())
}

fn buffer(bytes: Vec<u8>) -> CfmBuffer {
    if bytes.is_empty() {
        return CfmBuffer {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
    }
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len();
    CfmBuffer {
        ptr: Box::into_raw(boxed) as *mut u8,
        len,
    }
}

/// Moves a zeroizing buffer into a [`CfmBuffer`] without leaving a copy behind.
fn secret_buffer(mut bytes: zeroize::Zeroizing<Vec<u8>>) -> CfmBuffer {
    // `into_boxed_slice` may reallocate (when capacity > len) and would leave the old
    // allocation unzeroized; copy into an exact allocation and let `Zeroizing` wipe the
    // original instead.
    let exact = bytes.as_slice().to_vec();
    bytes.zeroize();
    buffer(exact)
}

/// Releases a buffer returned by this library, zeroizing its contents first. Freeing an
/// empty buffer (`{ NULL, 0 }`) is a no-op.
///
/// # Safety
/// `buf` must be a buffer returned by this library and not freed before.
#[no_mangle]
pub unsafe extern "C" fn cfm_buffer_free(buf: CfmBuffer) {
    if buf.ptr.is_null() || buf.len == 0 {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `buf` was produced by `buffer` (a boxed slice of exactly `len` bytes).
        let mut boxed =
            unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(buf.ptr, buf.len)) };
        boxed.zeroize();
    }));
}

fn id_list(items: &[&str]) -> Result<Vec<Uuid>, Failure> {
    items
        .iter()
        .map(|s| Uuid::parse_str(s).map_err(|_| code("BAD_UUID")))
        .collect()
}

fn transfer_context(ids: &[&str]) -> Result<TransferContext, Failure> {
    if ids.len() != 7 {
        return Err(code("BAD_CONTEXT"));
    }
    let ids = id_list(ids)?;
    Ok(TransferContext {
        transfer_id: ids[0],
        tenant_id: ids[1],
        application_id: ids[2],
        environment_id: ids[3],
        principal_id: ids[4],
        source_device_id: ids[5],
        target_device_id: ids[6],
    })
}

fn hash32(b: &[u8]) -> Result<transparency::Hash, Failure> {
    b.try_into().map_err(|_| code("KT_BAD_HASH"))
}

fn proof_hashes(proof: &[u8]) -> Result<Vec<transparency::Hash>, Failure> {
    if !proof.len().is_multiple_of(32) {
        return Err(code("KT_BAD_PROOF"));
    }
    proof.chunks(32).map(hash32).collect()
}

/// Splits `[u32 big-endian len][bytes]...` (several KeyPackages in one argument; the
/// format of the wasm binding's `addDevices`).
fn unpack(packed: &[u8]) -> Result<Vec<Vec<u8>>, Failure> {
    let mut out = Vec::new();
    let mut rest = packed;
    while !rest.is_empty() {
        if rest.len() < 4 {
            return Err(code("BAD_PACKING"));
        }
        let (head, tail) = rest.split_at(4);
        let n = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        if tail.len() < n || out.len() >= 1_000 {
            return Err(code("BAD_PACKING"));
        }
        out.push(tail[..n].to_vec());
        rest = &tail[n..];
    }
    Ok(out)
}

/// The inverse of [`unpack`] (used for the member list output).
fn pack<I: IntoIterator<Item = Vec<u8>>>(items: I) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        out.extend_from_slice(&(item.len() as u32).to_be_bytes());
        out.extend_from_slice(&item);
    }
    out
}

// ---------------------------------------------------------------------------------------
// Stateless functions (wasm: message_aad, inspect_key_package, verify_*, files, transfer)
// ---------------------------------------------------------------------------------------

/// The CF Meet AAD of one application message (binds tenant, application, environment,
/// conversation, client message id and sending device). Each argument is a UUID string.
///
/// # Safety
/// Every `(ptr, len)` pair must satisfy the ABI input rules; `out` must be writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_message_aad(
    tenant: *const u8,
    tenant_len: usize,
    application: *const u8,
    application_len: usize,
    environment: *const u8,
    environment_len: usize,
    conversation: *const u8,
    conversation_len: usize,
    client_message: *const u8,
    client_message_len: usize,
    sender_device: *const u8,
    sender_device_len: usize,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract on every input pair.
        let b = unsafe {
            ApplicationMessageBinding {
                tenant_id: uuid(tenant, tenant_len)?,
                application_id: uuid(application, application_len)?,
                environment_id: uuid(environment, environment_len)?,
                conversation_id: uuid(conversation, conversation_len)?,
                client_message_id: uuid(client_message, client_message_len)?,
                sender_device_id: uuid(sender_device, sender_device_len)?,
            }
        };
        // SAFETY: caller contract on `out`.
        unsafe { write(out, buffer(binding::application_message_aad(&b))) }
    })
}

/// Fully validates a published KeyPackage (signature, cipher suite, lifetime at `now`
/// Unix seconds) and returns its device id (UUID string) and signature public key.
///
/// # Safety
/// ABI input rules; both out pointers writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_inspect_key_package(
    encoded: *const u8,
    encoded_len: usize,
    now: u64,
    out_device_id: *mut CfmBuffer,
    out_signature_key: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        if out_device_id.is_null() || out_signature_key.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let encoded = unsafe { slice(encoded, encoded_len) }?;
        let kp = validate_key_package(encoded, now, &LifetimePolicy::default())
            .map_err(|e| code(e.code()))?;
        // SAFETY: both checked non-NULL; caller contract.
        unsafe {
            write(out_device_id, buffer(kp.device_id.to_string().into_bytes()))?;
            write(out_signature_key, buffer(kp.signature_key))
        }
    })
}

/// Verifies a key-transparency tree head signed by the pinned log key. `*out_valid` is 0
/// for a forged head, a substituted log or a wrong key.
///
/// # Safety
/// ABI input rules; `out_valid` writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_verify_tree_head(
    log_public_key: *const u8,
    log_public_key_len: usize,
    log_id: *const u8,
    log_id_len: usize,
    tree_size: u64,
    root_hash: *const u8,
    root_hash_len: usize,
    timestamp_micros: i64,
    signature: *const u8,
    signature_len: usize,
    out_valid: *mut u8,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract on every input pair.
        let (key, log_id, root, sig) = unsafe {
            (
                slice(log_public_key, log_public_key_len)?,
                string(log_id, log_id_len)?,
                slice(root_hash, root_hash_len)?,
                slice(signature, signature_len)?,
            )
        };
        let input = transparency::tree_head_signing_input(
            log_id,
            tree_size,
            &hash32(root)?,
            timestamp_micros,
        );
        let ok = transparency::verify_tree_head_signature(key, &input, sig);
        // SAFETY: caller contract on `out_valid`.
        unsafe { write(out_valid, u8::from(ok)) }
    })
}

/// Verifies that `leaf_data` is leaf `index` of the tree of `size` with `root`. `proof`
/// is the concatenation of the 32-byte proof hashes.
///
/// # Safety
/// ABI input rules; `out_valid` writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_verify_inclusion(
    index: u64,
    size: u64,
    leaf_data: *const u8,
    leaf_data_len: usize,
    proof: *const u8,
    proof_len: usize,
    root: *const u8,
    root_len: usize,
    out_valid: *mut u8,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract on every input pair.
        let (leaf, proof, root) = unsafe {
            (
                slice(leaf_data, leaf_data_len)?,
                slice(proof, proof_len)?,
                slice(root, root_len)?,
            )
        };
        let proof = proof_hashes(proof)?;
        let ok = transparency::verify_inclusion(
            index,
            size,
            &transparency::leaf_hash(leaf),
            &proof,
            &hash32(root)?,
        );
        // SAFETY: caller contract on `out_valid`.
        unsafe { write(out_valid, u8::from(ok)) }
    })
}

/// Verifies that the tree of `new_size` / `new_root` extends the one of `old_size` /
/// `old_root` (no rewritten or rolled-back history).
///
/// # Safety
/// ABI input rules; `out_valid` writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_verify_consistency(
    old_size: u64,
    new_size: u64,
    old_root: *const u8,
    old_root_len: usize,
    new_root: *const u8,
    new_root_len: usize,
    proof: *const u8,
    proof_len: usize,
    out_valid: *mut u8,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract on every input pair.
        let (old_root, new_root, proof) = unsafe {
            (
                slice(old_root, old_root_len)?,
                slice(new_root, new_root_len)?,
                slice(proof, proof_len)?,
            )
        };
        let proof = proof_hashes(proof)?;
        let ok = transparency::verify_consistency(
            old_size,
            new_size,
            &hash32(old_root)?,
            &hash32(new_root)?,
            &proof,
        );
        // SAFETY: caller contract on `out_valid`.
        unsafe { write(out_valid, u8::from(ok)) }
    })
}

/// A file encrypted for upload. Its descriptor (content key, file name, media type)
/// stays inside this handle until bound to the server-assigned attachment id
/// ([`cfm_encrypted_file_descriptor_for`]).
pub struct CfmEncryptedFile {
    ciphertext: Vec<u8>,
    descriptor: AttachmentDescriptor,
}

/// Encrypts a file for upload (client-side; the server stores ciphertext only).
///
/// # Safety
/// ABI input rules; `out` writable. Release the handle with [`cfm_encrypted_file_free`].
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_encrypt_file(
    plaintext: *const u8,
    plaintext_len: usize,
    media_type: *const u8,
    media_type_len: usize,
    file_name: *const u8,
    file_name_len: usize,
    out: *mut *mut CfmEncryptedFile,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract on every input pair.
        let (plaintext, media_type, file_name) = unsafe {
            (
                slice(plaintext, plaintext_len)?,
                string(media_type, media_type_len)?,
                string(file_name, file_name_len)?,
            )
        };
        let enc = attachment::encrypt_attachment(plaintext, media_type, file_name)
            .map_err(|_| code("ATTACHMENT_ENCRYPT_FAILED"))?;
        let handle = Box::new(CfmEncryptedFile {
            ciphertext: enc.ciphertext,
            descriptor: enc.descriptor,
        });
        // SAFETY: checked non-NULL; caller contract.
        unsafe { write(out, Box::into_raw(handle)) }
    })
}

/// # Safety
/// `file` must be NULL or a live handle from [`cfm_encrypt_file`].
unsafe fn file_ref<'a>(file: *const CfmEncryptedFile) -> Result<&'a CfmEncryptedFile, Failure> {
    // SAFETY: caller contract (a live handle, or NULL which is refused).
    unsafe { file.as_ref() }.ok_or(INVALID)
}

/// The ciphertext to upload.
///
/// # Safety
/// `file` a live handle; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_encrypted_file_ciphertext(
    file: *const CfmEncryptedFile,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { file_ref(file) }?;
        // SAFETY: caller contract.
        unsafe { write(out, buffer(f.ciphertext.clone())) }
    })
}

/// Lower-case hex SHA-256 of the ciphertext (what the upload intent declares).
///
/// # Safety
/// `file` a live handle; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_encrypted_file_sha256(
    file: *const CfmEncryptedFile,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { file_ref(file) }?;
        let hex = f.descriptor.ciphertext_sha256.clone().into_bytes();
        // SAFETY: caller contract.
        unsafe { write(out, buffer(hex)) }
    })
}

/// Size of the ciphertext in bytes.
///
/// # Safety
/// `file` a live handle; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_encrypted_file_size(
    file: *const CfmEncryptedFile,
    out: *mut u64,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let f = unsafe { file_ref(file) }?;
        // SAFETY: caller contract.
        unsafe { write(out, f.descriptor.ciphertext_size) }
    })
}

/// The descriptor bound to the server-assigned attachment id (SECRET: it carries the
/// content key; place it only inside an MLS application message).
///
/// # Safety
/// `file` a live handle; ABI input rules; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_encrypted_file_descriptor_for(
    file: *const CfmEncryptedFile,
    attachment_id: *const u8,
    attachment_id_len: usize,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let (f, id) = unsafe { (file_ref(file)?, uuid(attachment_id, attachment_id_len)?) };
        let bytes = zeroize::Zeroizing::new(f.descriptor.clone().for_attachment(id).to_bytes());
        // SAFETY: caller contract.
        unsafe { write(out, secret_buffer(bytes)) }
    })
}

/// Releases an encrypted-file handle (NULL is a no-op).
///
/// # Safety
/// `file` must be NULL or a live handle from [`cfm_encrypt_file`], not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn cfm_encrypted_file_free(file: *mut CfmEncryptedFile) {
    if file.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: produced by `Box::into_raw` in `cfm_encrypt_file`, freed once.
        drop(unsafe { Box::from_raw(file) });
    }));
}

/// Decrypts a downloaded attachment. Refuses a ciphertext that does not match the
/// descriptor's digest or authentication (`ATTACHMENT_TAMPERED`), and a descriptor of
/// another attachment (`DESCRIPTOR_ATTACHMENT_MISMATCH`).
///
/// # Safety
/// ABI input rules; every out pointer writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_decrypt_file(
    descriptor: *const u8,
    descriptor_len: usize,
    attachment_id: *const u8,
    attachment_id_len: usize,
    ciphertext: *const u8,
    ciphertext_len: usize,
    out_plaintext: *mut CfmBuffer,
    out_media_type: *mut CfmBuffer,
    out_file_name: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        if out_plaintext.is_null() || out_media_type.is_null() || out_file_name.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract on every input pair.
        let (descriptor, ciphertext) = unsafe {
            (
                slice(descriptor, descriptor_len)?,
                slice(ciphertext, ciphertext_len)?,
            )
        };
        let d = AttachmentDescriptor::from_bytes(descriptor).map_err(|_| code("BAD_DESCRIPTOR"))?;
        // SAFETY: caller contract.
        if d.attachment_id != unsafe { uuid(attachment_id, attachment_id_len) }? {
            return Err(code("DESCRIPTOR_ATTACHMENT_MISMATCH"));
        }
        let plaintext = attachment::decrypt_attachment(&d, ciphertext)
            .map_err(|_| code("ATTACHMENT_TAMPERED"))?;
        // SAFETY: all checked non-NULL; caller contract.
        unsafe {
            write(out_plaintext, buffer(plaintext))?;
            write(out_media_type, buffer(d.media_type.clone().into_bytes()))?;
            write(out_file_name, buffer(d.file_name.clone().into_bytes()))
        }
    })
}

/// The protected presentation metadata of a descriptor (shown before download).
///
/// # Safety
/// ABI input rules; every out pointer writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_descriptor_info(
    descriptor: *const u8,
    descriptor_len: usize,
    out_media_type: *mut CfmBuffer,
    out_file_name: *mut CfmBuffer,
    out_size: *mut u64,
) -> CfmStatus {
    guard(|| {
        if out_media_type.is_null() || out_file_name.is_null() || out_size.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let descriptor = unsafe { slice(descriptor, descriptor_len) }?;
        let d = AttachmentDescriptor::from_bytes(descriptor).map_err(|_| code("BAD_DESCRIPTOR"))?;
        // SAFETY: all checked non-NULL; caller contract.
        unsafe {
            write(out_media_type, buffer(d.media_type.clone().into_bytes()))?;
            write(out_file_name, buffer(d.file_name.clone().into_bytes()))?;
            write(out_size, d.plaintext_size)
        }
    })
}

/// Whether `bytes` is an attachment descriptor (an encrypted attachment message).
///
/// # Safety
/// ABI input rules; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_is_descriptor(
    bytes: *const u8,
    bytes_len: usize,
    out: *mut u8,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let bytes = unsafe { slice(bytes, bytes_len) }?;
        let ok = AttachmentDescriptor::from_bytes(bytes).is_ok();
        // SAFETY: caller contract.
        unsafe { write(out, u8::from(ok)) }
    })
}

/// SOURCE of a history transfer: checks the target device signed the transfer request
/// with its registered key. `ctx` is the 7 context UUIDs (transfer, tenant, application,
/// environment, principal, source device, target device).
///
/// # Safety
/// ABI input rules (`ctx` an array of `ctx_count` [`CfmBytes`]); `out_valid` writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_verify_transfer_request(
    target_device_key: *const u8,
    target_device_key_len: usize,
    ctx: *const CfmBytes,
    ctx_count: usize,
    transfer_key: *const u8,
    transfer_key_len: usize,
    expires_at: i64,
    signature: *const u8,
    signature_len: usize,
    out_valid: *mut u8,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract on every input.
        let (target, ctx, transfer_key, signature) = unsafe {
            (
                slice(target_device_key, target_device_key_len)?,
                strings(ctx, ctx_count)?,
                slice(transfer_key, transfer_key_len)?,
                slice(signature, signature_len)?,
            )
        };
        let ok = history_transfer::verify_request(
            target,
            &transfer_context(&ctx)?,
            transfer_key,
            expires_at,
            signature,
        );
        // SAFETY: caller contract.
        unsafe { write(out_valid, u8::from(ok)) }
    })
}

/// The short code both devices show; the user confirms they match.
///
/// # Safety
/// ABI input rules; `out` writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_comparison_code(
    ctx: *const CfmBytes,
    ctx_count: usize,
    transfer_key: *const u8,
    transfer_key_len: usize,
    source_key: *const u8,
    source_key_len: usize,
    target_key: *const u8,
    target_key_len: usize,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract on every input.
        let (ctx, transfer_key, source_key, target_key) = unsafe {
            (
                strings(ctx, ctx_count)?,
                slice(transfer_key, transfer_key_len)?,
                slice(source_key, source_key_len)?,
                slice(target_key, target_key_len)?,
            )
        };
        let c = history_transfer::comparison_code(
            &transfer_context(&ctx)?,
            transfer_key,
            source_key,
            target_key,
        );
        // SAFETY: caller contract.
        unsafe { write(out, buffer(c.into_bytes())) }
    })
}

// ---------------------------------------------------------------------------------------
// Device handle (wasm: WebDevice)
// ---------------------------------------------------------------------------------------

struct DeviceState {
    device: Device,
    groups: Vec<(Vec<u8>, ClientGroup)>,
    transfer: Option<TransferKeys>,
}

/// One device: its MLS signature key, KeyPackage private keys, groups and (while a
/// history transfer is pending) its single-use transfer key. Opaque to C.
pub struct CfmDevice {
    state: Mutex<DeviceState>,
}

impl DeviceState {
    /// The device's state of one group: in memory, or read back from the device's own
    /// storage on first use (an imported device).
    fn group(&mut self, group_id: &[u8]) -> Result<&mut ClientGroup, Failure> {
        if let Some(i) = self.groups.iter().position(|(g, _)| g == group_id) {
            return Ok(&mut self.groups[i].1);
        }
        let g = self
            .device
            .load_group(group_id)
            .map_err(|_| code("GROUP_STATE_INVALID"))?
            .ok_or_else(|| code("UNKNOWN_GROUP"))?;
        self.groups.push((group_id.to_vec(), g));
        let last = self.groups.len() - 1;
        Ok(&mut self.groups[last].1)
    }

    /// Splits the borrow: the device and one of its groups.
    fn device_and_group(
        &mut self,
        group_id: &[u8],
    ) -> Result<(&Device, &mut ClientGroup), Failure> {
        self.group(group_id)?;
        let i = self
            .groups
            .iter()
            .position(|(g, _)| g == group_id)
            .ok_or_else(|| code("UNKNOWN_GROUP"))?;
        Ok((&self.device, &mut self.groups[i].1))
    }
}

/// Runs `f` on the locked device state.
///
/// # Safety
/// `device` must be NULL or a live handle.
unsafe fn with_device<R>(
    device: *const CfmDevice,
    f: impl FnOnce(&mut DeviceState) -> Result<R, Failure>,
) -> Result<R, Failure> {
    // SAFETY: caller contract (a live handle, or NULL which is refused).
    let d = unsafe { device.as_ref() }.ok_or(INVALID)?;
    let mut state = d.state.lock().map_err(|_| POISONED)?;
    f(&mut state)
}

fn new_handle(device: Device) -> *mut CfmDevice {
    Box::into_raw(Box::new(CfmDevice {
        state: Mutex::new(DeviceState {
            device,
            groups: Vec::new(),
            transfer: None,
        }),
    }))
}

fn commit_result(c: CommitOutput) -> CfmCommitResult {
    let has_welcome = c.welcome.is_some();
    CfmCommitResult {
        commit: buffer(c.commit),
        welcome: buffer(c.welcome.unwrap_or_default()),
        has_welcome: u8::from(has_welcome),
        epoch: c.epoch,
    }
}

/// Generates the keys of a new device with the server-assigned `device_id` (UUID).
///
/// # Safety
/// ABI input rules; `out` writable. Release with [`cfm_device_free`].
#[no_mangle]
pub unsafe extern "C" fn cfm_device_new(
    device_id: *const u8,
    device_id_len: usize,
    out: *mut *mut CfmDevice,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let id = unsafe { uuid(device_id, device_id_len) }?;
        let device = Device::generate(id).map_err(|_| code("KEY_GENERATION_FAILED"))?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, new_handle(device)) }
    })
}

/// Restores a device from [`cfm_device_export_state`] (decrypted by the caller).
/// Refuses a malformed state or one of another device (`DEVICE_STATE_INVALID`).
///
/// # Safety
/// ABI input rules; `out` writable. Release with [`cfm_device_free`].
#[no_mangle]
pub unsafe extern "C" fn cfm_device_from_state(
    device_id: *const u8,
    device_id_len: usize,
    state: *const u8,
    state_len: usize,
    out: *mut *mut CfmDevice,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let (id, state) = unsafe { (uuid(device_id, device_id_len)?, slice(state, state_len)?) };
        let device = Device::import_state(id, state).map_err(|_| code("DEVICE_STATE_INVALID"))?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, new_handle(device)) }
    })
}

/// The device's complete secret state (keys and every group), exactly as the wasm
/// binding's `exportState`. SECRET: the caller encrypts it under a platform-protected,
/// non-extractable key before it leaves memory. The buffer is zeroized by
/// [`cfm_buffer_free`].
///
/// # Safety
/// `device` a live handle; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_export_state(
    device: *const CfmDevice,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let state = unsafe { with_device(device, |s| Ok(s.device.export_state())) }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, secret_buffer(state)) }
    })
}

/// The device id (UUID string).
///
/// # Safety
/// `device` a live handle; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_id(device: *const CfmDevice, out: *mut CfmBuffer) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let id = unsafe { with_device(device, |s| Ok(s.device.device_id().to_string())) }?;
        // SAFETY: caller contract.
        unsafe { write(out, buffer(id.into_bytes())) }
    })
}

/// The device signature PUBLIC key (Ed25519, 32 bytes).
///
/// # Safety
/// `device` a live handle; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_signature_public_key(
    device: *const CfmDevice,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let key = unsafe { with_device(device, |s| Ok(s.device.signature_public_key())) }?;
        // SAFETY: caller contract.
        unsafe { write(out, buffer(key)) }
    })
}

/// One ordinary KeyPackage for publication.
///
/// # Safety
/// `device` a live handle; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_key_package(
    device: *const CfmDevice,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let kp = unsafe {
            with_device(device, |s| {
                s.device
                    .key_package()
                    .map_err(|_| code("KEY_PACKAGE_FAILED"))
            })
        }?;
        // SAFETY: caller contract.
        unsafe { write(out, buffer(kp)) }
    })
}

/// The registration KeyPackage answering a server challenge (device binding). `nonce`
/// must be exactly the binding nonce length (`BAD_NONCE` otherwise).
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_registration_key_package(
    device: *const CfmDevice,
    challenge_id: *const u8,
    challenge_id_len: usize,
    nonce: *const u8,
    nonce_len: usize,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract on the inputs.
        let (challenge, nonce) = unsafe {
            (
                uuid(challenge_id, challenge_id_len)?,
                slice(nonce, nonce_len)?,
            )
        };
        let nonce: [u8; binding::NONCE_LEN] = nonce.try_into().map_err(|_| code("BAD_NONCE"))?;
        // SAFETY: caller contract.
        let kp = unsafe {
            with_device(device, |s| {
                s.device
                    .registration_key_package(challenge, &nonce)
                    .map_err(|_| code("KEY_PACKAGE_FAILED"))
            })
        }?;
        // SAFETY: caller contract.
        unsafe { write(out, buffer(kp)) }
    })
}

/// Whether the device holds the group (in memory or in its imported storage).
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_has_group(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    out: *mut u8,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let gid = unsafe { slice(group_id, group_id_len) }?;
        // SAFETY: caller contract.
        let has = unsafe { with_device(device, |s| Ok(s.group(gid).is_ok())) }?;
        // SAFETY: caller contract.
        unsafe { write(out, u8::from(has)) }
    })
}

/// Creates a group with the conversation's group id (this device the only member).
/// Refuses an existing group (`GROUP_EXISTS`). Writes the epoch.
///
/// # Safety
/// `device` a live handle; ABI input rules; `out_epoch` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_create_group(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    out_epoch: *mut u64,
) -> CfmStatus {
    guard(|| {
        if out_epoch.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let gid = unsafe { slice(group_id, group_id_len) }?;
        // SAFETY: caller contract.
        let epoch = unsafe {
            with_device(device, |s| {
                if s.group(gid).is_ok() {
                    return Err(code("GROUP_EXISTS"));
                }
                let g = s
                    .device
                    .create_group(gid)
                    .map_err(|_| code("GROUP_CREATE_REFUSED"))?;
                let e = g.epoch();
                s.groups.push((gid.to_vec(), g));
                Ok(e)
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out_epoch, epoch) }
    })
}

/// Joins from a Welcome; refuses a Welcome of another group than `group_id`
/// (`WELCOME_GROUP_MISMATCH`). Writes the epoch.
///
/// # Safety
/// `device` a live handle; ABI input rules; `out_epoch` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_join(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    welcome: *const u8,
    welcome_len: usize,
    out_epoch: *mut u64,
) -> CfmStatus {
    guard(|| {
        if out_epoch.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let (gid, welcome) =
            unsafe { (slice(group_id, group_id_len)?, slice(welcome, welcome_len)?) };
        // SAFETY: caller contract.
        let epoch = unsafe {
            with_device(device, |s| {
                let g = s
                    .device
                    .join(welcome)
                    .map_err(|_| code("WELCOME_REFUSED"))?;
                if g.group_id() != gid {
                    return Err(code("WELCOME_GROUP_MISMATCH"));
                }
                let e = g.epoch();
                s.groups.retain(|(id, _)| id != gid);
                s.groups.push((gid.to_vec(), g));
                Ok(e)
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out_epoch, epoch) }
    })
}

/// Commits adding the devices of the packed KeyPackages (`[u32 BE len][bytes]...`, each
/// validated by OpenMLS) and merges it locally.
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable. Free both buffers of `out`.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_add_devices(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    packed: *const u8,
    packed_len: usize,
    out: *mut CfmCommitResult,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let (gid, packed) = unsafe { (slice(group_id, group_id_len)?, slice(packed, packed_len)?) };
        let packages = unpack(packed)?;
        if packages.is_empty() {
            return Err(code("NO_KEY_PACKAGES"));
        }
        // SAFETY: caller contract.
        let c = unsafe {
            with_device(device, |s| {
                let (d, g) = s.device_and_group(gid)?;
                d.add_devices(g, &packages).map_err(|_| code("ADD_REFUSED"))
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, commit_result(c)) }
    })
}

/// Commits removing every leaf of the listed device ids (UUID strings) and merges it.
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable. Free both buffers of `out`.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_remove_devices(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    device_ids: *const CfmBytes,
    device_ids_count: usize,
    out: *mut CfmCommitResult,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let (gid, ids) = unsafe {
            (
                slice(group_id, group_id_len)?,
                strings(device_ids, device_ids_count)?,
            )
        };
        let ids = id_list(&ids)?;
        // SAFETY: caller contract.
        let c = unsafe {
            with_device(device, |s| {
                let (d, g) = s.device_and_group(gid)?;
                d.remove_devices(g, &ids)
                    .map_err(|_| code("REMOVE_REFUSED"))
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, commit_result(c)) }
    })
}

/// Encrypts an application message authenticated with `aad` (see [`cfm_message_aad`]).
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_device_encrypt(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    aad: *const u8,
    aad_len: usize,
    plaintext: *const u8,
    plaintext_len: usize,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let (gid, aad, plaintext) = unsafe {
            (
                slice(group_id, group_id_len)?,
                slice(aad, aad_len)?,
                slice(plaintext, plaintext_len)?,
            )
        };
        // SAFETY: caller contract.
        let ct = unsafe {
            with_device(device, |s| {
                let (d, g) = s.device_and_group(gid)?;
                d.encrypt(g, aad, plaintext)
                    .map_err(|_| code("ENCRYPT_REFUSED"))
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, buffer(ct)) }
    })
}

/// Decrypts an application message (only when MLS authenticated exactly the expected AAD
/// with it; `has_expected_aad` 0 means none was expected, which refuses every application
/// message with `AAD_MISMATCH`) or verifies and merges a commit. A tampered or replayed
/// message is refused (`MESSAGE_REFUSED`).
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable. Free `out.plaintext`.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_device_process(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    message: *const u8,
    message_len: usize,
    expected_aad: *const u8,
    expected_aad_len: usize,
    has_expected_aad: u8,
    out: *mut CfmProcessResult,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let (gid, message) =
            unsafe { (slice(group_id, group_id_len)?, slice(message, message_len)?) };
        let expected = if has_expected_aad != 0 {
            // SAFETY: caller contract.
            Some(unsafe { slice(expected_aad, expected_aad_len) }?)
        } else {
            None
        };
        // SAFETY: caller contract.
        let result = unsafe {
            with_device(device, |s| {
                let (d, g) = s.device_and_group(gid)?;
                let p = d.process(g, message).map_err(|_| code("MESSAGE_REFUSED"))?;
                let epoch = g.epoch();
                Ok(match p {
                    Processed::Application { aad, plaintext } => {
                        if expected != Some(aad.as_slice()) {
                            let mut plaintext = plaintext;
                            plaintext.zeroize();
                            return Err(code("AAD_MISMATCH"));
                        }
                        CfmProcessResult {
                            kind: CFM_PROCESSED_APPLICATION,
                            plaintext: buffer(plaintext),
                            epoch,
                        }
                    }
                    Processed::CommitMerged { epoch } => CfmProcessResult {
                        kind: CFM_PROCESSED_COMMIT,
                        plaintext: buffer(Vec::new()),
                        epoch,
                    },
                    Processed::Proposal => CfmProcessResult {
                        kind: CFM_PROCESSED_PROPOSAL,
                        plaintext: buffer(Vec::new()),
                        epoch,
                    },
                })
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, result) }
    })
}

/// The group's current epoch.
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_epoch(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    out: *mut u64,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let gid = unsafe { slice(group_id, group_id_len) }?;
        // SAFETY: caller contract.
        let e = unsafe { with_device(device, |s| Ok(s.group(gid)?.epoch())) }?;
        // SAFETY: caller contract.
        unsafe { write(out, e) }
    })
}

/// The group's member device ids as the MLS tree records them, packed as
/// `[u32 BE len][UUID string]...`.
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_members(
    device: *const CfmDevice,
    group_id: *const u8,
    group_id_len: usize,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        // SAFETY: caller contract.
        let gid = unsafe { slice(group_id, group_id_len) }?;
        // SAFETY: caller contract.
        let members = unsafe {
            with_device(device, |s| {
                Ok(s.group(gid)?
                    .member_devices()
                    .iter()
                    .map(|d| d.to_string().into_bytes())
                    .collect::<Vec<_>>())
            })
        }?;
        // SAFETY: caller contract.
        unsafe { write(out, buffer(pack(members))) }
    })
}

/// TARGET of a history transfer: makes the single-use transfer key (kept in the handle)
/// and returns its public half.
///
/// # Safety
/// `device` a live handle; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_begin_transfer(
    device: *const CfmDevice,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let public = unsafe {
            with_device(device, |s| {
                let keys = TransferKeys::generate().map_err(|_| code("KEY_GENERATION_FAILED"))?;
                let public = keys.public_key().to_vec();
                s.transfer = Some(keys);
                Ok(public)
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, buffer(public)) }
    })
}

/// TARGET: signs the transfer request with the device key (`NO_TRANSFER` before
/// [`cfm_device_begin_transfer`]).
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_sign_transfer_request(
    device: *const CfmDevice,
    ctx: *const CfmBytes,
    ctx_count: usize,
    expires_at: i64,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let ctx = transfer_context(&unsafe { strings(ctx, ctx_count) }?)?;
        // SAFETY: caller contract.
        let sig = unsafe {
            with_device(device, |s| {
                let keys = s.transfer.as_ref().ok_or_else(|| code("NO_TRANSFER"))?;
                history_transfer::sign_request(&s.device, &ctx, keys, expires_at)
                    .map_err(|_| code("TRANSFER_SIGN_FAILED"))
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, buffer(sig)) }
    })
}

/// SOURCE: seals the history archive to the target's transfer key and signs it.
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_device_seal_history(
    device: *const CfmDevice,
    ctx: *const CfmBytes,
    ctx_count: usize,
    transfer_key: *const u8,
    transfer_key_len: usize,
    history: *const u8,
    history_len: usize,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let (ctx, transfer_key, history) = unsafe {
            (
                strings(ctx, ctx_count)?,
                slice(transfer_key, transfer_key_len)?,
                slice(history, history_len)?,
            )
        };
        let ctx = transfer_context(&ctx)?;
        // SAFETY: caller contract.
        let sealed = unsafe {
            with_device(device, |s| {
                history_transfer::seal_history(&s.device, &ctx, transfer_key, history)
                    .map_err(|_| code("TRANSFER_SEAL_FAILED"))
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, buffer(sealed)) }
    })
}

/// TARGET: verifies the source device signature and opens the archive. The transfer key
/// is consumed (single use) whether or not opening succeeds.
///
/// # Safety
/// `device` a live handle; ABI input rules; `out` writable.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn cfm_device_open_history(
    device: *const CfmDevice,
    source_key: *const u8,
    source_key_len: usize,
    ctx: *const CfmBytes,
    ctx_count: usize,
    sealed: *const u8,
    sealed_len: usize,
    out: *mut CfmBuffer,
) -> CfmStatus {
    guard(|| {
        if out.is_null() {
            return Err(INVALID);
        }
        // SAFETY: caller contract.
        let (source_key, ctx, sealed) = unsafe {
            (
                slice(source_key, source_key_len)?,
                strings(ctx, ctx_count)?,
                slice(sealed, sealed_len)?,
            )
        };
        // SAFETY: caller contract.
        let history = unsafe {
            with_device(device, |s| {
                let keys = s.transfer.take().ok_or_else(|| code("NO_TRANSFER"))?;
                let ctx = transfer_context(&ctx)?;
                history_transfer::open_history(&keys, source_key, &ctx, sealed).map_err(|e| {
                    code(match e {
                        history_transfer::TransferError::BadSignature => "TRANSFER_BAD_SIGNATURE",
                        history_transfer::TransferError::Malformed => "TRANSFER_MALFORMED",
                        _ => "TRANSFER_REFUSED",
                    })
                })
            })
        }?;
        // SAFETY: checked non-NULL.
        unsafe { write(out, buffer(history)) }
    })
}

/// Releases a device handle and every key it holds (NULL is a no-op).
///
/// # Safety
/// `device` must be NULL or a live handle, not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn cfm_device_free(device: *mut CfmDevice) {
    if device.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: produced by `Box::into_raw` in `new_handle`, freed once.
        drop(unsafe { Box::from_raw(device) });
    }));
}

#[cfg(test)]
mod tests;

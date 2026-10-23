//! Tests of the C ABI, called exactly as a C / Dart caller would (raw pointers, out
//! parameters, explicit frees), over the REAL crypto core.

use super::*;
use std::ffi::CStr;
use std::ptr::{null, null_mut};

const TENANT: &str = "6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f01";
const APP: &str = "6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f02";
const ENV: &str = "6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f03";
const CONV: &str = "6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f04";
const MSG: &str = "6f1c1f43-2a8e-4e0a-9d43-0d6e5f0b8f05";
const ALICE: &str = "0b6a8f9e-1c2d-4e3f-8a9b-0c1d2e3f4a01";
const BOB: &str = "0b6a8f9e-1c2d-4e3f-8a9b-0c1d2e3f4a02";
const GROUP: &[u8; 32] = b"cfmeet-test-group-0000000000001!";

fn last_error() -> String {
    // SAFETY: the library returns a valid NUL-terminated string.
    unsafe { CStr::from_ptr(cfm_last_error()) }
        .to_str()
        .unwrap()
        .to_owned()
}

fn empty() -> CfmBuffer {
    CfmBuffer {
        ptr: null_mut(),
        len: 0,
    }
}

/// Copies and frees a library buffer.
fn take(buf: CfmBuffer) -> Vec<u8> {
    let v = if buf.len == 0 {
        Vec::new()
    } else {
        // SAFETY: a buffer the library returned.
        unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }.to_vec()
    };
    // SAFETY: returned by the library, freed once.
    unsafe { cfm_buffer_free(buf) };
    v
}

fn ok(status: CfmStatus) {
    assert_eq!(status, CFM_OK, "call failed: {}", last_error());
}

fn refused(status: CfmStatus, expected: &str) {
    assert_eq!(status, CFM_ERROR, "expected refusal {expected}");
    assert_eq!(last_error(), expected);
}

fn device(id: &str) -> *mut CfmDevice {
    let mut d = null_mut();
    // SAFETY: valid inputs and out pointer.
    ok(unsafe { cfm_device_new(id.as_ptr(), id.len(), &mut d) });
    assert!(!d.is_null());
    d
}

fn key_package(d: *mut CfmDevice) -> Vec<u8> {
    let mut out = empty();
    // SAFETY: live handle.
    ok(unsafe { cfm_device_key_package(d, &mut out) });
    take(out)
}

fn aad(sender: &str) -> Vec<u8> {
    let mut out = empty();
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_message_aad(
            TENANT.as_ptr(),
            TENANT.len(),
            APP.as_ptr(),
            APP.len(),
            ENV.as_ptr(),
            ENV.len(),
            CONV.as_ptr(),
            CONV.len(),
            MSG.as_ptr(),
            MSG.len(),
            sender.as_ptr(),
            sender.len(),
            &mut out,
        )
    });
    take(out)
}

fn packed(items: &[&[u8]]) -> Vec<u8> {
    pack(items.iter().map(|i| i.to_vec()))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Commit {
    commit: Vec<u8>,
    welcome: Option<Vec<u8>>,
    epoch: u64,
}

fn commit(out: CfmCommitResult) -> Commit {
    let has = out.has_welcome != 0;
    let commit = take(out.commit);
    let welcome = take(out.welcome);
    Commit {
        commit,
        welcome: has.then_some(welcome),
        epoch: out.epoch,
    }
}

fn add(d: *mut CfmDevice, kps: &[&[u8]]) -> Result<Commit, CfmStatus> {
    let p = packed(kps);
    let mut out = CfmCommitResult {
        commit: empty(),
        welcome: empty(),
        has_welcome: 0,
        epoch: 0,
    };
    // SAFETY: live handle; valid inputs.
    let s = unsafe {
        cfm_device_add_devices(
            d,
            GROUP.as_ptr(),
            GROUP.len(),
            p.as_ptr(),
            p.len(),
            &mut out,
        )
    };
    if s == CFM_OK {
        Ok(commit(out))
    } else {
        Err(s)
    }
}

fn encrypt(d: *mut CfmDevice, aad: &[u8], pt: &[u8]) -> Vec<u8> {
    let mut out = empty();
    // SAFETY: live handle; valid inputs.
    ok(unsafe {
        cfm_device_encrypt(
            d,
            GROUP.as_ptr(),
            GROUP.len(),
            aad.as_ptr(),
            aad.len(),
            pt.as_ptr(),
            pt.len(),
            &mut out,
        )
    });
    take(out)
}

fn process(
    d: *mut CfmDevice,
    msg: &[u8],
    expected: Option<&[u8]>,
) -> Result<(u32, Vec<u8>, u64), CfmStatus> {
    let mut out = CfmProcessResult {
        kind: 0,
        plaintext: empty(),
        epoch: 0,
    };
    let (p, l, has) = match expected {
        Some(a) => (a.as_ptr(), a.len(), 1),
        None => (null(), 0, 0),
    };
    // SAFETY: live handle; valid inputs.
    let s = unsafe {
        cfm_device_process(
            d,
            GROUP.as_ptr(),
            GROUP.len(),
            msg.as_ptr(),
            msg.len(),
            p,
            l,
            has,
            &mut out,
        )
    };
    if s != CFM_OK {
        return Err(s);
    }
    Ok((out.kind, take(out.plaintext), out.epoch))
}

/// Alice creates the group, adds Bob from his KeyPackage, Bob joins from the Welcome.
fn two_member_group() -> (*mut CfmDevice, *mut CfmDevice) {
    let (alice, bob) = (device(ALICE), device(BOB));
    let mut epoch = u64::MAX;
    // SAFETY: live handle; valid inputs.
    ok(unsafe { cfm_device_create_group(alice, GROUP.as_ptr(), GROUP.len(), &mut epoch) });
    assert_eq!(epoch, 0);
    let c = add(alice, &[&key_package(bob)]).unwrap();
    assert_eq!(c.epoch, 0);
    let welcome = c.welcome.expect("an add carries a Welcome");
    // SAFETY: live handle; valid inputs.
    ok(unsafe {
        cfm_device_join(
            bob,
            GROUP.as_ptr(),
            GROUP.len(),
            welcome.as_ptr(),
            welcome.len(),
            &mut epoch,
        )
    });
    assert_eq!(epoch, 1);
    (alice, bob)
}

fn free(d: *mut CfmDevice) {
    // SAFETY: live handle, freed once.
    unsafe { cfm_device_free(d) };
}

#[test]
fn abi_version_and_empty_last_error() {
    assert_eq!(cfm_abi_version(), CFM_ABI_VERSION);
    let d = device(ALICE);
    assert_eq!(last_error(), "");
    free(d);
}

#[test]
fn null_and_malformed_arguments_fail_closed() {
    let mut d = null_mut();
    // NULL pointer with a non-zero length.
    // SAFETY: exercising the NULL checks.
    assert_eq!(
        unsafe { cfm_device_new(null(), 36, &mut d) },
        CFM_INVALID_ARGUMENT
    );
    assert_eq!(last_error(), "INVALID_ARGUMENT");
    assert!(d.is_null());
    // NULL out pointer.
    // SAFETY: as above.
    assert_eq!(
        unsafe { cfm_device_new(ALICE.as_ptr(), ALICE.len(), null_mut()) },
        CFM_INVALID_ARGUMENT
    );
    // Bad UUID / UTF-8.
    // SAFETY: valid pointers.
    refused(
        unsafe { cfm_device_new(b"nope".as_ptr(), 4, &mut d) },
        "BAD_UUID",
    );
    // SAFETY: valid pointers.
    refused(
        unsafe { cfm_device_new([0xff, 0xfe].as_ptr(), 2, &mut d) },
        "BAD_UTF8",
    );
    // NULL handle.
    let mut out = empty();
    // SAFETY: exercising the NULL handle check.
    assert_eq!(
        unsafe { cfm_device_key_package(null(), &mut out) },
        CFM_INVALID_ARGUMENT
    );
    // Freeing NULL handles and empty buffers is a no-op.
    // SAFETY: NULL / empty are documented no-ops.
    unsafe {
        cfm_device_free(null_mut());
        cfm_encrypted_file_free(null_mut());
        cfm_buffer_free(empty());
    }
}

#[test]
fn key_package_inspection_reports_the_device_and_refuses_tampering() {
    let d = device(ALICE);
    let kp = key_package(d);
    let mut sig = empty();
    // SAFETY: live handle.
    ok(unsafe { cfm_device_signature_public_key(d, &mut sig) });
    let sig = take(sig);
    assert_eq!(sig.len(), 32);

    let (mut id, mut key) = (empty(), empty());
    // SAFETY: valid inputs.
    ok(unsafe { cfm_inspect_key_package(kp.as_ptr(), kp.len(), now(), &mut id, &mut key) });
    assert_eq!(String::from_utf8(take(id)).unwrap(), ALICE);
    assert_eq!(take(key), sig);

    let mut bad = kp.clone();
    let i = bad.len() - 10;
    bad[i] ^= 0x01;
    let (mut id, mut key) = (empty(), empty());
    // SAFETY: valid inputs.
    let s = unsafe { cfm_inspect_key_package(bad.as_ptr(), bad.len(), now(), &mut id, &mut key) };
    assert_eq!(s, CFM_ERROR);
    assert!(!last_error().is_empty());
    assert!(id.ptr.is_null() && key.ptr.is_null(), "no output on error");
    free(d);
}

#[test]
fn registration_key_package_requires_an_exact_nonce() {
    let d = device(ALICE);
    let challenge = "8d0b1a5e-7f7c-4d4e-9a3b-2b2c2d2e2f30";
    let mut out = empty();
    let nonce = [7u8; 32];
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_device_registration_key_package(
            d,
            challenge.as_ptr(),
            challenge.len(),
            nonce.as_ptr(),
            32,
            &mut out,
        )
    });
    assert!(!take(out).is_empty());
    let mut out = empty();
    // SAFETY: valid inputs.
    refused(
        unsafe {
            cfm_device_registration_key_package(
                d,
                challenge.as_ptr(),
                challenge.len(),
                nonce.as_ptr(),
                31,
                &mut out,
            )
        },
        "BAD_NONCE",
    );
    free(d);
}

#[test]
fn two_devices_exchange_messages_and_refuse_tampering_replay_and_wrong_aad() {
    let (alice, bob) = two_member_group();
    let a_aad = aad(ALICE);
    let ct = encrypt(alice, &a_aad, b"hello bob");
    let (kind, pt, epoch) = process(bob, &ct, Some(&a_aad)).unwrap();
    assert_eq!(
        (kind, pt.as_slice(), epoch),
        (CFM_PROCESSED_APPLICATION, &b"hello bob"[..], 1)
    );

    // Replay of the same ciphertext.
    assert_eq!(process(bob, &ct, Some(&a_aad)), Err(CFM_ERROR));
    assert_eq!(last_error(), "MESSAGE_REFUSED");

    // Tampered ciphertext.
    let mut bad = encrypt(alice, &a_aad, b"second");
    let i = bad.len() - 5;
    bad[i] ^= 0x80;
    assert_eq!(process(bob, &bad, Some(&a_aad)), Err(CFM_ERROR));
    assert_eq!(last_error(), "MESSAGE_REFUSED");

    // A message bound to another sender (AAD of Bob) is refused, and so is a missing AAD.
    let ct = encrypt(alice, &a_aad, b"third");
    assert_eq!(process(bob, &ct, Some(&aad(BOB))), Err(CFM_ERROR));
    assert_eq!(last_error(), "AAD_MISMATCH");
    let ct = encrypt(alice, &a_aad, b"fourth");
    assert_eq!(process(bob, &ct, None), Err(CFM_ERROR));
    assert_eq!(last_error(), "AAD_MISMATCH");

    // Members as recorded by the MLS tree (packed list).
    let mut out = empty();
    // SAFETY: live handle; valid inputs.
    ok(unsafe { cfm_device_members(bob, GROUP.as_ptr(), GROUP.len(), &mut out) });
    let mut members: Vec<String> = unpack(&take(out))
        .unwrap()
        .into_iter()
        .map(|m| String::from_utf8(m).unwrap())
        .collect();
    members.sort();
    assert_eq!(members, vec![ALICE.to_owned(), BOB.to_owned()]);

    // Removing Bob: Alice moves to epoch 2; Bob processes the commit and can no longer read.
    let ids = [CfmBytes {
        ptr: BOB.as_ptr(),
        len: BOB.len(),
    }];
    let mut out = CfmCommitResult {
        commit: empty(),
        welcome: empty(),
        has_welcome: 0,
        epoch: 0,
    };
    // SAFETY: live handle; valid inputs.
    ok(unsafe {
        cfm_device_remove_devices(
            alice,
            GROUP.as_ptr(),
            GROUP.len(),
            ids.as_ptr(),
            1,
            &mut out,
        )
    });
    let c = commit(out);
    assert!(c.welcome.is_none());
    let mut e = 0;
    // SAFETY: live handle.
    ok(unsafe { cfm_device_epoch(alice, GROUP.as_ptr(), GROUP.len(), &mut e) });
    assert_eq!(e, 2);
    let (kind, _, _) = process(bob, &c.commit, None).unwrap();
    assert_eq!(kind, CFM_PROCESSED_COMMIT);
    let after = encrypt(alice, &a_aad, b"bob is gone");
    assert!(process(bob, &after, Some(&a_aad)).is_err());
    free(alice);
    free(bob);
}

#[test]
fn group_lifecycle_refusals() {
    let (alice, bob) = two_member_group();
    let mut e = 0;
    // Creating an existing group.
    // SAFETY: live handle.
    refused(
        unsafe { cfm_device_create_group(alice, GROUP.as_ptr(), GROUP.len(), &mut e) },
        "GROUP_EXISTS",
    );
    // Unknown group.
    let other = [9u8; 32];
    // SAFETY: live handle.
    refused(
        unsafe { cfm_device_epoch(alice, other.as_ptr(), 32, &mut e) },
        "UNKNOWN_GROUP",
    );
    let mut has = 9;
    // SAFETY: live handle.
    ok(unsafe { cfm_device_has_group(alice, other.as_ptr(), 32, &mut has) });
    assert_eq!(has, 0);
    // SAFETY: live handle.
    ok(unsafe { cfm_device_has_group(bob, GROUP.as_ptr(), GROUP.len(), &mut has) });
    assert_eq!(has, 1);
    // No KeyPackages / malformed packing / a garbage KeyPackage.
    assert_eq!(add(alice, &[]).err(), Some(CFM_ERROR));
    assert_eq!(last_error(), "NO_KEY_PACKAGES");
    let mut out = CfmCommitResult {
        commit: empty(),
        welcome: empty(),
        has_welcome: 0,
        epoch: 0,
    };
    // SAFETY: live handle; valid inputs.
    refused(
        unsafe {
            cfm_device_add_devices(
                alice,
                GROUP.as_ptr(),
                GROUP.len(),
                [0, 0, 0, 9, 1].as_ptr(),
                5,
                &mut out,
            )
        },
        "BAD_PACKING",
    );
    assert_eq!(add(alice, &[b"garbage"]).err(), Some(CFM_ERROR));
    assert_eq!(last_error(), "ADD_REFUSED");
    // A Welcome for another group id.
    let carol = device("0b6a8f9e-1c2d-4e3f-8a9b-0c1d2e3f4a03");
    let c = add(alice, &[&key_package(carol)]).unwrap();
    let w = c.welcome.unwrap();
    // SAFETY: live handle; valid inputs.
    refused(
        unsafe { cfm_device_join(carol, other.as_ptr(), 32, w.as_ptr(), w.len(), &mut e) },
        "WELCOME_GROUP_MISMATCH",
    );
    // SAFETY: live handle; valid inputs.
    refused(
        unsafe { cfm_device_join(carol, GROUP.as_ptr(), 32, b"x".as_ptr(), 1, &mut e) },
        "WELCOME_REFUSED",
    );
    free(alice);
    free(bob);
    free(carol);
}

#[test]
fn exported_state_restores_the_device_and_its_groups_only_for_the_same_device() {
    let (alice, bob) = two_member_group();
    let mut st = empty();
    // SAFETY: live handle.
    ok(unsafe { cfm_device_export_state(bob, &mut st) });
    let state = take(st);
    free(bob);

    let mut restored = null_mut();
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_device_from_state(
            BOB.as_ptr(),
            BOB.len(),
            state.as_ptr(),
            state.len(),
            &mut restored,
        )
    });
    let a_aad = aad(ALICE);
    let ct = encrypt(alice, &a_aad, b"after restore");
    let (_, pt, _) = process(restored, &ct, Some(&a_aad)).unwrap();
    assert_eq!(pt, b"after restore");

    let mut other = null_mut();
    // State of Bob under Alice's id; truncated state.
    // SAFETY: valid inputs.
    refused(
        unsafe {
            cfm_device_from_state(
                ALICE.as_ptr(),
                ALICE.len(),
                state.as_ptr(),
                state.len(),
                &mut other,
            )
        },
        "DEVICE_STATE_INVALID",
    );
    // SAFETY: valid inputs.
    refused(
        unsafe {
            cfm_device_from_state(
                BOB.as_ptr(),
                BOB.len(),
                state.as_ptr(),
                state.len() - 1,
                &mut other,
            )
        },
        "DEVICE_STATE_INVALID",
    );
    assert!(other.is_null());
    free(alice);
    free(restored);
}

#[test]
fn attachments_round_trip_and_refuse_tampering() {
    let mut f = null_mut();
    let pt = b"%PDF-1.7 attachment body";
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_encrypt_file(
            pt.as_ptr(),
            pt.len(),
            b"application/pdf".as_ptr(),
            15,
            b"a.pdf".as_ptr(),
            5,
            &mut f,
        )
    });
    let (mut ct, mut sha) = (empty(), empty());
    let mut size = 0;
    // SAFETY: live handle.
    unsafe {
        ok(cfm_encrypted_file_ciphertext(f, &mut ct));
        ok(cfm_encrypted_file_sha256(f, &mut sha));
        ok(cfm_encrypted_file_size(f, &mut size));
    }
    let ct = take(ct);
    assert_eq!(size, ct.len() as u64);
    assert_eq!(take(sha).len(), 64);
    let att = "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f";
    let mut d = empty();
    // SAFETY: live handle; valid inputs.
    ok(unsafe { cfm_encrypted_file_descriptor_for(f, att.as_ptr(), att.len(), &mut d) });
    // SAFETY: live handle, freed once.
    unsafe { cfm_encrypted_file_free(f) };
    let desc = take(d);

    let mut is = 0;
    // SAFETY: valid inputs.
    ok(unsafe { cfm_is_descriptor(desc.as_ptr(), desc.len(), &mut is) });
    assert_eq!(is, 1);
    // SAFETY: valid inputs.
    ok(unsafe { cfm_is_descriptor(b"hello".as_ptr(), 5, &mut is) });
    assert_eq!(is, 0);

    let (mut mt, mut name) = (empty(), empty());
    let mut psize = 0;
    // SAFETY: valid inputs.
    ok(unsafe { cfm_descriptor_info(desc.as_ptr(), desc.len(), &mut mt, &mut name, &mut psize) });
    assert_eq!(take(mt), b"application/pdf");
    assert_eq!(take(name), b"a.pdf");
    assert_eq!(psize, pt.len() as u64);

    let (mut p, mut mt, mut name) = (empty(), empty(), empty());
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_decrypt_file(
            desc.as_ptr(),
            desc.len(),
            att.as_ptr(),
            att.len(),
            ct.as_ptr(),
            ct.len(),
            &mut p,
            &mut mt,
            &mut name,
        )
    });
    assert_eq!(take(p), pt);
    take(mt);
    take(name);

    let mut bad = ct.clone();
    bad[20] ^= 1;
    let (mut p, mut mt, mut name) = (empty(), empty(), empty());
    // SAFETY: valid inputs.
    refused(
        unsafe {
            cfm_decrypt_file(
                desc.as_ptr(),
                desc.len(),
                att.as_ptr(),
                att.len(),
                bad.as_ptr(),
                bad.len(),
                &mut p,
                &mut mt,
                &mut name,
            )
        },
        "ATTACHMENT_TAMPERED",
    );
    let other = "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e70";
    // SAFETY: valid inputs.
    refused(
        unsafe {
            cfm_decrypt_file(
                desc.as_ptr(),
                desc.len(),
                other.as_ptr(),
                other.len(),
                ct.as_ptr(),
                ct.len(),
                &mut p,
                &mut mt,
                &mut name,
            )
        },
        "DESCRIPTOR_ATTACHMENT_MISMATCH",
    );
}

#[test]
fn transparency_proofs() {
    let (a, b) = (transparency::leaf_hash(b"a"), transparency::leaf_hash(b"b"));
    let root = transparency::node_hash(&a, &b);
    let mut valid = 9;
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_verify_inclusion(
            0,
            2,
            b"a".as_ptr(),
            1,
            b.as_ptr(),
            32,
            root.as_ptr(),
            32,
            &mut valid,
        )
    });
    assert_eq!(valid, 1);
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_verify_inclusion(
            1,
            2,
            b"a".as_ptr(),
            1,
            b.as_ptr(),
            32,
            root.as_ptr(),
            32,
            &mut valid,
        )
    });
    assert_eq!(valid, 0);
    // SAFETY: valid inputs.
    refused(
        unsafe {
            cfm_verify_inclusion(
                0,
                2,
                b"a".as_ptr(),
                1,
                b.as_ptr(),
                31,
                root.as_ptr(),
                32,
                &mut valid,
            )
        },
        "KT_BAD_PROOF",
    );
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_verify_consistency(
            1,
            2,
            a.as_ptr(),
            32,
            root.as_ptr(),
            32,
            b.as_ptr(),
            32,
            &mut valid,
        )
    });
    assert_eq!(valid, 1);
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_verify_consistency(
            1,
            2,
            b.as_ptr(),
            32,
            root.as_ptr(),
            32,
            b.as_ptr(),
            32,
            &mut valid,
        )
    });
    assert_eq!(valid, 0);
    // A forged tree head signature.
    let key = [1u8; 32];
    let sig = [0u8; 64];
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_verify_tree_head(
            key.as_ptr(),
            32,
            b"log".as_ptr(),
            3,
            2,
            root.as_ptr(),
            32,
            0,
            sig.as_ptr(),
            64,
            &mut valid,
        )
    });
    assert_eq!(valid, 0);
    // SAFETY: valid inputs.
    refused(
        unsafe {
            cfm_verify_tree_head(
                key.as_ptr(),
                32,
                b"log".as_ptr(),
                3,
                2,
                root.as_ptr(),
                31,
                0,
                sig.as_ptr(),
                64,
                &mut valid,
            )
        },
        "KT_BAD_HASH",
    );
}

#[test]
fn history_transfer_between_two_devices() {
    let (source, target) = (device(ALICE), device(BOB));
    let ctx_ids = [
        "aaaaaaaa-0000-4000-8000-000000000001",
        TENANT,
        APP,
        ENV,
        "aaaaaaaa-0000-4000-8000-000000000005",
        ALICE,
        BOB,
    ];
    let ctx: Vec<CfmBytes> = ctx_ids
        .iter()
        .map(|s| CfmBytes {
            ptr: s.as_ptr(),
            len: s.len(),
        })
        .collect();
    let mut out = empty();
    // Signing before beginning a transfer.
    // SAFETY: live handle; valid inputs.
    refused(
        unsafe {
            cfm_device_sign_transfer_request(target, ctx.as_ptr(), 7, 4_000_000_000, &mut out)
        },
        "NO_TRANSFER",
    );
    // SAFETY: live handle.
    ok(unsafe { cfm_device_begin_transfer(target, &mut out) });
    let tkey = take(out);
    let mut out = empty();
    // SAFETY: live handle; valid inputs.
    ok(unsafe {
        cfm_device_sign_transfer_request(target, ctx.as_ptr(), 7, 4_000_000_000, &mut out)
    });
    let req_sig = take(out);
    // Bad context arity.
    let mut out = empty();
    // SAFETY: live handle; valid inputs.
    refused(
        unsafe {
            cfm_device_sign_transfer_request(target, ctx.as_ptr(), 6, 4_000_000_000, &mut out)
        },
        "BAD_CONTEXT",
    );

    let (mut skey, mut tdkey) = (empty(), empty());
    // SAFETY: live handles.
    unsafe {
        ok(cfm_device_signature_public_key(source, &mut skey));
        ok(cfm_device_signature_public_key(target, &mut tdkey));
    }
    let (skey, tdkey) = (take(skey), take(tdkey));
    let mut valid = 0;
    // SAFETY: valid inputs.
    ok(unsafe {
        cfm_verify_transfer_request(
            tdkey.as_ptr(),
            32,
            ctx.as_ptr(),
            7,
            tkey.as_ptr(),
            tkey.len(),
            4_000_000_000,
            req_sig.as_ptr(),
            req_sig.len(),
            &mut valid,
        )
    });
    assert_eq!(valid, 1);

    let (mut c1, mut c2) = (empty(), empty());
    // SAFETY: valid inputs.
    unsafe {
        ok(cfm_comparison_code(
            ctx.as_ptr(),
            7,
            tkey.as_ptr(),
            tkey.len(),
            skey.as_ptr(),
            32,
            tdkey.as_ptr(),
            32,
            &mut c1,
        ));
        ok(cfm_comparison_code(
            ctx.as_ptr(),
            7,
            tkey.as_ptr(),
            tkey.len(),
            skey.as_ptr(),
            32,
            tdkey.as_ptr(),
            32,
            &mut c2,
        ));
    }
    let code1 = take(c1);
    assert!(!code1.is_empty());
    assert_eq!(code1, take(c2));

    let mut out = empty();
    // SAFETY: live handle; valid inputs.
    ok(unsafe {
        cfm_device_seal_history(
            source,
            ctx.as_ptr(),
            7,
            tkey.as_ptr(),
            tkey.len(),
            b"archive".as_ptr(),
            7,
            &mut out,
        )
    });
    let sealed = take(out);
    // Wrong source key: refused, and the single-use key is consumed.
    let mut out = empty();
    // SAFETY: live handle; valid inputs.
    refused(
        unsafe {
            cfm_device_open_history(
                target,
                tdkey.as_ptr(),
                32,
                ctx.as_ptr(),
                7,
                sealed.as_ptr(),
                sealed.len(),
                &mut out,
            )
        },
        "TRANSFER_BAD_SIGNATURE",
    );
    // SAFETY: live handle; valid inputs.
    refused(
        unsafe {
            cfm_device_open_history(
                target,
                skey.as_ptr(),
                32,
                ctx.as_ptr(),
                7,
                sealed.as_ptr(),
                sealed.len(),
                &mut out,
            )
        },
        "NO_TRANSFER",
    );
    free(source);
    free(target);
}

#[test]
fn history_transfer_opens_with_the_right_source_key() {
    let (source, target) = (device(ALICE), device(BOB));
    let ids = [
        "aaaaaaaa-0000-4000-8000-000000000001",
        TENANT,
        APP,
        ENV,
        "aaaaaaaa-0000-4000-8000-000000000005",
        ALICE,
        BOB,
    ];
    let ctx: Vec<CfmBytes> = ids
        .iter()
        .map(|s| CfmBytes {
            ptr: s.as_ptr(),
            len: s.len(),
        })
        .collect();
    let mut out = empty();
    // SAFETY: live handle.
    ok(unsafe { cfm_device_begin_transfer(target, &mut out) });
    let tkey = take(out);
    let mut skey = empty();
    // SAFETY: live handle.
    ok(unsafe { cfm_device_signature_public_key(source, &mut skey) });
    let skey = take(skey);
    let mut out = empty();
    // SAFETY: live handle; valid inputs.
    ok(unsafe {
        cfm_device_seal_history(
            source,
            ctx.as_ptr(),
            7,
            tkey.as_ptr(),
            tkey.len(),
            b"archive".as_ptr(),
            7,
            &mut out,
        )
    });
    let sealed = take(out);
    let mut out = empty();
    // SAFETY: live handle; valid inputs.
    ok(unsafe {
        cfm_device_open_history(
            target,
            skey.as_ptr(),
            32,
            ctx.as_ptr(),
            7,
            sealed.as_ptr(),
            sealed.len(),
            &mut out,
        )
    });
    assert_eq!(take(out), b"archive");
    free(source);
    free(target);
}

#[test]
fn panics_are_caught_and_poison_the_handle() {
    let d = device(ALICE);
    let status = guard(|| {
        // SAFETY: live handle.
        unsafe {
            with_device(d, |_| -> Result<(), Failure> {
                panic!("boom with secret-ish text")
            })
        }
    });
    assert_eq!(status, CFM_PANIC);
    assert_eq!(last_error(), "INTERNAL_PANIC");
    let mut out = empty();
    // SAFETY: live (poisoned) handle.
    assert_eq!(unsafe { cfm_device_key_package(d, &mut out) }, CFM_PANIC);
    assert_eq!(last_error(), "HANDLE_POISONED");
    free(d);
}

#[test]
fn packing_round_trips_and_is_bounded() {
    let p = pack(vec![b"one".to_vec(), Vec::new(), b"three".to_vec()]);
    assert_eq!(
        unpack(&p).unwrap(),
        vec![b"one".to_vec(), Vec::new(), b"three".to_vec()]
    );
    assert_eq!(unpack(&[0, 0]).unwrap_err().1, "BAD_PACKING");
    let many = pack((0..1_001).map(|_| Vec::new()));
    assert_eq!(unpack(&many).unwrap_err().1, "BAD_PACKING");
}

#[test]
fn secret_buffers_are_exact_and_freed() {
    let b = secret_buffer(zeroize::Zeroizing::new(vec![1, 2, 3]));
    assert_eq!(b.len, 3);
    assert_eq!(take(b), vec![1, 2, 3]);
    let e = secret_buffer(zeroize::Zeroizing::new(Vec::new()));
    assert!(e.ptr.is_null());
}

#[test]
fn the_c_header_declares_every_exported_function() {
    let lib = include_str!("lib.rs");
    let header = include_str!("../include/cf_messaging_crypto.h");
    let mut exported = 0;
    for line in lib.lines() {
        let line = line.trim_start();
        let Some(rest) = line
            .strip_prefix("pub unsafe extern \"C\" fn ")
            .or_else(|| line.strip_prefix("pub extern \"C\" fn "))
        else {
            continue;
        };
        let name = rest.split('(').next().unwrap();
        exported += 1;
        assert!(
            header.contains(&format!(" {name}(")) || header.contains(&format!("*{name}(")),
            "{name} missing from the C header"
        );
    }
    assert!(exported >= 35, "only {exported} exported functions found");
    assert!(header.contains(&format!("#define CFM_ABI_VERSION {CFM_ABI_VERSION}u")));
}

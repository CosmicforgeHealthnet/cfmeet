//! Canonical binding formats shared by clients and the Messaging backend.
//!
//! * **Device credential identity** — the identity of the MLS `BasicCredential` in every
//!   KeyPackage a device publishes: `cfmeet-messaging-device:v1:<device id>`. The device
//!   id is SERVER-assigned (with the registration challenge), so a client can never pick
//!   another device's identity.
//! * **Registration binding** — the RFC 9420 `application_id` leaf-node extension of the
//!   one-off *registration KeyPackage*:
//!   `cfmeet-messaging-registration:v1:<challenge id>:<nonce hex>`. The leaf node
//!   signature (RFC 9420 §7.2, verified by OpenMLS) covers it, so a valid registration
//!   KeyPackage proves possession of the device signature key AND freshness (the
//!   single-use server challenge) without any custom signature construction.
//!
//! Both are ASCII, versioned and parsed strictly (lower-case hyphenated UUIDs, 64
//! lower-case hex nonce characters).

use uuid::Uuid;

const DEVICE_PREFIX: &str = "cfmeet-messaging-device:v1:";
const REGISTRATION_PREFIX: &str = "cfmeet-messaging-registration:v1:";

/// Length of a registration nonce in bytes.
pub const NONCE_LEN: usize = 32;

/// The credential identity of `device_id`.
pub fn device_credential_identity(device_id: Uuid) -> Vec<u8> {
    format!("{DEVICE_PREFIX}{}", device_id.hyphenated()).into_bytes()
}

/// The device id a credential identity names, if it is exactly the canonical format.
pub fn parse_device_credential_identity(identity: &[u8]) -> Option<Uuid> {
    let text = std::str::from_utf8(identity).ok()?;
    let id = text.strip_prefix(DEVICE_PREFIX)?;
    canonical_uuid(id)
}

/// The registration binding of a challenge.
pub fn registration_binding(challenge_id: Uuid, nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut out = format!("{REGISTRATION_PREFIX}{}:", challenge_id.hyphenated());
    for b in nonce {
        out.push_str(&format!("{b:02x}"));
    }
    out.into_bytes()
}

/// `(challenge id, nonce)` of a registration binding in the canonical format.
pub fn parse_registration_binding(binding: &[u8]) -> Option<(Uuid, [u8; NONCE_LEN])> {
    let text = std::str::from_utf8(binding).ok()?;
    let rest = text.strip_prefix(REGISTRATION_PREFIX)?;
    let (id, hex) = rest.split_once(':')?;
    let id = canonical_uuid(id)?;
    if hex.len() != NONCE_LEN * 2 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut nonce = [0u8; NONCE_LEN];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).ok()?;
        nonce[i] = u8::from_str_radix(s, 16).ok()?;
    }
    Some((id, nonce))
}

fn canonical_uuid(text: &str) -> Option<Uuid> {
    let id = Uuid::parse_str(text).ok()?;
    (id.hyphenated().to_string() == text).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_identities_round_trip_and_are_strict() {
        let id = Uuid::new_v4();
        let identity = device_credential_identity(id);
        assert_eq!(parse_device_credential_identity(&identity), Some(id));
        let upper = format!(
            "{DEVICE_PREFIX}{}",
            id.hyphenated().to_string().to_uppercase()
        );
        for bad in [
            upper.as_bytes().to_vec(),
            format!("{DEVICE_PREFIX}{}", id.simple()).into_bytes(),
            format!("cfmeet-messaging-device:v2:{}", id.hyphenated()).into_bytes(),
            format!("{DEVICE_PREFIX}{} ", id.hyphenated()).into_bytes(),
            b"alice@example.com".to_vec(),
            vec![0xff, 0xfe],
        ] {
            assert_eq!(parse_device_credential_identity(&bad), None);
        }
    }

    #[test]
    fn registration_bindings_round_trip_and_are_strict() {
        let id = Uuid::new_v4();
        let nonce = [0xabu8; NONCE_LEN];
        let binding = registration_binding(id, &nonce);
        assert_eq!(parse_registration_binding(&binding), Some((id, nonce)));
        let text = String::from_utf8(binding).unwrap();
        for bad in [
            text.to_uppercase(),
            text[..text.len() - 2].to_string(),
            format!("{text}00"),
            text.replace("registration:v1", "registration:v0"),
        ] {
            assert_eq!(parse_registration_binding(bad.as_bytes()), None, "{bad}");
        }
    }
}

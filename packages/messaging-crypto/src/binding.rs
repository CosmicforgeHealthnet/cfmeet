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
//! * **Application-message AAD** — the MLS `authenticated_data` of every application
//!   message (Part V Appendix H.1):
//!   `cfmeet-messaging-aad:v2:<security profile>:<protocol>:<cipher suite>:<tenant id>:
//!   <application id>:<environment id>:<conversation id>:<client message id>:<sender
//!   device id>` (one line). It binds the security boundary (profile, protocol, pinned
//!   suite), the authorization scope and the message identity, so a ciphertext cannot be
//!   transplanted across tenants, applications, environments, conversations, profiles or
//!   devices. Only client-known values: the server's canonical sequence / message id are
//!   bound separately by the server's acceptance record (services/messaging).
//!
//! All are ASCII, versioned and parsed strictly (lower-case hyphenated UUIDs, 64
//! lower-case hex nonce characters).

use uuid::Uuid;

use crate::profile;

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

const AAD_PREFIX: &str = "cfmeet-messaging-aad:v2:";

/// What the MLS authenticated data of an application message binds. The security profile,
/// protocol and cipher suite are fixed by [`crate::profile`] (the only E2EE profile).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApplicationMessageBinding {
    pub tenant_id: Uuid,
    pub application_id: Uuid,
    pub environment_id: Uuid,
    pub conversation_id: Uuid,
    pub client_message_id: Uuid,
    pub sender_device_id: Uuid,
}

impl ApplicationMessageBinding {
    fn ids(&self) -> [Uuid; 6] {
        [
            self.tenant_id,
            self.application_id,
            self.environment_id,
            self.conversation_id,
            self.client_message_id,
            self.sender_device_id,
        ]
    }
}

fn aad_header() -> String {
    format!(
        "{AAD_PREFIX}{}:{}:{}:",
        profile::SECURITY_PROFILE,
        profile::PROTOCOL_ID,
        profile::CIPHERSUITE_NAME
    )
}

/// The MLS authenticated data of an application message.
pub fn application_message_aad(binding: &ApplicationMessageBinding) -> Vec<u8> {
    let ids: Vec<String> = binding
        .ids()
        .iter()
        .map(|id| id.hyphenated().to_string())
        .collect();
    format!("{}{}", aad_header(), ids.join(":")).into_bytes()
}

/// The binding of a canonical AAD; `None` for any other profile, protocol, suite, version
/// or encoding.
pub fn parse_application_message_aad(aad: &[u8]) -> Option<ApplicationMessageBinding> {
    let text = std::str::from_utf8(aad).ok()?;
    let mut parts = text.strip_prefix(aad_header().as_str())?.split(':');
    let mut next = || canonical_uuid(parts.next()?);
    let binding = ApplicationMessageBinding {
        tenant_id: next()?,
        application_id: next()?,
        environment_id: next()?,
        conversation_id: next()?,
        client_message_id: next()?,
        sender_device_id: next()?,
    };
    parts.next().is_none().then_some(binding)
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

    fn binding() -> ApplicationMessageBinding {
        ApplicationMessageBinding {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            conversation_id: Uuid::new_v4(),
            client_message_id: Uuid::new_v4(),
            sender_device_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn application_aad_round_trips_and_is_strict() {
        let b = binding();
        let aad = application_message_aad(&b);
        assert_eq!(parse_application_message_aad(&aad), Some(b));
        let text = String::from_utf8(aad).unwrap();
        assert!(text.starts_with(
            "cfmeet-messaging-aad:v2:PRIVATE_E2EE:MLS_1_0:MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519:"
        ));
        for bad in [
            format!("{text}:extra"),
            text.to_uppercase(),
            text.replace(":v2:", ":v1:"),
            text.replace("PRIVATE_E2EE", "MANAGED"),
            text.replace("MLS_1_0", "MLS_2_0"),
            text.replace("AES128GCM", "CHACHA20POLY1305"),
            text[..text.len() - 1].to_string(),
        ] {
            assert_eq!(parse_application_message_aad(bad.as_bytes()), None, "{bad}");
        }
    }

    #[test]
    fn every_bound_field_changes_the_aad() {
        let b = binding();
        let aad = application_message_aad(&b);
        let other = Uuid::new_v4();
        let variants = [
            ApplicationMessageBinding {
                tenant_id: other,
                ..b
            },
            ApplicationMessageBinding {
                application_id: other,
                ..b
            },
            ApplicationMessageBinding {
                environment_id: other,
                ..b
            },
            ApplicationMessageBinding {
                conversation_id: other,
                ..b
            },
            ApplicationMessageBinding {
                client_message_id: other,
                ..b
            },
            ApplicationMessageBinding {
                sender_device_id: other,
                ..b
            },
            // Swapping two ids is a different binding, not the same bytes.
            ApplicationMessageBinding {
                tenant_id: b.application_id,
                application_id: b.tenant_id,
                ..b
            },
        ];
        for v in variants {
            assert_ne!(application_message_aad(&v), aad, "{v:?}");
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

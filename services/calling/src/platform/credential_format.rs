//! API credential wire format.
//!
//! ```text
//! cfmk_<env>_<public_id>_<secret>
//!      │     │           └─ 32 bytes from the OS CSPRNG, 64 lowercase hex chars
//!      │     └─ 12 random bytes, 24 lowercase hex chars: non-secret lookup id
//!      └─ sbx | test | live (convenience only)
//! ```
//!
//! The environment tag lets humans and secret scanners recognise a credential; it grants
//! nothing. The persisted environment binding of the credential is authoritative, and
//! the tag is also bound into the verifier message, so a credential presented with an
//! edited tag never verifies. Tenant / application identifiers are deliberately absent.

use std::fmt;

use rand::{rngs::OsRng, RngCore};
use zeroize::Zeroizing;

use crate::domain::{EnvironmentKind, IssuedSecret};

pub const CREDENTIAL_PREFIX: &str = "cfmk";
pub const PUBLIC_ID_BYTES: usize = 12;
pub const SECRET_BYTES: usize = 32;
/// Presented values longer than this are rejected before any parsing or lookup.
pub const MAX_PRESENTED_LEN: usize = 128;
/// Domain-separation label of the verifier message.
const VERIFIER_CONTEXT: &[u8] = b"cfmeet.calling.api-credential.v1";

/// A freshly generated credential. The raw secret exists only inside `token` (redacted,
/// zeroized) and `secret` (zeroized) until both are dropped.
pub(crate) struct GeneratedCredential {
    pub public_id: String,
    pub last_four: String,
    pub secret: Zeroizing<[u8; SECRET_BYTES]>,
    pub token: IssuedSecret,
}

impl fmt::Debug for GeneratedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedCredential")
            .field("public_id", &self.public_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

pub(crate) fn generate(kind: EnvironmentKind) -> GeneratedCredential {
    let mut public = [0u8; PUBLIC_ID_BYTES];
    OsRng.fill_bytes(&mut public);
    let mut secret = Zeroizing::new([0u8; SECRET_BYTES]);
    OsRng.fill_bytes(secret.as_mut());

    let public_id = hex::encode(public);
    let secret_hex = Zeroizing::new(hex::encode(secret.as_ref()));
    let last_four = secret_hex[secret_hex.len() - 4..].to_string();
    let token = IssuedSecret::new(format!(
        "{CREDENTIAL_PREFIX}_{}_{public_id}_{}",
        kind.credential_tag(),
        secret_hex.as_str()
    ));
    GeneratedCredential {
        public_id,
        last_four,
        secret,
        token,
    }
}

/// A credential as presented by a client, after syntactic validation.
pub(crate) struct PresentedCredential {
    pub tag_kind: EnvironmentKind,
    pub public_id: String,
    pub secret: Zeroizing<[u8; SECRET_BYTES]>,
}

impl fmt::Debug for PresentedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentedCredential")
            .field("tag_kind", &self.tag_kind)
            .field("public_id", &self.public_id)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Strict parse. Returns `None` for anything that is not exactly a well-formed credential.
pub(crate) fn parse(presented: &str) -> Option<PresentedCredential> {
    if presented.len() > MAX_PRESENTED_LEN || !presented.is_ascii() {
        return None;
    }
    let rest = presented
        .strip_prefix(CREDENTIAL_PREFIX)?
        .strip_prefix('_')?;
    let mut parts = rest.split('_');
    let (tag, public_id, secret_hex) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let tag_kind = EnvironmentKind::from_credential_tag(tag)?;
    if !is_lower_hex(public_id, PUBLIC_ID_BYTES * 2) || !is_lower_hex(secret_hex, SECRET_BYTES * 2)
    {
        return None;
    }
    let mut secret = Zeroizing::new([0u8; SECRET_BYTES]);
    hex::decode_to_slice(secret_hex, secret.as_mut()).ok()?;
    Some(PresentedCredential {
        tag_kind,
        public_id: public_id.to_string(),
        secret,
    })
}

/// The message the verifier MACs: context label, environment tag, public id and secret,
/// length-delimited.
pub(crate) fn verifier_message(
    kind: EnvironmentKind,
    public_id: &str,
    secret: &[u8; SECRET_BYTES],
) -> Zeroizing<Vec<u8>> {
    let tag = kind.credential_tag().as_bytes();
    let mut message = Zeroizing::new(Vec::with_capacity(
        VERIFIER_CONTEXT.len() + tag.len() + public_id.len() + SECRET_BYTES + 3,
    ));
    message.extend_from_slice(VERIFIER_CONTEXT);
    message.push(0);
    message.extend_from_slice(tag);
    message.push(0);
    message.extend_from_slice(public_id.as_bytes());
    message.push(0);
    message.extend_from_slice(secret);
    message
}

/// Safe display form: identifies the credential without revealing its secret.
pub fn masked(kind: EnvironmentKind, public_id: &str, last_four: &str) -> String {
    format!(
        "{CREDENTIAL_PREFIX}_{}_{public_id}_{}{last_four}",
        kind.credential_tag(),
        "*".repeat(SECRET_BYTES * 2 - 4)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_credentials_parse_back_and_are_unique() {
        let a = generate(EnvironmentKind::Production);
        let b = generate(EnvironmentKind::Production);
        assert_ne!(a.public_id, b.public_id);
        assert_ne!(a.secret.as_ref(), b.secret.as_ref());
        let token = a.token.expose_once();
        assert!(token.starts_with("cfmk_live_"));
        assert!(token.ends_with(&a.last_four));
        assert_eq!(token.len(), 5 + 5 + 24 + 1 + 64);
        let parsed = parse(token).expect("parses");
        assert_eq!(parsed.tag_kind, EnvironmentKind::Production);
        assert_eq!(parsed.public_id, a.public_id);
        assert_eq!(parsed.secret.as_ref(), a.secret.as_ref());
    }

    #[test]
    fn environment_tags_are_distinct() {
        assert!(generate(EnvironmentKind::Sandbox)
            .token
            .expose_once()
            .starts_with("cfmk_sbx_"));
        assert!(generate(EnvironmentKind::Test)
            .token
            .expose_once()
            .starts_with("cfmk_test_"));
    }

    #[test]
    fn rejects_malformed_values() {
        let good = generate(EnvironmentKind::Sandbox);
        let token = good.token.expose_once().to_string();
        let cases = [
            String::new(),
            "Bearer x".into(),
            token.to_uppercase(),
            token.replacen("cfmk_", "cfmx_", 1),
            token.replacen("_sbx_", "_prod_", 1),
            format!("{token}_extra"),
            format!("{token}0"),
            token[..token.len() - 1].to_string(),
            token.replacen("cfmk_sbx_", "cfmk_sbx__", 1),
            format!("{}{}", &token[..token.len() - 1], "g"),
            "x".repeat(MAX_PRESENTED_LEN + 1),
            format!("{}é", &token[..token.len() - 2]),
        ];
        for case in cases {
            assert!(parse(&case).is_none(), "accepted {case:?}");
        }
    }

    #[test]
    fn verifier_message_binds_environment_tag() {
        let g = generate(EnvironmentKind::Production);
        let live = verifier_message(EnvironmentKind::Production, &g.public_id, &g.secret);
        let sbx = verifier_message(EnvironmentKind::Sandbox, &g.public_id, &g.secret);
        assert_ne!(live.as_slice(), sbx.as_slice());
    }

    #[test]
    fn masked_form_hides_the_secret() {
        let g = generate(EnvironmentKind::Test);
        let masked = masked(EnvironmentKind::Test, &g.public_id, &g.last_four);
        let token = g.token.expose_once();
        assert_eq!(masked.len(), token.len());
        assert_ne!(masked, token);
        assert!(masked.contains(&g.public_id));
        let secret_part = &token[token.len() - 64..token.len() - 4];
        assert!(!masked.contains(secret_part));
    }

    #[test]
    fn debug_output_is_redacted() {
        let g = generate(EnvironmentKind::Sandbox);
        let token = g.token.expose_once().to_string();
        let secret_hex = &token[token.len() - 64..];
        let dbg = format!("{g:?} {:?}", parse(&token).unwrap());
        assert!(!dbg.contains(secret_hex));
        assert!(dbg.contains("[REDACTED]"));
    }
}

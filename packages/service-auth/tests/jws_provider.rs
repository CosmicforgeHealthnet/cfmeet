//! The CF Meet `jsonwebtoken` provider (`cf_service_auth::jwt`) is a drop-in for the
//! former bundled `rust_crypto` backend for the algorithms CF Meet uses: the golden tokens
//! below were produced by `jsonwebtoken` 11.1.0 with `rust_crypto` (fixed Ed25519 key and
//! HMAC secret) and must be reproduced byte for byte; every other algorithm is refused.

use cf_service_auth::jwt;
use jsonwebtoken::{errors::ErrorKind, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::{json, Value};

const ED_PKCS8_HEX: &str = "3051020101300506032b6570042204200707070707070707070707070707070707070707070707070707070707070707812100ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const ED_PUB_HEX: &str = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const ED_TOKEN: &str = "eyJ0eXAiOiJjZi1zZXJ2aWNlK2p3dCIsImFsZyI6IkVkRFNBIiwia2lkIjoiZ29sZGVuLTEifQ.eyJhdWQiOiJjZm1lZXQucGxhdGZvcm0tYWNjZXNzIiwiZXhwIjoxNzAwMDAwMTIwLCJpYXQiOjE3MDAwMDAwMDAsImlzcyI6ImNmbWVldC5jYWxsaW5nIiwianRpIjoiMDE5M2I5ZDQtMDAwMC03MDAwLTgwMDAtMDAwMDAwMDAwMDAxIn0.M9gPb3Z1toVCE8Qp4rtC77hGhEbm94oPMgTeDHwN2hE4_60LKPDJ1Rpl9P_5ifHNY4LRwBQ4qzufbjiE8_WFAg";
const HS_SECRET: &[u8] = b"golden-hmac-secret-0123456789abcdef";
const HS_TOKEN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJhdWQiOiJjZm1lZXQucGxhdGZvcm0tYWNjZXNzIiwiZXhwIjoxNzAwMDAwMTIwLCJpYXQiOjE3MDAwMDAwMDAsImlzcyI6ImNmbWVldC5jYWxsaW5nIiwianRpIjoiMDE5M2I5ZDQtMDAwMC03MDAwLTgwMDAtMDAwMDAwMDAwMDAxIn0.shdOD-xftMOfA1hyZ-WJtWqb5a6ZMRnCjmmWW2sZ_xk";

fn claims() -> Value {
    json!({"iss":"cfmeet.calling","aud":"cfmeet.platform-access","iat":1700000000u64,
           "exp":1700000120u64,"jti":"0193b9d4-0000-7000-8000-000000000001"})
}

fn validation(alg: Algorithm) -> Validation {
    let mut v = Validation::new(alg);
    v.validate_exp = false;
    v.required_spec_claims.clear();
    v.set_audience(&["cfmeet.platform-access"]);
    v
}

fn ed_header() -> Header {
    let mut h = Header::new(Algorithm::EdDSA);
    h.kid = Some("golden-1".into());
    h.typ = Some("cf-service+jwt".into());
    h
}

#[test]
fn eddsa_tokens_are_byte_identical_to_the_former_backend_and_verify() {
    let der = hex::decode(ED_PKCS8_HEX).unwrap();
    let token = jwt::encode(&ed_header(), &claims(), &EncodingKey::from_ed_der(&der)).unwrap();
    assert_eq!(token, ED_TOKEN);
    let public = DecodingKey::from_ed_der(&hex::decode(ED_PUB_HEX).unwrap());
    let data = jwt::decode::<Value>(ED_TOKEN, &public, &validation(Algorithm::EdDSA)).unwrap();
    assert_eq!(data.claims, claims());
    // One flipped signature bit is refused.
    let mut forged = ED_TOKEN.to_string();
    let last = forged.pop().unwrap();
    forged.push(if last == 'A' { 'B' } else { 'A' });
    assert!(jwt::decode::<Value>(&forged, &public, &validation(Algorithm::EdDSA)).is_err());
    // Another key is refused.
    let other = ed25519_dalek::SigningKey::from_bytes(&[8u8; 32]);
    let other = DecodingKey::from_ed_der(other.verifying_key().as_bytes());
    assert!(jwt::decode::<Value>(ED_TOKEN, &other, &validation(Algorithm::EdDSA)).is_err());
}

#[test]
fn hmac_tokens_are_byte_identical_to_the_former_backend_and_verify() {
    let token = jwt::encode(
        &Header::new(Algorithm::HS256),
        &claims(),
        &EncodingKey::from_secret(HS_SECRET),
    )
    .unwrap();
    assert_eq!(token, HS_TOKEN);
    let key = DecodingKey::from_secret(HS_SECRET);
    assert!(jwt::decode::<Value>(HS_TOKEN, &key, &validation(Algorithm::HS256)).is_ok());
    let wrong = DecodingKey::from_secret(b"another-secret-0123456789abcdef!!");
    assert!(jwt::decode::<Value>(HS_TOKEN, &wrong, &validation(Algorithm::HS256)).is_err());
    for alg in [Algorithm::HS384, Algorithm::HS512] {
        let t = jwt::encode(
            &Header::new(alg),
            &claims(),
            &EncodingKey::from_secret(HS_SECRET),
        )
        .unwrap();
        assert!(
            jwt::decode::<Value>(&t, &key, &validation(alg)).is_ok(),
            "{alg:?}"
        );
    }
}

#[test]
fn every_other_algorithm_is_refused() {
    let key = EncodingKey::from_secret(HS_SECRET);
    for alg in [
        Algorithm::RS256,
        Algorithm::RS384,
        Algorithm::RS512,
        Algorithm::PS256,
        Algorithm::PS384,
        Algorithm::PS512,
        Algorithm::ES256,
        Algorithm::ES384,
    ] {
        let err = jwt::sign(b"x.y", &key, alg).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                ErrorKind::InvalidAlgorithm | ErrorKind::InvalidKeyFormat
            ),
            "{alg:?}: {err:?}"
        );
    }
    // An EdDSA key never signs HS256 and an HMAC key never signs EdDSA.
    let der = hex::decode(ED_PKCS8_HEX).unwrap();
    assert!(jwt::sign(b"x.y", &EncodingKey::from_ed_der(&der), Algorithm::HS256).is_err());
    assert!(jwt::sign(b"x.y", &key, Algorithm::EdDSA).is_err());
}

#[test]
fn install_is_idempotent() {
    jwt::install();
    jwt::install();
    assert!(jwt::encode(
        &ed_header(),
        &claims(),
        &EncodingKey::from_ed_der(&hex::decode(ED_PKCS8_HEX).unwrap())
    )
    .is_ok());
}

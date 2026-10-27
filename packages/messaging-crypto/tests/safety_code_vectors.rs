//! Known-answer vectors of the safety code / QR payload (Part V Appendix G.5, L): fixed
//! principal ids and device keys must produce exactly the recorded fingerprints, code and
//! payload, so every client SDK computes the same value. Produced by this implementation
//! (a regression and interoperability anchor, not an independent reference).

use cf_messaging_crypto::verification::{
    principal_fingerprint, qr_payload, safety_code, verify_qr, PrincipalKeys,
};
use uuid::Uuid;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn principal(v: &serde_json::Value) -> PrincipalKeys {
    PrincipalKeys {
        principal_id: Uuid::parse_str(v["principalId"].as_str().unwrap()).unwrap(),
        device_keys: v["deviceKeys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| unhex(k.as_str().unwrap()))
            .collect(),
    }
}

#[test]
fn safety_code_vectors_match() {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("vectors/safety_code_v1.json")).unwrap();
    let cases = doc["cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let (a, b) = (principal(&case["a"]), principal(&case["b"]));
        if std::env::var("PRINT_VECTORS").is_ok() {
            println!(
                "{} {} {} {}",
                hex(&principal_fingerprint(&a).unwrap()),
                hex(&principal_fingerprint(&b).unwrap()),
                safety_code(&a, &b).unwrap(),
                hex(&qr_payload(&a, &b).unwrap())
            );
            continue;
        }
        assert_eq!(
            hex(&principal_fingerprint(&a).unwrap()),
            case["fingerprintA"]
        );
        assert_eq!(
            hex(&principal_fingerprint(&b).unwrap()),
            case["fingerprintB"]
        );
        assert_eq!(safety_code(&a, &b).unwrap(), case["safetyCode"]);
        assert_eq!(safety_code(&b, &a).unwrap(), case["safetyCode"]);
        let qr = unhex(case["qrPayload"].as_str().unwrap());
        assert_eq!(qr_payload(&a, &b).unwrap(), qr);
        assert!(verify_qr(&qr, &b, &a));
    }
}

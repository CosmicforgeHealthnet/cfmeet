//! Webhook signature vectors shared with the platform SDKs (Part VI §52/§53).
//!
//! `sdks/testdata/webhook-signature-vectors.json` is produced by THIS reference
//! implementation: the TypeScript and Dart SDK verifiers are tested against it, so there
//! is one signature protocol and every implementation agrees with the one that signs.
//! The test fails when the file is stale; regenerate with
//! `CF_WEBHOOK_VECTORS_WRITE=1 cargo test --test sdk_vectors`.

use cf_webhooks::signing::{self, VerifyError, DEFAULT_TOLERANCE_SECONDS};

const SECRET: &str = "whsec_test-only-sdk-vector-fixture-secret";
const OTHER: &str = "whsec_test-only-mismatch-vector-fixture-secret";

fn esc(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

struct Case {
    name: &'static str,
    secret: &'static str,
    signature: String,
    timestamp: String,
    delivery_id: &'static str,
    body: &'static str,
    now: i64,
    expected: &'static str,
}

fn outcome(r: Result<(), VerifyError>) -> &'static str {
    match r {
        Ok(()) => "VALID",
        Err(VerifyError::Malformed) => "MALFORMED",
        Err(VerifyError::StaleTimestamp) => "STALE_TIMESTAMP",
        Err(VerifyError::Mismatch) => "MISMATCH",
    }
}

fn vectors() -> String {
    let ts = 1_760_000_000i64;
    let body = r#"{"id":"whe_0123456789abcdef0123456789abcdef","type":"call.completed","data":{"callId":"7f1c0a52-8d8e-4bde-9a51-6c1e0f2b3a4d","note":"héllo ✓"}}"#;
    let sig = |secret: &str, t: i64, id: &str, b: &str| {
        signing::sign(secret.as_bytes(), t, id, b.as_bytes())
    };
    let good = sig(SECRET, ts, "whd_vector_1", body);
    let cases = vec![
        Case {
            name: "valid",
            secret: SECRET,
            signature: good.clone(),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_1",
            body,
            now: ts + 30,
            expected: "",
        },
        Case {
            name: "valid_empty_body",
            secret: SECRET,
            signature: sig(SECRET, ts, "whd_vector_2", ""),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_2",
            body: "",
            now: ts,
            expected: "",
        },
        Case {
            name: "valid_at_tolerance_edge",
            secret: SECRET,
            signature: good.clone(),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_1",
            body,
            now: ts - DEFAULT_TOLERANCE_SECONDS,
            expected: "",
        },
        Case {
            name: "body_tampered",
            secret: SECRET,
            signature: good.clone(),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_1",
            body: r#"{"id":"whe_0123456789abcdef0123456789abcdef","type":"call.completed"}"#,
            now: ts,
            expected: "",
        },
        Case {
            name: "delivery_id_swapped",
            secret: SECRET,
            signature: good.clone(),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_9",
            body,
            now: ts,
            expected: "",
        },
        Case {
            name: "timestamp_swapped",
            secret: SECRET,
            signature: good.clone(),
            timestamp: (ts + 1).to_string(),
            delivery_id: "whd_vector_1",
            body,
            now: ts,
            expected: "",
        },
        Case {
            name: "wrong_secret",
            secret: OTHER,
            signature: good.clone(),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_1",
            body,
            now: ts,
            expected: "",
        },
        Case {
            name: "stale_timestamp",
            secret: SECRET,
            signature: good.clone(),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_1",
            body,
            now: ts + DEFAULT_TOLERANCE_SECONDS + 1,
            expected: "",
        },
        Case {
            name: "future_timestamp",
            secret: SECRET,
            signature: good.clone(),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_1",
            body,
            now: ts - DEFAULT_TOLERANCE_SECONDS - 1,
            expected: "",
        },
        Case {
            name: "unknown_version",
            secret: SECRET,
            signature: good.replacen("v1=", "v2=", 1),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_1",
            body,
            now: ts,
            expected: "",
        },
        Case {
            name: "not_hex",
            secret: SECRET,
            signature: "v1=zz".into(),
            timestamp: ts.to_string(),
            delivery_id: "whd_vector_1",
            body,
            now: ts,
            expected: "",
        },
        Case {
            name: "timestamp_not_a_number",
            secret: SECRET,
            signature: good,
            timestamp: "17600000OO".into(),
            delivery_id: "whd_vector_1",
            body,
            now: ts,
            expected: "",
        },
    ];
    let mut out = String::from("{\n  \"description\": \"CF Meet webhook signature vectors (CF-Webhook-Signature v1 = hex HMAC-SHA256(secret, 'v1:' + timestamp + ':' + delivery id + ':' + raw body)). GENERATED by packages/webhooks/tests/sdk_vectors.rs from the reference signer; do not edit.\",\n");
    out.push_str(&format!(
        "  \"toleranceSeconds\": {DEFAULT_TOLERANCE_SECONDS},\n  \"vectors\": [\n"
    ));
    let n = cases.len();
    for (i, mut c) in cases.into_iter().enumerate() {
        let r = signing::verify(
            c.secret.as_bytes(),
            &c.signature,
            &c.timestamp,
            c.delivery_id,
            c.body.as_bytes(),
            c.now,
            DEFAULT_TOLERANCE_SECONDS,
        );
        c.expected = outcome(r);
        out.push_str(&format!(
            "    {{ \"name\": \"{}\", \"secret\": \"{}\", \"signature\": \"{}\", \"timestamp\": \"{}\", \"deliveryId\": \"{}\", \"body\": \"{}\", \"now\": {}, \"expected\": \"{}\" }}{}\n",
            c.name, esc(c.secret), esc(&c.signature), esc(&c.timestamp), c.delivery_id, esc(c.body), c.now, c.expected,
            if i + 1 < n { "," } else { "" }
        ));
    }
    out.push_str("  ]\n}\n");
    out
}

#[test]
fn sdk_signature_vectors_are_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sdks/testdata/webhook-signature-vectors.json");
    let expected = vectors();
    for want in [
        "\"VALID\"",
        "\"MISMATCH\"",
        "\"STALE_TIMESTAMP\"",
        "\"MALFORMED\"",
    ] {
        assert!(expected.contains(want), "{want}");
    }
    if std::env::var("CF_WEBHOOK_VECTORS_WRITE").as_deref() == Ok("1") {
        std::fs::write(&path, &expected).unwrap();
    }
    let actual = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        actual == expected,
        "{} is stale: CF_WEBHOOK_VECTORS_WRITE=1 cargo test --test sdk_vectors",
        path.display()
    );
}

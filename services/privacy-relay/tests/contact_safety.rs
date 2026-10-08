//! No telephone number (or other contact plaintext) is accepted, stored, logged or
//! returned by §26–§30.

mod common;

use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use serde_json::json;

const PHONE_LIKE: &[&str] = &[
    "+2348012345678",
    "2348012345678",
    "08012345678",
    "+1 (415) 555-0100",
    "tel:+14155550100",
    "sip:alice@example.com",
];

#[tokio::test]
async fn telephone_like_values_never_enter_storage_or_responses() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let mut transcript = String::new();
    for phone in PHONE_LIKE {
        for (path, body) in [
            ("/internal/v1/contexts", {
                let mut v = context_request(&[a, b], "BIDIRECTIONAL", &[]);
                v["externalContextReference"] = json!(phone);
                v
            }),
            ("/internal/v1/contexts", {
                let mut v = context_request(&[a, b], "BIDIRECTIONAL", &[]);
                v["participants"][1] = json!({ "contactReferenceId": phone });
                v
            }),
            ("/internal/v1/contexts", {
                let mut v = context_request(&[a, b], "BIDIRECTIONAL", &[]);
                v["participants"][1] = json!({ "phoneNumber": phone });
                v
            }),
            ("/internal/v1/contexts", {
                let mut v = context_request(&[a, b], "BIDIRECTIONAL", &[]);
                v["destinationNumber"] = json!(phone);
                v
            }),
            ("/internal/v1/contexts", {
                let mut v = context_request(&[a, b], "BIDIRECTIONAL", &[]);
                v["e164"] = json!(phone);
                v
            }),
        ] {
            let r = h.command(path, &c, body).await;
            assert_eq!(r.code(), "VALIDATION_ERROR", "{phone}");
            transcript.push_str(&r.text);
        }
    }
    // Legitimate traffic, then scan every table and every response.
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let sid = h.app_session(&c, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    transcript.push_str(&h.authorize(&c, &sid, a, &[b]).await.text);
    transcript.push_str(&h.lifecycle(&c, &id, "revoke", 1).await.text);
    transcript.push_str(
        &h.get(&format!("/internal/v1/contexts/{id}/evidence"), &c)
            .await
            .text,
    );
    let dump = dump_database(h.conn()).await;
    for phone in PHONE_LIKE {
        assert!(!dump.contains(phone), "{phone} stored");
        assert!(!transcript.contains(phone), "{phone} echoed in a response");
    }
    assert!(!transcript.contains("cfmk_"));
    // No plaintext-shaped column anywhere; Contact Vault material only in the vault tables.
    for column in [
        "phone",
        "msisdn",
        "e164",
        "telephone",
        "sip_uri",
        "destination_number",
        "plaintext",
        "raw_number",
    ] {
        assert_eq!(
            h.count(&format!("SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' AND column_name LIKE '%{column}%'")).await,
            0,
            "no {column} column"
        );
    }
    for (column, tables) in [
        ("ciphertext", "'protected_contacts'"),
        ("nonce", "'protected_contacts'"),
        ("key_version", "'protected_contacts'"),
    ] {
        assert_eq!(
            h.count(&format!("SELECT count(*) FROM information_schema.columns WHERE table_schema = 'public' AND column_name LIKE '%{column}%' AND table_name NOT IN ({tables})")).await,
            0,
            "{column} outside the Contact Vault"
        );
    }
    // The database itself refuses a phone-like reference.
    assert!(exec(h.conn(), &format!("UPDATE communication_contexts SET external_context_reference = '2348012345678' WHERE id = '{id}'")).await.is_err());
}

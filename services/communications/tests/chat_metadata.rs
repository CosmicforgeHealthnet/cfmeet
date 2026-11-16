//! CF Meet chat remediation M2 / M3: the Hub's views of Messaging's groups-in-common and
//! shared-media answers. Only the owner's vocabulary passes; MANAGED display metadata
//! is relayed, PRIVATE_E2EE metadata never is (it is not held by the server), and media
//! retrieval stays a reference to the access-checked grant operation of the SAME
//! conversation and attachment. Every view is checked against the published Hub
//! contract (`packages/contracts/communications/http/v1.openapi.json`).
use communications::{chat::media_item, relationships::common_group_view};
use serde_json::{json, Value};
use uuid::Uuid;

fn component(name: &str) -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/contracts/communications/http/v1.openapi.json"
    );
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    doc["components"]["schemas"][name].clone()
}

/// Structural conformance: no property outside the schema (additionalProperties: false),
/// every required property present, enum / type of each top-level value respected.
fn conforms(name: &str, v: &Value) {
    let schema = component(name);
    let props = schema["properties"].as_object().unwrap();
    for (k, value) in v.as_object().unwrap() {
        let p = props
            .get(k)
            .unwrap_or_else(|| panic!("{name}: unexpected {k}"));
        if let Some(e) = p["enum"].as_array() {
            assert!(e.contains(value), "{name}.{k}: {value} not in {e:?}");
        }
        let types: Vec<&str> = match &p["type"] {
            Value::String(t) => vec![t.as_str()],
            Value::Array(t) => t.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        if !types.is_empty() {
            let actual = match value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Number(n) if n.is_i64() => "integer",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            assert!(
                types.contains(&actual) || (actual == "integer" && types.contains(&"number")),
                "{name}.{k}: {actual} not in {types:?}"
            );
        }
    }
    for r in schema["required"].as_array().unwrap() {
        assert!(v.get(r.as_str().unwrap()).is_some(), "{name}: missing {r}");
    }
}

#[test]
fn common_groups_relay_managed_titles_and_never_invent_e2ee_names() {
    let id = Uuid::new_v4();
    let managed = common_group_view(&json!({"conversationId":id,"contextClass":"GROUP",
        "securityProfile":"MANAGED","title":"Weekend hikers","avatarRef":"avatar:hikers-1","memberCount":3}))
    .unwrap();
    conforms("CommonGroup", &managed);
    assert_eq!(
        managed,
        json!({"conversationId":id,"contextClass":"GROUP","securityProfile":"MANAGED",
            "title":"Weekend hikers","avatarRef":"avatar:hikers-1","memberCount":3})
    );
    // A title reported for a PRIVATE_E2EE group is never relayed.
    let e2ee = common_group_view(&json!({"conversationId":id,"contextClass":"GROUP",
        "securityProfile":"PRIVATE_E2EE","title":"should not exist","memberCount":2}))
    .unwrap();
    conforms("CommonGroup", &e2ee);
    assert!(e2ee["title"].is_null() && e2ee["avatarRef"].is_null());
    // An older Messaging image (ids only) keeps working with metadata null.
    let old = common_group_view(&json!({"conversationId":id,"contextClass":"GROUP"})).unwrap();
    conforms("CommonGroup", &old);
    assert!(old["securityProfile"].is_null() && old["memberCount"].is_null());
    // Control characters, oversize titles and negative counts are dropped.
    let odd = common_group_view(&json!({"conversationId":id,"contextClass":"GROUP",
        "securityProfile":"MANAGED","title":"a\u{7}b","avatarRef":"x".repeat(300),"memberCount":-1}))
    .unwrap();
    assert!(odd["title"].is_null() && odd["avatarRef"].is_null() && odd["memberCount"].is_null());
    // Outside the owner's vocabulary: a dependency failure, never a guessed group.
    assert!(common_group_view(&json!({"conversationId":id,"contextClass":"DIRECT"})).is_err());
    assert!(common_group_view(
        &json!({"conversationId":id,"contextClass":"GROUP","securityProfile":"OTHER"})
    )
    .is_err());
}

#[test]
fn media_items_carry_managed_names_and_only_same_attachment_grant_references() {
    let conversation = Uuid::new_v4();
    let attachment = Uuid::new_v4();
    let base = format!("v1/conversations/{conversation}/attachments/{attachment}");
    let managed = media_item(
        conversation,
        &json!({"attachmentId":attachment,"messageId":Uuid::new_v4(),"messageSequence":3,"state":"READY",
            "sizeBytes":2048,"mediaClass":"IMAGE","createdAt":"2026-11-16T10:00:00Z","uploaderPrincipalId":Uuid::new_v4(),
            "securityProfile":"MANAGED","kind":"IMAGE","fileName":"Holiday.png","mediaType":"image/png",
            "metadataLocation":"SERVER",
            "download":{"method":"POST","path":format!("{base}/download-grants")},
            "thumbnail":{"method":"POST","path":format!("{base}/thumbnail-grants"),"mediaType":"image/png"}}),
    );
    conforms("MediaItem", &managed);
    assert_eq!(managed["fileName"], "Holiday.png");
    assert_eq!(managed["mediaType"], "image/png");
    assert_eq!(managed["kind"], "IMAGE");
    assert_eq!(managed["metadataLocation"], "SERVER");
    assert_eq!(
        managed["download"],
        json!({"method":"POST","path":format!("{base}/download-grants")})
    );
    assert_eq!(
        managed["thumbnail"]["path"],
        format!("{base}/thumbnail-grants")
    );
    // The uploader principal is not part of the consumer view.
    assert!(managed.get("uploaderPrincipalId").is_none());

    // PRIVATE_E2EE: whatever arrives, no name / type / kind / thumbnail is relayed.
    let e2ee = media_item(
        conversation,
        &json!({"attachmentId":attachment,"messageId":null,"state":"READY","sizeBytes":4096,
            "mediaClass":"ENCRYPTED","createdAt":"2026-11-16T10:00:00Z","securityProfile":"PRIVATE_E2EE",
            "fileName":"secret.png","mediaType":"image/png","kind":"IMAGE",
            "download":{"method":"POST","path":format!("{base}/download-grants")},
            "thumbnail":{"method":"POST","path":format!("{base}/thumbnail-grants"),"mediaType":"image/png"}}),
    );
    conforms("MediaItem", &e2ee);
    for absent in ["fileName", "mediaType", "kind", "thumbnail"] {
        assert!(e2ee[absent].is_null(), "{absent}");
    }
    assert_eq!(e2ee["metadataLocation"], "ENCRYPTED_DESCRIPTOR");
    assert!(!e2ee.to_string().contains("secret.png"));

    // A reference to another conversation / attachment, or an absolute URL, is dropped.
    for path in [
        format!(
            "v1/conversations/{}/attachments/{attachment}/download-grants",
            Uuid::new_v4()
        ),
        format!("https://storage.example/{attachment}?X-Amz-Signature=x"),
    ] {
        let v = media_item(
            conversation,
            &json!({"attachmentId":attachment,"state":"READY","sizeBytes":1,"mediaClass":"OTHER",
                "securityProfile":"MANAGED","download":{"method":"POST","path":path}}),
        );
        assert!(v["download"].is_null(), "{v}");
    }
    // An older Messaging image (no profile) keeps the 1.x fields; nothing is guessed.
    let old = media_item(
        conversation,
        &json!({"attachmentId":attachment,"messageId":null,"state":"READY","sizeBytes":10,
            "mediaClass":"ENCRYPTED","createdAt":"2026-11-16T10:00:00Z"}),
    );
    conforms("MediaItem", &old);
    assert!(old["metadataLocation"].is_null() && old["download"].is_null());
}

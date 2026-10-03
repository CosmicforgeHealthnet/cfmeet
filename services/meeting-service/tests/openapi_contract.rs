//! OpenAPI is the authoritative consumer contract (spec §13): the committed export must
//! match the code, cover every v2 route, and never describe secrets or provider ids.

use serde_json::Value;

const EXPORT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packages/contracts/openapi/meeting-service.json"
);

fn generated() -> Value {
    serde_json::from_str(&backend::swagger::openapi_json()).unwrap()
}

#[test]
fn committed_contract_matches_code() {
    let committed: Value =
        serde_json::from_str(&std::fs::read_to_string(EXPORT).expect("exported contract exists"))
            .expect("valid JSON");
    assert_eq!(
        committed,
        generated(),
        "packages/contracts/openapi/meeting-service.json is stale; regenerate with \
         `cargo run --bin export_openapi > ../../packages/contracts/openapi/meeting-service.json`"
    );
}

#[test]
fn every_v2_route_is_documented() {
    let doc = generated();
    let paths = doc["paths"].as_object().unwrap();
    let expected = [
        ("post", "/api/v2/meeting-spaces"),
        ("get", "/api/v2/meeting-spaces"),
        ("get", "/api/v2/meeting-spaces/{id}"),
        ("patch", "/api/v2/meeting-spaces/{id}"),
        ("post", "/api/v2/meeting-spaces/{id}/archive"),
        ("post", "/api/v2/meeting-spaces/{id}/duplicate"),
        ("post", "/api/v2/meeting-spaces/{id}/owner-transfer"),
        ("put", "/api/v2/meeting-spaces/{id}/recurrence"),
        ("get", "/api/v2/meeting-spaces/{id}/recurrence"),
        ("delete", "/api/v2/meeting-spaces/{id}/recurrence"),
        ("get", "/api/v2/meeting-spaces/{id}/members"),
        ("post", "/api/v2/meeting-spaces/{id}/members"),
        ("patch", "/api/v2/meeting-spaces/{id}/members/{memberId}"),
        ("delete", "/api/v2/meeting-spaces/{id}/members/{memberId}"),
        ("post", "/api/v2/meeting-spaces/{id}/invitations"),
        ("get", "/api/v2/meeting-spaces/{id}/invitations"),
        (
            "post",
            "/api/v2/meeting-spaces/{id}/invitations/{invitationId}/revoke",
        ),
        ("post", "/api/v2/invitations/{token}/resolve"),
        ("post", "/api/v2/meeting-spaces/{id}/conferences"),
        ("get", "/api/v2/meeting-spaces/{id}/conferences"),
        ("post", "/api/v2/instant-meetings"),
        ("get", "/api/v2/resolve/{identifier}"),
        ("get", "/api/v2/conferences/{id}"),
        ("post", "/api/v2/conferences/{id}/prepare"),
        ("post", "/api/v2/conferences/{id}/start"),
        ("post", "/api/v2/conferences/{id}/join"),
        ("post", "/api/v2/conferences/{id}/leave"),
        ("post", "/api/v2/conferences/{id}/end"),
        ("post", "/api/v2/conferences/{id}/lock"),
        ("post", "/api/v2/conferences/{id}/unlock"),
        ("post", "/api/v2/conferences/{id}/cancel"),
        ("post", "/api/v2/conferences/{id}/reschedule"),
        ("get", "/api/v2/conferences/{id}/participants"),
        ("get", "/api/v2/conferences/{id}/participants/me"),
        ("get", "/api/v2/conferences/{id}/waiting-room"),
        ("post", "/api/v2/conferences/{id}/waiting-room/admit"),
        ("post", "/api/v2/conferences/{id}/waiting-room/deny"),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/return-to-waiting",
        ),
        (
            "patch",
            "/api/v2/conferences/{id}/participants/{participantId}/role",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/remove",
        ),
        (
            "post",
            "/api/v2/conferences/{id}/participants/{participantId}/ban",
        ),
        ("get", "/api/v2/admin/compat/legacy-meetings/{id}/shadow"),
        ("post", "/api/v2/admin/compat/backfill"),
    ];
    for (method, path) in expected {
        assert!(
            paths.get(path).and_then(|p| p.get(method)).is_some(),
            "{method} {path} undocumented"
        );
    }
    let documented_v2 = paths
        .iter()
        .filter(|(p, _)| p.starts_with("/api/v2"))
        .map(|(_, item)| item.as_object().unwrap().len())
        .sum::<usize>();
    assert_eq!(
        documented_v2,
        expected.len(),
        "every documented v2 operation is in the route table"
    );
}

#[test]
fn contract_never_describes_secrets_or_provider_ids() {
    let doc = generated();
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    let forbidden = [
        "passcodeHash",
        "passcode_hash",
        "tokenHash",
        "token_hash",
        "credentialHash",
        "credential_hash",
        "rtcRoomName",
        "rtc_room_name",
        "rtcRoomId",
        "rtc_room_id",
        "guestIdentity",
        "denialInternalNote",
    ];
    for (name, schema) in schemas {
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for f in forbidden {
                assert!(!props.contains_key(f), "schema {name} exposes {f}");
            }
        }
    }
}

#[test]
fn public_error_codes_are_published() {
    let doc = generated();
    let codes: Vec<&str> = doc["components"]["schemas"]["ErrorCode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for required in [
        "MEETING_NOT_FOUND",
        "MEETING_ACCESS_DENIED",
        "MEETING_PASSCODE_REQUIRED",
        "MEETING_PASSCODE_INVALID",
        "INVITATION_INVALID",
        "INVITATION_EXPIRED",
        "CONFERENCE_NOT_LIVE",
        "CONFERENCE_LOCKED",
        "WAITING_ROOM_REQUIRED",
        "PARTICIPANT_NOT_FOUND",
        "PARTICIPANT_BANNED",
        "PERMISSION_DENIED",
        "CONFERENCE_STATE_CONFLICT",
        "RTC_PROVIDER_UNAVAILABLE",
        "RECORDING_NOT_ALLOWED",
    ] {
        assert!(codes.contains(&required), "{required} missing");
    }
}

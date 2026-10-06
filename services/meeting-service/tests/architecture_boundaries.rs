//! Part K — module-boundary certification (K-E2E-09, §29–§31).
//!
//! Static checks over the source tree: provider SDKs stay inside provider adapters,
//! handlers never mint provider credentials or call provider SDKs, and `/api/v2`
//! handlers stay thin (no transactions, no direct entity access, no role matrices).

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
}

/// Source without `//` comments (doc text may legitimately name providers).
fn code_of(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| match l.find("//") {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rel(path: &Path) -> String {
    path.strip_prefix(root())
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

/// Provider adapter modules (and the composition root that wires them).
const PROVIDER_ADAPTERS: &[&str] = &[
    "services/rtc_provider.rs",
    "services/livekit.rs",
    "services/artifacts/recorder.rs",
];

/// K-E2E-09: the LiveKit SDK is used only inside provider adapters; no handler,
/// worker or business service generates provider tokens or calls the SDK.
#[test]
fn k_e2e_09_livekit_sdk_is_confined_to_provider_adapters() {
    let mut files = vec![];
    rust_files(&root(), &mut files);
    for f in &files {
        let r = rel(f);
        let code = code_of(f);
        let uses_sdk = code.contains("livekit_api") || code.contains("livekit_protocol");
        if uses_sdk {
            assert!(
                PROVIDER_ADAPTERS.contains(&r.as_str()),
                "{r} uses the LiveKit SDK outside a provider adapter"
            );
        }
        if r.starts_with("handlers/") || r.starts_with("workers/") || r.starts_with("routes/") {
            for forbidden in [
                "AccessToken",
                "VideoGrants",
                "LiveKitService",
                "LiveKitProvider",
                "LiveKitEgressRecorder",
                "generate_join_token",
                "issue_join_credentials",
                "RoomClient",
                "EgressClient",
            ] {
                assert!(
                    !code.contains(forbidden),
                    "{r} references provider internals `{forbidden}`"
                );
            }
        }
    }
    // Concrete providers are constructed only at the composition root.
    for f in &files {
        let r = rel(f);
        let code = code_of(f);
        if code.contains("LiveKitProvider::new") || code.contains("LiveKitEgressRecorder::new") {
            assert!(
                r == "services/meeting_domain.rs" || PROVIDER_ADAPTERS.contains(&r.as_str()),
                "{r} constructs a concrete provider"
            );
        }
    }
}

/// §30: `/api/v2` handlers parse, resolve the actor, call one service and map the
/// result — no transactions, no entity access, no permission matrices, no provider
/// calls, no raw SQL.
#[test]
fn k_v2_handlers_are_thin_transport() {
    let mut files = vec![];
    rust_files(&root().join("handlers/v2"), &mut files);
    assert!(files.len() >= 15);
    for f in &files {
        let r = rel(f);
        let code = code_of(f);
        for forbidden in [
            ".begin()",
            "TransactionTrait",
            "Entity::find",
            "Entity::update",
            "Entity::delete",
            "Entity::insert",
            "ActiveModel",
            "Statement::",
            "execute_unprepared",
            "Permission::",
            "PermissionContext",
            "MeetingPermissionService",
            "core.rtc",
            ".rtc.",
            "remove_participant(",
            "update_permissions(",
            "terminate_room(",
        ] {
            assert!(!code.contains(forbidden), "{r} contains `{forbidden}`");
        }
    }
}

/// §31: provider side effects never run inside a business transaction in the v2
/// control plane — provider commands are written to the outbox in the transaction and
/// executed after commit (`rtc.*` PROVIDER_COMMAND rows).
#[test]
fn k_provider_commands_flow_through_the_outbox() {
    let events = std::fs::read_to_string(root().join("services/meeting_events.rs")).unwrap();
    for command in [
        "rtc.room_prepare_requested",
        "rtc.room_terminate_requested",
        "rtc.participant_remove_requested",
        "rtc.permissions_update_requested",
    ] {
        assert!(
            events.contains(command),
            "{command} is an outbox provider command"
        );
    }
    let control = code_of(&root().join("services/rtc_control.rs"));
    assert!(control.contains("PROVIDER_COMMAND") || control.contains("ProviderCommand"));
}

/// §28: the legacy models stay compatibility structures — no Phase 2 aggregate field
/// (space / conference / participant-session authority) was added to them.
#[test]
fn k_legacy_models_are_not_extended_into_v2_aggregates() {
    for (file, forbidden) in [
        (
            "models/meetings.rs",
            &[
                "meeting_space_id",
                "conference_id",
                "access_policy",
                "waiting_room_policy",
            ][..],
        ),
        (
            "models/participants.rs",
            &[
                "conference_participant_id",
                "admission_state",
                "media_identity",
            ][..],
        ),
    ] {
        let code = code_of(&root().join(file));
        for f in forbidden {
            assert!(!code.contains(f), "{file} grew v2 field {f}");
        }
    }
}

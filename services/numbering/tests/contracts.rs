//! Part IV C5: one vocabulary in three places — the Rust domain types, the database CHECK
//! constraints and the `cfmeet.numbering.common` contract — plus the error codes, the
//! domain rule codes, the event types and the assignment policy (Rust vs SQL).
mod common;

use std::collections::BTreeSet;

use common::{contracts::read, scalar, strings, Harness};
use numbering::{
    http::error::ErrorCode,
    numbers::{
        assignments::allowed, ActorKind, AssignmentStatus, Availability, BindingChangeReason,
        Capability, CapabilitySource, CapabilityStatus, ClassificationSource, CostEvidence,
        EnvironmentKind, FailureCategory, InventorySource, LifecycleState, NumberType, OwnerKind,
        OwnershipClass, ReleaseState, ReservationStatus, RestrictionFlag, RiskTier, SmsTargetKind,
        UncertainOperation, VoiceTargetKind,
    },
};

fn set<'a>(values: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    values.into_iter().map(str::to_string).collect()
}

fn contract(def: &str) -> BTreeSet<String> {
    read("numbering/common/v1.schema.json")["$defs"][def]["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("{def} has no enum"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

/// The quoted values of a CHECK constraint's `IN (...)` list.
async fn check_values(h: &Harness, constraint: &str) -> BTreeSet<String> {
    let def = strings(
        h.conn(),
        &format!(
            "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = '{constraint}'"
        ),
    )
    .await
    .pop()
    .unwrap_or_else(|| panic!("{constraint} missing"));
    def.split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

macro_rules! all {
    ($t:ty) => {
        set(<$t>::ALL.iter().map(|v| v.as_str()))
    };
}

#[tokio::test]
async fn rust_contract_and_database_share_one_vocabulary() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let cases: Vec<(&str, BTreeSet<String>, &[&str])> = vec![
        (
            "LifecycleState",
            all!(LifecycleState),
            &["ck_number_resources_state"],
        ),
        (
            "InventorySource",
            all!(InventorySource),
            &[
                "ck_number_resources_inventory_source",
                "ck_number_inventory_snapshots_source",
            ],
        ),
        (
            "NumberType",
            all!(NumberType),
            &["ck_number_resources_type"],
        ),
        (
            "ClassificationSource",
            all!(ClassificationSource),
            &["ck_number_resources_classification_source"],
        ),
        (
            "Capability",
            all!(Capability),
            &[
                "ck_number_capabilities_capability",
                "ck_dcn_assignments_capabilities",
                "ck_number_inventory_snapshots_capabilities",
            ],
        ),
        (
            "CapabilityStatus",
            all!(CapabilityStatus),
            &["ck_number_capabilities_status"],
        ),
        (
            "CapabilitySource",
            all!(CapabilitySource),
            &["ck_number_capabilities_source"],
        ),
        (
            "Availability",
            all!(Availability),
            &["ck_number_inventory_snapshots_availability"],
        ),
        (
            "ReservationStatus",
            all!(ReservationStatus),
            &["ck_number_reservations_status"],
        ),
        (
            "ReleaseState",
            all!(ReleaseState),
            &["ck_number_reservations_release"],
        ),
        (
            "AssignmentStatus",
            all!(AssignmentStatus),
            &["ck_dcn_assignments_status"],
        ),
        (
            "OwnerKind",
            all!(OwnerKind),
            &["ck_dcn_assignments_owner_kind"],
        ),
        (
            "OwnershipClass",
            all!(OwnershipClass),
            &["ck_dcn_assignments_class"],
        ),
        (
            "VoiceTargetKind",
            all!(VoiceTargetKind),
            &["ck_number_routing_profiles_voice"],
        ),
        (
            "SmsTargetKind",
            all!(SmsTargetKind),
            &["ck_number_routing_profiles_sms"],
        ),
        (
            "ActorKind",
            all!(ActorKind),
            &[
                "ck_number_lifecycle_events_actor",
                "ck_number_assignment_history_actor",
            ],
        ),
        (
            "EnvironmentKind",
            all!(EnvironmentKind),
            &["ck_number_reservations_kind", "ck_dcn_assignments_kind"],
        ),
        ("RiskTier", all!(RiskTier), &["ck_number_resources_risk"]),
        (
            "CostEvidence",
            all!(CostEvidence),
            &["ck_number_resources_cost_evidence"],
        ),
        ("FailureCategory", all!(FailureCategory), &[]),
        ("UncertainOperation", all!(UncertainOperation), &[]),
        (
            "BindingChangeReason",
            all!(BindingChangeReason),
            &["ck_number_provider_bindings_reason"],
        ),
        ("RestrictionFlag", all!(RestrictionFlag), &[]),
    ];
    for (def, rust, constraints) in cases {
        assert_eq!(contract(def), rust, "contract {def} vs Rust");
        for c in constraints {
            let mut db = check_values(&h, c).await;
            if *c == "ck_number_reservations_release" {
                db.retain(|v| rust.contains(v));
            }
            assert_eq!(db, rust, "database {c} vs Rust {def}");
        }
    }
    // The NumberType a source may OBSERVE excludes UNCLASSIFIED.
    let observed = check_values(&h, "ck_number_inventory_snapshots_type").await;
    assert_eq!(
        observed,
        all!(NumberType)
            .into_iter()
            .filter(|t| t != "UNCLASSIFIED")
            .collect()
    );
}

#[tokio::test]
async fn the_assignment_policy_is_one_edge_set_in_rust_and_sql() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let mut edges = 0;
    for from in AssignmentStatus::ALL {
        for to in AssignmentStatus::ALL {
            let sql = scalar(
                h.conn(),
                &format!("SELECT CASE WHEN dcn_assignment_transition_allowed('{from}', '{to}') THEN 1 ELSE 0 END::bigint"),
            )
            .await;
            assert_eq!(sql == 1, allowed(*from, *to), "{from} -> {to}");
            edges += sql;
        }
    }
    assert_eq!(edges, 19);
}

#[test]
fn error_and_rule_codes_match_the_contract() {
    assert_eq!(
        contract("ErrorCode"),
        set(ErrorCode::ALL.iter().map(|c| c.as_str()))
    );
    // Every domain rule code the source can produce is declared.
    let rules = contract("DomainRule");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/numbers");
    let mut used = BTreeSet::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for marker in [
            "precondition(\"",
            "conflict(\"",
            "precondition(\n                \"",
        ] {
            for part in text.split(marker).skip(1) {
                used.insert(part.split('"').next().unwrap().to_string());
            }
        }
    }
    let undeclared: Vec<_> = used.difference(&rules).collect();
    assert!(
        undeclared.is_empty(),
        "undeclared rule codes: {undeclared:?}"
    );
    assert!(used.len() >= 20, "{used:?}");
}

#[test]
fn every_event_type_the_source_writes_is_a_contract_variant() {
    let events = read("numbering/events/v1.schema.json");
    let declared: BTreeSet<String> = events["$defs"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|d| {
            d["allOf"][1]["properties"]["event_type"]["const"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut written = BTreeSet::new();
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(d).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let text = std::fs::read_to_string(&p).unwrap();
                for part in text.split("event_type: \"").skip(1) {
                    written.insert(part.split('"').next().unwrap().to_string());
                }
            }
        }
    }
    assert_eq!(written, declared);
}

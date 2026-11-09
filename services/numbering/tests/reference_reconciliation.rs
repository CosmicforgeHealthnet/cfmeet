//! DATA-017 — Numbering's report-only reference reconciliation: invalid foreign
//! references are REPORTED (environment recorded under two tenants, an organizational
//! number whose owner is not the organization, a principal owner that is a tenant id),
//! nothing is repaired or deleted.

mod common;

use common::{actor, scalar, scope, voice_to_principal, Harness};
use numbering::{
    domain::RequestContext,
    numbers::{assignments::NewAssignment, EnvironmentKind, OwnerScope, OwnershipClass},
    reference_reconciliation::{report, ReferenceCheck, ReferenceReport},
};
use uuid::Uuid;

fn check<'a>(r: &'a ReferenceReport, name: &str) -> &'a ReferenceCheck {
    r.checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("{name}"))
}

const TABLES: &[&str] = &[
    "number_resources",
    "number_reservations",
    "dcn_assignments",
    "number_assignment_history",
    "number_routing_profiles",
];

async fn counts(h: &Harness) -> Vec<i64> {
    let mut out = Vec::new();
    for t in TABLES {
        out.push(h.scalar(&format!("SELECT count(*) FROM {t}")).await);
    }
    out
}

async fn assign(h: &Harness, s: &OwnerScope, class: OwnershipClass, owner_ref: Uuid) -> Uuid {
    let (resource, _) = h.held(s).await;
    h.numbers
        .request_assignment(
            &RequestContext::generate(),
            s,
            actor(),
            NewAssignment {
                number_resource_id: resource.id,
                ownership_class: class,
                owner_ref,
                policy_snapshot_ref: None,
                kyc_profile_ref: None,
            },
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn invalid_foreign_references_are_reported_and_nothing_is_deleted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let clean = report(h.conn()).await.unwrap();
    assert_eq!(clean.anomalies, 0, "{clean:#?}");
    assert!(clean.checks.iter().all(|c| c.status == "OK"));
    assert_eq!(clean.service, "numbering");
    assert!(clean.remote_verification.starts_with("NOT_PERFORMED"));

    // Consistent references: an organizational number owned by the authenticated
    // organization, a personal number owned by a principal.
    let s = scope(EnvironmentKind::Sandbox);
    assign(&h, &s, OwnershipClass::Organization, s.tenant_id).await;
    assign(&h, &s, OwnershipClass::Personal, Uuid::new_v4()).await;
    let ok = report(h.conn()).await.unwrap();
    assert_eq!(ok.anomalies, 0, "{ok:#?}");
    assert_eq!(
        check(&ok, "organization_owner_refs").distinct_foreign_ids,
        1
    );
    assert_eq!(check(&ok, "principal_owner_refs").rows, 1);
    assert!(check(&ok, "environment_single_owner").rows >= 4);

    // Invalid references.
    let bad_org = assign(&h, &s, OwnershipClass::Organization, Uuid::new_v4()).await;
    assign(&h, &s, OwnershipClass::Professional, s.tenant_id).await;
    let other = OwnerScope {
        tenant_id: Uuid::new_v4(),
        application_id: Uuid::new_v4(),
        ..s
    };
    h.numbers
        .create_routing_profile(
            &RequestContext::generate(),
            &other,
            actor(),
            voice_to_principal(),
        )
        .await
        .unwrap();
    let before = counts(&h).await;
    let r = report(h.conn()).await.unwrap();
    let bad_history = scalar(
        h.conn(),
        &format!(
            "SELECT count(*) FROM number_assignment_history WHERE assignment_id = '{bad_org}'"
        ),
    )
    .await;
    assert_eq!(
        check(&r, "organization_owner_refs").anomalies,
        1 + bad_history,
        "{r:#?}"
    );
    assert_eq!(check(&r, "organization_owner_refs").status, "ATTENTION");
    assert_eq!(check(&r, "principal_owner_refs").anomalies, 1);
    assert_eq!(check(&r, "environment_single_owner").anomalies, 1);
    assert_eq!(check(&r, "assignment_history_owners").anomalies, 0);
    assert_eq!(
        r.anomalies,
        r.checks.iter().map(|c| c.anomalies).sum::<i64>()
    );
    // Report only: nothing changed.
    assert_eq!(counts(&h).await, before);
    // Ids and counters only: the report carries no number.
    let text = serde_json::to_string(&r).unwrap();
    assert!(!text.contains('+'), "{text}");
}

//! Part IV Workstream F: KYC requirements, cases, protected evidence, deterministic and
//! manual verification, expiry / revalidation / revocation / appeals and eligibility.
mod common;

use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use base64::Engine;
use chrono::Duration;
use common::{
    catalog, contracts::conforms, exec, in_txn, kyc as fx, scalar, scope, strings, Harness,
};
use numbering::{
    domain::{RequestContext, SharedClock},
    infra::{jobs::JobRunner, RetryPolicy},
    numbers::{
        assignments::NewAssignment,
        capabilities::NewCapability,
        eligibility::{EligibilityPolicy, EligibilityRequest, Standing},
        kyc::{
            evidence::EvidenceBytes,
            verifier::{DeterministicKycVerifier, ManualKycVerifier, Scenario},
            EligibilityOutcome as O, EvidenceKind as K, KycService, KycStatus as S, ManualReview,
            NewCase, RequirementType as T, ReviewDecision, SubjectKind, TenantClassification,
            VerificationHandler,
        },
        Actor, ActorKind, Capability as C, CapabilitySource, DomainError, EnvironmentKind,
        InventorySource, NumberType, OwnerScope, OwnershipClass,
    },
};
use serde_json::{json, Value};
use uuid::Uuid;

const COMMON: &str = "numbering/common/v1.schema.json#/$defs/";

fn ctx() -> RequestContext {
    RequestContext::generate()
}

fn personal(subject: Uuid) -> NewCase {
    NewCase {
        subject_kind: SubjectKind::Individual,
        subject_ref: subject,
        country: "US".into(),
        number_type: NumberType::Mobile,
        ownership_class: OwnershipClass::Personal,
        purpose_code: "PERSONAL_LINE".into(),
        provider_id: None,
        tenant_classification: TenantClassification::External,
    }
}

fn organization(subject: Uuid) -> NewCase {
    NewCase {
        subject_kind: SubjectKind::Organization,
        ownership_class: OwnershipClass::Organization,
        purpose_code: "CUSTOMER_SUPPORT".into(),
        ..personal(subject)
    }
}

fn pdf(marker: &str) -> EvidenceBytes {
    EvidenceBytes::new(fx::pdf(marker))
}

struct Kyc {
    h: Harness,
    kyc: KycService,
    store: Arc<cf_object_storage::MemoryObjectStore>,
    det: DeterministicKycVerifier,
}

async fn deterministic() -> Option<Kyc> {
    let h = Harness::new().await?;
    let (store, vault) = fx::vault();
    let det = DeterministicKycVerifier::new();
    let kyc = fx::service(&h, vault, Arc::new(det.clone()));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    Some(Kyc { h, kyc, store, det })
}

async fn manual() -> Option<Kyc> {
    let h = Harness::new().await?;
    let (store, vault) = fx::vault();
    let kyc = fx::service(&h, vault, Arc::new(ManualKycVerifier));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    Some(Kyc {
        h,
        kyc,
        store,
        det: DeterministicKycVerifier::new(),
    })
}

fn runner(h: &Harness, kyc: &KycService, name: &str) -> JobRunner {
    let clock: SharedClock = Arc::new(h.clock.clone());
    JobRunner::new(
        h.conn().clone(),
        clock,
        name,
        Duration::seconds(30),
        RetryPolicy {
            base: Duration::seconds(2),
            max: Duration::seconds(30),
        },
    )
    .with_handler(Arc::new(VerificationHandler { kyc: kyc.clone() }))
}

/// Opens a personal case, attaches identity evidence and submits it.
async fn submitted(k: &Kyc, s: &OwnerScope, subject: Uuid) -> Uuid {
    let case = k
        .kyc
        .create_case(&ctx(), s, personal(subject), None)
        .await
        .unwrap()
        .value;
    k.kyc
        .attach_evidence(
            &ctx(),
            s,
            case.id,
            K::Passport,
            "application/pdf",
            pdf("passport"),
            None,
        )
        .await
        .unwrap();
    let case = k.kyc.case(s, case.id).await.unwrap();
    k.kyc
        .submit_case(&ctx(), s, case.id, case.revision, None)
        .await
        .unwrap();
    case.id
}

fn statuses(decisions: &[numbering::numbers::kyc::cases::DecisionRow]) -> Vec<String> {
    decisions.iter().map(|d| d.to_status.clone()).collect()
}

fn standing() -> Standing {
    Standing {
        active: true,
        first_party: false,
        live_telecom_allowed: false,
    }
}

fn ask(
    subject: Uuid,
    number_type: NumberType,
    ownership: OwnershipClass,
    caps: &[C],
) -> EligibilityRequest {
    EligibilityRequest {
        subject_kind: if matches!(
            ownership,
            OwnershipClass::Personal | OwnershipClass::Professional
        ) {
            SubjectKind::Individual
        } else {
            SubjectKind::Organization
        },
        subject_ref: subject,
        country: "US".into(),
        number_type,
        ownership_class: ownership,
        capabilities: caps.to_vec(),
        number_resource_id: None,
        provider_id: None,
    }
}

fn codes(d: &numbering::numbers::eligibility::EligibilityDecisionRow) -> Vec<String> {
    d.reasons
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["code"].as_str().unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------
// Step 5: requirements
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn requirements_are_versioned_scoped_snapshotted_and_fail_closed() {
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    use numbering::numbers::kyc::requirements::RequirementQuery;
    let q = |t: NumberType, o: OwnershipClass, provider: Option<&str>| RequirementQuery {
        country: "US".into(),
        inventory_class: numbering::numbers::InventoryClass::Synthetic,
        number_type: t,
        ownership_class: o,
        provider_id: provider.map(str::to_string),
        tenant_classification: TenantClassification::External,
    };
    // Generic: no KYC. Most specific selector wins.
    let generic = h
        .numbers
        .resolve_requirements(&q(NumberType::Geographic, OwnershipClass::Campaign, None))
        .await
        .unwrap();
    assert!(!generic.kyc_mandatory && generic.items.is_empty());
    let personal_req = h
        .numbers
        .resolve_requirements(&q(NumberType::Mobile, OwnershipClass::Personal, None))
        .await
        .unwrap();
    assert!(personal_req.kyc_mandatory && personal_req.synthetic);
    assert_eq!(
        personal_req
            .mandatory_items()
            .map(|i| i.requirement_type)
            .collect::<Vec<_>>(),
        vec![T::IndividualIdentity]
    );
    assert_eq!(
        personal_req.snapshot["nonProduction"], true,
        "synthetic policy is explicitly non-production"
    );
    let org = h
        .numbers
        .resolve_requirements(&q(NumberType::Mobile, OwnershipClass::Organization, None))
        .await
        .unwrap();
    assert_eq!(
        org.mandatory_items()
            .map(|i| i.requirement_type)
            .collect::<Vec<_>>(),
        vec![T::BusinessRegistration, T::PurposeOfUse]
    );

    // A provider extension merges (never replaces) and only for that provider.
    let mut ext = fx::set(
        Some(NumberType::Mobile),
        Some(OwnershipClass::Personal),
        vec![fx::item(T::Address, true, &[K::ProofOfAddress])],
        90,
        None,
    );
    ext.scope = numbering::numbers::kyc::RequirementScope::ProviderExtension;
    ext.provider_id = Some("synthetic".into());
    ext.kyc_mandatory = true;
    h.numbers
        .publish_requirements(&ctx(), fx::operator(), "CARRIER_RULE", ext)
        .await
        .unwrap();
    let with_ext = h
        .numbers
        .resolve_requirements(&q(
            NumberType::Mobile,
            OwnershipClass::Personal,
            Some("synthetic"),
        ))
        .await
        .unwrap();
    assert_eq!(with_ext.set_ids.len(), 2);
    assert_eq!(with_ext.approval_validity_days, 90);
    assert_eq!(
        with_ext
            .mandatory_items()
            .map(|i| i.requirement_type)
            .collect::<Vec<_>>(),
        vec![T::IndividualIdentity, T::Address]
    );
    assert_eq!(
        h.numbers
            .resolve_requirements(&q(NumberType::Mobile, OwnershipClass::Personal, None))
            .await
            .unwrap()
            .set_ids
            .len(),
        1
    );

    // Versioning: a new version supersedes; the case keeps its exact snapshot.
    let s = scope(EnvironmentKind::Sandbox);
    let case = k
        .kyc
        .create_case(&ctx(), &s, personal(Uuid::new_v4()), None)
        .await
        .unwrap()
        .value;
    let before = case.requirement_snapshot.clone();
    h.numbers
        .publish_requirements(
            &ctx(),
            fx::operator(),
            "POLICY_UPDATE",
            fx::set(
                Some(NumberType::Mobile),
                Some(OwnershipClass::Personal),
                vec![
                    fx::item(T::IndividualIdentity, true, &[K::Passport]),
                    fx::item(T::Address, true, &[K::ProofOfAddress]),
                ],
                365,
                None,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        k.kyc.case(&s, case.id).await.unwrap().requirement_snapshot,
        before
    );
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM number_kyc_requirements WHERE selector_key = 'BASE|US|SYNTHETIC|MOBILE|PERSONAL|*|*'").await,
        2
    );
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM number_kyc_requirements WHERE selector_key = 'BASE|US|SYNTHETIC|MOBILE|PERSONAL|*|*' AND status = 'CURRENT'").await,
        1
    );

    // Fail closed: no requirements for a country / class → KYC_REQUIREMENTS_UNKNOWN.
    let mut unknown = q(NumberType::Mobile, OwnershipClass::Personal, None);
    unknown.country = "NG".into();
    unknown.inventory_class = numbering::numbers::InventoryClass::Real;
    assert!(matches!(
        h.numbers.resolve_requirements(&unknown).await.unwrap_err(),
        DomainError::Precondition {
            code: "KYC_REQUIREMENTS_UNKNOWN",
            ..
        }
    ));
    // Synthetic policy is never production policy, and vice versa.
    let mut real_synthetic = fx::set(None, None, vec![], 365, None);
    real_synthetic.inventory_class = numbering::numbers::InventoryClass::Real;
    assert!(h
        .numbers
        .publish_requirements(&ctx(), fx::operator(), "BAD", real_synthetic)
        .await
        .is_err());
    let mut bad_kind = fx::set(
        Some(NumberType::Mobile),
        None,
        vec![fx::item(T::BusinessRegistration, true, &[K::Passport])],
        365,
        None,
    );
    bad_kind.ownership_class = Some(OwnershipClass::Organization);
    assert!(h
        .numbers
        .publish_requirements(&ctx(), fx::operator(), "BAD", bad_kind)
        .await
        .is_err());
    // Unknown requirements refuse case creation too.
    let mut ng = personal(Uuid::new_v4());
    ng.country = "NG".into();
    assert!(matches!(
        k.kyc
            .create_case(&ctx(), &scope(EnvironmentKind::Production), ng, None)
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "KYC_REQUIREMENTS_UNKNOWN",
            ..
        }
    ));
    // No KYC needed → no case.
    let mut geo = organization(Uuid::new_v4());
    geo.number_type = NumberType::Geographic;
    geo.ownership_class = OwnershipClass::Campaign;
    assert!(matches!(
        k.kyc.create_case(&ctx(), &s, geo, None).await.unwrap_err(),
        DomainError::Precondition {
            code: "KYC_NOT_REQUIRED",
            ..
        }
    ));
    // Published versions are events.
    assert!(
        scalar(h.conn(), "SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.kyc-requirements.published'").await >= 5
    );
}

// ---------------------------------------------------------------------------------------
// Step 6: lifecycle, history, database guards
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_case_lifecycle_is_guarded_and_its_history_is_immutable() {
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    let s = scope(EnvironmentKind::Sandbox);
    let subject = Uuid::new_v4();
    let case = k
        .kyc
        .create_case(&ctx(), &s, personal(subject), None)
        .await
        .unwrap()
        .value;
    assert_eq!((case.status(), case.revision), (S::Draft, 1));
    // DRAFT cannot be submitted (no evidence yet): not an edge.
    assert!(matches!(
        k.kyc
            .submit_case(&ctx(), &s, case.id, 1, None)
            .await
            .unwrap_err(),
        DomainError::InvalidTransition { .. }
    ));
    // An optional requirement's evidence alone is incomplete.
    k.kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::ProofOfAddress,
            "application/pdf",
            pdf("address"),
            None,
        )
        .await
        .unwrap();
    let c = k.kyc.case(&s, case.id).await.unwrap();
    assert_eq!(c.status(), S::EvidenceRequired);
    assert_eq!(
        k.kyc.missing_requirements(&c).await.unwrap(),
        vec![T::IndividualIdentity]
    );
    assert!(matches!(
        k.kyc
            .submit_case(&ctx(), &s, case.id, c.revision, None)
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "MANDATORY_EVIDENCE_MISSING",
            ..
        }
    ));
    // Evidence a case does not ask for is refused.
    assert!(matches!(
        k.kyc
            .attach_evidence(
                &ctx(),
                &s,
                case.id,
                K::BusinessRegistrationCertificate,
                "application/pdf",
                pdf("biz"),
                None
            )
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "EVIDENCE_NOT_REQUIRED",
            ..
        }
    ));
    k.kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::IdentityDocument,
            "image/png",
            EvidenceBytes::new(vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2]),
            None,
        )
        .await
        .unwrap();
    let c = k.kyc.case(&s, case.id).await.unwrap();
    // Stale revision.
    assert!(matches!(
        k.kyc
            .submit_case(&ctx(), &s, case.id, c.revision - 1, None)
            .await
            .unwrap_err(),
        DomainError::VersionConflict(_)
    ));
    let c = k
        .kyc
        .submit_case(&ctx(), &s, case.id, c.revision, None)
        .await
        .unwrap()
        .value;
    assert_eq!(c.status(), S::Submitted);
    // No evidence once submitted.
    assert!(matches!(
        k.kyc
            .attach_evidence(
                &ctx(),
                &s,
                case.id,
                K::Passport,
                "application/pdf",
                pdf("late"),
                None
            )
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "CASE_NOT_COLLECTING_EVIDENCE",
            ..
        }
    ));
    runner(h, &k.kyc, "w1").run_due().await.unwrap();
    let c = k.kyc.case(&s, case.id).await.unwrap();
    assert_eq!(c.status(), S::Approved);
    assert!(c.approved_at_time(h.now()) && c.expires_at.unwrap() > h.now());
    assert_eq!(
        c.revalidate_at.unwrap() - c.approved_at.unwrap(),
        Duration::days(180)
    );
    let decisions = k.kyc.decisions(case.id).await.unwrap();
    assert_eq!(
        statuses(&decisions),
        [
            "DRAFT",
            "EVIDENCE_REQUIRED",
            "SUBMITTED",
            "UNDER_REVIEW",
            "APPROVED"
        ]
    );
    assert_eq!(
        decisions
            .iter()
            .map(|d| d.case_revision)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(decisions[4].verifier_kind, "DETERMINISTIC");
    assert_eq!(
        scalar(h.conn(), &format!("SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.kyc-case.changed' AND aggregate_id = '{}'", case.id)).await,
        5
    );
    // Other scopes: not found (no oracle).
    let other = scope(EnvironmentKind::Sandbox);
    assert!(matches!(
        k.kyc.case(&other, case.id).await.unwrap_err(),
        DomainError::NotFound(_)
    ));
    assert!(matches!(
        k.kyc
            .attach_evidence(
                &ctx(),
                &other,
                case.id,
                K::Passport,
                "application/pdf",
                pdf("x"),
                None
            )
            .await
            .unwrap_err(),
        DomainError::NotFound(_)
    ));

    // The database enforces the same rules independently.
    let id = case.id;
    let err = exec(h.conn(), &format!("UPDATE number_kyc_cases SET status = 'REVOKED', revoked_at = now(), decision_reason = 'X_X', revision = revision + 1 WHERE id = '{id}'")).await.unwrap_err();
    assert!(
        err.to_string().contains("only through a domain transition"),
        "{err}"
    );
    let err = in_txn(h.conn(), &[
        "SELECT set_config('numbering.kyc', 'transition', true)",
        &format!("UPDATE number_kyc_cases SET status = 'DRAFT', revision = revision + 1 WHERE id = '{id}'"),
    ]).await.unwrap_err();
    assert!(err.to_string().contains("not allowed"), "{err}");
    let err = in_txn(h.conn(), &[
        "SELECT set_config('numbering.kyc', 'transition', true)",
        &format!("UPDATE number_kyc_cases SET subject_ref = gen_random_uuid(), revision = revision + 1 WHERE id = '{id}'"),
    ]).await.unwrap_err();
    assert!(err.to_string().contains("immutable"), "{err}");
    for sql in [
        format!("UPDATE number_kyc_decisions SET reason_code = 'TAMPERED' WHERE case_id = '{id}'"),
        format!("DELETE FROM number_kyc_decisions WHERE case_id = '{id}'"),
        format!("DELETE FROM number_kyc_cases WHERE id = '{id}'"),
        format!("UPDATE number_kyc_evidence_refs SET object_key = 'kyc-evidence/{id}/{id}' WHERE case_id = '{id}'"),
        format!("DELETE FROM number_kyc_evidence_refs WHERE case_id = '{id}'"),
        "TRUNCATE number_kyc_decisions".to_string(),
    ] {
        assert!(exec(h.conn(), &sql).await.is_err(), "{sql}");
    }
    // An approval needs an expiry (CHECK), a manual decision needs an operator.
    let err = exec(h.conn(), &format!(
        "INSERT INTO number_kyc_decisions (id, case_id, case_revision, from_status, to_status, decision_kind, verifier_kind, \
           actor_kind, actor_ref, reason_code, evidence_ids, detail, correlation_id, decided_at) VALUES (gen_random_uuid(), '{id}', 99, \
           'UNDER_REVIEW', 'APPROVED', 'MANUAL_REVIEW', 'MANUAL', 'SYSTEM', 'x', 'OK_OK', '{{}}', '{{}}', 'corr_abcdefgh', now())"
    )).await.unwrap_err();
    assert!(
        err.to_string().contains("ck_number_kyc_decisions_manual"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------------------
// Step 6: protected evidence
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn evidence_is_sealed_typed_minimal_audited_and_retained() {
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    let s = scope(EnvironmentKind::Sandbox);
    let case = k
        .kyc
        .create_case(&ctx(), &s, personal(Uuid::new_v4()), None)
        .await
        .unwrap()
        .value;
    let marker = "PASSPORT-NO-X9Z31-SECRET";
    let e = k
        .kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::Passport,
            "application/pdf",
            pdf(marker),
            None,
        )
        .await
        .unwrap()
        .value;
    assert_eq!(
        (e.status.as_str(), e.requirement_type.as_str()),
        ("ACTIVE", "INDIVIDUAL_IDENTITY")
    );
    // At rest: sealed, never the plaintext or its base64.
    let raw = k.store.get_raw(&e.object_key).unwrap();
    let raw_text = String::from_utf8_lossy(&raw);
    let b64 = base64::engine::general_purpose::STANDARD.encode(fx::pdf(marker));
    assert!(!raw_text.contains(marker) && !raw_text.contains(&b64[..24]));
    assert!(e
        .object_key
        .starts_with(&format!("kyc-evidence/{}/", case.id)));
    // The database keeps a reference only.
    let row = strings(
        h.conn(),
        &format!(
            "SELECT row_to_json(e)::text FROM number_kyc_evidence_refs e WHERE id = '{}'",
            e.id
        ),
    )
    .await
    .pop()
    .unwrap();
    assert!(!row.contains(marker));

    // Privileged read: operators only, every attempt audited, integrity verified.
    assert!(matches!(
        k.kyc
            .read_evidence(
                &ctx(),
                &Actor::new(ActorKind::Customer, "sa"),
                e.id,
                "AUDIT"
            )
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "PRIVILEGED_ACCESS_REQUIRED",
            ..
        }
    ));
    let (_, bytes) = k
        .kyc
        .read_evidence(&ctx(), &fx::operator(), e.id, "MANUAL_REVIEW")
        .await
        .unwrap();
    assert_eq!(bytes.expose(), fx::pdf(marker).as_slice());
    assert_eq!(
        strings(h.conn(), &format!("SELECT outcome::text FROM number_kyc_evidence_access WHERE evidence_id = '{}' ORDER BY accessed_at, outcome", e.id)).await,
        vec!["DENIED", "GRANTED"]
    );
    assert!(exec(
        h.conn(),
        "UPDATE number_kyc_evidence_access SET outcome = 'GRANTED'"
    )
    .await
    .is_err());
    // A sealed blob moved under another reference does not open (bound to its id).
    let e2 = k
        .kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::ProofOfAddress,
            "application/pdf",
            pdf("address"),
            None,
        )
        .await
        .unwrap()
        .value;
    use cf_object_storage::ObjectStore;
    k.store
        .put(&e2.object_key, raw.clone(), "application/octet-stream")
        .await
        .unwrap();
    assert!(matches!(
        k.kyc
            .read_evidence(&ctx(), &fx::operator(), e2.id, "MANUAL_REVIEW")
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "EVIDENCE_INTEGRITY",
            ..
        }
    ));
    // Minimum data: type, size and magic bytes.
    for (t, b) in [
        ("application/pdf", b"MZ not a pdf".to_vec()),
        ("text/plain", b"hello".to_vec()),
        ("image/jpeg", vec![]),
        (
            "application/pdf",
            [b"%PDF-".as_slice(), &vec![0u8; 5 * 1024 * 1024]].concat(),
        ),
    ] {
        assert!(matches!(
            k.kyc
                .attach_evidence(
                    &ctx(),
                    &s,
                    case.id,
                    K::Passport,
                    t,
                    EvidenceBytes::new(b),
                    None
                )
                .await
                .unwrap_err(),
            DomainError::Validation(_)
        ));
    }
    assert_eq!(k.store.len(), 2, "a refused upload leaves no object behind");

    // Without a protected store evidence is refused (never stored unprotected).
    let unprotected = KycService::new(
        h.numbers.clone(),
        Arc::new(numbering::numbers::kyc::evidence::EvidenceVault::unconfigured()),
        Arc::new(ManualKycVerifier),
        Default::default(),
    );
    assert!(matches!(
        unprotected
            .attach_evidence(
                &ctx(),
                &s,
                case.id,
                K::Passport,
                "application/pdf",
                pdf("x"),
                None
            )
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "EVIDENCE_STORAGE_UNAVAILABLE",
            ..
        }
    ));

    // Retention: content is deleted, the reference stays PURGED, reads are refused.
    h.clock.advance(Duration::days(731));
    assert_eq!(k.kyc.purge_expired_evidence(100).await.unwrap(), 2);
    assert_eq!(k.store.len(), 0);
    let purged = k.kyc.evidence(case.id).await.unwrap();
    assert!(purged
        .iter()
        .all(|e| e.status == "PURGED" && e.purged_at.is_some()));
    assert!(matches!(
        k.kyc
            .read_evidence(&ctx(), &fx::operator(), e.id, "AUDIT")
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "EVIDENCE_PURGED",
            ..
        }
    ));
    // Purge is one-way.
    assert!(exec(h.conn(), &format!("UPDATE number_kyc_evidence_refs SET status = 'ACTIVE', purged_at = NULL WHERE id = '{}'", e.id)).await.is_err());
}

// ---------------------------------------------------------------------------------------
// Step 7: deterministic verifier
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_deterministic_verifier_reproduces_every_outcome() {
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    let s = scope(EnvironmentKind::Sandbox);
    let w = runner(h, &k.kyc, "w1");

    // Rejection.
    let rejected = Uuid::new_v4();
    k.det.script(
        rejected,
        Scenario::Reject {
            reason: "DOCUMENT_MISMATCH".into(),
        },
    );
    let id = submitted(&k, &s, rejected).await;
    w.run_due().await.unwrap();
    let c = k.kyc.case(&s, id).await.unwrap();
    assert_eq!(
        (c.status(), c.decision_reason.as_deref()),
        (S::Rejected, Some("DOCUMENT_MISMATCH"))
    );

    // More evidence → resubmission → approval with a short validity → expiry.
    let more = Uuid::new_v4();
    k.det.script(
        more,
        Scenario::MoreEvidence {
            missing: vec![T::Address],
        },
    );
    let id = submitted(&k, &s, more).await;
    w.run_due().await.unwrap();
    let c = k.kyc.case(&s, id).await.unwrap();
    assert_eq!(c.status(), S::MoreEvidenceRequired);
    let last = k.kyc.decisions(id).await.unwrap().pop().unwrap();
    assert_eq!(last.detail["missingRequirements"], json!(["ADDRESS"]));
    k.kyc
        .attach_evidence(
            &ctx(),
            &s,
            id,
            K::ProofOfAddress,
            "application/pdf",
            pdf("address"),
            None,
        )
        .await
        .unwrap();
    k.det.script(more, Scenario::Approve { valid_for_days: 10 });
    let c = k
        .kyc
        .submit_case(&ctx(), &s, id, c.revision, None)
        .await
        .unwrap()
        .value;
    assert_eq!(c.status(), S::Submitted);
    w.run_due().await.unwrap();
    let c = k.kyc.case(&s, id).await.unwrap();
    assert_eq!(c.status(), S::Approved);
    assert_eq!(
        c.expires_at.unwrap() - c.approved_at.unwrap(),
        Duration::days(10)
    );
    h.clock.advance(Duration::days(11));
    let expired = k.kyc.expire_due(&ctx(), 100).await.unwrap();
    assert!(expired.iter().any(|e| e.id == id));
    assert_eq!(k.kyc.case(&s, id).await.unwrap().status(), S::Expired);

    // A validity longer than the policy is capped.
    let long = Uuid::new_v4();
    k.det.script(
        long,
        Scenario::Approve {
            valid_for_days: 10_000,
        },
    );
    let id_long = submitted(&k, &s, long).await;
    w.run_due().await.unwrap();
    let c = k.kyc.case(&s, id_long).await.unwrap();
    assert_eq!(
        c.expires_at.unwrap() - c.approved_at.unwrap(),
        Duration::days(365)
    );

    // Duplicate responses: applied once.
    let dup = Uuid::new_v4();
    k.det.script(dup, Scenario::DuplicateApproval);
    let id = submitted(&k, &s, dup).await;
    w.run_due().await.unwrap();
    let v = k.kyc.verifications(id).await.unwrap().pop().unwrap();
    let before = k.kyc.decisions(id).await.unwrap().len();
    // The same response delivered again (a replayed job) changes nothing.
    assert_eq!(
        k.kyc.run_verification(&ctx(), v.id).await.unwrap(),
        numbering::infra::jobs::JobOutcome::Succeeded
    );
    assert_eq!(k.kyc.decisions(id).await.unwrap().len(), before);

    // Revalidation: renewal, then a failed revalidation revokes.
    h.clock.advance(Duration::days(181));
    let renewed = Uuid::new_v4();
    let id_renewed = submitted(&k, &s, renewed).await;
    w.run_due().await.unwrap();
    h.clock.advance(Duration::days(181));
    k.det.script_revalidation(
        renewed,
        Scenario::Approve {
            valid_for_days: 365,
        },
    );
    k.det.script_revalidation(
        long,
        Scenario::Reject {
            reason: "SANCTIONS_HIT".into(),
        },
    );
    assert!(k.kyc.schedule_revalidations(&ctx(), 100).await.unwrap() >= 2);
    // Scheduling twice does not duplicate.
    assert_eq!(k.kyc.schedule_revalidations(&ctx(), 100).await.unwrap(), 0);
    w.run_due().await.unwrap();
    let r = k.kyc.case(&s, id_renewed).await.unwrap();
    assert_eq!(
        (
            r.status(),
            r.approved_at,
            r.expires_at.unwrap() > h.now() + Duration::days(300)
        ),
        (S::Approved, Some(h.now()), true)
    );
    assert_eq!(
        statuses(&k.kyc.decisions(id_renewed).await.unwrap())
            .last()
            .unwrap(),
        "APPROVED"
    );
    let l = k.kyc.case(&s, id_long).await.unwrap();
    assert_eq!(
        (l.status(), l.decision_reason.as_deref()),
        (S::Revoked, Some("REVALIDATION_FAILED"))
    );
    assert!(l.revoked_at.is_some());
}

#[tokio::test]
async fn an_unknown_verifier_outcome_is_polled_never_resubmitted_across_a_worker_restart() {
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    let s = scope(EnvironmentKind::Sandbox);
    let subject = Uuid::new_v4();
    k.det.script(
        subject,
        Scenario::UnknownThen(Box::new(Scenario::Approve { valid_for_days: 30 })),
    );
    let id = submitted(&k, &s, subject).await;
    let report = runner(h, &k.kyc, "worker-a").run_due().await.unwrap();
    assert_eq!(report.retried, 1);
    let c = k.kyc.case(&s, id).await.unwrap();
    assert_eq!(c.status(), S::UnderReview, "unknown is not a decision");
    let v = k.kyc.verifications(id).await.unwrap().pop().unwrap();
    assert_eq!(v.status, "UNKNOWN");
    // The process restarts: a new runner and a new service over the same database and
    // the same (external) verifier state.
    let (_, vault) = fx::vault();
    let restarted = fx::service(h, vault, Arc::new(k.det.clone()));
    h.clock.advance(Duration::seconds(5));
    runner(h, &restarted, "worker-b").run_due().await.unwrap();
    let c = restarted.case(&s, id).await.unwrap();
    assert_eq!(c.status(), S::Approved);
    let calls: Vec<_> = k
        .det
        .calls()
        .into_iter()
        .filter(|(r, _)| *r == v.request_ref)
        .map(|(_, kind)| kind)
        .collect();
    assert_eq!(calls, ["verify", "status"], "polled, never a second check");
    assert_eq!(
        restarted
            .verifications(id)
            .await
            .unwrap()
            .pop()
            .unwrap()
            .status,
        "COMPLETED"
    );
}

// ---------------------------------------------------------------------------------------
// Step 7: audited manual review, revocation, appeals
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn manual_review_is_privileged_explicit_evidence_based_and_never_bypasses_requirements() {
    let Some(k) = manual().await else {
        return;
    };
    let h = &k.h;
    let s = scope(EnvironmentKind::Sandbox);
    let org = Uuid::new_v4();
    let case = k
        .kyc
        .create_case(&ctx(), &s, organization(org), None)
        .await
        .unwrap()
        .value;
    let reg = k
        .kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::BusinessRegistrationCertificate,
            "application/pdf",
            pdf("reg"),
            None,
        )
        .await
        .unwrap()
        .value;
    let purpose = k
        .kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::PurposeStatement,
            "application/pdf",
            pdf("purpose"),
            None,
        )
        .await
        .unwrap()
        .value;
    let c = k.kyc.case(&s, case.id).await.unwrap();
    k.kyc
        .submit_case(&ctx(), &s, case.id, c.revision, None)
        .await
        .unwrap();
    runner(h, &k.kyc, "w").run_due().await.unwrap();
    let c = k.kyc.case(&s, case.id).await.unwrap();
    assert_eq!(
        c.status(),
        S::UnderReview,
        "the manual adapter waits for a human"
    );
    let review = |d: ReviewDecision, ids: Vec<Uuid>, rev: i64| ManualReview {
        decision: d,
        reason_code: "DOCUMENTS_VERIFIED".into(),
        evidence_ids: ids,
        expected_revision: rev,
        missing: vec![],
        valid_for_days: None,
    };
    // Not an operator.
    assert!(matches!(
        k.kyc
            .review_case(
                &ctx(),
                &Actor::system("bot"),
                case.id,
                review(ReviewDecision::Approve, vec![reg.id], c.revision)
            )
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "PRIVILEGED_ACCESS_REQUIRED",
            ..
        }
    ));
    // No / foreign evidence.
    assert!(matches!(
        k.kyc
            .review_case(
                &ctx(),
                &fx::operator(),
                case.id,
                review(ReviewDecision::Approve, vec![], c.revision)
            )
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "EVIDENCE_NOT_IN_CASE",
            ..
        }
    ));
    assert!(matches!(
        k.kyc
            .review_case(
                &ctx(),
                &fx::operator(),
                case.id,
                review(ReviewDecision::Approve, vec![Uuid::new_v4()], c.revision)
            )
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "EVIDENCE_NOT_IN_CASE",
            ..
        }
    ));
    // A malformed reason / a request for evidence naming nothing.
    let mut bad = review(ReviewDecision::Approve, vec![reg.id], c.revision);
    bad.reason_code = "ok".into();
    assert!(k
        .kyc
        .review_case(&ctx(), &fx::operator(), case.id, bad)
        .await
        .is_err());
    assert!(matches!(
        k.kyc
            .review_case(
                &ctx(),
                &fx::operator(),
                case.id,
                review(ReviewDecision::RequestEvidence, vec![reg.id], c.revision)
            )
            .await
            .unwrap_err(),
        DomainError::Validation(_)
    ));
    // A mandatory requirement without ACTIVE evidence cannot be approved by hand.
    exec(h.conn(), &format!("UPDATE number_kyc_evidence_refs SET status = 'PURGED', purged_at = now() WHERE id = '{}'", purpose.id)).await.unwrap();
    assert!(matches!(
        k.kyc
            .review_case(
                &ctx(),
                &fx::operator(),
                case.id,
                review(ReviewDecision::Approve, vec![reg.id], c.revision)
            )
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "MANDATORY_EVIDENCE_MISSING",
            ..
        }
    ));
    // Reject → appeal → approve: the appeal is OVERTURNED, linked to the decision.
    let mut reject = review(ReviewDecision::Reject, vec![reg.id], c.revision);
    reject.reason_code = "INCOMPLETE_FILING".into();
    let (c, rejection) = k
        .kyc
        .review_case(&ctx(), &fx::operator(), case.id, reject)
        .await
        .unwrap();
    assert_eq!(
        (
            c.status(),
            rejection.decision_kind.as_str(),
            rejection.actor_kind.as_str()
        ),
        (S::Rejected, "MANUAL_REVIEW", "OPERATOR")
    );
    assert_eq!(rejection.evidence_ids, vec![reg.id]);
    assert!(matches!(
        k.kyc
            .open_appeal(
                &ctx(),
                &scope(EnvironmentKind::Sandbox),
                case.id,
                "WRONG_DECISION",
                None
            )
            .await
            .unwrap_err(),
        DomainError::NotFound(_)
    ));
    let appeal = k
        .kyc
        .open_appeal(&ctx(), &s, case.id, "FILING_CORRECTED", None)
        .await
        .unwrap()
        .value;
    assert_eq!(
        (appeal.status.as_str(), appeal.appealed_decision_id),
        ("OPEN", rejection.id)
    );
    assert!(matches!(
        k.kyc
            .open_appeal(&ctx(), &s, case.id, "AGAIN", None)
            .await
            .unwrap_err(),
        DomainError::Precondition {
            code: "NOT_APPEALABLE",
            ..
        }
    ));
    exec(h.conn(), &format!("UPDATE number_kyc_evidence_refs SET status = 'ACTIVE', purged_at = NULL WHERE id = '{}'", purpose.id)).await.unwrap_err();
    // Evidence comes back by a new upload only after more evidence is requested.
    let c = k.kyc.case(&s, case.id).await.unwrap();
    let mut ask_more = review(ReviewDecision::RequestEvidence, vec![reg.id], c.revision);
    ask_more.missing = vec![T::PurposeOfUse];
    ask_more.reason_code = "PURPOSE_STATEMENT_NEEDED".into();
    let (c, _) = k
        .kyc
        .review_case(&ctx(), &fx::operator(), case.id, ask_more)
        .await
        .unwrap();
    assert_eq!(c.status(), S::MoreEvidenceRequired);
    let purpose2 = k
        .kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::PurposeStatement,
            "application/pdf",
            pdf("purpose v2"),
            None,
        )
        .await
        .unwrap()
        .value;
    k.kyc
        .submit_case(&ctx(), &s, case.id, c.revision, None)
        .await
        .unwrap();
    runner(h, &k.kyc, "w").run_due().await.unwrap();
    let c = k.kyc.case(&s, case.id).await.unwrap();
    let mut approve = review(
        ReviewDecision::Approve,
        vec![reg.id, purpose2.id],
        c.revision,
    );
    approve.valid_for_days = Some(30);
    let (c, approval) = k
        .kyc
        .review_case(&ctx(), &fx::operator(), case.id, approve)
        .await
        .unwrap();
    assert_eq!(c.status(), S::Approved);
    assert_eq!(
        c.expires_at.unwrap() - c.approved_at.unwrap(),
        Duration::days(30)
    );
    assert_eq!(
        (c.reviewer_kind.as_deref(), c.reviewer_ref.as_deref()),
        (Some("OPERATOR"), Some("kyc-reviewer"))
    );
    let appeals = k.kyc.appeals(case.id).await.unwrap();
    assert_eq!(
        (
            appeals[0].status.as_str(),
            appeals[0].resolution_decision_id
        ),
        ("OVERTURNED", Some(approval.id))
    );
    // Revocation (operator only) → appeal → rejection: UPHELD.
    assert!(k
        .kyc
        .revoke_case(&ctx(), &Actor::system("x"), case.id, "FRAUD_SIGNAL")
        .await
        .is_err());
    let c = k
        .kyc
        .revoke_case(&ctx(), &fx::operator(), case.id, "FRAUD_SIGNAL")
        .await
        .unwrap();
    assert_eq!(c.status(), S::Revoked);
    k.kyc
        .open_appeal(&ctx(), &s, case.id, "NOT_FRAUD", None)
        .await
        .unwrap();
    let c = k.kyc.case(&s, case.id).await.unwrap();
    let (_, upheld) = k
        .kyc
        .review_case(
            &ctx(),
            &fx::operator(),
            case.id,
            review(ReviewDecision::Reject, vec![reg.id], c.revision),
        )
        .await
        .unwrap();
    let appeals = k.kyc.appeals(case.id).await.unwrap();
    assert_eq!(
        (
            appeals[1].status.as_str(),
            appeals[1].resolution_decision_id
        ),
        ("UPHELD", Some(upheld.id))
    );
    // A resolved appeal is final.
    assert!(exec(
        h.conn(),
        &format!(
            "UPDATE number_kyc_appeals SET status = 'OPEN', resolved_at = NULL WHERE id = '{}'",
            appeals[0].id
        )
    )
    .await
    .is_err());
    // The full immutable history, one decision per revision.
    let history = k.kyc.decisions(case.id).await.unwrap();
    assert_eq!(
        statuses(&history),
        [
            "DRAFT",
            "EVIDENCE_REQUIRED",
            "SUBMITTED",
            "UNDER_REVIEW",
            "REJECTED",
            "UNDER_REVIEW",
            "MORE_EVIDENCE_REQUIRED",
            "SUBMITTED",
            "UNDER_REVIEW",
            "APPROVED",
            "REVOKED",
            "UNDER_REVIEW",
            "REJECTED"
        ]
    );
    let revisions: Vec<i64> = history.iter().map(|d| d.case_revision).collect();
    assert_eq!(revisions, (1..=history.len() as i64).collect::<Vec<_>>());
}

// ---------------------------------------------------------------------------------------
// Step 8: eligibility
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn eligibility_is_explainable_tenant_isolated_fail_closed_and_never_reserves() {
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    let n = &h.numbers;
    let s = scope(EnvironmentKind::Sandbox);
    let policy = EligibilityPolicy::default();
    let eval = |s: OwnerScope, st: Standing, p: EligibilityPolicy, r: EligibilityRequest| async move {
        n.evaluate_eligibility(&ctx(), &s, st, &p, r).await.unwrap()
    };
    let reservations = scalar(h.conn(), "SELECT count(*) FROM number_reservations").await;

    // No KYC required: ELIGIBLE, no reasons.
    let org = Uuid::new_v4();
    let d = eval(
        s,
        standing(),
        policy.clone(),
        ask(
            org,
            NumberType::Geographic,
            OwnershipClass::Campaign,
            &[C::VoiceInbound],
        ),
    )
    .await;
    assert_eq!((d.outcome(), codes(&d).len()), (O::Eligible, 0));
    assert!(d.valid_until > h.now() && d.valid_until <= h.now() + Duration::minutes(30));
    conforms(
        &format!("{COMMON}EligibilityDecision"),
        &numbering::http::kyc::eligibility_view(&d),
    );
    // Capability truth: SMS outbound is excluded on synthetic US geographic; inbound ≠ outbound.
    let d = eval(
        s,
        standing(),
        policy.clone(),
        ask(
            org,
            NumberType::Geographic,
            OwnershipClass::Campaign,
            &[C::SmsInbound, C::SmsOutbound],
        ),
    )
    .await;
    assert_eq!(d.outcome(), O::UnsupportedCapability);
    assert_eq!(d.reasons[0]["capability"], "SMS_OUTBOUND");
    assert_eq!(d.reasons.as_array().unwrap().len(), 1);
    // A KYC-required class: required → incomplete → pending → eligible.
    let person = Uuid::new_v4();
    let mobile = || {
        ask(
            person,
            NumberType::Mobile,
            OwnershipClass::Personal,
            &[C::VoiceInbound, C::SmsInbound],
        )
    };
    let d = eval(s, standing(), policy.clone(), mobile()).await;
    assert_eq!(
        (d.outcome(), codes(&d)),
        (O::KycRequired, vec!["KYC_REQUIRED".to_string()])
    );
    let case = k
        .kyc
        .create_case(&ctx(), &s, personal(person), None)
        .await
        .unwrap()
        .value;
    assert_eq!(
        codes(&eval(s, standing(), policy.clone(), mobile()).await),
        ["KYC_INCOMPLETE"]
    );
    k.kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::Passport,
            "application/pdf",
            pdf("p"),
            None,
        )
        .await
        .unwrap();
    let c = k.kyc.case(&s, case.id).await.unwrap();
    k.kyc
        .submit_case(&ctx(), &s, case.id, c.revision, None)
        .await
        .unwrap();
    let d = eval(s, standing(), policy.clone(), mobile()).await;
    assert_eq!(
        (d.outcome(), codes(&d)),
        (O::PendingReview, vec!["KYC_PENDING_REVIEW".to_string()])
    );
    runner(h, &k.kyc, "w").run_due().await.unwrap();
    let approved = k.kyc.case(&s, case.id).await.unwrap();
    let d = eval(s, standing(), policy.clone(), mobile()).await;
    assert_eq!(d.outcome(), O::Eligible);
    assert_eq!(
        (d.kyc_case_id, d.kyc_case_revision),
        (Some(case.id), Some(approved.revision))
    );
    // Validity never outlives the approval.
    let long_ttl = EligibilityPolicy {
        ttl: Duration::days(400),
        ..policy.clone()
    };
    let d = eval(s, standing(), long_ttl, mobile()).await;
    assert_eq!(d.valid_until, approved.expires_at.unwrap());
    // Tenant isolation: another scope's approval is not this scope's.
    let other = scope(EnvironmentKind::Sandbox);
    assert_eq!(
        eval(other, standing(), policy.clone(), mobile())
            .await
            .outcome(),
        O::KycRequired
    );
    assert!(matches!(
        n.eligibility_decision(&other, d.id).await.unwrap_err(),
        DomainError::NotFound(_)
    ));

    // Fail-closed refusals.
    let geo = || {
        ask(
            org,
            NumberType::Geographic,
            OwnershipClass::Campaign,
            &[C::VoiceInbound],
        )
    };
    let mut gb = geo();
    gb.country = "GB".into();
    assert_eq!(
        codes(&eval(s, standing(), policy.clone(), gb).await),
        [
            "COUNTRY_NOT_SUPPORTED",
            "KYC_REQUIREMENTS_UNKNOWN",
            "NUMBER_TYPE_NOT_SUPPORTED"
        ]
    );
    assert_eq!(
        codes(
            &eval(
                s,
                Standing {
                    active: false,
                    ..standing()
                },
                policy.clone(),
                geo()
            )
            .await
        ),
        ["TENANT_NOT_ACTIVE"]
    );
    assert_eq!(
        codes(
            &eval(
                s,
                standing(),
                EligibilityPolicy {
                    enabled: false,
                    ..policy.clone()
                },
                geo()
            )
            .await
        ),
        ["FEATURE_DISABLED"]
    );
    let mut restricted = policy.clone();
    restricted.risk_restricted_countries.insert("US".into());
    let d = eval(s, standing(), restricted, geo()).await;
    assert_eq!(
        (d.outcome(), codes(&d)),
        (O::RiskRestricted, vec!["RISK_RESTRICTED".to_string()])
    );
    let d = eval(
        s,
        standing(),
        EligibilityPolicy {
            max_numbers_sandbox: 0,
            ..policy.clone()
        },
        geo(),
    )
    .await;
    assert_eq!(
        (d.outcome(), codes(&d)),
        (O::LimitExceeded, vec!["NUMBER_LIMIT_REACHED".to_string()])
    );
    let mut mismatch = geo();
    mismatch.subject_kind = SubjectKind::Individual;
    assert_eq!(
        codes(&eval(s, standing(), policy.clone(), mismatch).await),
        ["OWNERSHIP_SUBJECT_MISMATCH"]
    );
    // Production: live telecom must be allowed; the real catalog and real requirements must exist.
    catalog::verified_ng(h).await;
    let prod = scope(EnvironmentKind::Production);
    let mut ng = ask(
        org,
        NumberType::Mobile,
        OwnershipClass::Campaign,
        &[C::VoiceInbound],
    );
    ng.country = "NG".into();
    assert_eq!(
        codes(&eval(prod, standing(), policy.clone(), ng.clone()).await),
        ["KYC_REQUIREMENTS_UNKNOWN", "LIVE_TELECOM_NOT_ALLOWED"]
    );
    let live = Standing {
        live_telecom_allowed: true,
        ..standing()
    };
    let d = eval(prod, live, policy.clone(), ng.clone()).await;
    assert_eq!(
        (d.outcome(), codes(&d)),
        (O::Denied, vec!["KYC_REQUIREMENTS_UNKNOWN".to_string()])
    );
    // Synthetic catalog never serves production.
    let mut us_prod = geo();
    us_prod.country = "US".into();
    assert!(codes(&eval(prod, live, policy.clone(), us_prod).await)
        .contains(&"COUNTRY_NOT_SUPPORTED".to_string()));

    // Requirements changing after approval ask for more evidence.
    h.numbers
        .publish_requirements(
            &ctx(),
            fx::operator(),
            "POLICY_UPDATE",
            fx::set(
                Some(NumberType::Mobile),
                Some(OwnershipClass::Personal),
                vec![
                    fx::item(T::IndividualIdentity, true, &[K::Passport]),
                    fx::item(T::Address, true, &[K::ProofOfAddress]),
                ],
                365,
                None,
            ),
        )
        .await
        .unwrap();
    let d = eval(s, standing(), policy.clone(), mobile()).await;
    assert_eq!(
        (d.outcome(), codes(&d)),
        (
            O::AdditionalEvidenceRequired,
            vec!["KYC_REQUIREMENTS_CHANGED".to_string()]
        )
    );
    // Revocation changes eligibility immediately.
    k.kyc
        .revoke_case(&ctx(), &fx::operator(), case.id, "FRAUD_SIGNAL")
        .await
        .unwrap();
    let d = eval(s, standing(), policy.clone(), mobile()).await;
    assert_eq!(
        (d.outcome(), codes(&d)),
        (O::Denied, vec!["KYC_REVOKED".to_string()])
    );

    // Never reserves; decisions are append-only and evented.
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM number_reservations").await,
        reservations
    );
    assert!(exec(
        h.conn(),
        "UPDATE number_eligibility_decisions SET outcome = 'ELIGIBLE', reasons = '[]'"
    )
    .await
    .is_err());
    assert!(exec(h.conn(), "DELETE FROM number_eligibility_decisions")
        .await
        .is_err());
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.eligibility.evaluated'").await,
        scalar(h.conn(), "SELECT count(*) FROM number_eligibility_decisions").await
    );
}

#[tokio::test]
async fn an_assignment_becomes_eligible_only_with_an_in_force_eligible_decision_of_its_scope() {
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    let n = &h.numbers;
    let s = scope(EnvironmentKind::Sandbox);
    // Per-number evidence (catalog support alone never establishes a number's support).
    let evidenced = |id: Uuid| async move {
        n.record_capability(
            &ctx(),
            id,
            NewCapability::declared(C::VoiceInbound, CapabilitySource::Synthetic),
        )
        .await
        .unwrap();
    };
    let r = catalog::classified(h, InventorySource::SyntheticTest, NumberType::Geographic).await;
    evidenced(r.id).await;
    let a = n
        .request_assignment(
            &ctx(),
            &s,
            common::actor(),
            NewAssignment {
                number_resource_id: r.id,
                ownership_class: OwnershipClass::Campaign,
                owner_ref: Uuid::new_v4(),
                policy_snapshot_ref: None,
                kyc_profile_ref: None,
            },
        )
        .await
        .unwrap();
    let a = n
        .submit_for_eligibility(&ctx(), common::actor(), a.id, a.revision)
        .await
        .unwrap();
    let org = Uuid::new_v4();
    let policy = EligibilityPolicy::default();
    let mut req = ask(
        org,
        NumberType::Geographic,
        OwnershipClass::Campaign,
        &[C::VoiceInbound],
    );
    req.number_resource_id = Some(r.id);
    let refused = n
        .evaluate_eligibility(
            &ctx(),
            &s,
            standing(),
            &policy,
            ask(
                org,
                NumberType::Geographic,
                OwnershipClass::Campaign,
                &[C::SmsOutbound],
            ),
        )
        .await
        .unwrap();
    let foreign = n
        .evaluate_eligibility(
            &ctx(),
            &scope(EnvironmentKind::Sandbox),
            standing(),
            &policy,
            req.clone(),
        )
        .await
        .unwrap();
    let mut other_number = req.clone();
    let second = catalog::classified(h, InventorySource::SyntheticTest, NumberType::Geographic)
        .await
        .id;
    evidenced(second).await;
    other_number.number_resource_id = Some(second);
    // Without evidence, the specific number is not supported (fail closed).
    let mut bare = other_number.clone();
    bare.number_resource_id = Some(
        catalog::classified(h, InventorySource::SyntheticTest, NumberType::Geographic)
            .await
            .id,
    );
    let bare = n
        .evaluate_eligibility(&ctx(), &s, standing(), &policy, bare)
        .await
        .unwrap();
    assert_eq!(
        (bare.outcome(), codes(&bare)),
        (
            O::UnsupportedCapability,
            vec!["CAPABILITY_NOT_SUPPORTED".to_string()]
        )
    );
    let other_number = n
        .evaluate_eligibility(&ctx(), &s, standing(), &policy, other_number)
        .await
        .unwrap();
    let ok = n
        .evaluate_eligibility(&ctx(), &s, standing(), &policy, req)
        .await
        .unwrap();
    assert_eq!(
        (ok.outcome(), foreign.outcome(), other_number.outcome()),
        (O::Eligible, O::Eligible, O::Eligible)
    );
    for bad in [Uuid::new_v4(), refused.id, foreign.id, other_number.id] {
        assert!(matches!(
            n.record_eligibility(&ctx(), common::actor(), a.id, a.revision, bad)
                .await
                .unwrap_err(),
            DomainError::Precondition {
                code: "ELIGIBILITY_REQUIRED",
                ..
            }
        ));
    }
    // Expired decisions do not admit.
    h.clock.advance(Duration::minutes(31));
    assert!(n
        .record_eligibility(&ctx(), common::actor(), a.id, a.revision, ok.id)
        .await
        .is_err());
    h.clock.advance(Duration::minutes(-31));
    let a = n
        .record_eligibility(&ctx(), common::actor(), a.id, a.revision, ok.id)
        .await
        .unwrap();
    assert_eq!(a.eligibility_decision_ref, Some(ok.id));
    // Consumers read the assignment's KYC standing (none required here).
    let st = k.kyc.assignment_kyc_status(a.id).await.unwrap();
    assert!(st.in_force && !st.kyc_required);
}

// ---------------------------------------------------------------------------------------
// Concurrency and idempotency
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_and_repeated_operations_are_idempotent_and_serialized() {
    let Some(k) = manual().await else {
        return;
    };
    let h = &k.h;
    let s = scope(EnvironmentKind::Sandbox);
    let subject = Uuid::new_v4();
    // The same key, concurrently: one case.
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let (kyc, s) = (k.kyc.clone(), s);
        tasks.push(tokio::spawn(async move {
            kyc.create_case(&ctx(), &s, personal(subject), Some("create-key-0001"))
                .await
        }));
    }
    let mut ids = std::collections::BTreeSet::new();
    for t in tasks {
        match t.await.unwrap() {
            Ok(o) => {
                ids.insert(o.value.id);
            }
            Err(DomainError::Conflict {
                code: "IDEMPOTENCY_IN_PROGRESS",
                ..
            }) => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(ids.len(), 1);
    assert_eq!(
        scalar(h.conn(), "SELECT count(*) FROM number_kyc_cases").await,
        1
    );
    let replay = k
        .kyc
        .create_case(&ctx(), &s, personal(subject), Some("create-key-0001"))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert!(matches!(
        k.kyc
            .create_case(&ctx(), &s, organization(subject), Some("create-key-0001"))
            .await
            .unwrap_err(),
        DomainError::Conflict {
            code: "IDEMPOTENCY_KEY_REUSED",
            ..
        }
    ));
    let id = *ids.iter().next().unwrap();
    // The same evidence with a key: stored once.
    for _ in 0..2 {
        k.kyc
            .attach_evidence(
                &ctx(),
                &s,
                id,
                K::Passport,
                "application/pdf",
                pdf("p"),
                Some("evidence-key-01"),
            )
            .await
            .unwrap();
    }
    assert_eq!(k.store.len(), 1);
    // Concurrent submissions at the same revision: exactly one wins.
    let rev = k.kyc.case(&s, id).await.unwrap().revision;
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let (kyc, s) = (k.kyc.clone(), s);
        tasks.push(tokio::spawn(async move {
            kyc.submit_case(&ctx(), &s, id, rev, None).await
        }));
    }
    let wins = futures_count(tasks).await;
    assert_eq!(wins, 1);
    assert_eq!(
        scalar(
            h.conn(),
            &format!("SELECT count(*) FROM number_kyc_verifications WHERE case_id = '{id}'")
        )
        .await,
        1
    );
    runner(h, &k.kyc, "w").run_due().await.unwrap();
    // Concurrent reviewers: one decision.
    let c = k.kyc.case(&s, id).await.unwrap();
    let ev = k.kyc.evidence(id).await.unwrap()[0].id;
    let mut tasks = Vec::new();
    for d in [
        ReviewDecision::Approve,
        ReviewDecision::Reject,
        ReviewDecision::Approve,
    ] {
        let kyc = k.kyc.clone();
        tasks.push(tokio::spawn(async move {
            kyc.review_case(
                &ctx(),
                &fx::operator(),
                id,
                ManualReview {
                    decision: d,
                    reason_code: "REVIEWED".into(),
                    evidence_ids: vec![ev],
                    expected_revision: c.revision,
                    missing: vec![],
                    valid_for_days: None,
                },
            )
            .await
        }));
    }
    assert_eq!(futures_count(tasks).await, 1);
    assert_eq!(
        scalar(h.conn(), &format!("SELECT count(*) FROM number_kyc_decisions WHERE case_id = '{id}' AND decision_kind = 'MANUAL_REVIEW'")).await,
        1
    );
}

async fn futures_count<T>(tasks: Vec<tokio::task::JoinHandle<Result<T, DomainError>>>) -> usize {
    let mut ok = 0;
    for t in tasks {
        if t.await.unwrap().is_ok() {
            ok += 1;
        }
    }
    ok
}

// ---------------------------------------------------------------------------------------
// Sensitive data
// ---------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn evidence_content_never_reaches_logs_events_decisions_or_errors() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    let s = scope(EnvironmentKind::Sandbox);
    let marker = "SSN-078-05-1120-SECRET";
    let subject = Uuid::new_v4();
    let case = k
        .kyc
        .create_case(&ctx(), &s, personal(subject), None)
        .await
        .unwrap()
        .value;
    let e = k
        .kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::Passport,
            "application/pdf",
            pdf(marker),
            None,
        )
        .await
        .unwrap()
        .value;
    // A refused upload's error does not echo content either.
    let err = k
        .kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::Passport,
            "image/png",
            pdf(marker),
            None,
        )
        .await
        .unwrap_err();
    assert!(!err.to_string().contains(marker));
    let c = k.kyc.case(&s, case.id).await.unwrap();
    k.kyc
        .submit_case(&ctx(), &s, case.id, c.revision, None)
        .await
        .unwrap();
    runner(h, &k.kyc, "w").run_due().await.unwrap();
    let (_, bytes) = k
        .kyc
        .read_evidence(&ctx(), &fx::operator(), e.id, "AUDIT")
        .await
        .unwrap();
    assert!(format!("{bytes:?}").contains("redacted"));
    let b64 = base64::engine::general_purpose::STANDARD.encode(fx::pdf(marker));
    let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).to_string();
    assert!(
        !logs.contains(marker) && !logs.contains(&b64[..20]),
        "content in logs"
    );
    for table in [
        "numbering_outbox_events",
        "number_kyc_decisions",
        "number_kyc_cases",
        "number_kyc_evidence_refs",
        "number_kyc_evidence_access",
        "number_kyc_verifications",
        "numbering_jobs",
        "numbering_idempotency_keys",
    ] {
        let dump = strings(
            h.conn(),
            &format!("SELECT COALESCE(string_agg(row_to_json(t)::text, ''), '') FROM {table} t"),
        )
        .await
        .join("");
        assert!(
            !dump.contains(marker) && !dump.contains(&b64[..20]),
            "content in {table}"
        );
    }
    // Events carry ids and codes only — never the object key or digest.
    let events = strings(
        h.conn(),
        "SELECT payload::text FROM numbering_outbox_events WHERE event_type LIKE 'number.kyc%'",
    )
    .await
    .join("");
    assert!(!events.contains("kyc-evidence/") && !events.contains(&e.sha256));
    // Customer-facing views omit internal storage facts.
    let view = numbering::http::kyc::evidence_view(&e).to_string();
    assert!(
        !view.contains("kyc-evidence/") && !view.contains(&e.sha256) && !view.contains("sealed")
    );
}

// ---------------------------------------------------------------------------------------
// HTTP: scopes, isolation, idempotency, contracts, privileged access
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn kyc_apis_enforce_scopes_isolation_idempotency_and_contracts() {
    let Some(h) = Harness::with_platform_access().await else {
        return;
    };
    let (store, vault) = fx::vault();
    let det = DeterministicKycVerifier::new();
    let kyc = fx::service(&h, vault, Arc::new(det.clone()));
    let h = h.with_kyc(kyc.clone(), EligibilityPolicy::default());
    catalog::synthetic_us(&h).await;
    fx::synthetic_us(&h).await;
    let pa = h.pa.clone().unwrap();
    let kyc_scopes = ["numbers.kyc.read", "numbers.kyc.manage"];
    let alice = pa.customer("SANDBOX", &kyc_scopes, &kyc_scopes).await;
    let bob = pa.customer("SANDBOX", &kyc_scopes, &kyc_scopes).await;
    let reader = pa
        .customer("SANDBOX", &["numbers.read"], &["numbers.read"])
        .await;
    let ops = pa
        .customer_as(
            "PRODUCTION",
            "FIRST_PARTY",
            &["numbers.admin", "numbers.read"],
            &["numbers.admin", "numbers.read"],
        )
        .await;
    let call =
        |token: &str, m: Method, p: &str, b: Option<Value>, op: Option<&str>, key: Option<&str>| {
            let mut builder = axum::http::Request::builder()
                .method(m)
                .uri(p)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json");
            if let Some(op) = op {
                builder = builder.header("x-cf-operator", op);
            }
            if let Some(key) = key {
                builder = builder.header("idempotency-key", key);
            }
            let body = b.map(|b| b.to_string()).unwrap_or_default();
            let req = builder.body(axum::body::Body::from(body)).unwrap();
            let h = &h;
            async move { h.raw(req).await }
        };

    // Requirements.
    let r = call(
        &alice.secret,
        Method::GET,
        "/v1/numbers/kyc/requirements/US/MOBILE/PERSONAL",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}KycRequirements"), &r.body);
    assert_eq!(r.body["nonProduction"], true);
    let r = call(
        &reader.secret,
        Method::GET,
        "/v1/numbers/kyc/requirements/US/MOBILE/PERSONAL",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INSUFFICIENT_SCOPE")
    );
    let r = call(
        &alice.secret,
        Method::GET,
        "/v1/numbers/kyc/requirements/ZZ/MOBILE/PERSONAL",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.body["error"]["reason"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("KYC_REQUIREMENTS_UNKNOWN")
        )
    );

    // Case creation with idempotency.
    let subject = Uuid::new_v4();
    let body = json!({ "subjectKind": "INDIVIDUAL", "subjectRef": subject, "countryCode": "US", "numberType": "MOBILE",
                       "ownershipClass": "PERSONAL", "purposeCode": "PERSONAL_LINE" });
    let created = call(
        &alice.secret,
        Method::POST,
        "/v1/numbers/kyc/cases",
        Some(body.clone()),
        None,
        Some("case-key-00001"),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.body);
    conforms(&format!("{COMMON}KycCase"), &created.body);
    let replay = call(
        &alice.secret,
        Method::POST,
        "/v1/numbers/kyc/cases",
        Some(body.clone()),
        None,
        Some("case-key-00001"),
    )
    .await;
    assert_eq!(
        (replay.status, &replay.body["kycCaseId"]),
        (StatusCode::OK, &created.body["kycCaseId"])
    );
    let mut changed = body.clone();
    changed["purposeCode"] = json!("OTHER_PURPOSE");
    let reused = call(
        &alice.secret,
        Method::POST,
        "/v1/numbers/kyc/cases",
        Some(changed),
        None,
        Some("case-key-00001"),
    )
    .await;
    assert_eq!(
        (reused.status, reused.code()),
        (StatusCode::UNPROCESSABLE_ENTITY, "IDEMPOTENCY_KEY_REUSED")
    );
    let r = call(
        &reader.secret,
        Method::POST,
        "/v1/numbers/kyc/cases",
        Some(body.clone()),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let case_id = created.body["kycCaseId"].as_str().unwrap().to_string();

    // Evidence upload (base64), isolation, submission.
    let marker = "DL-889-SECRET";
    let content = base64::engine::general_purpose::STANDARD.encode(fx::pdf(marker));
    let evidence = json!({ "evidenceKind": "PASSPORT", "contentType": "application/pdf", "contentBase64": content });
    let r = call(
        &bob.secret,
        Method::POST,
        &format!("/v1/numbers/kyc/cases/{case_id}/evidence"),
        Some(evidence.clone()),
        None,
        None,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "another tenant's case does not exist for it"
    );
    let r = call(
        &alice.secret,
        Method::POST,
        &format!("/v1/numbers/kyc/cases/{case_id}/evidence"),
        Some(evidence.clone()),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    conforms(&format!("{COMMON}KycEvidence"), &r.body);
    assert!(!r.body.to_string().contains(marker) && !r.body.to_string().contains(&content[..20]));
    let evidence_id = r.body["evidenceId"].as_str().unwrap().to_string();
    let bad = json!({ "evidenceKind": "PASSPORT", "contentType": "application/pdf", "contentBase64": "not base64!!" });
    let r = call(
        &alice.secret,
        Method::POST,
        &format!("/v1/numbers/kyc/cases/{case_id}/evidence"),
        Some(bad),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = call(
        &alice.secret,
        Method::GET,
        &format!("/v1/numbers/kyc/cases/{case_id}"),
        None,
        None,
        None,
    )
    .await;
    conforms(&format!("{COMMON}KycCase"), &r.body);
    let rev = r.body["revision"].as_i64().unwrap();
    let r = call(
        &bob.secret,
        Method::GET,
        &format!("/v1/numbers/kyc/cases/{case_id}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &alice.secret,
        Method::POST,
        &format!("/v1/numbers/kyc/cases/{case_id}/submit"),
        Some(json!({ "expectedRevision": rev })),
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.body["status"].as_str()),
        (StatusCode::OK, Some("SUBMITTED")),
        "{:?}",
        r.body
    );
    runner(&h, &kyc, "w").run_due().await.unwrap();
    let r = call(
        &alice.secret,
        Method::GET,
        &format!("/v1/numbers/kyc/cases/{case_id}/history"),
        None,
        None,
        None,
    )
    .await;
    conforms(&format!("{COMMON}KycCaseHistory"), &r.body);
    assert_eq!(
        r.body["decisions"].as_array().unwrap().last().unwrap()["toStatus"],
        "APPROVED"
    );

    // Eligibility over HTTP.
    let ask = json!({ "subjectKind": "INDIVIDUAL", "subjectRef": subject, "countryCode": "US", "numberType": "MOBILE",
                      "ownershipClass": "PERSONAL", "capabilities": ["VOICE_INBOUND"] });
    let r = call(
        &alice.secret,
        Method::POST,
        "/v1/numbers/eligibility",
        Some(ask.clone()),
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.body["outcome"].as_str()),
        (StatusCode::OK, Some("ELIGIBLE")),
        "{:?}",
        r.body
    );
    conforms(&format!("{COMMON}EligibilityDecision"), &r.body);
    let decision = r.body["eligibilityDecisionId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = call(
        &bob.secret,
        Method::POST,
        "/v1/numbers/eligibility",
        Some(ask),
        None,
        None,
    )
    .await;
    assert_eq!(
        r.body["outcome"], "KYC_REQUIRED",
        "bob's scope has no approval"
    );
    let r = call(
        &bob.secret,
        Method::GET,
        &format!("/v1/numbers/eligibility/{decision}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Administration: numbers.admin + operator; content only in the audited access route.
    let r = call(
        &alice.secret,
        Method::GET,
        &format!("/v1/admin/kyc/cases/{case_id}"),
        None,
        Some("ops.x"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(
        &ops.secret,
        Method::GET,
        &format!("/v1/admin/kyc/cases/{case_id}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::BAD_REQUEST, "OPERATOR_REQUIRED")
    );
    let r = call(
        &ops.secret,
        Method::GET,
        &format!("/v1/admin/kyc/cases/{case_id}"),
        None,
        Some("ops.alice"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}AdminKycCase"), &r.body);
    assert!(!r.body.to_string().contains(marker));
    let r = call(
        &alice.secret,
        Method::POST,
        &format!("/v1/admin/kyc/evidence/{evidence_id}/access"),
        Some(json!({ "purposeCode": "AUDIT" })),
        Some("ops.x"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(
        &ops.secret,
        Method::POST,
        &format!("/v1/admin/kyc/evidence/{evidence_id}/access"),
        Some(json!({ "purposeCode": "AUDIT" })),
        Some("ops.alice"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    conforms(&format!("{COMMON}EvidenceContent"), &r.body);
    assert_eq!(r.body["contentBase64"], content);
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(
        scalar(h.conn(), &format!("SELECT count(*) FROM number_kyc_evidence_access WHERE evidence_id = '{evidence_id}' AND accessor_ref = 'ops.alice' AND outcome = 'GRANTED'")).await,
        1
    );
    // Revocation over HTTP changes eligibility immediately.
    let r = call(
        &ops.secret,
        Method::POST,
        &format!("/v1/admin/kyc/cases/{case_id}/revoke"),
        Some(json!({ "reasonCode": "FRAUD_SIGNAL" })),
        Some("ops.alice"),
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.body["status"].as_str()),
        (StatusCode::OK, Some("REVOKED")),
        "{:?}",
        r.body
    );
    // Requirement publication over HTTP.
    let publish = json!({ "scopeKind": "BASE", "countryCode": "US", "inventoryClass": "SYNTHETIC", "numberType": "TOLL_FREE",
        "kycMandatory": true, "items": [{ "type": "BUSINESS_REGISTRATION", "mandatory": true, "acceptedEvidence": ["BUSINESS_REGISTRATION_CERTIFICATE"] }],
        "approvalValidityDays": 365, "source": "SYNTHETIC_TEST", "sourceVersion": "s-1", "reasonCode": "TOLL_FREE_POLICY" });
    conforms(&format!("{COMMON}PublishRequirementsRequest"), &publish);
    let r = call(
        &ops.secret,
        Method::POST,
        "/v1/admin/kyc/requirements",
        Some(publish),
        Some("ops.alice"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    conforms(&format!("{COMMON}RequirementSet"), &r.body);
    drop(store);
}

#[tokio::test]
async fn internal_consumers_read_the_kyc_standing_of_an_assignment() {
    let Some(k) = deterministic().await else {
        return;
    };
    let h = &k.h;
    let s = scope(EnvironmentKind::Sandbox);
    let person = Uuid::new_v4();
    submitted(&k, &s, person).await;
    runner(h, &k.kyc, "w").run_due().await.unwrap();
    let r = catalog::classified(h, InventorySource::SyntheticTest, NumberType::Mobile).await;
    let n = &h.numbers;
    let a = n
        .request_assignment(
            &ctx(),
            &s,
            common::actor(),
            NewAssignment {
                number_resource_id: r.id,
                ownership_class: OwnershipClass::Personal,
                owner_ref: person,
                policy_snapshot_ref: None,
                kyc_profile_ref: None,
            },
        )
        .await
        .unwrap();
    let a = n
        .submit_for_eligibility(&ctx(), common::actor(), a.id, a.revision)
        .await
        .unwrap();
    let d = n
        .evaluate_eligibility(
            &ctx(),
            &s,
            standing(),
            &EligibilityPolicy::default(),
            ask(
                person,
                NumberType::Mobile,
                OwnershipClass::Personal,
                &[C::VoiceInbound],
            ),
        )
        .await
        .unwrap();
    assert_eq!(d.outcome(), O::Eligible);
    let a = n
        .record_eligibility(&ctx(), common::actor(), a.id, a.revision, d.id)
        .await
        .unwrap();
    let path = format!("/internal/v1/assignments/{}/kyc-status", a.id);
    let reply = h.internal(&h.peers.calling, Method::GET, &path).await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    conforms(&format!("{COMMON}AssignmentKycStatus"), &reply.body);
    assert_eq!(
        (
            reply.body["kycRequired"].as_bool(),
            reply.body["inForce"].as_bool()
        ),
        (Some(true), Some(true))
    );
    // Revocation is visible to consumers at once.
    k.kyc
        .revoke_case(
            &ctx(),
            &fx::operator(),
            d.kyc_case_id.unwrap(),
            "FRAUD_SIGNAL",
        )
        .await
        .unwrap();
    let reply = h.internal(&h.peers.calling, Method::GET, &path).await;
    assert_eq!(
        (
            reply.body["inForce"].as_bool(),
            reply.body["status"].as_str()
        ),
        (Some(false), Some("REVOKED"))
    );
    // Untrusted services are refused.
    assert_eq!(
        h.internal(&h.peers.privacy_relay, Method::GET, &path)
            .await
            .status,
        401
    );
}

// ---------------------------------------------------------------------------------------
// Migration
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_kyc_migration_refuses_to_discard_protected_evidence_and_reapplies() {
    use numbering_migration::{Migrator, MigratorTrait};
    let Some(k) = deterministic().await else {
        return;
    };
    let s = scope(EnvironmentKind::Sandbox);
    let case = k
        .kyc
        .create_case(&ctx(), &s, personal(Uuid::new_v4()), None)
        .await
        .unwrap()
        .value;
    k.kyc
        .attach_evidence(
            &ctx(),
            &s,
            case.id,
            K::Passport,
            "application/pdf",
            pdf("p"),
            None,
        )
        .await
        .unwrap();
    let steps = {
        let all = Migrator::migrations();
        let at = all
            .iter()
            .position(|m| m.name() == "m20261018_000004_kyc_eligibility")
            .unwrap();
        (all.len() - at) as u32
    };
    assert!(Migrator::down(k.h.conn(), Some(steps)).await.is_err());
    assert_eq!(
        scalar(k.h.conn(), "SELECT count(*) FROM number_kyc_evidence_refs").await,
        1
    );
    let Some(fresh) = common::TestDb::empty().await else {
        return;
    };
    Migrator::up(&fresh.conn, None).await.unwrap();
    Migrator::down(&fresh.conn, Some(steps)).await.unwrap();
    assert_eq!(
        scalar(&fresh.conn, "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND (table_name LIKE 'number_kyc%' OR table_name = 'number_eligibility_decisions')").await,
        0
    );
    Migrator::up(&fresh.conn, None).await.unwrap();
    assert_eq!(
        scalar(&fresh.conn, "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND (table_name LIKE 'number_kyc%' OR table_name = 'number_eligibility_decisions')").await,
        8
    );
}

//! Part IV Workstream E certification: the versioned country / number-type catalog,
//! authoritative capability resolution, safe capability changes (expiry, revocation,
//! stale and conflicting evidence, binding replacement), normalized errors, tenant
//! isolation, revision / event consistency, database constraints and the external
//! verification claim policy.
mod common;

use axum::http::{Method, StatusCode};
use chrono::Duration;
use common::{
    actor,
    catalog::{classified, operator, synthetic_us, type_entry, verified_ng},
    contracts::conforms,
    exec, scope, strings, Harness,
};
use numbering::{
    domain::RequestContext,
    numbers::{
        bindings::NewBinding,
        capabilities::NewCapability,
        catalog::{CatalogProvenance, CountryEntry},
        limitations::EXTERNAL_VERIFICATION_NOTICE,
        resolution::reason,
        Actor, BindingChangeReason, Capability as C, CapabilityChangeKind, CapabilitySource,
        CapabilityStatus, CatalogSource, CatalogVerification, CountrySupport, DomainError,
        EnvironmentKind, InventoryClass, InventorySource, NumberType, OwnershipClass,
        RestrictionFlag,
    },
};
use serde_json::json;
use uuid::Uuid;

const RESOLUTION: &str = "numbering/common/v1.schema.json#/$defs/CapabilityResolution";

fn ctx() -> RequestContext {
    RequestContext::generate()
}

fn synthetic(c: C) -> NewCapability {
    NewCapability::declared(c, CapabilitySource::Synthetic)
}

fn real_verified(h: &Harness, c: C, source: CapabilitySource) -> NewCapability {
    NewCapability::verified(c, source, Uuid::new_v4(), h.now())
}

async fn classify_geographic(h: &Harness, id: Uuid) {
    let r = h.numbers.resource(id).await.unwrap();
    h.numbers
        .classify(
            &ctx(),
            id,
            r.revision,
            NumberType::Geographic,
            numbering::numbers::ClassificationSource::Synthetic,
        )
        .await
        .unwrap();
}

async fn record(h: &Harness, id: Uuid, new: NewCapability) {
    h.numbers.record_capability(&ctx(), id, new).await.unwrap();
}

// ---------------------------------------------------------------------------------------
// Catalog versioning
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_catalog_is_versioned_append_only_and_gap_free() {
    let Some(h) = Harness::new().await else {
        return;
    };
    assert_eq!(h.numbers.catalog_revision().await.unwrap(), 0);
    let entries = synthetic_us(&h).await;
    assert_eq!(h.numbers.catalog_revision().await.unwrap(), 4);
    assert_eq!(
        entries
            .iter()
            .map(|e| e.catalog_revision)
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    // Republishing the mobile entry supersedes the previous version at the new revision.
    let v2 = h
        .numbers
        .publish_number_type(
            &ctx(),
            operator(),
            "CATALOG_UPDATE",
            type_entry(
                "US",
                InventoryClass::Synthetic,
                NumberType::Mobile,
                &[C::VoiceInbound],
                &[C::SmsOutbound],
            ),
        )
        .await
        .unwrap();
    assert_eq!(v2.catalog_revision, 5);
    let history = h
        .numbers
        .number_type_history("US", InventoryClass::Synthetic)
        .await
        .unwrap();
    let mobiles: Vec<_> = history
        .iter()
        .filter(|e| e.number_type == "MOBILE")
        .collect();
    assert_eq!(mobiles.len(), 2);
    assert_eq!(
        (
            mobiles[0].status.as_str(),
            mobiles[0].superseded_at_revision
        ),
        ("SUPERSEDED", Some(5))
    );
    assert_eq!(mobiles[1].status, "CURRENT");
    assert_eq!(
        h.numbers
            .number_type_entries("US", InventoryClass::Synthetic)
            .await
            .unwrap()
            .len(),
        3
    );
    // Concurrent publishers serialize: every revision is distinct and consecutive.
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let n = h.numbers.clone();
        tasks.push(tokio::spawn(async move {
            n.publish_number_type(
                &RequestContext::generate(),
                operator(),
                "CATALOG_UPDATE",
                type_entry(
                    "US",
                    InventoryClass::Synthetic,
                    NumberType::National,
                    &[C::VoiceInbound],
                    &[],
                ),
            )
            .await
            .unwrap()
            .catalog_revision
        }));
    }
    let mut revs = Vec::new();
    for t in tasks {
        revs.push(t.await.unwrap());
    }
    revs.sort();
    assert_eq!(revs, (6..14).collect::<Vec<_>>());
    assert_eq!(
        h.scalar("SELECT count(*) FROM number_type_catalog WHERE number_type = 'NATIONAL' AND status = 'CURRENT'").await,
        1
    );
    // The database keeps the history: no content edits, deletes, gaps or reopened rows.
    let id = v2.id;
    for sql in [
        format!("UPDATE number_type_catalog SET supported_capabilities = '{{SMS_OUTBOUND}}' WHERE id = '{id}'"),
        format!("UPDATE number_type_catalog SET status = 'CURRENT', superseded_at_revision = NULL WHERE id = '{}'", mobiles[0].id),
        format!("DELETE FROM number_type_catalog WHERE id = '{id}'"),
        "TRUNCATE number_type_catalog".to_string(),
        "UPDATE number_catalog_revisions SET reason_code = 'X_X'".to_string(),
        "INSERT INTO number_catalog_revisions VALUES (99, 'OPERATOR', 'x', 'GAP', 'corr_abcdefgh', now())".to_string(),
    ] {
        assert!(exec(h.conn(), &sql).await.is_err(), "{sql}");
    }
    // Events: one per catalog revision.
    assert_eq!(
        h.scalar("SELECT count(*) FROM numbering_outbox_events WHERE event_type = 'number.catalog.revised'").await,
        13
    );
}

#[tokio::test]
async fn catalog_facts_are_evidence_backed_and_synthetic_stays_synthetic() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let real_country = |p: CatalogProvenance| CountryEntry {
        country: "NG".into(),
        inventory_class: InventoryClass::Real,
        support: CountrySupport::Supported,
        restriction_flags: vec![],
        jurisdiction_policy_ref: None,
        provenance: p,
    };
    // VERIFIED without evidence; synthetic provenance on a real class; real provenance on
    // the synthetic class; an unknown country; an expiry before the start.
    for (entry, code) in [
        (
            real_country(CatalogProvenance {
                source: CatalogSource::RegulatorPublication,
                verification: CatalogVerification::Verified,
                evidence_ref: None,
                effective_from: None,
                effective_to: None,
            }),
            "EVIDENCE_REQUIRED",
        ),
        (
            real_country(CatalogProvenance::synthetic()),
            "VALIDATION_ERROR",
        ),
        (
            CountryEntry {
                inventory_class: InventoryClass::Synthetic,
                ..real_country(CatalogProvenance::verified(
                    CatalogSource::OperatorAttestation,
                    Uuid::new_v4(),
                ))
            },
            "VALIDATION_ERROR",
        ),
        (
            CountryEntry {
                country: "ZZ".into(),
                ..real_country(CatalogProvenance::verified(
                    CatalogSource::OperatorAttestation,
                    Uuid::new_v4(),
                ))
            },
            "COUNTRY_UNKNOWN",
        ),
        (
            real_country(CatalogProvenance {
                effective_to: Some(h.now() - Duration::days(1)),
                ..CatalogProvenance::verified(CatalogSource::RegulatorPublication, Uuid::new_v4())
            }),
            "VALIDATION_ERROR",
        ),
    ] {
        let err = h
            .numbers
            .publish_country(&ctx(), operator(), "TEST", entry)
            .await
            .unwrap_err();
        assert_eq!(err.code(), code);
    }
    // Number types need a supported country first, and class attributes follow the type.
    let err = h
        .numbers
        .publish_number_type(
            &ctx(),
            operator(),
            "TEST",
            type_entry(
                "NG",
                InventoryClass::Real,
                NumberType::Mobile,
                &[C::VoiceInbound],
                &[],
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "COUNTRY_NOT_SUPPORTED");
    verified_ng(&h).await;
    for (number_type, cap) in [
        (NumberType::Geographic, C::MobileNumberClass),
        (NumberType::Mobile, C::TollFreeBusiness),
        (NumberType::National, C::GeographicLocal),
    ] {
        let err = h
            .numbers
            .publish_number_type(
                &ctx(),
                operator(),
                "TEST",
                type_entry("NG", InventoryClass::Real, number_type, &[cap], &[]),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.code(),
            "NUMBER_CLASS_MISMATCH",
            "{cap} on {number_type}"
        );
    }
    let err = h
        .numbers
        .publish_number_type(
            &ctx(),
            operator(),
            "TEST",
            type_entry(
                "NG",
                InventoryClass::Real,
                NumberType::Mobile,
                &[C::SmsInbound],
                &[C::SmsInbound],
            ),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation(_)));
    // The database refuses the same at its own level.
    for sql in [
        "INSERT INTO number_type_catalog (id, country_code, inventory_class, number_type, locality_class, \
           supported_capabilities, source, verification_status, effective_from, catalog_revision, recorded_by, recorded_at) \
         VALUES (gen_random_uuid(), 'NG', 'REAL', 'GEOGRAPHIC', 'LOCAL', '{MOBILE_NUMBER_CLASS}', \
           'PROVIDER_DOCUMENTATION', 'UNVERIFIED', now(), 1, 'raw', now())",
        "INSERT INTO number_type_catalog (id, country_code, inventory_class, number_type, locality_class, \
           source, verification_status, effective_from, catalog_revision, recorded_by, recorded_at) \
         VALUES (gen_random_uuid(), 'NG', 'REAL', 'MOBILE', 'NATIONAL', 'PROVIDER_DOCUMENTATION', 'VERIFIED', now(), 1, 'raw', now())",
        "INSERT INTO number_type_catalog (id, country_code, inventory_class, number_type, locality_class, \
           source, verification_status, effective_from, catalog_revision, recorded_by, recorded_at) \
         VALUES (gen_random_uuid(), 'NG', 'REAL', 'MOBILE', 'NATIONAL', 'SYNTHETIC_TEST', 'SYNTHETIC', now(), 1, 'raw', now())",
        "INSERT INTO number_country_catalog (id, country_code, inventory_class, calling_code, support_status, source, \
           verification_status, effective_from, catalog_revision, recorded_by, recorded_at) \
         VALUES (gen_random_uuid(), 'US', 'SYNTHETIC', '1', 'SUPPORTED', 'OPERATOR_ATTESTATION', 'UNVERIFIED', now(), 1, 'raw', now())",
    ] {
        assert!(exec(h.conn(), sql).await.is_err(), "{sql}");
    }
}

// ---------------------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn resolution_combines_catalog_evidence_binding_and_state() {
    let Some(h) = Harness::new().await else {
        return;
    };
    synthetic_us(&h).await;
    let r = classified(&h, InventorySource::SyntheticTest, NumberType::Mobile).await;
    // Country support never establishes number support: no evidence yet.
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    assert!(res.capabilities.iter().all(|c| !c.supported));
    assert_eq!(
        res.get(C::VoiceInbound).reason,
        Some(reason::EVIDENCE_MISSING)
    );
    assert_eq!(
        res.get(C::MmsRich).reason,
        Some(reason::CAPABILITY_EXCLUDED_BY_CATALOG)
    );
    for c in [C::VoiceInbound, C::SmsInbound, C::MobileNumberClass] {
        record(&h, r.id, synthetic(c)).await;
    }
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    assert_eq!(
        res.supported(),
        vec![C::VoiceInbound, C::SmsInbound, C::MobileNumberClass]
    );
    // Inbound never implies outbound.
    assert!(!res.get(C::VoiceOutbound).supported && !res.get(C::SmsOutbound).supported);
    assert_eq!(
        res.get(C::SmsOutbound).reason,
        Some(reason::EVIDENCE_MISSING)
    );
    // Class attributes are usable now; routable ones need an ACTIVE number and a binding.
    assert!(res.get(C::MobileNumberClass).usable);
    assert_eq!(
        res.get(C::VoiceInbound).reason,
        Some(reason::RESOURCE_NOT_ACTIVE)
    );
    let e = res.get(C::SmsInbound);
    assert!(e.provenance.synthetic && !e.provenance.carrier_confirmed);
    assert_eq!(
        e.provenance.catalog_verification.as_deref(),
        Some("SYNTHETIC")
    );
    assert_eq!(res.notices, vec![EXTERNAL_VERIFICATION_NOTICE]);
    assert_eq!(res.catalog_revision, 4);
    assert_eq!(res.capability_revision, 3);
    conforms(
        RESOLUTION,
        &serde_json::to_value(numbering::http::catalog::resolution_view(&res)).unwrap(),
    );

    // An ACTIVE assignment with a binding makes routable capabilities usable.
    let s = scope(EnvironmentKind::Sandbox);
    let j = h.active(&s, OwnershipClass::Personal).await;
    let res = h.numbers.resolve_capabilities(j.resource_id).await.unwrap();
    // The journey's number is UNCLASSIFIED: resolution fails closed.
    assert_eq!(
        res.get(C::VoiceInbound).reason,
        Some(reason::NUMBER_TYPE_UNCLASSIFIED)
    );
    let rr = h.numbers.resource(j.resource_id).await.unwrap();
    h.numbers
        .classify(
            &ctx(),
            rr.id,
            rr.revision,
            NumberType::Geographic,
            numbering::numbers::ClassificationSource::Synthetic,
        )
        .await
        .unwrap();
    let res = h.numbers.resolve_capabilities(j.resource_id).await.unwrap();
    let voice = res.get(C::VoiceInbound);
    assert!(voice.supported && voice.usable, "{voice:?}");
    assert!(voice.provenance.binding_ref.is_some());
    // GEOGRAPHIC: SMS outbound is excluded, the mobile class never applies.
    assert_eq!(
        res.get(C::SmsOutbound).reason,
        Some(reason::CAPABILITY_EXCLUDED_BY_CATALOG)
    );
    assert_eq!(
        res.get(C::MobileNumberClass).reason,
        Some(reason::NUMBER_CLASS_MISMATCH)
    );
    // Suspension: still supported, no longer usable.
    let a = h
        .numbers
        .set_assignment_suspended(
            &ctx(),
            actor(),
            j.assignment.id,
            j.assignment.revision,
            true,
            "PAYMENT_OVERDUE",
        )
        .await
        .unwrap();
    let res = h.numbers.resolve_capabilities(j.resource_id).await.unwrap();
    assert!(res.get(C::VoiceInbound).supported && !res.get(C::VoiceInbound).usable);
    assert_eq!(
        res.get(C::VoiceInbound).reason,
        Some(reason::RESOURCE_NOT_ACTIVE)
    );
    let _ = a;
    // Deterministic: the same inputs resolve identically.
    let again = h.numbers.resolve_capabilities(j.resource_id).await.unwrap();
    assert_eq!(
        serde_json::to_value(&res.capabilities).unwrap(),
        serde_json::to_value(&again.capabilities).unwrap()
    );
}

#[tokio::test]
async fn real_numbers_need_verified_catalog_and_verified_evidence() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Only the synthetic catalog exists: a real number has no catalog at all.
    synthetic_us(&h).await;
    let r = classified(&h, InventorySource::LiveProvider, NumberType::Mobile).await;
    record(
        &h,
        r.id,
        real_verified(&h, C::SmsInbound, CapabilitySource::ProviderInventory),
    )
    .await;
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    assert_eq!(
        res.get(C::SmsInbound).reason,
        Some(reason::CATALOG_COUNTRY_MISSING)
    );
    assert_eq!(res.inventory_class, InventoryClass::Real);
    // An UNVERIFIED real catalog never authorizes a capability.
    h.numbers
        .publish_country(
            &ctx(),
            operator(),
            "PENDING_REVIEW",
            CountryEntry {
                country: "NG".into(),
                inventory_class: InventoryClass::Real,
                support: CountrySupport::Supported,
                restriction_flags: vec![],
                jurisdiction_policy_ref: None,
                provenance: CatalogProvenance {
                    source: CatalogSource::OperatorAttestation,
                    verification: CatalogVerification::Unverified,
                    evidence_ref: None,
                    effective_from: None,
                    effective_to: None,
                },
            },
        )
        .await
        .unwrap();
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    assert_eq!(
        res.get(C::SmsInbound).reason,
        Some(reason::CATALOG_UNVERIFIED)
    );
    verified_ng(&h).await;
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    assert!(res.get(C::SmsInbound).supported);
    assert!(
        !res.get(C::SmsInbound).provenance.carrier_confirmed,
        "inventory evidence is not a carrier confirmation"
    );
    // DECLARED evidence on a real number is not enough.
    record(
        &h,
        r.id,
        NewCapability::declared(C::VoiceInbound, CapabilitySource::ProviderInventory),
    )
    .await;
    assert_eq!(
        h.numbers
            .resolve_capabilities(r.id)
            .await
            .unwrap()
            .get(C::VoiceInbound)
            .reason,
        Some(reason::EVIDENCE_UNVERIFIED)
    );
    // A provider confirmation through an active binding is a carrier confirmation.
    let b = h
        .numbers
        .activate_binding(
            &ctx(),
            r.id,
            common::binding_for(InventorySource::LiveProvider, &[C::VoiceInbound]),
        )
        .await
        .unwrap();
    record(
        &h,
        r.id,
        NewCapability {
            binding_id: Some(b.id),
            ..real_verified(&h, C::VoiceInbound, CapabilitySource::ProviderConfirmation)
        },
    )
    .await;
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    let v = res.get(C::VoiceInbound);
    assert!(v.supported && v.provenance.carrier_confirmed && !v.provenance.synthetic);
    // Synthetic evidence is never carrier confirmation, whatever it claims.
    let s = classified(&h, InventorySource::SyntheticTest, NumberType::Mobile).await;
    let sb = h
        .numbers
        .activate_binding(
            &ctx(),
            s.id,
            common::binding_for(InventorySource::SyntheticTest, &[C::VoiceInbound]),
        )
        .await
        .unwrap();
    let err = h
        .numbers
        .record_capability(
            &ctx(),
            s.id,
            NewCapability {
                binding_id: Some(sb.id),
                ..real_verified(&h, C::VoiceInbound, CapabilitySource::ProviderConfirmation)
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::Validation(_)), "{err:?}");
    let err = h
        .numbers
        .record_capability(
            &ctx(),
            s.id,
            NewCapability {
                status: CapabilityStatus::Verified,
                evidence_ref: Some(Uuid::new_v4()),
                verified_at: Some(h.now()),
                ..synthetic(C::VoiceInbound)
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "EVIDENCE_REQUIRED");
}

#[tokio::test]
async fn combinations_restrictions_and_false_mobile_classification_are_prevented() {
    let Some(h) = Harness::new().await else {
        return;
    };
    synthetic_us(&h).await;
    // SMS on a GEOGRAPHIC number never makes it mobile, even with recorded evidence.
    let g = classified(&h, InventorySource::SyntheticTest, NumberType::Geographic).await;
    for c in [C::SmsInbound, C::MobileNumberClass, C::GeographicLocal] {
        record(&h, g.id, synthetic(c)).await;
    }
    let res = h.numbers.resolve_capabilities(g.id).await.unwrap();
    assert!(res.get(C::SmsInbound).supported);
    assert_eq!(
        res.get(C::MobileNumberClass).reason,
        Some(reason::NUMBER_CLASS_MISMATCH)
    );
    assert!(res.get(C::GeographicLocal).supported);
    assert_eq!(
        res.get(C::TollFreeBusiness).reason,
        Some(reason::NUMBER_CLASS_MISMATCH)
    );
    // Toll-free: SMS excluded by the catalog whatever the number's evidence says.
    let t = classified(&h, InventorySource::SyntheticTest, NumberType::TollFree).await;
    for c in [C::VoiceInbound, C::SmsOutbound, C::TollFreeBusiness] {
        record(&h, t.id, synthetic(c)).await;
    }
    let res = h.numbers.resolve_capabilities(t.id).await.unwrap();
    assert_eq!(res.supported(), vec![C::VoiceInbound, C::TollFreeBusiness]);
    assert_eq!(
        res.get(C::SmsOutbound).reason,
        Some(reason::CAPABILITY_EXCLUDED_BY_CATALOG)
    );
    // A capability the catalog does not mention is unknown, hence unsupported.
    let n = classified(&h, InventorySource::SyntheticTest, NumberType::National).await;
    record(&h, n.id, synthetic(C::VoiceInbound)).await;
    assert_eq!(
        h.numbers
            .resolve_capabilities(n.id)
            .await
            .unwrap()
            .get(C::VoiceInbound)
            .reason,
        Some(reason::CATALOG_TYPE_MISSING)
    );
    // Unclassified numbers fail closed.
    let u = h.available_number(InventorySource::SyntheticTest).await;
    record(&h, u.id, synthetic(C::VoiceInbound)).await;
    assert_eq!(
        h.numbers
            .resolve_capabilities(u.id)
            .await
            .unwrap()
            .get(C::VoiceInbound)
            .reason,
        Some(reason::NUMBER_TYPE_UNCLASSIFIED)
    );
    // Provider-specific and regional entries take precedence over the generic one.
    let mut regional = type_entry(
        "US",
        InventoryClass::Synthetic,
        NumberType::Mobile,
        &[C::VoiceInbound],
        &[C::SmsInbound],
    );
    regional.provider_id = Some("synthetic".into());
    h.numbers
        .publish_number_type(&ctx(), operator(), "PROVIDER_LIMIT", regional)
        .await
        .unwrap();
    let m = classified(&h, InventorySource::SyntheticTest, NumberType::Mobile).await;
    record(&h, m.id, synthetic(C::SmsInbound)).await;
    // Generic entry until the number is served by that provider.
    assert!(
        h.numbers
            .resolve_capabilities(m.id)
            .await
            .unwrap()
            .get(C::SmsInbound)
            .supported
    );
    h.numbers
        .activate_binding(
            &ctx(),
            m.id,
            common::binding_for(InventorySource::SyntheticTest, &[C::VoiceInbound]),
        )
        .await
        .unwrap();
    let res = h.numbers.resolve_capabilities(m.id).await.unwrap();
    assert_eq!(
        res.get(C::SmsInbound).reason,
        Some(reason::CAPABILITY_EXCLUDED_BY_CATALOG)
    );
    // An unsupported country fails everything; restrictions are merged and reported.
    h.numbers
        .publish_country(
            &ctx(),
            operator(),
            "COUNTRY_RESTRICTED",
            CountryEntry {
                country: "US".into(),
                inventory_class: InventoryClass::Synthetic,
                support: CountrySupport::Restricted,
                restriction_flags: vec![RestrictionFlag::NoPremiumDestinations],
                jurisdiction_policy_ref: None,
                provenance: CatalogProvenance::synthetic(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        h.numbers
            .resolve_capabilities(g.id)
            .await
            .unwrap()
            .restriction_flags,
        vec!["NO_PREMIUM_DESTINATIONS".to_string()]
    );
    h.numbers
        .publish_country(
            &ctx(),
            operator(),
            "COUNTRY_CLOSED",
            CountryEntry {
                country: "US".into(),
                inventory_class: InventoryClass::Synthetic,
                support: CountrySupport::Unsupported,
                restriction_flags: vec![],
                jurisdiction_policy_ref: None,
                provenance: CatalogProvenance::synthetic(),
            },
        )
        .await
        .unwrap();
    let res = h.numbers.resolve_capabilities(g.id).await.unwrap();
    assert!(res
        .capabilities
        .iter()
        .all(|c| c.reason == Some(reason::COUNTRY_NOT_SUPPORTED)));
}

// ---------------------------------------------------------------------------------------
// Capability changes
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn expired_stale_and_conflicting_evidence_cannot_authorize() {
    let Some(h) = Harness::new().await else {
        return;
    };
    verified_ng(&h).await;
    let r = classified(&h, InventorySource::LiveProvider, NumberType::Mobile).await;
    let expires = h.now() + Duration::days(30);
    record(
        &h,
        r.id,
        NewCapability {
            expires_at: Some(expires),
            ..real_verified(&h, C::SmsOutbound, CapabilitySource::ProviderInventory)
        },
    )
    .await;
    assert!(
        h.numbers
            .resolve_capabilities(r.id)
            .await
            .unwrap()
            .get(C::SmsOutbound)
            .supported
    );
    // Expiry changes truth immediately, before any sweep runs.
    h.clock.set(expires);
    assert_eq!(
        h.numbers
            .resolve_capabilities(r.id)
            .await
            .unwrap()
            .get(C::SmsOutbound)
            .reason,
        Some(reason::EVIDENCE_EXPIRED)
    );
    // The sweep makes it durable and evented, once.
    let swept = h.numbers.expire_capabilities(&ctx(), 100).await.unwrap();
    assert_eq!(swept.len(), 1);
    assert_eq!(swept[0].change_kind, CapabilityChangeKind::Expiry);
    assert_eq!(swept[0].capability.status, "EXPIRED");
    assert!(h
        .numbers
        .expire_capabilities(&ctx(), 100)
        .await
        .unwrap()
        .is_empty());
    // Fresh evidence after expiry is accepted (an expired record no longer has authority).
    record(
        &h,
        r.id,
        real_verified(&h, C::SmsOutbound, CapabilitySource::ManualImport),
    )
    .await;
    assert!(
        h.numbers
            .resolve_capabilities(r.id)
            .await
            .unwrap()
            .get(C::SmsOutbound)
            .supported
    );
    // A stale observation is refused (older than the current record's).
    let err = h
        .numbers
        .record_capability(
            &ctx(),
            r.id,
            NewCapability {
                source_observed_at: Some(h.now() - Duration::hours(1)),
                ..NewCapability {
                    status: CapabilityStatus::Withdrawn,
                    ..NewCapability::declared(C::SmsOutbound, CapabilitySource::ManualImport)
                }
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "STALE_EVIDENCE");
    // Authority: inventory (2) outranks a manual import (1)…
    h.clock.advance(Duration::minutes(1));
    record(
        &h,
        r.id,
        real_verified(&h, C::SmsOutbound, CapabilitySource::ProviderInventory),
    )
    .await;
    // …so a later manual import cannot override it upward…
    h.clock.advance(Duration::minutes(1));
    let err = h
        .numbers
        .record_capability(
            &ctx(),
            r.id,
            real_verified(&h, C::SmsOutbound, CapabilitySource::ManualImport),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "SOURCE_AUTHORITY_CONFLICT");
    // …but any source may downgrade (fail safe), and a higher one may then upgrade.
    record(
        &h,
        r.id,
        NewCapability {
            status: CapabilityStatus::Restricted,
            ..NewCapability::declared(C::SmsOutbound, CapabilitySource::ManualImport)
        },
    )
    .await;
    assert_eq!(
        h.numbers
            .resolve_capabilities(r.id)
            .await
            .unwrap()
            .get(C::SmsOutbound)
            .reason,
        Some(reason::CAPABILITY_RESTRICTED)
    );
    h.clock.advance(Duration::minutes(1));
    record(
        &h,
        r.id,
        real_verified(&h, C::SmsOutbound, CapabilitySource::ProviderInventory),
    )
    .await;
    // Optimistic concurrency on the capability revision.
    let current = h.numbers.resource(r.id).await.unwrap().capability_revision;
    let err = h
        .numbers
        .record_capability(
            &ctx(),
            r.id,
            NewCapability {
                expected_capability_revision: Some(current - 1),
                ..real_verified(&h, C::SmsInbound, CapabilitySource::ProviderInventory)
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::VersionConflict(_)));
    // A catalog entry that expires stops authorizing at its expiry.
    let mut short = type_entry(
        "NG",
        InventoryClass::Real,
        NumberType::National,
        &[C::VoiceInbound],
        &[],
    );
    short.provenance.effective_to = Some(h.now() + Duration::days(1));
    h.numbers
        .publish_number_type(&ctx(), operator(), "TEMPORARY", short)
        .await
        .unwrap();
    let n = classified(&h, InventorySource::LiveProvider, NumberType::National).await;
    record(
        &h,
        n.id,
        real_verified(&h, C::VoiceInbound, CapabilitySource::ProviderInventory),
    )
    .await;
    assert!(
        h.numbers
            .resolve_capabilities(n.id)
            .await
            .unwrap()
            .get(C::VoiceInbound)
            .supported
    );
    h.clock.advance(Duration::days(2));
    assert_eq!(
        h.numbers
            .resolve_capabilities(n.id)
            .await
            .unwrap()
            .get(C::VoiceInbound)
            .reason,
        Some(reason::CATALOG_TYPE_MISSING)
    );
}

#[tokio::test]
async fn upgrades_downgrades_and_revocations_are_versioned_audited_and_evented() {
    let Some(h) = Harness::new().await else {
        return;
    };
    synthetic_us(&h).await;
    let s = scope(EnvironmentKind::Sandbox);
    let j = h.active(&s, OwnershipClass::Organization).await; // records VOICE_INBOUND
    let id = j.resource_id;
    classify_geographic(&h, id).await;
    let before = h.numbers.resource(id).await.unwrap();
    let kinds = |h: &Harness| {
        let conn = h.conn().clone();
        async move {
            strings(
                &conn,
                &format!(
                    "SELECT payload->>'change_kind' || ':' || (payload->>'capability_revision') FROM numbering_outbox_events \
                      WHERE event_type = 'number.capability.changed' AND aggregate_id = '{id}' ORDER BY sequence"
                ),
            )
            .await
        }
    };
    assert_eq!(kinds(&h).await, vec!["INITIAL:1"]);
    // Downgrade needs an acknowledgement while an assignment relies on it.
    let withdraw = NewCapability {
        status: CapabilityStatus::Withdrawn,
        ..synthetic(C::VoiceInbound)
    };
    assert_eq!(
        h.numbers
            .record_capability(&ctx(), id, withdraw.clone())
            .await
            .unwrap_err()
            .code(),
        "CAPABILITY_IN_USE"
    );
    let down = h
        .numbers
        .record_capability(
            &ctx(),
            id,
            NewCapability {
                acknowledge_impact: true,
                ..withdraw
            },
        )
        .await
        .unwrap();
    assert_eq!(down.change_kind, CapabilityChangeKind::Downgrade);
    assert_eq!(down.impacted_assignments, vec![j.assignment.id]);
    let up = h
        .numbers
        .record_capability(&ctx(), id, synthetic(C::VoiceInbound))
        .await
        .unwrap();
    assert_eq!(up.change_kind, CapabilityChangeKind::Upgrade);
    let refresh = h
        .numbers
        .record_capability(&ctx(), id, synthetic(C::VoiceInbound))
        .await
        .unwrap();
    assert_eq!(refresh.change_kind, CapabilityChangeKind::Refresh);
    // Revocation: operators only, immediate, impact reported; not re-enabled by a system.
    assert_eq!(
        h.numbers
            .revoke_capability(&ctx(), actor(), id, C::VoiceInbound, "ABUSE")
            .await
            .unwrap_err()
            .code(),
        "PRIVILEGED_ACCESS_REQUIRED"
    );
    let revoked = h
        .numbers
        .revoke_capability(
            &ctx(),
            Actor::operator("trust-safety-1"),
            id,
            C::VoiceInbound,
            "ABUSE",
        )
        .await
        .unwrap();
    assert_eq!(revoked.change_kind, CapabilityChangeKind::Revocation);
    assert_eq!(revoked.impacted_assignments, vec![j.assignment.id]);
    assert_eq!(
        h.numbers
            .resolve_capabilities(id)
            .await
            .unwrap()
            .get(C::VoiceInbound)
            .reason,
        Some(reason::CAPABILITY_REVOKED)
    );
    assert_eq!(
        h.numbers
            .record_capability(&ctx(), id, synthetic(C::VoiceInbound))
            .await
            .unwrap_err()
            .code(),
        "REVOCATION_ACTIVE"
    );
    let regrant = h
        .numbers
        .record_capability(
            &ctx(),
            id,
            NewCapability {
                actor: Some(Actor::operator("trust-safety-1")),
                reason_code: Some("APPEAL_GRANTED".into()),
                ..synthetic(C::VoiceInbound)
            },
        )
        .await
        .unwrap();
    assert_eq!(regrant.change_kind, CapabilityChangeKind::Upgrade);
    // Every change: one record, one event, capability revision +1, resource revision +1.
    assert_eq!(
        kinds(&h).await,
        vec![
            "INITIAL:1",
            "DOWNGRADE:2",
            "UPGRADE:3",
            "REFRESH:4",
            "REVOCATION:5",
            "UPGRADE:6"
        ]
    );
    let after = h.numbers.resource(id).await.unwrap();
    assert_eq!(after.capability_revision, before.capability_revision + 5);
    assert_eq!(after.revision, before.revision + 5);
    let history = h.numbers.capability_history(id).await.unwrap();
    assert_eq!(history.len(), 6);
    assert_eq!(
        history.iter().filter(|c| c.effective_to.is_none()).count(),
        1
    );
    assert_eq!(history[4].actor_ref.as_deref(), Some("trust-safety-1"));
    assert_eq!(history[4].reason_code.as_deref(), Some("ABUSE"));
    // The record trail is immutable; capability revisions never go back.
    for sql in [
        format!("UPDATE number_capabilities SET status = 'VERIFIED' WHERE number_resource_id = '{id}'"),
        format!("UPDATE number_capabilities SET reason_code = 'X_X' WHERE number_resource_id = '{id}' AND effective_to IS NULL"),
        format!("UPDATE number_resources SET capability_revision = 0, revision = revision + 1 WHERE id = '{id}'"),
    ] {
        assert!(exec(h.conn(), &sql).await.is_err(), "{sql}");
    }
}

#[tokio::test]
async fn binding_replacement_keeps_the_canonical_id_and_ends_its_confirmations() {
    let Some(h) = Harness::new().await else {
        return;
    };
    verified_ng(&h).await;
    let r = classified(&h, InventorySource::LiveProvider, NumberType::Mobile).await;
    let b1 = h
        .numbers
        .activate_binding(
            &ctx(),
            r.id,
            common::binding_for(
                InventorySource::LiveProvider,
                &[C::VoiceInbound, C::SmsInbound],
            ),
        )
        .await
        .unwrap();
    record(
        &h,
        r.id,
        NewCapability {
            binding_id: Some(b1.id),
            ..real_verified(&h, C::VoiceInbound, CapabilitySource::ProviderConfirmation)
        },
    )
    .await;
    assert!(
        h.numbers
            .resolve_capabilities(r.id)
            .await
            .unwrap()
            .get(C::VoiceInbound)
            .provenance
            .carrier_confirmed
    );
    // Binding provenance must be real: another number's binding, an unserved capability.
    let other = classified(&h, InventorySource::LiveProvider, NumberType::Mobile).await;
    let ob = h
        .numbers
        .activate_binding(
            &ctx(),
            other.id,
            common::binding_for(InventorySource::LiveProvider, &[C::VoiceInbound]),
        )
        .await
        .unwrap();
    for (binding, cap) in [(ob.id, C::VoiceInbound), (b1.id, C::SmsOutbound)] {
        let err = h
            .numbers
            .record_capability(
                &ctx(),
                r.id,
                NewCapability {
                    binding_id: Some(binding),
                    ..real_verified(&h, cap, CapabilitySource::ProviderConfirmation)
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "BINDING_MISMATCH");
    }
    // Replacing the provider binding: the canonical id stays, the old confirmation ends.
    let b2 = h
        .numbers
        .replace_binding(
            &ctx(),
            b1.id,
            NewBinding {
                change_reason: BindingChangeReason::ProviderReplacement,
                ..common::binding_for(
                    InventorySource::LiveProvider,
                    &[C::VoiceInbound, C::SmsInbound],
                )
            },
        )
        .await
        .unwrap();
    assert_eq!(b2.number_resource_id, r.id);
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    assert_eq!(res.number_resource_id, r.id);
    assert_eq!(
        res.get(C::VoiceInbound).reason,
        Some(reason::EVIDENCE_BINDING_ENDED)
    );
    assert!(!res.get(C::VoiceInbound).provenance.carrier_confirmed);
    // The ended binding can no longer confirm anything; the new one can.
    let err = h
        .numbers
        .record_capability(
            &ctx(),
            r.id,
            NewCapability {
                binding_id: Some(b1.id),
                ..real_verified(&h, C::VoiceInbound, CapabilitySource::ProviderConfirmation)
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "BINDING_MISMATCH");
    record(
        &h,
        r.id,
        NewCapability {
            binding_id: Some(b2.id),
            ..real_verified(&h, C::VoiceInbound, CapabilitySource::ProviderConfirmation)
        },
    )
    .await;
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    assert!(res.get(C::VoiceInbound).provenance.carrier_confirmed);
    // Served by the new binding; routable usability also needs an ACTIVE number.
    assert_eq!(res.get(C::VoiceInbound).provenance.binding_ref, Some(b2.id));
    assert_eq!(
        res.get(C::VoiceInbound).reason,
        Some(reason::RESOURCE_NOT_ACTIVE)
    );
    assert_eq!(res.binding_revision, 2);
}

#[tokio::test]
async fn unsupported_requests_return_normalized_errors_without_fallback() {
    let Some(h) = Harness::new().await else {
        return;
    };
    synthetic_us(&h).await;
    let r = classified(&h, InventorySource::SyntheticTest, NumberType::TollFree).await;
    record(&h, r.id, synthetic(C::VoiceInbound)).await;
    let err = h
        .numbers
        .require_capability(r.id, C::SmsOutbound, false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "CAPABILITY_NOT_SUPPORTED");
    assert!(err.to_string().contains("CAPABILITY_EXCLUDED_BY_CATALOG"));
    let err = h
        .numbers
        .require_capability(r.id, C::VoiceInbound, true)
        .await
        .unwrap_err();
    assert_eq!(err.code(), "CAPABILITY_NOT_USABLE");
    assert!(
        h.numbers
            .require_capability(r.id, C::VoiceInbound, false)
            .await
            .unwrap()
            .supported
    );
    // An unknown resource is NOT_FOUND, never another number's capabilities.
    assert!(matches!(
        h.numbers
            .require_capability(Uuid::new_v4(), C::VoiceInbound, false)
            .await
            .unwrap_err(),
        DomainError::NotFound(_)
    ));
    // The internal API serves the same resolution (contract-conformant).
    let reply = h
        .internal(
            &h.peers.calling,
            Method::GET,
            &format!("/internal/v1/number-resources/{}/capabilities", r.id),
        )
        .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    conforms(RESOLUTION, &reply.body);
    assert_eq!(reply.body["notices"][0], EXTERNAL_VERIFICATION_NOTICE);
    let reply = h
        .internal(
            &h.peers.privacy_relay,
            Method::GET,
            &format!("/internal/v1/number-resources/{}/capabilities", r.id),
        )
        .await;
    assert_eq!(reply.status, 401);
}

#[tokio::test]
async fn external_verification_is_never_promised() {
    let Some(h) = Harness::new().await else {
        return;
    };
    synthetic_us(&h).await;
    let mut claim = type_entry(
        "US",
        InventoryClass::Synthetic,
        NumberType::Mobile,
        &[C::SmsInbound],
        &[],
    );
    claim.limitations = json!({ "note": "Accepted by WhatsApp and banks" });
    assert_eq!(
        h.numbers
            .publish_number_type(&ctx(), operator(), "TEST", claim)
            .await
            .unwrap_err()
            .code(),
        "LIMITATION_NOT_ALLOWED"
    );
    let r = classified(&h, InventorySource::SyntheticTest, NumberType::Mobile).await;
    for limitations in [
        json!({ "externalVerification": { "compatibility": "VARIES", "guaranteed": true } }),
        json!({ "otpGuaranteed": true }),
        json!({ "note": "OTP delivery guaranteed" }),
    ] {
        let err = h
            .numbers
            .record_capability(
                &ctx(),
                r.id,
                NewCapability {
                    limitations: limitations.clone(),
                    ..synthetic(C::SmsInbound)
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "LIMITATION_NOT_ALLOWED", "{limitations}");
    }
    record(
        &h,
        r.id,
        NewCapability {
            limitations: json!({ "throughputPerSecond": 1, "registrationRequired": true }),
            ..synthetic(C::SmsInbound)
        },
    )
    .await;
    let res = h.numbers.resolve_capabilities(r.id).await.unwrap();
    let sms = res.get(C::SmsInbound);
    assert_eq!(sms.limitations["notice"], EXTERNAL_VERIFICATION_NOTICE);
    assert_eq!(
        sms.limitations["externalVerification"],
        json!({ "compatibility": "VARIES", "guaranteed": false })
    );
    assert_eq!(sms.limitations["throughputPerSecond"], 1);
    assert!(!serde_json::to_string(&res)
        .unwrap()
        .contains("\"guaranteed\":true"));
}

// ---------------------------------------------------------------------------------------
// HTTP: tenant isolation, administration authorization, catalog views
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn catalog_and_capabilities_over_http_are_scoped_and_admin_gated() {
    let Some(h) = Harness::with_platform_access().await else {
        return;
    };
    let pa = h.pa.clone().unwrap();
    let alice = pa
        .customer(
            "SANDBOX",
            &["numbers.read", "numbers.search"],
            &["numbers.read", "numbers.search"],
        )
        .await;
    let bob = pa
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
    // Administration: publish the synthetic catalog over HTTP as the operator console.
    let country = json!({
        "countryCode": "US", "inventoryClass": "SYNTHETIC", "supportStatus": "SUPPORTED",
        "provenance": { "source": "SYNTHETIC_TEST", "verificationStatus": "SYNTHETIC" }, "reasonCode": "SYNTHETIC_CATALOG",
    });
    let reply = h
        .customer_json(
            &ops.secret,
            Method::POST,
            "/v1/admin/catalog/countries",
            Some(country.clone()),
            None,
        )
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::BAD_REQUEST, "OPERATOR_REQUIRED")
    );
    let reply = h
        .customer_json(
            &ops.secret,
            Method::POST,
            "/v1/admin/catalog/countries",
            Some(country.clone()),
            Some("ops.alice"),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    conforms(
        "numbering/common/v1.schema.json#/$defs/CountryCatalogEntry",
        &reply.body,
    );
    let mobile = json!({
        "countryCode": "US", "inventoryClass": "SYNTHETIC", "numberType": "MOBILE", "localityClass": "NATIONAL",
        "supportedCapabilities": ["VOICE_INBOUND", "SMS_INBOUND", "MOBILE_NUMBER_CLASS"],
        "provenance": { "source": "SYNTHETIC_TEST", "verificationStatus": "SYNTHETIC" }, "reasonCode": "SYNTHETIC_CATALOG",
    });
    let reply = h
        .customer_json(
            &ops.secret,
            Method::POST,
            "/v1/admin/catalog/number-types",
            Some(mobile.clone()),
            Some("ops.alice"),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    conforms(
        "numbering/common/v1.schema.json#/$defs/NumberTypeCatalogEntry",
        &reply.body,
    );
    let mut geographic = mobile.clone();
    geographic["numberType"] = json!("GEOGRAPHIC");
    geographic["localityClass"] = json!("LOCAL");
    geographic["supportedCapabilities"] = json!(["VOICE_INBOUND", "GEOGRAPHIC_LOCAL"]);
    let reply = h
        .customer_json(
            &ops.secret,
            Method::POST,
            "/v1/admin/catalog/number-types",
            Some(geographic),
            Some("ops.alice"),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.body);
    // Non-admin credentials (numbers.read only, external tenants) are refused.
    let reply = h
        .customer_json(
            &alice.secret,
            Method::POST,
            "/v1/admin/catalog/number-types",
            Some(mobile),
            Some("ops.alice"),
        )
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::FORBIDDEN, "INSUFFICIENT_SCOPE")
    );
    // Platform Access refuses the privileged scope to an external tenant altogether.
    let (status, _) = {
        let slug = format!("x{}", &Uuid::new_v4().simple().to_string()[..10]);
        let (_, t) = pa
            .admin(
                reqwest::Method::POST,
                "/internal/v1/admin/tenants",
                Some(json!({ "slug": slug, "displayName": "x", "classification": "EXTERNAL" })),
            )
            .await;
        let (_, app) = pa
            .admin(
                reqwest::Method::POST,
                &format!(
                    "/internal/v1/admin/tenants/{}/applications",
                    t["id"].as_str().unwrap()
                ),
                Some(json!({ "slug": "app", "displayName": "x" })),
            )
            .await;
        pa.admin(
            reqwest::Method::POST,
            &format!(
                "/internal/v1/admin/applications/{}/scopes",
                app["id"].as_str().unwrap()
            ),
            Some(json!({ "scope": "numbers.admin", "privileged": true })),
        )
        .await
    };
    assert!(
        status >= 400,
        "an external tenant never holds numbers.admin"
    );
    // Catalog view by environment class: sandbox sees the synthetic catalog.
    let reply = h
        .customer_json(
            &alice.secret,
            Method::GET,
            "/v1/numbers/catalog/countries/US",
            None,
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    conforms(
        "numbering/common/v1.schema.json#/$defs/CatalogCountryView",
        &reply.body,
    );
    assert_eq!(reply.body["country"]["inventoryClass"], "SYNTHETIC");
    assert_eq!(reply.body["notice"], EXTERNAL_VERIFICATION_NOTICE);
    // Production sees the real catalog only (none published: not found, never synthetic).
    let reply = h
        .customer_json(
            &ops.secret,
            Method::GET,
            "/v1/numbers/catalog/countries/US",
            None,
            None,
        )
        .await;
    assert_eq!(reply.status, 404);
    // Capabilities of Alice's own assignment; Bob gets NOT_FOUND.
    let alice_scope = numbering::numbers::OwnerScope {
        tenant_id: alice.tenant_id,
        application_id: alice.application_id,
        environment_id: alice.environment_id,
        environment_kind: EnvironmentKind::Sandbox,
        service_account_id: alice.service_account_id,
    };
    let j = h.active(&alice_scope, OwnershipClass::Personal).await;
    classify_geographic(&h, j.resource_id).await;
    let path = format!("/v1/numbers/assignments/{}/capabilities", j.assignment.id);
    let reply = h
        .customer_json(&alice.secret, Method::GET, &path, None, None)
        .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    conforms(RESOLUTION, &reply.body);
    let reply = h
        .customer_json(&bob.secret, Method::GET, &path, None, None)
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::NOT_FOUND, "NOT_FOUND")
    );
    // Revocation over HTTP (admin) changes truth immediately.
    let reply = h
        .customer_json(
            &ops.secret,
            Method::POST,
            &format!(
                "/v1/admin/number-resources/{}/capabilities/VOICE_INBOUND/revoke",
                j.resource_id
            ),
            Some(json!({ "reasonCode": "ABUSE" })),
            Some("ops.alice"),
        )
        .await;
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    conforms(
        "numbering/common/v1.schema.json#/$defs/CapabilityRevocation",
        &reply.body,
    );
    let reply = h
        .customer_json(&alice.secret, Method::GET, &path, None, None)
        .await;
    let voice = reply.body["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["capability"] == "VOICE_INBOUND")
        .unwrap()
        .clone();
    assert_eq!(voice["reason"], "CAPABILITY_REVOKED");
    // Malformed admin bodies are normalized errors.
    let reply = h
        .customer_json(
            &ops.secret,
            Method::POST,
            "/v1/admin/catalog/countries",
            Some(json!({ "countryCode": "US", "bogus": 1 })),
            Some("ops.alice"),
        )
        .await;
    assert_eq!(
        (reply.status, reply.code()),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR")
    );
    conforms("numbering/common/v1.schema.json#/$defs/Error", &reply.body);
}

// ---------------------------------------------------------------------------------------
// Migration
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_catalog_migration_rolls_back_and_reapplies_safely() {
    use numbering_migration::{Migrator, MigratorTrait};
    let Some(db) = common::TestDb::empty().await else {
        return;
    };
    Migrator::up(&db.conn, Some(2)).await.unwrap();
    // A–D data written before the upgrade (raw: the current code expects the new schema).
    let rid = Uuid::new_v4();
    exec(
        &db.conn,
        &format!(
            "INSERT INTO number_resources (id, e164, country_code, calling_code, number_type, classification_source, \
               inventory_source, state, revision, state_changed_at, discovered_at, risk_tier, cost_evidence, created_at, updated_at) \
             VALUES ('{rid}', '+15550109999', 'US', '1', 'MOBILE', 'SYNTHETIC', 'SYNTHETIC_TEST', 'DISCOVERED', 1, now(), now(), \
               'UNASSESSED', 'UNKNOWN', now(), now())"
        ),
    )
    .await
    .unwrap();
    Migrator::up(&db.conn, None).await.unwrap();
    // Existing resources survive with capability revision 0 and the domain works on them.
    let db = std::sync::Arc::new(db);
    let h = Harness::build(
        db.clone(),
        None,
        numbering::domain::ManualClock::new(common::t0()),
    );
    synthetic_us(&h).await;
    let r = h.numbers.resource(rid).await.unwrap();
    assert_eq!(
        (r.capability_revision, r.number_type.as_str()),
        (0, "MOBILE")
    );
    record(&h, r.id, synthetic(C::VoiceInbound)).await;
    assert!(
        h.numbers
            .resolve_capabilities(r.id)
            .await
            .unwrap()
            .get(C::VoiceInbound)
            .supported
    );
    // Rollback refuses to discard EXPIRED / REVOKED evidence…
    h.numbers
        .revoke_capability(
            &ctx(),
            Actor::operator("ops"),
            r.id,
            C::VoiceInbound,
            "ABUSE",
        )
        .await
        .unwrap();
    assert!(Migrator::down(&db.conn, Some(1)).await.is_err());
    // …and otherwise restores the previous schema (fresh database), then re-applies.
    let Some(fresh) = common::TestDb::empty().await else {
        return;
    };
    Migrator::up(&fresh.conn, None).await.unwrap();
    Migrator::down(&fresh.conn, Some(1)).await.unwrap();
    assert_eq!(
        common::scalar(&fresh.conn, "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_name LIKE 'number_%catalog%'").await,
        0
    );
    Migrator::up(&fresh.conn, None).await.unwrap();
    assert_eq!(
        common::scalar(&fresh.conn, "SELECT count(*) FROM information_schema.tables WHERE table_name LIKE 'number_%catalog%'").await,
        3
    );
}

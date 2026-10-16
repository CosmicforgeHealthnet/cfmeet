//! Routing Engine persistence (Part E §37–§38, §23–§24) against PostgreSQL: route plans,
//! idempotent re-selection, the typed routing policy store, and application calls routed
//! through the engine (APP_RTC).

mod common;

use calling::{
    calls::{
        attributes::SafeAttributes,
        kinds::{RouteClass, RouteState},
        routes::{NewRoute, PolicyVersion},
        state::CallState,
    },
    routing::{
        engine::{
            select, ContactFacts, ContactVerification, DestinationFacts, DuplicateRisk,
            EndpointClass, Evidence, RouteDirection, RouteInputs, TelecomFlags,
        },
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        store, RoutingClass,
    },
};
use chrono::Utc;
use common::{app::AppWorld, calls::CallWorld};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

fn pstn_policy() -> RoutingPolicy {
    RoutingPolicy {
        allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::AppToPstn],
        app_first: true,
        customer_may_select_public_route: false,
        public_calling_enabled: true,
        callback_relay_enabled: false,
        require_verified_contacts: true,
        destinations: DestinationPolicy {
            allow: vec!["NG".into()],
            deny: vec![],
            default: CountryDecision::Deny,
        },
        max_failover_attempts: 1,
        max_estimated_leg_cost_minor: None,
        duplicate_risk: calling::routing::policy::DuplicateRiskPolicy::BlockUntilReconciled,
        compliance_required_countries: vec![],
    }
}

fn pstn_inputs(contact: Uuid) -> RouteInputs {
    RouteInputs {
        direction: RouteDirection::Outbound,
        source: EndpointClass::Application,
        source_contact: None,
        destinations: vec![DestinationFacts {
            class: EndpointClass::PublicTelephone,
            app_reachable: Evidence::Unknown,
            contact: Some(ContactFacts {
                contact_reference: contact,
                verification: Evidence::Known(ContactVerification::Verified),
                country: Evidence::Known("NG".into()),
            }),
        }],
        requested: None,
        authorized: true,
        permitted_classes: vec![RoutingClass::AppToPstn],
        relay_session_id: Some(Uuid::new_v4()),
        context_id: Some(Uuid::new_v4()),
        live_telecom_allowed: true,
        flags: TelecomFlags {
            public_outbound: true,
            public_inbound: false,
            callback_relay: false,
        },
        routes: Evidence::Unknown,
        spend_headroom_minor: Evidence::Known(1_000),
        operational_incident: Evidence::Known(false),
        duplicate_risk: DuplicateRisk::None,
        compliance: calling::routing::compliance::ComplianceFacts::not_required(),
    }
}

async fn count(w: &CallWorld, sql: &str) -> i64 {
    w.count(sql).await
}

#[tokio::test]
async fn identical_route_selection_never_creates_a_second_active_route() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Routing).await;
    let inputs = pstn_inputs(Uuid::new_v4());
    let plan = select(&pstn_policy(), &inputs, Utc::now())
        .plan()
        .cloned()
        .unwrap();
    let new = |plan| NewRoute {
        route_class: RouteClass::Mixed,
        policy_version: PolicyVersion::parse("ROUTING_POLICY_V1.r1").unwrap(),
        decision_snapshot: SafeAttributes::new(),
        plan: Some(plan),
    };
    let first = w
        .calls()
        .record_route(&w.cmd, call.id, new(plan.clone()))
        .await
        .unwrap();
    assert_eq!(first.routing_class, Some(RoutingClass::AppToPstn));
    // Recomputing against unchanged inputs (later clock) yields the same plan identity.
    let again = select(
        &pstn_policy(),
        &inputs,
        Utc::now() + chrono::Duration::seconds(5),
    )
    .plan()
    .cloned()
    .unwrap();
    let second = w
        .calls()
        .record_route(&w.cmd, call.id, new(again))
        .await
        .unwrap();
    assert_eq!(first.id, second.id);
    let sql = format!(
        "SELECT count(*) FROM call_routes WHERE call_id = '{}' AND state IN ('SELECTED','ACTIVE')",
        call.id.as_uuid()
    );
    assert_eq!(count(&w, &sql).await, 1);
    // Persisted: class, bounded plan, fingerprint, attempt budget, version, validity.
    let row =
        w.h.conn()
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "SELECT routing_class, plan_fingerprint, max_attempts::int, route_version, \
                 plan_expires_at IS NOT NULL, route_plan::text FROM call_routes WHERE id = '{}'",
                    first.id.as_uuid()
                ),
            ))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(row.try_get_by_index::<String>(0).unwrap(), "APP_TO_PSTN");
    assert_eq!(
        row.try_get_by_index::<String>(1).unwrap(),
        plan.fingerprint()
    );
    assert_eq!(row.try_get_by_index::<i32>(2).unwrap(), 2);
    assert_eq!(row.try_get_by_index::<i64>(3).unwrap(), 1);
    assert!(row.try_get_by_index::<bool>(4).unwrap());
    let text = row.try_get_by_index::<String>(5).unwrap();
    assert!(!text.contains('+') && !text.to_lowercase().contains("provider"));
    // A state change bumps the version; once superseded, a re-selection is a new route.
    w.calls()
        .update_route_state(&w.cmd, call.id, first.id, RouteState::Superseded)
        .await
        .unwrap();
    let third = w
        .calls()
        .record_route(&w.cmd, call.id, new(plan.clone()))
        .await
        .unwrap();
    assert_ne!(third.id, first.id);
    // A plan whose class does not match the network class is refused.
    let mismatch = NewRoute {
        route_class: RouteClass::Application,
        ..new(plan)
    };
    assert!(w
        .calls()
        .record_route(&w.cmd, call.id, mismatch)
        .await
        .is_err());
    // The plan is immutable once recorded.
    let err = common::calls::sql(
        &w.h,
        &format!(
            "UPDATE call_routes SET routing_class = 'PSTN_RELAY' WHERE id = '{}'",
            third.id.as_uuid()
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("immutable"), "{err}");
}

#[tokio::test]
async fn routing_policies_are_typed_versioned_and_fail_closed() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let env = w.scope().environment_id().as_uuid();
    let tenant = w.scope().tenant_id().as_uuid();
    let app = w.scope().application_id().as_uuid();
    let default = store::load_policy(w.h.conn(), env).await.unwrap();
    assert_eq!(default.revision, 0);
    assert_eq!(default.policy, RoutingPolicy::default());
    assert_eq!(default.version_label(), "ROUTING_POLICY_V1.default");
    let r1 = store::store_policy(w.h.conn(), tenant, app, env, &pstn_policy(), Utc::now())
        .await
        .unwrap();
    let r2 = store::store_policy(w.h.conn(), tenant, app, env, &pstn_policy(), Utc::now())
        .await
        .unwrap();
    assert_eq!((r1, r2), (1, 2));
    let loaded = store::load_policy(w.h.conn(), env).await.unwrap();
    assert_eq!(loaded.policy, pstn_policy());
    assert_eq!(loaded.version_label(), "ROUTING_POLICY_V1.r2");
    // Another application cannot overwrite this environment's policy.
    assert!(store::store_policy(
        w.h.conn(),
        tenant,
        Uuid::new_v4(),
        env,
        &pstn_policy(),
        Utc::now()
    )
    .await
    .is_err());
    // Invalid documents are refused; a corrupted stored document fails closed.
    let mut bad = pstn_policy();
    bad.max_failover_attempts = 9;
    assert!(
        store::store_policy(w.h.conn(), tenant, app, env, &bad, Utc::now())
            .await
            .is_err()
    );
    common::calls::sql(
        &w.h,
        &format!(
            "UPDATE call_routing_policies SET policy = '{{\"carrier\": \"x\"}}'::jsonb WHERE environment_id = '{env}'"
        ),
    )
    .await
    .unwrap();
    let corrupted = store::load_policy(w.h.conn(), env).await.unwrap();
    assert_eq!(corrupted.policy, RoutingPolicy::default());
    assert_eq!(corrupted.revision, 0);
}

#[tokio::test]
async fn application_calls_route_app_rtc_through_the_engine() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let alice = w.principal("alice").await;
    let bob = w.principal("bob").await;
    let a = w.device(&alice, "a").await;
    let _b = w.device(&bob, "b").await;
    let created = w.create_call("VOICE", &alice, &a, &[&bob], "route-1").await;
    let call: Uuid = created.json["call"]["callId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let row = w
        .h()
        .conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT routing_class, route_class, plan_fingerprint IS NOT NULL, \
                 route_plan->>'policyVersion' FROM call_routes WHERE call_id = '{call}'"
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get_by_index::<String>(0).unwrap(), "APP_RTC");
    assert_eq!(row.try_get_by_index::<String>(1).unwrap(), "APPLICATION");
    assert!(row.try_get_by_index::<bool>(2).unwrap());
    assert_eq!(
        row.try_get_by_index::<String>(3).unwrap(),
        "ROUTING_POLICY_V1.default"
    );
    // A principal without an application endpoint is never moved to a paid route.
    let carol = w.principal("carol").await;
    let failed = w
        .create_call("VOICE", &alice, &a, &[&carol], "route-2")
        .await;
    assert_eq!(failed.json["call"]["state"], "FAILED");
    assert_eq!(failed.json["call"]["failureCategory"], "ROUTING");
}

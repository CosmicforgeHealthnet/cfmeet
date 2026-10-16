//! Paid-action safety on Calling's side (Part E §153–§163): durable, serialized spend
//! reservations (no check-then-spend race), the Part B application / credential caps
//! made real, and velocity per destination, principal, service account and context.

mod common;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use calling::{
    domain::EnvironmentKind,
    models::calls,
    routing::{
        policy::{CountryDecision, DestinationPolicy, RoutingPolicy},
        RoutingClass,
    },
    telephony::spend::{self, SpendDenial, SpendPolicy, SpendScope},
};
use common::{
    app::{AppWorld, Device},
    privacy::FakePrivacyRelay,
    scalar_i64,
    telephony::FakeTelephony,
};
use sea_orm::{ConnectionTrait, EntityTrait, TransactionTrait};
use serde_json::json;
use uuid::Uuid;

struct World {
    w: AppWorld,
    privacy: Arc<FakePrivacyRelay>,
    caller: Device,
    session: Uuid,
}

fn policy() -> SpendPolicy {
    SpendPolicy {
        currency: "NGN".into(),
        leg_allowance_minor: 1_000,
        max_leg_seconds: 600,
        daily_limit_minor: 1_000_000,
        monthly_limit_minor: 10_000_000,
        max_concurrent_legs: 100,
        max_originations_per_minute: 1_000,
        max_per_party_per_hour: 100,
        max_per_principal_per_hour: None,
        max_per_service_account_per_hour: None,
        max_per_context_per_hour: None,
        enabled: true,
    }
}

async fn world() -> Option<World> {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(FakeTelephony::healthy());
    let w = AppWorld::with_public_network(privacy.clone(), telephony, None).await?;
    privacy.allow(w.w.initiator.id().as_uuid());
    let scope = w.w.scope();
    let env = w.w.consumer.env(EnvironmentKind::Production).id.as_uuid();
    calling::routing::store::store_policy(
        w.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        env,
        &RoutingPolicy {
            allowed_route_classes: vec![RoutingClass::AppRtc, RoutingClass::AppToPstn],
            public_calling_enabled: true,
            destinations: DestinationPolicy {
                allow: vec!["NG".into()],
                deny: vec![],
                default: CountryDecision::Deny,
            },
            ..RoutingPolicy::default()
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let caller = w.device(&w.w.initiator, "caller-main").await;
    let x = World {
        w,
        privacy,
        caller,
        session: Uuid::new_v4(),
    };
    x.set(policy()).await;
    Some(x)
}

impl World {
    fn scope(&self) -> SpendScope {
        let scope = self.w.w.scope();
        SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: scope.environment_id().as_uuid(),
        }
    }

    async fn set(&self, p: SpendPolicy) {
        spend::store_policy(self.w.h().conn(), self.scope(), &p, chrono::Utc::now())
            .await
            .unwrap();
    }

    /// A public call (its own reservation included) — returns the Call row.
    async fn call(&self, key: &str) -> calls::Model {
        let contact = self.privacy.add_contact("NG");
        let r = self
            .w
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "initiatorPrincipalId": self.w.w.initiator.id(),
                    "initiatorEndpointId": self.caller.endpoint,
                    "targetContactReference": contact,
                    "privacyRelaySessionId": self.session,
                }),
                &[("idempotency-key", key)],
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let id: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        calls::Entity::find_by_id(id)
            .one(self.w.h().conn())
            .await
            .unwrap()
            .unwrap()
    }

    /// `n` more public legs on `call` (to reserve for).
    async fn legs(&self, call: &calls::Model, n: usize) -> Vec<Uuid> {
        let mut out = Vec::new();
        for _ in 0..n {
            let id = Uuid::new_v4();
            self.w
                .h()
                .conn()
                .execute(sea_orm::Statement::from_sql_and_values(
                    sea_orm::DbBackend::Postgres,
                    "INSERT INTO call_legs (id, call_id, tenant_id, application_id, environment_id, direction, \
                       leg_kind, state, created_at, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, 'OUTBOUND', 'PUBLIC_NETWORK', 'CREATED', now(), now())",
                    vec![
                        id.into(),
                        call.id.into(),
                        call.tenant_id.into(),
                        call.application_id.into(),
                        call.environment_id.into(),
                    ],
                ))
                .await
                .unwrap();
            out.push(id);
        }
        out
    }

    async fn reserve(&self, call: &calls::Model, leg: Uuid) -> Result<(), SpendDenial> {
        let txn = self.w.h().conn().begin().await.unwrap();
        let r = spend::reserve_in(&txn, call, leg, Some(Uuid::new_v4()), chrono::Utc::now())
            .await
            .unwrap();
        txn.commit().await.unwrap();
        r.map(|_| ())
    }
}

#[tokio::test]
async fn concurrent_reservations_never_exceed_the_limit() {
    let Some(x) = world().await else { return };
    let call = x.call("race").await; // one open reservation already
    x.set(SpendPolicy {
        max_concurrent_legs: 4,
        ..policy()
    })
    .await;
    let legs = x.legs(&call, 12).await;
    // Every reservation holds its transaction open for a while before committing: any
    // check-then-spend gap would let all of them see the same "3 left" and overshoot.
    let tasks = legs.into_iter().map(|leg| {
        let conn = x.w.h().conn().clone();
        let call = call.clone();
        tokio::spawn(async move {
            let txn = conn.begin().await.unwrap();
            let r = spend::reserve_in(&txn, &call, leg, Some(Uuid::new_v4()), chrono::Utc::now())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(60)).await;
            txn.commit().await.unwrap();
            r
        })
    });
    let handles: Vec<_> = tasks.collect();
    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }
    let granted = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(granted, 3, "{results:?}");
    assert!(results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .all(|d| *d == SpendDenial::Concurrency));
    // Bounded metrics: the blocks are counted by reason; no identifier is a label.
    assert!(
        calling::metrics::counter(
            "calling_telecom_spend_blocks_total",
            &[("reason", "CONCURRENT_PUBLIC_LEG_LIMIT")]
        ) >= 9
    );
    let text = calling::metrics::render();
    assert!(
        text.contains("calling_public_originations_total{route_class=\"APP_TO_PSTN\"}"),
        "{text}"
    );
    for id in [call.id, call.initiator_principal_id, call.environment_id] {
        assert!(!text.contains(&id.to_string()), "an id leaked into metrics");
    }
    assert_eq!(
        scalar_i64(
            x.w.h().conn(),
            &format!(
                "SELECT count(*) FROM call_spend_reservations WHERE environment_id = '{}' AND status = 'RESERVED'",
                call.environment_id
            )
        )
        .await,
        4
    );
}

#[tokio::test]
async fn part_b_application_and_credential_caps_are_enforced() {
    let Some(x) = world().await else { return };
    let call = x.call("caps").await; // 1_000 used
    let conn = x.w.h().conn();
    // Application cap (all environments of the application): 3_000 a day.
    conn.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "INSERT INTO platform_application_limits (application_id, tenant_id, telecom_spend_daily_limit_minor) \
         VALUES ($1, $2, 3000) ON CONFLICT (application_id) \
         DO UPDATE SET telecom_spend_daily_limit_minor = 3000",
        vec![call.application_id.into(), call.tenant_id.into()],
    ))
    .await
    .unwrap();
    let legs = x.legs(&call, 4).await;
    assert_eq!(x.reserve(&call, legs[0]).await, Ok(()));
    assert_eq!(x.reserve(&call, legs[1]).await, Ok(()));
    assert_eq!(
        x.reserve(&call, legs[2]).await,
        Err(SpendDenial::ApplicationDailyLimit)
    );
    // Credential cap (the API credential that created the Call): monthly 2_000.
    conn.execute_unprepared(&format!(
        "UPDATE platform_application_limits SET telecom_spend_daily_limit_minor = NULL WHERE application_id = '{}'",
        call.application_id
    ))
    .await
    .unwrap();
    conn.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "INSERT INTO platform_credential_limits (credential_id, tenant_id, application_id, telecom_spend_monthly_limit_minor) \
         VALUES ($1, $2, $3, 2000) ON CONFLICT (credential_id) \
         DO UPDATE SET telecom_spend_monthly_limit_minor = 2000",
        vec![
            call.created_by_credential_id.into(),
            call.tenant_id.into(),
            call.application_id.into(),
        ],
    ))
    .await
    .unwrap();
    assert_eq!(
        x.reserve(&call, legs[3]).await,
        Err(SpendDenial::CredentialMonthlyLimit)
    );
    // The refusal reaches the API as TELECOM_SPEND_LIMIT_EXCEEDED, nothing is dialed.
    let contact = x.privacy.add_contact("NG");
    let r =
        x.w.post_with(
            "/v1/calls",
            json!({
                "kind": "VOICE",
                "initiatorPrincipalId": x.w.w.initiator.id(),
                "initiatorEndpointId": x.caller.endpoint,
                "targetContactReference": contact,
                "privacyRelaySessionId": x.session,
            }),
            &[("idempotency-key", "caps-api")],
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (
            StatusCode::TOO_MANY_REQUESTS,
            Some("TELECOM_SPEND_LIMIT_EXCEEDED")
        ),
        "{}",
        r.text
    );
}

#[tokio::test]
async fn velocity_per_principal_service_account_and_destination() {
    let Some(x) = world().await else { return };
    let call = x.call("vel").await; // 1 reservation by this principal / service account
    let legs = x.legs(&call, 6).await;
    // Per principal.
    x.set(SpendPolicy {
        max_per_principal_per_hour: Some(2),
        ..policy()
    })
    .await;
    assert_eq!(x.reserve(&call, legs[0]).await, Ok(()));
    assert_eq!(
        x.reserve(&call, legs[1]).await,
        Err(SpendDenial::PrincipalVelocity)
    );
    // Per service account.
    x.set(SpendPolicy {
        max_per_service_account_per_hour: Some(2),
        ..policy()
    })
    .await;
    assert_eq!(
        x.reserve(&call, legs[1]).await,
        Err(SpendDenial::ServiceAccountVelocity)
    );
    // Per destination (same protected contact).
    x.set(SpendPolicy {
        max_per_party_per_hour: 1,
        ..policy()
    })
    .await;
    let contact = Uuid::new_v4();
    let txn = x.w.h().conn().begin().await.unwrap();
    assert!(
        spend::reserve_in(&txn, &call, legs[2], Some(contact), chrono::Utc::now())
            .await
            .unwrap()
            .is_ok()
    );
    assert_eq!(
        spend::reserve_in(&txn, &call, legs[3], Some(contact), chrono::Utc::now())
            .await
            .unwrap(),
        Err(SpendDenial::ContactVelocity)
    );
    txn.commit().await.unwrap();
    // Disabled policy: nothing.
    x.set(SpendPolicy {
        enabled: false,
        ..policy()
    })
    .await;
    assert_eq!(x.reserve(&call, legs[4]).await, Err(SpendDenial::Disabled));
}

#[tokio::test]
async fn estimated_cost_is_reserved_and_actual_cost_settled_without_pretending() {
    let Some(x) = world().await else { return };
    let call = x.call("settle").await;
    let legs = x.legs(&call, 2).await;
    for leg in &legs {
        assert_eq!(x.reserve(&call, *leg).await, Ok(()));
    }
    let conn = x.w.h().conn();
    let txn = conn.begin().await.unwrap();
    // Known provider cost; unknown cost keeps the conservative estimate, flagged.
    assert!(
        spend::settle_in(&txn, legs[0], 240, true, 31, chrono::Utc::now())
            .await
            .unwrap()
    );
    assert!(
        spend::settle_in(&txn, legs[1], 1_000, false, 60, chrono::Utc::now())
            .await
            .unwrap()
    );
    // Settling twice is a no-op.
    assert!(
        !spend::settle_in(&txn, legs[0], 999, true, 99, chrono::Utc::now())
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();
    assert_eq!(
        scalar_i64(
            conn,
            &format!(
                "SELECT settled_minor FROM call_spend_reservations WHERE call_leg_id = '{}' AND cost_known",
                legs[0]
            )
        )
        .await,
        240
    );
    assert_eq!(
        scalar_i64(
            conn,
            &format!(
                "SELECT count(*) FROM call_spend_reservations WHERE call_leg_id = '{}' AND NOT cost_known",
                legs[1]
            )
        )
        .await,
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spend_reservation_contention_is_serialized_and_measured() {
    let Some(x) = world().await else { return };
    let call = x.call("contention").await;
    let legs = x.legs(&call, 60).await;
    let started = std::time::Instant::now();
    let tasks: Vec<_> = legs
        .into_iter()
        .map(|leg| {
            let conn = x.w.h().conn().clone();
            let call = call.clone();
            tokio::spawn(async move {
                let txn = conn.begin().await.unwrap();
                let r =
                    spend::reserve_in(&txn, &call, leg, Some(Uuid::new_v4()), chrono::Utc::now())
                        .await
                        .unwrap();
                txn.commit().await.unwrap();
                r.is_ok()
            })
        })
        .collect();
    let mut granted = 0;
    for t in tasks {
        if t.await.unwrap() {
            granted += 1;
        }
    }
    let elapsed = started.elapsed();
    assert_eq!(granted, 60);
    eprintln!(
        "LOAD: 60 contending spend reservations (one environment lock) in {elapsed:?} ({:.0}/s)",
        60.0 / elapsed.as_secs_f64()
    );
    assert_eq!(
        scalar_i64(
            x.w.h().conn(),
            &format!(
                "SELECT count(*) FROM call_spend_reservations WHERE call_id = '{}'",
                call.id
            )
        )
        .await,
        61
    );
}

/// Reserves one leg per (call model, leg) concurrently; every reservation holds its
/// transaction open before committing, so a check-then-spend gap would overshoot.
async fn race(
    x: &World,
    models: &[calls::Model],
    per_model: usize,
) -> Vec<Result<(), SpendDenial>> {
    let mut handles = Vec::new();
    for model in models {
        for leg in x.legs(model, per_model).await {
            let conn = x.w.h().conn().clone();
            let call = model.clone();
            handles.push(tokio::spawn(async move {
                let txn = conn.begin().await.unwrap();
                let r =
                    spend::reserve_in(&txn, &call, leg, Some(Uuid::new_v4()), chrono::Utc::now())
                        .await
                        .unwrap();
                tokio::time::sleep(Duration::from_millis(60)).await;
                txn.commit().await.unwrap();
                r.map(|_| ())
            }));
        }
    }
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

/// Another service account (and credential) of the world's application environment.
async fn other_service_account(x: &World, name: &str) -> (Uuid, Uuid) {
    let h = x.w.h();
    let env = x.w.w.consumer.env(EnvironmentKind::Production).id;
    let sa = h
        .platform
        .create_service_account(
            env,
            calling::platform::service_accounts::NewServiceAccount {
                name: name.into(),
                description: None,
            },
        )
        .await
        .unwrap();
    let c = h
        .issue(sa.id, &[calling::platform::scopes::names::CALLS_WRITE])
        .await;
    (sa.id.as_uuid(), c.credential.id.as_uuid())
}

/// Part G §70: a service-account spend ceiling is shared by ALL of its credentials.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_account_spend_ceiling_is_shared_by_its_credentials() {
    let Some(x) = world().await else { return };
    let call = x.call("sa-spend").await; // its first leg already holds one allowance
    let sa = x.w.w.consumer.sa(EnvironmentKind::Production);
    let second =
        x.w.h()
            .issue(sa, &[calling::platform::scopes::names::CALLS_WRITE])
            .await;
    x.w.h()
        .platform
        .set_service_account_limits(
            sa,
            calling::platform::limits::LimitValues {
                telecom_spend_daily_limit_minor: Some(4_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut via_second = call.clone();
    via_second.created_by_credential_id = second.credential.id.as_uuid();
    let results = race(&x, &[call.clone(), via_second], 4).await;
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        3,
        "{results:?}"
    );
    assert!(results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .all(|d| *d == SpendDenial::ServiceAccountDailyLimit));
    assert_eq!(
        scalar_i64(
            x.w.h().conn(),
            &format!(
                "SELECT COALESCE(SUM(amount_minor), 0)::bigint FROM call_spend_reservations \
                 WHERE service_account_id = '{}' AND status = 'RESERVED'",
                sa.as_uuid()
            )
        )
        .await,
        4_000,
        "never above the shared ceiling"
    );
    // Another service account of the same environment keeps its own allowance.
    let (other_sa, other_c) = other_service_account(&x, "other").await;
    let mut other = call.clone();
    other.created_by_service_account_id = other_sa;
    other.created_by_credential_id = other_c;
    let results = race(&x, &[other], 2).await;
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
}

/// Part G §70: a principal spend ceiling holds across every service account and
/// credential acting for that principal; the per-hour velocity guard is separate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn principal_spend_ceiling_holds_across_call_paths() {
    let Some(x) = world().await else { return };
    let call = x.call("p-spend").await; // one allowance already reserved for P
    x.w.h()
        .platform
        .set_principal_limits(
            calling::domain::PrincipalId::from_uuid(call.initiator_principal_id),
            calling::platform::limits::LimitValues {
                telecom_spend_daily_limit_minor: Some(3_000),
                telecom_spend_monthly_limit_minor: Some(50_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let (other_sa, other_c) = other_service_account(&x, "second-path").await;
    let mut via_other = call.clone();
    via_other.created_by_service_account_id = other_sa;
    via_other.created_by_credential_id = other_c;
    let results = race(&x, &[call.clone(), via_other], 3).await;
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        2,
        "{results:?}"
    );
    assert!(results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .all(|d| *d == SpendDenial::PrincipalDailyLimit));
    // Another principal is unaffected (only shared parents could stop it).
    let mut other_principal = call.clone();
    other_principal.initiator_principal_id = x.w.w.recipient.id().as_uuid();
    let results = race(&x, &[other_principal], 2).await;
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
}

/// Part H §77.22 — unknown cost. The strategy is the reservation itself: before any
/// ORIGINATE exists, a positive allowance (`leg_allowance_minor`, > 0 by CHECK) bounded by
/// the daily / monthly caps is reserved for at most `max_leg_seconds`, whatever the
/// route's estimated cost. An UNKNOWN estimated cost therefore never opens an unreserved
/// paid leg, and an UNKNOWN actual cost settles conservatively at the reserved amount,
/// creates no new paid action and never rewrites the immutable usage fact.
#[tokio::test]
async fn an_unknown_cost_never_opens_an_unreserved_or_unbounded_paid_leg() {
    use calling::routing::engine::{CandidateRoute, Evidence, RouteHealth};
    use calling::telephony::events::TelephonyEvent;
    use common::{public::PublicWorld, telephony::leg_event};

    let Some(x) = PublicWorld::new().await else {
        return;
    };
    // Telephony knows the route is healthy but not what a leg costs.
    x.telephony.set_routes(Evidence::Known(vec![CandidateRoute {
        route_ref: "ng-a".into(),
        health: RouteHealth::Healthy,
        capacity_available: Evidence::Known(true),
        estimated_leg_cost_minor: Evidence::Unknown,
        quality_permille: Evidence::Unknown,
    }]));
    // Room for exactly one allowance today.
    x.set_spend(SpendPolicy {
        daily_limit_minor: 5_000,
        ..common::public::spend_policy()
    })
    .await;
    let r = x.call_to(x.contact, "unknown-cost-1").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // The ORIGINATE carries a positive reservation equal to the allowance, within the
    // cap, for at most max_leg_seconds.
    let originates = x.telephony.originates();
    assert_eq!(originates.len(), 1);
    let spend = &originates[0].1["spend"];
    assert_eq!(spend["reservedAmountMinor"], 5_000);
    assert_eq!(spend["maxDurationSeconds"], 3600);
    let leg: Uuid =
        x.w.h()
            .conn()
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                "SELECT id FROM call_legs WHERE call_id = '{call}' AND leg_kind = 'PUBLIC_NETWORK'"
            ),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
    assert_eq!(
        x.count(&format!(
            "SELECT count(*) FROM call_spend_reservations WHERE call_leg_id = '{leg}' \
             AND status = 'RESERVED' AND amount_minor = 5000 AND max_duration_seconds = 3600"
        ))
        .await,
        1
    );
    // An unknown estimate does not bypass the cap: the next leg is refused, nothing new
    // is reserved or commanded.
    let r = x.call_to(x.contact, "unknown-cost-2").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "TELECOM_SPEND_LIMIT_EXCEEDED");
    assert_eq!(x.effects().await, (1, 1, 1));

    // The actual cost turns out UNKNOWN: settled at the reservation, marked unknown.
    let t = x.telephony.telephony_leg_of(leg).unwrap();
    let apply = |value: serde_json::Value| {
        let x = &x;
        async move {
            let event: TelephonyEvent = serde_json::from_value(value).unwrap();
            x.w.h()
                .state
                .app
                .apply_telephony_event(&event)
                .await
                .unwrap()
                .as_str()
        }
    };
    let mut unknown = leg_event("USAGE_FINALIZED", call, leg, t, None, 5);
    unknown["usage"] = json!({ "connectedDurationMs": 90_000, "billableSeconds": 90,
                               "costKnown": false, "destinationCountry": "NG" });
    assert_eq!(apply(unknown).await, "APPLIED");
    let settled_sql = format!(
        "SELECT settled_minor FROM call_spend_reservations WHERE call_leg_id = '{leg}' \
         AND status = 'SETTLED' AND NOT cost_known"
    );
    let settled = || x.count(&settled_sql);
    assert_eq!(settled().await, 5_000);
    let fact = || async {
        common::enterprise::strings(
            x.w.h().conn(),
            &format!(
                "SELECT cost_status || ':' || COALESCE(cost_minor::text, '-') \
                 FROM call_usage_records WHERE leg_id = '{leg}'"
            ),
        )
        .await
    };
    let recorded = fact().await;
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].starts_with("UNKNOWN"), "{recorded:?}");
    // A later (known) usage report rewrites neither the settlement nor the fact, and no
    // new paid action follows.
    let mut late = leg_event("USAGE_FINALIZED", call, leg, t, None, 6);
    late["usage"] = json!({ "connectedDurationMs": 90_000, "billableSeconds": 90,
                            "costMinor": 1, "costKnown": true, "currency": "NGN",
                            "destinationCountry": "NG" });
    apply(late).await;
    assert_eq!(settled().await, 5_000);
    assert_eq!(fact().await, recorded);
    assert_eq!(x.telephony.originates().len(), 1);
    assert_eq!(x.effects().await, (1, 1, 1));
}

//! Part IV C4 / C6 / D10.19: the leased job queue (exclusivity across instances, bounded
//! retry, dead-lettering, restart recovery by retry safety, UNKNOWN_OUTCOME parking and
//! reconciliation) and idempotency keys (scoping, replay, mismatch, rollback, expiry).
mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use chrono::Duration;
use common::{exec, strings, Harness};
use numbering::{
    domain::{current_context, RequestContext, SharedClock},
    infra::{
        idempotency::{self, Begin, IdempotencyScope},
        jobs::{
            self, ClaimedJob, JobHandler, JobOutcome, JobRunner, NewJob, Resolution, RetrySafety,
        },
        RetryPolicy,
    },
};
use sea_orm::TransactionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

/// Counts runs per job id; answers from a script (default: succeeded).
struct Handler {
    kind: &'static str,
    runs: Mutex<HashMap<Uuid, u32>>,
    script: Mutex<Vec<JobOutcome>>,
    seen_correlations: Mutex<Vec<String>>,
}

impl Handler {
    fn new(kind: &'static str, script: Vec<JobOutcome>) -> Arc<Self> {
        Arc::new(Self {
            kind,
            runs: Mutex::new(HashMap::new()),
            script: Mutex::new(script),
            seen_correlations: Mutex::new(Vec::new()),
        })
    }

    fn total_runs(&self) -> u32 {
        self.runs.lock().unwrap().values().sum()
    }
}

#[async_trait::async_trait]
impl JobHandler for Handler {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn run(&self, job: &ClaimedJob) -> JobOutcome {
        *self.runs.lock().unwrap().entry(job.id).or_default() += 1;
        if let Some(ctx) = current_context() {
            self.seen_correlations
                .lock()
                .unwrap()
                .push(ctx.correlation_id.as_str().to_string());
        }
        tokio::task::yield_now().await;
        let mut script = self.script.lock().unwrap();
        if script.is_empty() {
            JobOutcome::Succeeded
        } else {
            script.remove(0)
        }
    }
}

fn runner(h: &Harness, name: &str, handler: Arc<Handler>) -> JobRunner {
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
    .with_handler(handler)
}

fn job(kind: &'static str, safety: RetrySafety, max_attempts: i32, h: &Harness) -> NewJob {
    NewJob {
        kind,
        dedupe_key: Some(format!("k-{}", Uuid::new_v4().simple())),
        payload: json!({ "resource": Uuid::new_v4() }),
        retry_safety: safety,
        max_attempts,
        run_at: h.now(),
    }
}

async fn status(h: &Harness, id: Uuid) -> String {
    strings(
        h.conn(),
        &format!("SELECT status FROM numbering_jobs WHERE id = '{id}'"),
    )
    .await
    .remove(0)
}

#[tokio::test]
async fn jobs_are_scheduled_atomically_and_deduplicated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    let txn = h.conn().begin().await.unwrap();
    jobs::enqueue(
        &txn,
        job("inventory.refresh", RetrySafety::Idempotent, 3, &h),
        &ctx,
        h.now(),
    )
    .await
    .unwrap();
    txn.rollback().await.unwrap();
    assert_eq!(h.scalar("SELECT count(*) FROM numbering_jobs").await, 0);
    let mut j = job("inventory.refresh", RetrySafety::Idempotent, 3, &h);
    j.dedupe_key = Some("refresh-ng-2031-03-01".into());
    let (first, created) = jobs::enqueue(h.conn(), j.clone(), &ctx, h.now())
        .await
        .unwrap();
    assert!(created);
    let (again, created) = jobs::enqueue(h.conn(), j, &ctx, h.now()).await.unwrap();
    assert_eq!((again, created), (first, false));
    assert_eq!(h.scalar("SELECT count(*) FROM numbering_jobs").await, 1);
}

#[tokio::test]
async fn concurrent_runners_never_run_the_same_claim() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    for _ in 0..40 {
        jobs::enqueue(
            h.conn(),
            job("inventory.refresh", RetrySafety::Idempotent, 3, &h),
            &ctx,
            h.now(),
        )
        .await
        .unwrap();
    }
    let handler = Handler::new("inventory.refresh", vec![]);
    let mut tasks = Vec::new();
    for i in 0..4 {
        let r = Arc::new(runner(&h, &format!("worker-{i}"), handler.clone()).with_batch(5));
        tasks.push(tokio::spawn(async move {
            let mut done = 0;
            loop {
                let report = r.run_due().await.unwrap();
                if report.claimed == 0 {
                    return done;
                }
                done += report.succeeded;
            }
        }));
    }
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap();
    }
    assert_eq!(total, 40);
    assert_eq!(handler.total_runs(), 40);
    assert!(
        handler.runs.lock().unwrap().values().all(|n| *n == 1),
        "a job ran twice"
    );
    assert_eq!(
        h.scalar("SELECT count(*) FROM numbering_jobs WHERE status = 'SUCCEEDED'")
            .await,
        40
    );
    // Work continues the scheduling operation's correlation.
    assert!(handler
        .seen_correlations
        .lock()
        .unwrap()
        .iter()
        .all(|c| c == ctx.correlation_id.as_str()));
}

#[tokio::test]
async fn retries_back_off_until_exhausted_and_permanent_failures_are_final() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    let retry = || JobOutcome::Retry {
        code: "PROVIDER_UNAVAILABLE",
        message: "503 from inventory".into(),
    };
    let handler = Handler::new("inventory.refresh", vec![retry(), retry(), retry()]);
    let r = runner(&h, "w", handler.clone());
    let (id, _) = jobs::enqueue(
        h.conn(),
        job("inventory.refresh", RetrySafety::Idempotent, 3, &h),
        &ctx,
        h.now(),
    )
    .await
    .unwrap();
    assert_eq!(r.run_due().await.unwrap().retried, 1);
    assert_eq!(r.run_due().await.unwrap().claimed, 0, "backing off");
    h.clock.advance(Duration::seconds(2));
    assert_eq!(r.run_due().await.unwrap().retried, 1);
    h.clock.advance(Duration::seconds(4));
    assert_eq!(r.run_due().await.unwrap().dead_lettered, 1);
    assert_eq!(status(&h, id).await, "DEAD_LETTER");
    assert_eq!(
        strings(
            h.conn(),
            &format!(
                "SELECT last_error_code || '|' || attempts FROM numbering_jobs WHERE id = '{id}'"
            )
        )
        .await,
        vec!["PROVIDER_UNAVAILABLE|3".to_string()]
    );
    // Permanent failure: FAILED at once. A kind without a handler is never run blindly.
    let handler = Handler::new(
        "inventory.refresh",
        vec![JobOutcome::Failed {
            code: "NOT_OBTAINABLE",
            message: "gone".into(),
        }],
    );
    let r = runner(&h, "w", handler);
    let (id, _) = jobs::enqueue(
        h.conn(),
        job("inventory.refresh", RetrySafety::Idempotent, 5, &h),
        &ctx,
        h.now(),
    )
    .await
    .unwrap();
    assert_eq!(r.run_due().await.unwrap().failed, 1);
    assert_eq!(status(&h, id).await, "FAILED");
    let (orphan, _) = jobs::enqueue(
        h.conn(),
        job("unregistered.kind", RetrySafety::Idempotent, 5, &h),
        &ctx,
        h.now(),
    )
    .await
    .unwrap();
    r.run_due().await.unwrap();
    assert_eq!(
        status(&h, orphan).await,
        "PENDING",
        "only registered kinds are claimed"
    );
    // Final states are final at the database level.
    assert!(exec(
        h.conn(),
        &format!("UPDATE numbering_jobs SET status = 'PENDING' WHERE id = '{id}'")
    )
    .await
    .is_err());
}

#[tokio::test]
async fn a_restarted_worker_retries_idempotent_work_and_parks_provider_affecting_work() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ctx = RequestContext::generate();
    let refresh = Handler::new("inventory.refresh", vec![]);
    let reserve = Handler::new("provider.reserve", vec![]);
    let (idempotent, _) = jobs::enqueue(
        h.conn(),
        job("inventory.refresh", RetrySafety::Idempotent, 3, &h),
        &ctx,
        h.now(),
    )
    .await
    .unwrap();
    let (uncertain, _) = jobs::enqueue(
        h.conn(),
        job(
            "provider.reserve",
            RetrySafety::ReconcileOnUncertainty,
            3,
            &h,
        ),
        &ctx,
        h.now(),
    )
    .await
    .unwrap();
    // Instance A claims both and dies before finishing (no completion recorded).
    let a = runner(&h, "instance-a", refresh.clone()).with_handler(reserve.clone());
    let token = Uuid::new_v4();
    let claimed = a.claim(token).await.unwrap();
    assert_eq!(claimed.len(), 2);
    assert_eq!(status(&h, idempotent).await, "RUNNING");
    // Instance B: nothing to do while A's lease holds.
    let b = runner(&h, "instance-b", refresh.clone()).with_handler(reserve.clone());
    let report = b.run_due().await.unwrap();
    assert_eq!(
        (
            report.claimed,
            report.parked_unknown,
            report.recovered_for_retry
        ),
        (0, 0, 0)
    );
    // After the lease expires B recovers: the idempotent job is retried, the
    // provider-affecting one is parked UNKNOWN_OUTCOME and NOT run again.
    h.clock.advance(Duration::seconds(31));
    let report = b.run_due().await.unwrap();
    assert_eq!(
        (
            report.recovered_for_retry,
            report.parked_unknown,
            report.succeeded
        ),
        (1, 1, 1)
    );
    assert_eq!(status(&h, idempotent).await, "SUCCEEDED");
    assert_eq!(status(&h, uncertain).await, "UNKNOWN_OUTCOME");
    assert_eq!(
        reserve.total_runs(),
        0,
        "an uncertain provider operation is never repeated automatically"
    );
    for _ in 0..3 {
        h.clock.advance(Duration::minutes(5));
        b.run_due().await.unwrap();
    }
    assert_eq!(reserve.total_runs(), 0);
    // A's late completion is refused (the claim was lost).
    let mut report = Default::default();
    a.finish(&claimed[0], token, &JobOutcome::Succeeded, &mut report)
        .await
        .unwrap();
    assert_eq!(report.lost_claims, 1);
    // Readiness exposes the uncertainty; only reconciliation resolves it.
    let ready = h
        .raw(
            axum::http::Request::get("/ready")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(ready.body["jobsUnknownOutcome"], 1);
    assert!(exec(
        h.conn(),
        &format!("UPDATE numbering_jobs SET status = 'PENDING' WHERE id = '{uncertain}'")
    )
    .await
    .is_err());
    assert!(
        jobs::resolve_unknown(h.conn(), uncertain, Resolution::DidNotHappen, h.now())
            .await
            .unwrap()
    );
    assert_eq!(status(&h, uncertain).await, "FAILED");
    assert!(
        !jobs::resolve_unknown(h.conn(), uncertain, Resolution::Happened, h.now())
            .await
            .unwrap()
    );
    // A handler that reports an unknown outcome parks it the same way.
    let unknown = Handler::new(
        "provider.reserve",
        vec![JobOutcome::Unknown {
            code: "PROVIDER_TIMEOUT",
            message: "no answer after submit".into(),
        }],
    );
    let (id, _) = jobs::enqueue(
        h.conn(),
        job(
            "provider.reserve",
            RetrySafety::ReconcileOnUncertainty,
            3,
            &h,
        ),
        &ctx,
        h.now(),
    )
    .await
    .unwrap();
    assert_eq!(runner(&h, "c", unknown).run_due().await.unwrap().unknown, 1);
    assert_eq!(status(&h, id).await, "UNKNOWN_OUTCOME");
    // An exhausted idempotent job interrupted on its last attempt is dead-lettered.
    let (last, _) = jobs::enqueue(
        h.conn(),
        job("inventory.refresh", RetrySafety::Idempotent, 1, &h),
        &ctx,
        h.now(),
    )
    .await
    .unwrap();
    let d = runner(&h, "d", refresh.clone());
    assert_eq!(d.claim(Uuid::new_v4()).await.unwrap().len(), 1);
    h.clock.advance(Duration::seconds(31));
    assert_eq!(d.run_due().await.unwrap().dead_lettered, 1);
    assert_eq!(status(&h, last).await, "DEAD_LETTER");
}

fn customer(t: Uuid) -> IdempotencyScope {
    IdempotencyScope::Customer {
        tenant_id: t,
        application_id: Uuid::nil(),
        environment_id: Uuid::nil(),
    }
}

#[tokio::test]
async fn idempotency_keys_replay_refuse_mismatches_and_are_scoped() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ttl = Duration::hours(24);
    let tenant = Uuid::new_v4();
    let fp = idempotency::fingerprint(&[b"POST", b"/v1/numbers/reservations", br#"{"n":1}"#]);
    // First use inside the protected transaction, completed and committed.
    let txn = h.conn().begin().await.unwrap();
    let Begin::New(record) = idempotency::begin(
        &txn,
        &customer(tenant),
        "reserve",
        "key-00000001",
        &fp,
        h.now(),
        ttl,
    )
    .await
    .unwrap() else {
        panic!("first use")
    };
    let resource = Uuid::new_v4();
    idempotency::complete(
        &txn,
        record,
        201,
        Some(json!({ "id": resource })),
        Some(resource),
        h.now(),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    // Same request: the stored outcome replays.
    match idempotency::begin(
        h.conn(),
        &customer(tenant),
        "reserve",
        "key-00000001",
        &fp,
        h.now(),
        ttl,
    )
    .await
    .unwrap()
    {
        Begin::Replay {
            status,
            body,
            resource_id,
        } => {
            assert_eq!((status, resource_id), (201, Some(resource)));
            assert_eq!(body, Some(json!({ "id": resource })));
        }
        other => panic!("{other:?}"),
    }
    // Different request, same key: refused.
    let other_fp = idempotency::fingerprint(&[b"POST", b"/v1/numbers/reservations", br#"{"n":2}"#]);
    assert_eq!(
        idempotency::begin(
            h.conn(),
            &customer(tenant),
            "reserve",
            "key-00000001",
            &other_fp,
            h.now(),
            ttl
        )
        .await
        .unwrap(),
        Begin::Mismatch
    );
    // The same key in another tenant, operation, service or operator is a different key.
    for (scope, op) in [
        (customer(Uuid::new_v4()), "reserve"),
        (customer(tenant), "release"),
        (
            IdempotencyScope::Service("cfmeet.calling".into()),
            "reserve",
        ),
        (IdempotencyScope::Operator("ops-1".into()), "reserve"),
    ] {
        let txn = h.conn().begin().await.unwrap();
        assert!(matches!(
            idempotency::begin(&txn, &scope, op, "key-00000001", &fp, h.now(), ttl)
                .await
                .unwrap(),
            Begin::New(_)
        ));
        txn.rollback().await.unwrap();
    }
    // A failed operation rolls its key back: the retry runs again.
    let txn = h.conn().begin().await.unwrap();
    assert!(matches!(
        idempotency::begin(
            &txn,
            &customer(tenant),
            "reserve",
            "key-00000002",
            &fp,
            h.now(),
            ttl
        )
        .await
        .unwrap(),
        Begin::New(_)
    ));
    txn.rollback().await.unwrap();
    assert!(matches!(
        idempotency::begin(
            h.conn(),
            &customer(tenant),
            "reserve",
            "key-00000002",
            &fp,
            h.now(),
            ttl
        )
        .await
        .unwrap(),
        Begin::New(_)
    ));
    // A key held by an uncompleted operation is IN_PROGRESS for others.
    assert_eq!(
        idempotency::begin(
            h.conn(),
            &customer(tenant),
            "reserve",
            "key-00000002",
            &fp,
            h.now(),
            ttl
        )
        .await
        .unwrap(),
        Begin::InProgress
    );
    // Malformed keys are refused; expired keys are forgotten and reusable.
    for bad in ["short", "has space here", &"x".repeat(129)] {
        assert!(idempotency::begin(
            h.conn(),
            &customer(tenant),
            "reserve",
            bad,
            &fp,
            h.now(),
            ttl
        )
        .await
        .is_err());
    }
    h.clock.advance(Duration::hours(25));
    assert!(matches!(
        idempotency::begin(
            h.conn(),
            &customer(tenant),
            "reserve",
            "key-00000001",
            &other_fp,
            h.now(),
            ttl
        )
        .await
        .unwrap(),
        Begin::New(_)
    ));
    let purged = idempotency::purge_expired(h.conn(), h.now() + Duration::hours(25), 100)
        .await
        .unwrap();
    assert!(purged >= 1);
}

#[tokio::test]
async fn concurrent_duplicates_of_one_key_wait_for_the_first_and_replay_it() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ttl = Duration::hours(1);
    let fp = idempotency::fingerprint(&[b"op"]);
    let scope = customer(Uuid::new_v4());
    let txn = h.conn().begin().await.unwrap();
    let Begin::New(record) =
        idempotency::begin(&txn, &scope, "op", "dup-key-0001", &fp, h.now(), ttl)
            .await
            .unwrap()
    else {
        panic!()
    };
    // The duplicate blocks on the unique index until the first transaction ends…
    let conn = h.conn().clone();
    let now = h.now();
    let dup_scope = scope.clone();
    let fp2 = fp.clone();
    let dup = tokio::spawn(async move {
        idempotency::begin(&conn, &dup_scope, "op", "dup-key-0001", &fp2, now, ttl).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !dup.is_finished(),
        "the duplicate must wait for the first transaction"
    );
    idempotency::complete(
        &txn,
        record,
        200,
        Some(json!({ "ok": true })),
        None,
        h.now(),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    // …and then replays its outcome instead of running again.
    match dup.await.unwrap().unwrap() {
        Begin::Replay { status, .. } => assert_eq!(status, 200),
        other => panic!("{other:?}"),
    }
    let _: Value = json!(null);
}

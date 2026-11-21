//! Feature switch-on requests and release history (A8) through the real HQ chain, against
//! a fake GitHub API on a local listener (no network).

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::releases::{GitHubSettings, ReleaseWorkflow, RELEASE_OPERATIONS};
use admin_bridge::routes::commands;
use admin_bridge::store::OpStatus;
use chrono::{Duration, Utc};
use common::*;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const TOKEN: &str = "github_pat_test_only_value";

#[derive(Default)]
struct FakeGitHub {
    /// Status answered to a dispatch.
    dispatch_status: u16,
    dispatches: Vec<(String, String, Value)>,
    runs: Vec<Value>,
    run_queries: Vec<String>,
}

type Shared = Arc<Mutex<FakeGitHub>>;

async fn fake_github() -> (Shared, String) {
    let state: Shared = Arc::new(Mutex::new(FakeGitHub {
        dispatch_status: 204,
        ..Default::default()
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let s = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = l.accept().await else {
                continue;
            };
            let s = s.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(
                    move |req: hyper::Request<hyper::body::Incoming>| {
                        let s = s.clone();
                        async move {
                            let (parts, body) = req.into_parts();
                            let body = body.collect().await.unwrap().to_bytes();
                            let auth = parts
                                .headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default()
                                .to_string();
                            let path = parts.uri.path().to_string();
                            let mut g = s.lock().unwrap();
                            let (status, out) = if path.ends_with("/dispatches") {
                                g.dispatches.push((
                                    path,
                                    auth,
                                    serde_json::from_slice(&body).unwrap_or(Value::Null),
                                ));
                                (g.dispatch_status, Vec::new())
                            } else if path.ends_with("/runs") {
                                g.run_queries
                                    .push(parts.uri.query().unwrap_or_default().to_string());
                                let v =
                                    json!({"total_count": g.runs.len(), "workflow_runs": g.runs});
                                (200, serde_json::to_vec(&v).unwrap())
                            } else {
                                (404, Vec::new())
                            };
                            Ok::<_, std::convert::Infallible>(
                                hyper::Response::builder()
                                    .status(status)
                                    .body(Full::new(bytes::Bytes::from(out)))
                                    .unwrap(),
                            )
                        }
                    },
                );
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(tcp), svc)
                    .await;
            });
        }
    });
    (state, url)
}

fn workflow(url: &str, dir: &tempfile::TempDir) -> Arc<ReleaseWorkflow> {
    let token_file = dir.path().join("github.token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    Arc::new(ReleaseWorkflow::new(&GitHubSettings {
        api_url: url.into(),
        repository: "cosmicforge/cfmeet".into(),
        token_file,
    }))
}

fn run(id: i64, sha: &str, created: chrono::DateTime<Utc>) -> Value {
    let t = created.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    json!({
        "id": id, "name": "CF Meet coordinated production release", "event": "workflow_dispatch",
        "head_sha": sha, "head_branch": "main", "status": "completed", "conclusion": "success",
        "created_at": t, "updated_at": t, "display_title": "x", "actor": {"login": "someone"},
        "logs_url": "https://example.invalid/logs"
    })
}

fn approval(capability: &str, sub: &str) -> Value {
    let now = Utc::now();
    json!({
        "approval_id": uuid::Uuid::new_v4().to_string(),
        "capability": capability,
        "requester_ref": sub,
        "approver_ref": "release-approver",
        "intent_digest": admin_bridge::wire::sha256_hex(b"intent"),
        "decided_at": (now - Duration::minutes(1)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "expires_at": (now + Duration::hours(11)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    })
}

fn activation(sub: &str, body: Value) -> Call {
    let mut c = call(
        "/operations/activations",
        &scoped("release-request"),
        "operations.activation.request",
        &["cfmeet.release_manager"],
    );
    c.method = "POST";
    c.sub = sub.into();
    c.idempotency_key = body["operation_id"].as_str().map(str::to_string);
    c.body = serde_json::to_vec(&body).unwrap();
    c.approval = Some(approval("operations.activation.request", sub));
    c
}

fn activation_body(operation: &str) -> (String, Value) {
    let id = uuid::Uuid::new_v4().to_string();
    let body = json!({"operation_id": id, "expected_version": 0, "operation": operation, "release_sha": SHA, "confirm": "owner-approved"});
    (id, body)
}

fn readback(sub: &str, key: &str) -> Call {
    let mut c = call(
        &format!("/hq/operations/{key}"),
        &scoped("release-request"),
        "bridge.operations.read",
        &["cfmeet.release_manager"],
    );
    c.sub = sub.into();
    c
}

fn windowed(path: &str, permission: &str, sub: &str, cursor: &str) -> Call {
    let start = (Utc::now() - Duration::days(2)).format("%Y-%m-%dT%H:%M:%SZ");
    let end = (Utc::now() + Duration::seconds(30)).format("%Y-%m-%dT%H:%M:%SZ");
    let mut c = call(
        path,
        &format!(
            "{}&limit=10{cursor}&start={start}&end={end}",
            scoped("release-request")
        ),
        permission,
        &["cfmeet.release_manager"],
    );
    c.sub = sub.into();
    c
}

#[tokio::test]
async fn activation_requests_dispatch_once_and_list_their_runs() {
    let (gh, url) = fake_github().await;
    let dir = tempfile::tempdir().unwrap();
    let h = start(Options {
        switches_all_on: true,
        releases: Some(workflow(&url, &dir)),
        ..Default::default()
    })
    .await;
    let (id, body) = activation_body("activate-oauth");
    let first = activation("rm-0001", body.clone());
    let (status, v) = send(&h, &first).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["operation"]["status"], "SUCCEEDED");
    assert_eq!(v["result"]["operation"]["source_status"], "DISPATCHED");
    let r = &v["result"]["resource"];
    assert_eq!(
        (r["request_id"].as_str(), r["state"].as_str()),
        (Some(id.as_str()), Some("DISPATCHED"))
    );
    assert_eq!(
        (r["requested_by"].as_str(), r["approver"].as_str()),
        (Some("hq:rm-0001"), Some("hq:release-approver"))
    );
    {
        let g = gh.lock().unwrap();
        assert_eq!(g.dispatches.len(), 1);
        let (path, auth, sent) = &g.dispatches[0];
        assert_eq!(
            path,
            "/repos/cosmicforge/cfmeet/actions/workflows/cfmeet-coordinated-release.yml/dispatches"
        );
        assert_eq!(auth, &format!("Bearer {TOKEN}"));
        assert_eq!(
            sent,
            &json!({"ref": "main", "inputs": {"operation": "activate-oauth", "release_sha": SHA, "confirm": "owner-approved"}})
        );
    }
    // A replay (same key, body and approval; a fresh token) returns the outcome, no resend.
    let mut again = activation("rm-0001", body);
    again.approval = first.approval.clone();
    let (status, v) = send(&h, &again).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(gh.lock().unwrap().dispatches.len(), 1);
    // Not one of the workflow's options; a short sha: refused before anything is sent.
    let (_, mut bad) = activation_body("deploy-everything");
    assert_eq!(
        code(&send(&h, &activation("rm-0002", bad.clone())).await.1),
        "COMMAND_BODY_INVALID"
    );
    bad["operation"] = json!("inspect");
    bad["release_sha"] = json!("abc123");
    assert_eq!(
        code(&send(&h, &activation("rm-0003", bad)).await.1),
        "COMMAND_BODY_INVALID"
    );
    // GitHub refuses: FAILED, nothing ran.
    gh.lock().unwrap().dispatch_status = 422;
    let (refused_id, body) = activation_body("verify");
    let (status, v) = send(&h, &activation("rm-0004", body)).await;
    assert_eq!((status, code(&v)), (422, "OWNER_REFUSED"), "{v}");
    let failed = h
        .store
        .activations()
        .into_iter()
        .find(|a| a.request_id.to_string() == refused_id)
        .unwrap();
    assert_eq!(failed.state, "FAILED");
    assert_eq!(gh.lock().unwrap().dispatches.len(), 2);

    // The run shows up: the list links it and shows GitHub's latest state.
    gh.lock().unwrap().runs = vec![run(77, SHA, Utc::now())];
    let (status, v) = send(
        &h,
        &call(
            "/operations/activations",
            &format!("{}&limit=10", scoped("release-request")),
            "operations.activations.read",
            &["cfmeet.release_manager"],
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["freshness"], "LIVE");
    let items = v["result"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let dispatched = items
        .iter()
        .find(|i| i["request_id"] == id.as_str())
        .unwrap();
    assert_eq!(dispatched["run_id"], 77);
    assert_eq!(dispatched["run_status"], "COMPLETED");
    assert_eq!(dispatched["run_conclusion"], "SUCCESS");
    let refused = items
        .iter()
        .find(|i| i["request_id"] == refused_id.as_str())
        .unwrap();
    assert!(refused["run_id"].is_null() && refused["run_status"].is_null());

    // Release history: the runs, with the operation the bridge requested; nothing else.
    let (status, v) = send(
        &h,
        &windowed(
            "/operations/releases",
            "operations.releases.read",
            "rm-0005",
            "",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"]["items"][0];
    assert_eq!(
        (r["run_id"].as_i64(), r["operation"].as_str()),
        (Some(77), Some("activate-oauth"))
    );
    assert!(r.get("logs_url").is_none() && r.get("actor").is_none());
    assert!(v["result"]["next_cursor"].is_null());
    assert!(gh
        .lock()
        .unwrap()
        .run_queries
        .iter()
        .any(|q| q.contains("branch=main")));
}

#[tokio::test]
async fn an_unknown_dispatch_settles_from_the_runs_list_and_is_never_resent() {
    let (gh, url) = fake_github().await;
    let dir = tempfile::tempdir().unwrap();
    let h = start(Options {
        switches_all_on: true,
        releases: Some(workflow(&url, &dir)),
        ..Default::default()
    })
    .await;
    gh.lock().unwrap().dispatch_status = 502;
    let (id, body) = activation_body("activate-app-events");
    let (status, v) = send(&h, &activation("rm-0101", body)).await;
    assert_eq!((status, code(&v)), (502, "UNKNOWN_OUTCOME"), "{v}");
    let rec = h.store.activations().into_iter().next().unwrap();
    assert_eq!(rec.state, "REQUESTED");
    // No run listed yet: still unknown. Another commit's run is not this request's.
    gh.lock().unwrap().runs = vec![run(87, &"f".repeat(40), Utc::now())];
    let (_, v) = send(&h, &readback("rm-0101", &id)).await;
    assert_eq!(v["result"]["status"], "UNKNOWN_OUTCOME", "{v}");
    // The run is listed: SUCCEEDED, linked; GitHub was asked to dispatch exactly once.
    gh.lock().unwrap().runs.push(run(88, SHA, Utc::now()));
    let (_, v) = send(&h, &readback("rm-0101", &id)).await;
    assert_eq!(v["result"]["status"], "SUCCEEDED", "{v}");
    assert_eq!(v["result"]["source_version"], "88");
    let rec = h.store.activations().into_iter().next().unwrap();
    assert_eq!((rec.state.as_str(), rec.run_id), ("DISPATCHED", Some(88)));
    assert_eq!(gh.lock().unwrap().dispatches.len(), 1);

    // Unknown, and no run ever appears: FAILED as never received after 600 s (sweep).
    let (lost, body) = activation_body("activate-app-directory");
    let (_, v) = send(&h, &activation("rm-0102", body)).await;
    assert_eq!(code(&v), "UNKNOWN_OUTCOME");
    let key = uuid::Uuid::parse_str(&lost).unwrap();
    h.store.backdate_operation(key, 700);
    commands::sweep(&h.bridge).await;
    let op = h
        .store
        .operations()
        .into_iter()
        .find(|o| o.idempotency_key == key)
        .unwrap();
    assert_eq!(
        (op.status, op.source_status.as_deref()),
        (OpStatus::Failed, Some("NOT_RECEIVED"))
    );
    let rec = h
        .store
        .activations()
        .into_iter()
        .find(|a| a.request_id == key)
        .unwrap();
    assert_eq!(rec.state, "FAILED");
    assert_eq!(gh.lock().unwrap().dispatches.len(), 2);
}

#[tokio::test]
async fn activation_requests_are_disabled_without_github() {
    let h = start(Options {
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    let (_, body) = activation_body("inspect");
    let (status, v) = send(&h, &activation("rm-0201", body)).await;
    assert_eq!((status, code(&v)), (503, "CAPABILITY_DISABLED"), "{v}");
    assert!(h.store.operations().is_empty() && h.store.activations().is_empty());
    let (status, v) = send(
        &h,
        &windowed(
            "/operations/releases",
            "operations.releases.read",
            "rm-0202",
            "",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["freshness"], "UNAVAILABLE");
    assert_eq!(v["result"], json!({"items": [], "next_cursor": null}));
}

#[tokio::test]
async fn release_history_is_the_workflow_runs_without_logs() {
    let (gh, url) = fake_github().await;
    let dir = tempfile::tempdir().unwrap();
    let h = start(Options {
        switches_all_on: true,
        releases: Some(workflow(&url, &dir)),
        ..Default::default()
    })
    .await;
    // A run older than the window is dropped; a run of a different status is mapped.
    let mut queued = run(5, SHA, Utc::now() - Duration::hours(1));
    queued["status"] = json!("queued");
    queued["conclusion"] = Value::Null;
    gh.lock().unwrap().runs = vec![queued, run(4, SHA, Utc::now() - Duration::days(3))];
    let (status, v) = send(
        &h,
        &windowed(
            "/operations/releases",
            "operations.releases.read",
            "rm-0301",
            "",
        ),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let items = v["result"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0],
        json!({
            "run_id": 5, "event": "workflow_dispatch", "operation": null, "head_sha": SHA,
            "status": "QUEUED", "conclusion": null,
            "created_at": items[0]["created_at"], "updated_at": items[0]["updated_at"]
        })
    );
    let q = gh.lock().unwrap().run_queries[0].clone();
    assert!(
        q.contains("created=") && q.contains("per_page=10") && q.contains("page=1"),
        "{q}"
    );
    // A bad cursor is refused.
    let (_, v) = send(
        &h,
        &windowed(
            "/operations/releases",
            "operations.releases.read",
            "rm-0302",
            "&cursor=x",
        ),
    )
    .await;
    assert_eq!(code(&v), "QUERY_INVALID");
}

/// The bridge's list of release operations is the workflow's `options:` list, in order.
#[test]
fn release_operations_are_the_workflow_options() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.github/workflows/cfmeet-coordinated-release.yml");
    let text = std::fs::read_to_string(path).unwrap();
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("options: ["))
        .expect("the workflow's operation options");
    let options: Vec<&str> = line
        .trim_start_matches("options: [")
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .collect();
    assert_eq!(options, RELEASE_OPERATIONS);
}

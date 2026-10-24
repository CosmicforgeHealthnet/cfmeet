//! Part VI §50 service-account certification journey: the Developer Platform delegates
//! service-account create / suspend to Platform Access.
//!
//! The journey starts its own certification stack — the REAL service binaries, real
//! Ed25519 service authentication, one PostgreSQL database per service, real Redis — and
//! drives it through the real API Gateway with a CF Meet developer session (plus the
//! operator console, which issues the credential of the developer-created service
//! account through the Platform Access operator API — the operator path; customer
//! credential self-service (Section 54) is certified by `tests/chapters_10_13.rs`).
//! Nothing is stubbed.
//!
//! Prerequisites and skip rules are those of `tests/journeys.rs`: TEST_DATABASE_URL,
//! TEST_REDIS_URL and the service binaries (CFMEET_BIN_DIR); without them the journey
//! FAILS unless CFMEET_SKIP_DB_TESTS=1 skips explicitly.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use cfmeet_certification::{
    provision::{self, Auth, Resp},
    stack::{self, Stack},
    StackConfig,
};
use reqwest::Method;
use serde_json::{json, Value};
use uuid::Uuid;

static BUSY: AtomicBool = AtomicBool::new(false);

struct Serial;

impl Serial {
    async fn acquire() -> Serial {
        while BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Serial
    }
}

impl Drop for Serial {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

async fn start() -> Option<(Serial, Stack)> {
    let config = StackConfig::from_env().expect("certification prerequisites")?;
    let guard = Serial::acquire().await;
    let stack = Stack::start(config)
        .await
        .expect("certification stack starts");
    Some((guard, stack))
}

fn email(role: &str) -> String {
    format!(
        "{role}-{}@cert.example",
        &Uuid::new_v4().simple().to_string()[..10]
    )
}

#[track_caller]
fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

/// Scopes of the operator-issued credentials: a developer-platform read scope (the probe)
/// and the Section 50 service-account scope (the machine path).
const SCOPES: &[&str] = &["organizations.read", "service_accounts.manage"];

#[tokio::test]
async fn service_account_delegation_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    // 1. A CF Meet user creates organization -> project -> SANDBOX environment through
    //    the gateway (the Developer Platform delegates each to Platform Access).
    let sb = provision::developer_sandbox(&stack, &email("sa-owner"), SCOPES)
        .await
        .unwrap();
    let session = Auth::Session(&sb.user.session);
    let env = sb.environment_id;
    let base = format!("/v1/environments/{env}/service-accounts");

    // 2. The user creates a service account through the gateway, idempotently.
    let key = format!("sa-{}", Uuid::new_v4().simple());
    let create = |key: String| {
        let gw = gw.clone();
        let base = base.clone();
        let token = sb.user.session.clone();
        async move {
            gw.request(
                Method::POST,
                &base,
                Auth::Session(&token),
                Some(json!({ "name": "ci-runner", "description": "CI pipeline" })),
                &[("idempotency-key", key)],
            )
            .await
        }
    };
    let created = create(key.clone()).await;
    status(&created, 201, "POST /v1/environments/{id}/service-accounts");
    assert_eq!(created.body["status"], "ACTIVE");
    assert_eq!(created.body["environmentId"], json!(env));
    assert_eq!(created.body["organizationId"], json!(sb.organization_id));
    assert_eq!(created.body["projectId"], json!(sb.project_id));
    assert!(created.header("idempotent-replayed").is_none());
    let sa = created.uuid("/id");
    let replay = create(key).await;
    status(&replay, 201, "replayed create");
    assert_eq!(replay.uuid("/id"), sa, "the replay returns the original");
    assert_eq!(replay.header("idempotent-replayed"), Some("true"));
    // The developer reads it back; the list holds it next to the operator's `backend`.
    let one = gw.get(&format!("{base}/{sa}"), session).await;
    status(&one, 200, "GET service account");
    assert_eq!(one.body["name"], "ci-runner");
    let list = gw.get(&base, session).await;
    status(&list, 200, "GET service accounts");
    let names: Vec<&str> = list.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(
        names.contains(&"ci-runner") && names.contains(&"backend"),
        "{names:?}"
    );
    // Inputs are validated before anything is delegated.
    let bad = gw
        .post(&base, session, json!({ "name": "Not A Key" }))
        .await;
    status(&bad, 400, "invalid service account name");

    // 3. The operator console issues a credential for the developer-created service
    //    account (Section 54 self-service does not exist yet).
    let (credential_id, secret) = operator.credential(sa, SCOPES).await.unwrap();
    let cred = Auth::Credential(&secret);
    let probe = format!("/v1/organizations/{}", sb.organization_id);
    status(
        &gw.get(&probe, cred).await,
        200,
        "the new service account's credential authenticates",
    );
    // A machine credential with `service_accounts.manage` reads the service accounts too.
    let machine = Auth::Credential(&sb.customer.secret);
    let m = gw.get(&format!("{base}/{sa}"), machine).await;
    status(&m, 200, "machine GET service account");

    // 4. The user suspends it through the gateway: its credential stops authenticating
    //    at the gateway at once (Platform Access verifies every request).
    let s = gw
        .post(&format!("{base}/{sa}/suspend"), session, json!({}))
        .await;
    status(&s, 200, "POST …/suspend");
    assert_eq!(s.body["status"], "SUSPENDED");
    let refused = gw.get(&probe, cred).await;
    status(&refused, 401, "a suspended service account's credential");
    let refused = gw.get("/v1/developer-platform/context", cred).await;
    status(
        &refused,
        401,
        "a suspended service account's credential (context)",
    );
    // Suspending again is a state error; the machine view agrees with the authority.
    let again = gw
        .post(&format!("{base}/{sa}/suspend"), session, json!({}))
        .await;
    status(&again, 409, "suspending a suspended service account");
    assert_eq!(again.code(), Some("INVALID_STATE"));
    let m = gw.get(&format!("{base}/{sa}"), machine).await;
    assert_eq!(m.body["status"], "SUSPENDED", "{}", m.text);
    // The credential itself was not revoked: Platform Access still lists it ACTIVE.
    let c = operator
        .pa(
            Method::GET,
            &format!("/admin/service-accounts/{sa}/credentials"),
            None,
        )
        .await
        .unwrap();
    status(&c, 200, "operator credential list");
    let listed = c.body["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == json!(credential_id))
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(listed["status"], "ACTIVE", "{}", c.text);

    // 5. Resume: the same credential works again.
    let r = gw
        .post(&format!("{base}/{sa}/resume"), session, json!({}))
        .await;
    status(&r, 200, "POST …/resume");
    assert_eq!(r.body["status"], "ACTIVE");
    status(
        &gw.get(&probe, cred).await,
        200,
        "the resumed service account's credential authenticates again",
    );

    // 6. Another tenant's developer never sees it (NOT_FOUND, no hint).
    let other = provision::register_user(
        stack.url(stack::MEETING_SERVICE),
        &stack.config.redis_url,
        &stack.http,
        &email("sa-other"),
    )
    .await
    .unwrap();
    let other_session = Auth::Session(&other.session);
    for (method, path) in [
        (Method::GET, base.clone()),
        (Method::GET, format!("{base}/{sa}")),
        (Method::POST, format!("{base}/{sa}/suspend")),
    ] {
        let body = (method == Method::POST).then(|| json!({}));
        let r = gw.request(method, &path, other_session, body, &[]).await;
        status(&r, 404, &format!("another tenant: {path}"));
    }

    // 7. Every mutation is in the organization's activity history, without secrets.
    let act = gw
        .get(
            &format!(
                "/v1/organizations/{}/activity?limit=100",
                sb.organization_id
            ),
            session,
        )
        .await;
    status(&act, 200, "GET activity");
    let actions: Vec<&str> = act.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["targetId"] == json!(sa))
        .filter_map(|e| e["action"].as_str())
        .collect();
    assert_eq!(
        actions,
        [
            "service_account.resumed",
            "service_account.suspended",
            "service_account.created"
        ],
        "newest first, the idempotent replay recorded once"
    );
    assert!(!act.text.contains(&secret));
    stack.teardown().await;
}

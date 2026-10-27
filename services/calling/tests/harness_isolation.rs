//! The test harness never mistakes another test's Platform Access process for its own.
//!
//! Every test starts its own Platform Access on a probed free port. Another test's
//! instance can take that port between the probe and the bind; the new process then
//! exits, but `/ready` on the port is answered by the OTHER instance — whose trust
//! bundle does not know this test's keys, so every later call fails with
//! `401 INTERNAL_AUTHENTICATION_FAILED` (seen intermittently in CI). Here the port is
//! deliberately taken: the harness must notice and start its own instance elsewhere.

mod common;

use common::identity::PlatformAccess;
use uuid::Uuid;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_port_already_served_by_another_platform_access_is_never_adopted() {
    let Some(admin) = common::admin_url() else {
        return;
    };
    let Some(first) = PlatformAccess::start(&admin).await else {
        return;
    };
    let taken: u16 = first.base_url.rsplit(':').next().unwrap().parse().unwrap();
    let mut candidates = std::iter::once(taken).chain(std::iter::repeat_with(free_port));
    let second = PlatformAccess::start_with_ports(&admin, &[], &mut || candidates.next().unwrap())
        .await
        .expect("a second instance starts");
    assert_ne!(
        second.base_url, first.base_url,
        "the instance already on the port is not this test's"
    );
    // Each harness instance authenticates its own operator (404: authenticated, no such
    // tenant) — neither is talking to the other's process.
    let path = format!("/internal/v1/admin/tenants/{}", Uuid::new_v4());
    for pa in [&first, &second] {
        let (status, body) = pa.admin(reqwest::Method::GET, &path, None).await;
        assert_eq!(status, 404, "{} {body}", pa.base_url);
    }
}

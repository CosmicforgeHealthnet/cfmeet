"""Mutation tests of the gateway upstream-handler gate (INV-001)."""
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_gateway_upstreams as gate  # noqa: E402

ROUTER = r'''
const STATUS_PATH: &str = "/v1/status";

pub fn router() -> Router {
    Router::new()
        .route("/health", get(health))
        .route(STATUS_PATH, get(status))
        .nest("/v1/things", things())
        // .route("/v1/commented-out", get(gone))
        .route("/v1/relay/*path", any(relay))
}

fn things() -> Router {
    Router::new()
        .route("/", get(list).post(create))
        .route(
            "/:thing_id",
            get(read)
                .patch(update),
        )
        .route("/:thing_id/archive", on(MethodFilter::POST, archive))
}
'''

DISPATCH = r'''
match (method, parts.as_slice()) {
    ("GET", ["me"]) => me(),
    ("PUT", ["profiles", id, "block"]) | ("DELETE", ["profiles", id, "block"]) => block(),
    ("GET", ["files", rest @ ..]) => files(),
}
'''


class GatewayUpstreamGate(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        (root / "src").mkdir()
        (root / "src/router.rs").write_text(ROUTER, encoding="utf-8")
        (root / "api.rs").write_text(DISPATCH, encoding="utf-8")
        self.crate = gate.Crate([str(root / "src")])
        self.saved = dict(gate.RELAYS)
        gate.RELAYS.clear()
        gate.RELAYS["up"] = [("/v1/relay", str(root / "api.rs"))]

    def tearDown(self):
        gate.RELAYS.clear()
        gate.RELAYS.update(self.saved)
        self.tmp.cleanup()

    def test_routes_resolve_through_nesting_constants_and_method_chains(self):
        for method, path in [
            ("GET", "/health"),
            ("HEAD", "/health"),
            ("GET", "/v1/status"),
            ("GET", "/v1/things"),
            ("POST", "/v1/things"),
            ("GET", "/v1/things/{thingId}"),
            ("PATCH", "/v1/things/{thingId}"),
            ("POST", "/v1/things/{thingId}/archive"),
        ]:
            self.assertTrue(self.crate.resolves(method, path), f"{method} {path}")

    def test_missing_paths_and_methods_are_orphans(self):
        for method, path in [
            ("DELETE", "/v1/things/{thingId}"),
            ("GET", "/v1/things/{thingId}/archive"),
            ("GET", "/v1/things/{thingId}/missing"),
            ("GET", "/v1/commented-out"),
            ("GET", "/v2/things"),
            # a catch-all never satisfies a path on its own
            ("GET", "/v1/relay/anything"),
        ]:
            self.assertFalse(self.crate.resolves(method, path), f"{method} {path}")

    def test_relay_paths_must_match_a_dispatch_arm(self):
        cache = {}
        self.assertTrue(gate.relay_resolves("up", "GET", "/v1/relay/me", cache))
        self.assertTrue(gate.relay_resolves("up", "DELETE", "/v1/relay/profiles/{id}/block", cache))
        self.assertTrue(gate.relay_resolves("up", "GET", "/v1/relay/files/a/b", cache))
        for method, path in [
            ("POST", "/v1/relay/me"),
            ("GET", "/v1/relay/me/extra"),
            ("GET", "/v1/relay/profiles/{id}/block"),
            ("GET", "/v1/relay/unknown"),
            ("GET", "/v2/relay/me"),
        ]:
            self.assertFalse(gate.relay_resolves("up", method, path, cache), f"{method} {path}")



class RepositoryManifest(unittest.TestCase):
    def test_every_gateway_route_has_an_upstream_handler(self):
        self.assertEqual(gate.main(), 0)


if __name__ == "__main__":
    unittest.main()

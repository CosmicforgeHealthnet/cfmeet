"""Mutation tests for the service boundary guard.

* Synthetic repositories: a clean tree passes, and every rule fires on exactly the
  mutation it exists for — while the legitimate look-alikes (own database URL,
  TEST_DATABASE_URL, shared packages, own tables, comments) stay clean.
* The real repository passes, and copies of it with a single injected violation in
  services/calling fail with the expected rule (the working tree is never modified).

Run: python3 -m unittest discover -s scripts/service-boundaries -v
"""

from __future__ import annotations

import shutil
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import check_service_boundaries as guard  # noqa: E402

REPO = HERE.parents[1]


def write(root: Path, rel: str, content: str) -> Path:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(content).lstrip("\n"), encoding="utf-8")
    return path


def append(root: Path, rel: str, content: str) -> None:
    path = root / rel
    path.write_text(path.read_text(encoding="utf-8") + textwrap.dedent(content), encoding="utf-8")


def rules(root: Path) -> list[str]:
    return sorted({v.rule for v in guard.check(root)})


class SyntheticRepository(unittest.TestCase):
    """services/alpha, services/beta-service and packages/shared."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        write(self.root, "packages/shared/Cargo.toml", """
            [package]
            name = "shared"
            version = "0.1.0"
            [dependencies]
            serde = "1"
        """)
        write(self.root, "packages/shared/src/lib.rs", "pub fn shared() {}\n")
        write(self.root, "packages/service-auth/src/identity.rs", """
            pub mod services {
                pub const ALPHA: &str = "cfmeet.alpha";
                pub const GAMMA: &str = "cfmeet.gamma";
            }
        """)
        write(self.root, "services/alpha/Cargo.toml", """
            [package]
            name = "alpha"
            version = "0.1.0"
            [workspace]
            members = [".", "migration"]
            [dependencies]
            shared = { path = "../../packages/shared" }
            alpha-migration = { path = "migration" }
        """)
        write(self.root, "services/alpha/migration/Cargo.toml", """
            [package]
            name = "alpha-migration"
            version = "0.1.0"
        """)
        write(self.root, "services/alpha/migration/src/m1.rs", """
            const SQL: &str = "CREATE TABLE alpha_things (id uuid primary key); CREATE TABLE outbox_events (id uuid)";
        """)
        write(self.root, "services/alpha/src/lib.rs", """
            //! Alpha never reads MEETING_DATABASE_URL or beta_service tables; comments may say so.
            /* block comment: load_user_from_beta_db, "../beta-service/src" */
            pub fn database_url() -> String {
                std::env::var("DATABASE_URL").or_else(|_| std::env::var("ALPHA_DATABASE_URL")).unwrap()
            }
            pub const OWN: &str = "select * from alpha_things join outbox_events using (id)";
            pub fn message<'a>(x: &'a str) -> char { let _ = x; 'b' }
            pub fn alpha_db_pool() {}
            #[cfg(test)]
            mod tests {
                const T: &str = "select * from beta_items";
                fn load_from_beta_db() {}
            }
        """)
        write(self.root, "services/alpha/tests/it.rs", """
            fn url() -> String { std::env::var("TEST_DATABASE_URL").unwrap() }
            const FORBIDDEN_IN_A_GUARD: &[&str] = &["from beta_items", "beta_db"];
        """)
        write(self.root, "services/alpha/.env.example", """
            # Never set BETA_DATABASE_URL here.
            DATABASE_URL=postgres://alpha
            LOAD_DATABASE_URL=postgres://alpha-load
        """)
        write(self.root, "services/beta-service/Cargo.toml", """
            [package]
            name = "beta"
            version = "0.1.0"
            [dependencies]
            shared = { path = "../../packages/shared" }
        """)
        write(self.root, "services/beta-service/migration/src/m1.rs", """
            #[derive(DeriveIden)]
            enum BetaItems { Table, Id }
            fn up() { Table::create().table(BetaItems::Table); }
            const SQL: &str = "CREATE TABLE IF NOT EXISTS beta_users (id uuid); CREATE TABLE outbox_events (id uuid)";
        """)
        write(self.root, "services/beta-service/src/lib.rs", """
            pub const Q: &str = "select * from beta_items";
            fn beta_db() {}
        """)

    def tearDown(self):
        self._tmp.cleanup()

    def assertOnly(self, rule: str):
        found = guard.check(self.root)
        self.assertTrue(found, f"expected {rule}, guard found nothing")
        self.assertEqual({v.rule for v in found}, {rule}, "\n".join(map(str, found)))

    # -- baseline ------------------------------------------------------------

    def test_clean_tree_passes(self):
        self.assertEqual(guard.check(self.root), [])

    def test_table_ownership_is_derived_from_migrations(self):
        repo = guard.Repository(self.root)
        self.assertEqual(repo.tables["alpha"], {"alpha_things", "outbox_events"})
        self.assertEqual(repo.tables["beta-service"], {"beta_items", "beta_users", "outbox_events"})
        self.assertEqual(repo.known_services, {"alpha", "beta-service", "gamma"})

    # -- Cargo ---------------------------------------------------------------

    def test_cross_service_path_dependency(self):
        append(self.root, "services/alpha/Cargo.toml", 'beta = { path = "../beta-service" }\n')
        self.assertOnly("CARGO_CROSS_SERVICE_DEPENDENCY")

    def test_cross_service_dev_and_target_dependencies(self):
        append(self.root, "services/alpha/Cargo.toml", """
            [dev-dependencies]
            beta = { path = "../beta-service" }
        """)
        self.assertOnly("CARGO_CROSS_SERVICE_DEPENDENCY")

    def test_cross_service_target_specific_dependency(self):
        append(self.root, "services/alpha/Cargo.toml", """
            [target.'cfg(unix)'.dependencies]
            beta = { path = "../../services/beta-service" }
        """)
        self.assertOnly("CARGO_CROSS_SERVICE_DEPENDENCY")

    def test_cross_service_patch(self):
        append(self.root, "services/alpha/Cargo.toml", """
            [patch.crates-io]
            beta = { path = "../beta-service" }
        """)
        self.assertOnly("CARGO_CROSS_SERVICE_DEPENDENCY")

    def test_cross_service_workspace_member(self):
        path = self.root / "services/alpha/Cargo.toml"
        path.write_text(path.read_text().replace('members = [".", "migration"]', 'members = [".", "migration", "../beta-service/*"]'))
        self.assertOnly("CARGO_CROSS_SERVICE_DEPENDENCY")

    def test_cross_service_bin_target_path(self):
        append(self.root, "services/alpha/Cargo.toml", """
            [[bin]]
            name = "borrowed"
            path = "../beta-service/src/main.rs"
        """)
        self.assertOnly("CARGO_CROSS_SERVICE_DEPENDENCY")

    def test_package_depending_on_a_service(self):
        append(self.root, "packages/shared/Cargo.toml", 'alpha = { path = "../../services/alpha" }\n')
        self.assertOnly("PACKAGE_DEPENDS_ON_SERVICE")

    def test_path_outside_repository(self):
        append(self.root, "services/alpha/Cargo.toml", 'elsewhere = { path = "../../../../elsewhere" }\n')
        self.assertOnly("CARGO_PATH_OUTSIDE_REPOSITORY")

    # -- source --------------------------------------------------------------

    def test_include_of_another_services_source(self):
        append(self.root, "services/alpha/src/lib.rs", 'include!("../../beta-service/src/lib.rs");\n')
        self.assertIn("CROSS_SERVICE_SOURCE_INCLUDE", rules(self.root))

    def test_include_str_via_manifest_dir(self):
        append(self.root, "services/alpha/src/lib.rs", 'const S: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../beta-service/src/lib.rs"));\n')
        self.assertIn("CROSS_SERVICE_SOURCE_INCLUDE", rules(self.root))

    def test_path_attribute_module(self):
        append(self.root, "services/alpha/src/lib.rs", '#[path = "../../beta-service/src/lib.rs"]\nmod borrowed;\n')
        self.assertIn("CROSS_SERVICE_SOURCE_INCLUDE", rules(self.root))

    def test_package_including_service_source(self):
        append(self.root, "packages/shared/src/lib.rs", 'include!("../../../services/alpha/src/lib.rs");\n')
        self.assertOnly("CROSS_SERVICE_SOURCE_INCLUDE")

    def test_path_literal_into_another_service(self):
        append(self.root, "services/alpha/tests/it.rs", 'const M: &str = "../beta-service/migration/src/m1.rs";\n')
        self.assertOnly("CROSS_SERVICE_PATH_LITERAL")

    # -- data ----------------------------------------------------------------

    def test_other_services_database_url_in_code(self):
        append(self.root, "services/alpha/src/lib.rs", 'pub fn beta() -> String { std::env::var("BETA_DATABASE_URL").unwrap() }\n')
        self.assertOnly("CROSS_SERVICE_DATABASE_URL")

    def test_other_services_database_url_with_qualifier(self):
        append(self.root, "services/alpha/src/lib.rs", 'pub fn beta() -> String { std::env::var("LEGACY_BETA_SERVICE_DB_URL").unwrap() }\n')
        self.assertOnly("CROSS_SERVICE_DATABASE_URL")

    def test_future_services_database_url(self):
        append(self.root, "services/alpha/src/lib.rs", 'pub fn g() -> String { std::env::var("GAMMA_DATABASE_URL").unwrap() }\n')
        self.assertOnly("CROSS_SERVICE_DATABASE_URL")

    def test_other_services_database_url_in_tests_and_config(self):
        append(self.root, "services/alpha/tests/it.rs", 'fn b() { std::env::var("BETA_DATABASE_URL").unwrap(); }\n')
        append(self.root, "services/alpha/.env.example", "BETA_DATABASE_URL=postgres://beta\n")
        write(self.root, "services/alpha/docker-compose.yml", "services:\n  alpha:\n    environment:\n      BETA_DATABASE_URL: postgres://beta\n")
        found = guard.check(self.root)
        self.assertEqual({v.rule for v in found}, {"CROSS_SERVICE_DATABASE_URL"})
        self.assertEqual({v.path for v in found}, {"services/alpha/tests/it.rs", "services/alpha/.env.example", "services/alpha/docker-compose.yml"})

    def test_sql_on_another_services_table(self):
        append(self.root, "services/alpha/src/lib.rs", 'pub const Q2: &str = "SELECT id FROM beta_items WHERE true";\n')
        self.assertOnly("CROSS_SERVICE_TABLE_ACCESS")

    def test_sql_join_insert_update_and_seaorm_idents(self):
        for sql in ('"insert into beta_users values (1)"', '"UPDATE \\"beta_users\\" SET x = 1"', '"select 1 from alpha_things a join public.beta_items b on true"'):
            with self.subTest(sql=sql):
                self.tearDown()
                self.setUp()
                append(self.root, "services/alpha/src/lib.rs", f"pub const Q3: &str = {sql};\n")
                self.assertOnly("CROSS_SERVICE_TABLE_ACCESS")

    def test_shared_table_names_are_owned_per_service(self):
        # outbox_events exists in both services' own databases: never a violation.
        self.assertNotIn("CROSS_SERVICE_TABLE_ACCESS", rules(self.root))

    def test_cross_service_db_helper(self):
        append(self.root, "services/alpha/src/lib.rs", "pub async fn load_user_from_beta_db() {}\n")
        self.assertOnly("CROSS_SERVICE_DB_HELPER")

    def test_cross_service_db_helper_variants(self):
        for ident in ("beta_service_pool", "BetaDbConnection", "gamma_repository", "read_beta_tables"):
            with self.subTest(ident=ident):
                self.tearDown()
                self.setUp()
                snake = guard.camel_to_snake(ident) if ident[0].isupper() else ident
                append(self.root, "services/alpha/src/lib.rs", f"pub fn {snake}() {{}}\n")
                self.assertOnly("CROSS_SERVICE_DB_HELPER")

    # -- lexer ---------------------------------------------------------------

    def test_lexer_strips_comments_and_strings_without_losing_lines(self):
        src = 'let a = "x // y"; // c\nlet b = r#"q "z" "#; /* n /* m */ k */ let c = \'"\';\nlet d: &\'static str = "";'
        stripped = guard.strip_rust(src, keep_strings=False)
        self.assertEqual(stripped.count("\n"), src.count("\n"))
        self.assertNotIn("y", stripped)
        self.assertNotIn("z", stripped)
        self.assertNotIn("k", stripped)
        self.assertIn("'static", stripped)
        kept = guard.strip_rust(src, keep_strings=True)
        self.assertIn('"x // y"', kept)
        self.assertNotIn("// c", kept)

    def test_cfg_test_items_are_removed_but_following_code_is_kept(self):
        code = "#[cfg(test)]\nmod tests { fn a() { } }\nfn after() {}\n#[cfg(test)]\nuse x;\nfn last() {}"
        out = guard.remove_cfg_test_items(code)
        self.assertNotIn("fn a()", out)
        self.assertIn("fn after", out)
        self.assertIn("fn last", out)
        self.assertEqual(out.count("\n"), code.count("\n"))


class RealRepository(unittest.TestCase):
    """The working tree passes; single-violation copies of it fail."""

    @classmethod
    def setUpClass(cls):
        cls.repo = guard.Repository(REPO)

    def test_working_tree_is_clean(self):
        found = guard.check(REPO)
        self.assertEqual(found, [], "\n".join(map(str, found)))

    def test_guard_sees_the_real_services(self):
        self.assertIn("calling", self.repo.services)
        self.assertIn("meeting-service", self.repo.services)
        # Part IV services and the operator tooling identity.
        self.assertTrue({"platform-access", "numbering"} <= set(self.repo.services))
        self.assertIn("operator-console", self.repo.known_services)
        access = self.repo.tables["platform-access"]
        self.assertTrue({"platform_tenants", "platform_applications", "platform_environments",
                         "platform_service_accounts", "platform_api_credentials",
                         "platform_principals", "platform_identity_changes",
                         "platform_access_audit"} <= access, access)
        numbering = self.repo.tables["numbering"]
        self.assertTrue({"number_resources", "number_provider_bindings", "number_capabilities",
                         "number_inventory_snapshots", "number_reservations", "dcn_assignments",
                         "number_assignment_history", "number_routing_profiles"} <= numbering, numbering)
        self.assertFalse(numbering & self.repo.tables["calling"])
        self.assertTrue({"privacy-relay", "telephony"} <= self.repo.known_services)
        self.assertTrue({"users", "organizations", "meeting_spaces"} <= self.repo.tables["meeting-service"])
        calling = self.repo.tables["calling"]
        self.assertTrue(calling)
        # Calling owns its platform foundation and the call domain (Part C sections 15-25).
        call_domain = {
            "calls", "call_participants", "call_sessions", "call_session_intervals",
            "call_routes", "call_legs", "call_attempts", "call_authorizations",
            "call_events", "call_quality_records", "call_usage_records", "calling_outbox_events",
            "call_endpoints", "call_invitations", "call_answer_claims",
            "call_participant_media_permissions", "call_media_sessions", "call_commands",
            "call_transfers", "call_media_provider_events",
            # Part D sections 26-30: evidence of Privacy Relay decisions / enforcement.
            "call_privacy_authorizations", "call_privacy_enforcements",
            # Part E sections 37-38: the typed routing policy of the Routing Engine.
            "call_routing_policies",
            # Part E sections 44-47: Calling's side of public-network calls — the
            # protected parties, the durable Telephony command outbox, received Telephony
            # events, public bridges, spend policy / reservations and inbound owners.
            # Telephony's own tables live in services/telephony.
            "call_public_parties", "call_telephony_commands", "call_telephony_events",
            "call_public_bridges", "call_spend_policies", "call_spend_reservations",
            "call_inbound_owners",
            # Part F section 55: the group-level single-winner claim of ring-group / queue
            # offers (one answering participant per answer group of a Call).
            "call_answer_group_claims",
        }
        self.assertTrue(call_domain <= calling)
        # Part F sections 52-62: enterprise configuration and continuity are Calling-owned
        # (ADR 0018) and live under the enterprise_ / voicemail_ / call_recording prefixes;
        # calling_incidents is Calling's own operational incident record (ADR 0026).
        self.assertTrue({"enterprise_call_policies", "enterprise_departments",
                         "enterprise_department_members", "enterprise_admin_audit",
                         "calling_incidents"} <= calling)
        # Part G section 70: the tenant, service-account and principal quota levels are
        # Calling-owned platform limit tables (ADR 0028), beside the application /
        # credential levels.
        self.assertTrue({"platform_tenant_limits", "platform_application_limits",
                         "platform_service_account_limits", "platform_credential_limits",
                         "platform_principal_limits"} <= calling)
        for limits_table in ("platform_service_account_limits", "platform_principal_limits"):
            self.assertNotIn(limits_table, self.repo.tables["meeting-service"])
            self.assertNotIn(limits_table, self.repo.tables["telephony"])
            self.assertNotIn(limits_table, self.repo.tables["privacy-relay"])
        # Part G sections 67-68: customer webhooks (subscriptions, normalized events,
        # deliveries, attempts) are Calling-owned (ADR 0028).
        self.assertTrue({"webhook_subscriptions", "webhook_events", "webhook_deliveries",
                         "webhook_delivery_attempts"} <= calling)
        self.assertTrue(all(t.startswith(("platform_", "enterprise_", "voicemail_", "webhook_"))
                            or t in call_domain or t.startswith("call_recording")
                            or t == "calling_incidents"
                            # Part H sections 69-70: block / opt-out rules, their refusal
                            # windows and abuse reports are Calling-owned (ADR 0029).
                            or t in {"calling_block_rules", "calling_abuse_windows",
                                     "calling_abuse_reports",
                                     # Part H section 79: orphaned protected objects
                                     # awaiting deletion; section 80: retention holds
                                     # and the retention audit (ADR 0030).
                                     "calling_orphan_objects", "calling_retention_holds",
                                     "calling_retention_audit"}
                            for t in calling), calling)
        # No second Call authority.
        self.assertFalse(calling & {"enterprise_calls", "pbx_calls", "voicemail_calls", "ivr_calls"})
        self.assertFalse(calling & self.repo.tables["meeting-service"])

    def test_privacy_relay_owns_its_tables(self):
        self.assertIn("privacy-relay", self.repo.services)
        relay = self.repo.tables["privacy-relay"]
        self.assertTrue({
            "communication_contexts", "communication_context_participants",
            "privacy_relay_sessions", "relay_authorization_decisions", "relay_audit_events",
            "privacy_relay_outbox", "privacy_relay_commands", "privacy_relay_enforcement_jobs",
        } <= relay, relay)
        self.assertFalse(relay & self.repo.tables["calling"])
        self.assertFalse(relay & self.repo.tables["meeting-service"])
        # Contact Vault (Part D sections 31-33) lives inside Privacy Relay only.
        self.assertTrue({"protected_contacts", "contact_access_audit"} <= relay, relay)
        for other in ("calling", "meeting-service"):
            self.assertFalse({t for t in self.repo.tables[other] if t.startswith(("protected_contact", "contact_"))},
                             f"{other} owns contact tables")

    def mutated(self, mutate) -> list[guard.Violation]:
        with tempfile.TemporaryDirectory() as tmp:
            copy = Path(tmp)
            for top in ("services", "packages"):
                for path in self.repo.files(REPO / top):
                    rel = path.relative_to(REPO)
                    (copy / rel).parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(path, copy / rel)
            mutate(copy)
            return guard.check(copy)

    def assertMutationFails(self, rule: str, mutate):
        found = self.mutated(mutate)
        self.assertTrue(any(v.rule == rule for v in found), f"{rule} not detected: {found}")

    def test_calling_depending_on_meeting_service(self):
        self.assertMutationFails(
            "CARGO_CROSS_SERVICE_DEPENDENCY",
            lambda r: append(r, "services/calling/Cargo.toml", '\n[dev-dependencies.backend]\npath = "../meeting-service"\n'),
        )

    def test_calling_reading_the_meeting_database_url(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: append(r, "services/calling/src/config/mod.rs", '\npub fn meeting() -> Option<String> { std::env::var("MEETING_DATABASE_URL").ok() }\n'),
        )

    def test_calling_querying_meeting_tables(self):
        self.assertMutationFails(
            "CROSS_SERVICE_TABLE_ACCESS",
            lambda r: append(r, "services/calling/src/platform/principals.rs", '\nconst LOOKUP: &str = "SELECT id, email FROM users WHERE id = $1";\n'),
        )

    def test_calling_meeting_db_helper(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DB_HELPER",
            lambda r: append(r, "services/calling/src/platform/principals.rs", "\npub async fn load_user_from_meeting_db() {}\n"),
        )

    def test_calling_including_meeting_source(self):
        self.assertMutationFails(
            "CROSS_SERVICE_SOURCE_INCLUDE",
            lambda r: append(r, "services/calling/src/lib.rs", '\n#[path = "../../meeting-service/src/lib.rs"]\nmod meeting;\n'),
        )

    def test_shared_package_depending_on_calling(self):
        self.assertMutationFails(
            "PACKAGE_DEPENDS_ON_SERVICE",
            lambda r: append(r, "packages/service-auth/Cargo.toml", '\n[dev-dependencies.calling]\npath = "../../services/calling"\n'),
        )

    def test_meeting_service_reading_the_calling_database(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: append(r, "services/meeting-service/.env.example", "\nCALLING_DATABASE_URL=postgres://calling\n"),
        )

    def test_calling_reading_a_future_services_database(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: append(r, "services/calling/.env.example", "\nTELEPHONY_DATABASE_URL=postgres://telephony\n"),
        )

    def test_privacy_relay_reading_the_calling_database(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: append(r, "services/privacy-relay/.env.example", "\nCALLING_DATABASE_URL=postgres://calling\n"),
        )

    def test_privacy_relay_depending_on_calling(self):
        self.assertMutationFails(
            "CARGO_CROSS_SERVICE_DEPENDENCY",
            lambda r: append(r, "services/privacy-relay/Cargo.toml", '\n[dev-dependencies.calling]\npath = "../calling"\n'),
        )

    def test_privacy_relay_querying_calling_tables(self):
        self.assertMutationFails(
            "CROSS_SERVICE_TABLE_ACCESS",
            lambda r: append(r, "services/privacy-relay/src/db.rs", '\nconst CALLS: &str = "SELECT id FROM call_participants WHERE principal_id = $1";\n'),
        )

    def test_privacy_relay_platform_principal_lookup(self):
        self.assertMutationFails(
            "CROSS_SERVICE_TABLE_ACCESS",
            lambda r: append(r, "services/privacy-relay/src/db.rs", '\nconst P: &str = "SELECT status FROM platform_principals WHERE id = $1";\n'),
        )

    def test_privacy_relay_including_calling_source(self):
        self.assertMutationFails(
            "CROSS_SERVICE_SOURCE_INCLUDE",
            lambda r: append(r, "services/privacy-relay/src/lib.rs", '\n#[path = "../../calling/src/privacy_relay/mod.rs"]\nmod calling_privacy;\n'),
        )

    def test_calling_reading_protected_contacts(self):
        self.assertMutationFails(
            "CROSS_SERVICE_TABLE_ACCESS",
            lambda r: append(r, "services/calling/src/privacy_relay/contacts.rs", '\nconst C: &str = "SELECT ciphertext FROM protected_contacts WHERE id = $1";\n'),
        )

    def test_calling_reading_the_contact_vault_database(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: append(r, "services/calling/.env.example", "\nCONTACT_VAULT_DATABASE_URL=postgres://vault\n"),
        )

    def test_calling_querying_privacy_relay_tables(self):
        self.assertMutationFails(
            "CROSS_SERVICE_TABLE_ACCESS",
            lambda r: append(r, "services/calling/src/privacy_relay/mod.rs", '\nconst C: &str = "SELECT status FROM communication_contexts WHERE id = $1";\n'),
        )

    def test_calling_reading_the_privacy_relay_database(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: append(r, "services/calling/.env.example", "\nPRIVACY_RELAY_DATABASE_URL=postgres://relay\n"),
        )

    # Part D §31–§36 boundaries.
    def test_relay_inventory_under_calling(self):
        self.assertMutationFails(
            "PRIVACY_RELAY_DOMAIN_ELSEWHERE",
            lambda r: write(r, "services/calling/migration/src/m20990101_000099_relay_numbers.rs",
                            'const UP: &str = "CREATE TABLE relay_number_inventory (id UUID PRIMARY KEY)";\n'),
        )

    def test_calling_querying_relay_assignments(self):
        self.assertMutationFails(
            "CROSS_SERVICE_TABLE_ACCESS",
            lambda r: append(r, "services/calling/src/privacy_relay/mod.rs", '\nconst A: &str = "SELECT relay_number_id FROM relay_number_assignments WHERE id = $1";\n'),
        )

    def test_contact_vault_source_copied_into_calling(self):
        self.assertMutationFails(
            "CONTACT_VAULT_OUTSIDE_OWNER",
            lambda r: append(r, "services/calling/src/privacy_relay/contacts.rs", "\nmod contact_vault { pub fn expose_e164() {} }\n"),
        )

    def test_calling_depending_on_contact_crypto(self):
        self.assertMutationFails(
            "CONTACT_VAULT_OUTSIDE_OWNER",
            lambda r: append(r, "services/calling/Cargo.toml", '\n[dev-dependencies]\naes-gcm = "0.10"\n')
            if "[dev-dependencies]" not in (r / "services/calling/Cargo.toml").read_text()
            else (r / "services/calling/Cargo.toml").write_text(
                (r / "services/calling/Cargo.toml").read_text().replace("[dev-dependencies]", '[dev-dependencies]\nphonenumber = "0.3"', 1)),
        )

    def test_plaintext_phone_field_in_calling(self):
        self.assertMutationFails(
            "PLAINTEXT_CONTACT_FIELD",
            lambda r: append(r, "services/calling/src/privacy_relay/mod.rs", "\npub struct Leak { pub destination_number: String }\n"),
        )

    def test_permanent_numbering_inside_privacy_relay(self):
        self.assertMutationFails(
            "NUMBERING_IN_PRIVACY_RELAY",
            lambda r: append(r, "services/privacy-relay/src/relay_numbers/mod.rs", "\npub fn start_number_porting() {}\n"),
        )

    def test_contact_vault_service_created_early(self):
        self.assertMutationFails(
            "FUTURE_SERVICE_IMPLEMENTED",
            lambda r: write(r, "services/contact-vault/src/main.rs", "fn main() {}\n"),
        )

    def test_telephony_reading_the_privacy_relay_database(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: write(r, "services/telephony/.env.example", "PRIVACY_RELAY_DATABASE_URL=postgres://relay\n"),
        )

    # Part IV: Platform Access is the only credential authority; Numbering the only
    # number authority.
    def test_number_tables_outside_numbering(self):
        self.assertMutationFails(
            "NUMBERING_DOMAIN_ELSEWHERE",
            lambda r: write(r, "services/calling/migration/src/m_numbers.rs", 'const UP: &str = "CREATE TABLE number_resources (id UUID)";\n'),
        )
        self.assertMutationFails(
            "NUMBERING_DOMAIN_ELSEWHERE",
            lambda r: write(r, "services/privacy-relay/migration/src/m_dcn.rs", 'const UP: &str = "CREATE TABLE dcn_assignments (id UUID)";\n'),
        )

    # Part IV M: the SMS Gateway is the only SMS transport authority.
    def test_sms_tables_outside_the_sms_gateway(self):
        self.assertMutationFails(
            "SMS_DOMAIN_ELSEWHERE",
            lambda r: write(r, "services/numbering/migration/src/m_sms.rs", 'const UP: &str = "CREATE TABLE sms_messages (id UUID)";\n'),
        )
        self.assertMutationFails(
            "SMS_DOMAIN_ELSEWHERE",
            lambda r: write(r, "services/calling/migration/src/m_sms.rs", 'const UP: &str = "CREATE TABLE sms_transports (id UUID)";\n'),
        )

    # Part V A: Messaging owns conversations and device public keys; no E2EE secret is
    # ever a column; contexts are product-neutral.
    @staticmethod
    def messaging(r: Path, migration_sql: str = "CREATE TABLE messaging_conversations (id UUID)", code: str = "pub fn ok() {}\n"):
        write(r, "services/messaging/Cargo.toml", '[package]\nname = "messaging"\nversion = "0.1.0"\n[dependencies]\ncf-messaging-crypto = { path = "../../packages/messaging-crypto" }\n')
        write(r, "services/messaging/migration/src/m1.rs", f'const UP: &str = "{migration_sql}";\n')
        write(r, "services/messaging/src/lib.rs", code)

    def test_a_clean_messaging_service_passes(self):
        found = self.mutated(lambda r: self.messaging(
            r,
            "CREATE TABLE messaging_devices (id UUID, signature_public_key BYTEA, CONSTRAINT ck CHECK (NOT (detail ? 'plaintext'))); "
            "CREATE TABLE messaging_key_packages (id UUID); CREATE TABLE messaging_conversations (context_class TEXT)",
            "//! Never Karryam or HealthNet tables; contexts are opaque.\npub const SQL: &str = \"SELECT id FROM messaging_conversations\";\n",
        ))
        self.assertEqual([v for v in found if "messaging" in v.path], [], found)

    def test_messaging_tables_outside_messaging(self):
        self.assertMutationFails(
            "MESSAGING_DOMAIN_ELSEWHERE",
            lambda r: write(r, "services/meeting-service/migration/src/m_msg.rs", 'const UP: &str = "CREATE TABLE messaging_conversations (id UUID)";\n'),
        )

    def test_device_keys_or_mls_state_outside_messaging(self):
        self.assertMutationFails(
            "MESSAGING_CRYPTO_ELSEWHERE",
            lambda r: write(r, "services/platform-access/migration/src/m_kp.rs", 'const UP: &str = "CREATE TABLE platform_key_packages (id UUID)";\n'),
        )
        self.assertMutationFails(
            "MESSAGING_CRYPTO_ELSEWHERE",
            lambda r: write(r, "services/calling/migration/src/m_mls.rs", 'const UP: &str = "ALTER TABLE calls ADD COLUMN mls_group_id BYTEA";\n'),
        )

    def test_e2ee_private_material_in_a_schema(self):
        self.assertMutationFails(
            "E2EE_SECRET_IN_SCHEMA",
            lambda r: write(r, "services/platform-access/migration/src/m_dev.rs", 'const UP: &str = "ALTER TABLE platform_principals ADD COLUMN device_private_key BYTEA";\n'),
        )
        for column in ("epoch_secret BYTEA", "init_private BYTEA", "message_body TEXT", "plaintext TEXT", "attachment_key BYTEA", "recovery_key BYTEA"):
            with self.subTest(column=column):
                self.assertMutationFails(
                    "E2EE_SECRET_IN_SCHEMA",
                    lambda r, c=column: self.messaging(r, f"CREATE TABLE messaging_groups (id UUID, {c})"),
                )

    def test_product_business_tables_or_names_in_messaging(self):
        for table in ("messaging_orders", "messaging_trip_chats", "messaging_patients", "karryam_threads"):
            with self.subTest(table=table):
                self.assertMutationFails(
                    "PRODUCT_DOMAIN_IN_MESSAGING",
                    lambda r, t=table: self.messaging(r, f"CREATE TABLE {t} (id UUID)"),
                )
        self.assertMutationFails(
            "PRODUCT_DOMAIN_IN_MESSAGING",
            lambda r: self.messaging(r, code='pub fn allowed(app: &str) -> bool { app == "healthnet-clinic" }\n'),
        )

    def test_messaging_crossing_other_boundaries(self):
        self.assertMutationFails(
            "CROSS_SERVICE_TABLE_ACCESS",
            lambda r: self.messaging(r, code='pub const Q: &str = "SELECT body FROM conference_messages";\n'),
        )
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: self.messaging(r, code='pub fn u() -> Option<String> { std::env::var("PLATFORM_ACCESS_DATABASE_URL").ok() }\n'),
        )
        self.assertMutationFails(
            "CARGO_CROSS_SERVICE_DEPENDENCY",
            lambda r: (self.messaging(r), append(r, "services/messaging/Cargo.toml", 'meeting = { path = "../meeting-service" }\n')),
        )
        self.assertMutationFails(
            "PLAINTEXT_CONTACT_FIELD",
            lambda r: self.messaging(r, code="pub struct Party { pub phone_number: String }\n"),
        )
        self.assertMutationFails(
            "PLAINTEXT_CONTACT_FIELD",
            lambda r: self.messaging(r, "CREATE TABLE messaging_contacts (id UUID, msisdn TEXT)"),
        )

    def test_shared_packages_never_depend_on_messaging(self):
        self.assertMutationFails(
            "PACKAGE_DEPENDS_ON_SERVICE",
            lambda r: (self.messaging(r), append(r, "packages/messaging-crypto/Cargo.toml", '\n[dev-dependencies.messaging]\npath = "../../services/messaging"\n')),
        )

    def test_other_services_cannot_read_the_messaging_database(self):
        self.assertMutationFails(
            "CROSS_SERVICE_DATABASE_URL",
            lambda r: append(r, "services/meeting-service/src/main.rs", '\nconst M: &str = "MESSAGING_DATABASE_URL";\n'),
        )
        self.assertMutationFails(
            "CROSS_SERVICE_DB_HELPER",
            lambda r: append(r, "services/calling/src/main.rs", "\nfn load_conversations_from_messaging_db() {}\n"),
        )

    def test_credential_verification_outside_platform_access(self):
        self.assertMutationFails(
            "IDENTITY_AUTHORITY_ELSEWHERE",
            lambda r: append(r, "services/calling/src/main.rs", "\nfn local(k: &VerifierKeyring) {}\n"),
        )
        self.assertMutationFails(
            "IDENTITY_AUTHORITY_ELSEWHERE",
            lambda r: append(r, "services/numbering/src/main.rs", "\nfn local() { let _ = verifier_message; }\n"),
        )

    def test_calling_reading_the_credential_pepper(self):
        self.assertMutationFails(
            "FOREIGN_SECRET_REFERENCE",
            lambda r: append(r, "services/calling/src/main.rs", '\nconst P: &str = "API_CREDENTIAL_PEPPER_FILE";\n'),
        )

    # Part J section 98 / Part K section 109: secret blast radius.
    def test_calling_reading_telephony_provider_secrets(self):
        self.assertMutationFails(
            "FOREIGN_SECRET_REFERENCE",
            lambda r: append(r, "services/calling/src/main.rs", '\nconst D: &str = "TELECOM_PROVIDER_SECRETS_DIR";\n'),
        )

    def test_calling_mounting_a_contact_vault_keyring(self):
        self.assertMutationFails(
            "FOREIGN_SECRET_REFERENCE",
            lambda r: append(r, "services/calling/.env.example", "\nCONTACT_VAULT_KEYRING_FILE=/run/secrets/keyring\n"),
        )

    def test_telephony_reading_a_calling_webhook_key(self):
        self.assertMutationFails(
            "FOREIGN_SECRET_REFERENCE",
            lambda r: append(r, "services/telephony/src/main.rs", '\nconst K: &str = "CALLING_WEBHOOK_SECRET_KEY_FILE";\n'),
        )

    def test_privacy_relay_reading_the_ari_password(self):
        self.assertMutationFails(
            "FOREIGN_SECRET_REFERENCE",
            lambda r: append(r, "services/privacy-relay/src/main.rs", '\nconst P: &str = "TELEPHONY_ARI_PASSWORD";\n'),
        )

    def test_test_harnesses_may_start_another_service_with_its_secrets(self):
        # An e2e harness passes Telephony's secrets to the Telephony binary it starts;
        # a comment in deployment config may name another service's secret.
        found = self.mutated(lambda r: (
            write(r, "services/calling/tests/harness_secret_probe.rs", 'const D: &str = "TELECOM_PROVIDER_SECRETS_DIR";\n'),
            append(r, "services/calling/.env.example", "\n# CONTACT_VAULT_KEYRING_FILE is mounted into Privacy Relay only\n"),
        ))
        self.assertFalse([v for v in found if v.rule == "FOREIGN_SECRET_REFERENCE"], found)


if __name__ == "__main__":
    unittest.main()

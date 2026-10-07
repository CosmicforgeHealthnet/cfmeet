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
        self.assertTrue({"privacy-relay", "telephony"} <= self.repo.known_services)
        self.assertTrue({"users", "organizations", "meeting_spaces"} <= self.repo.tables["meeting-service"])
        self.assertTrue(self.repo.tables["calling"])
        self.assertTrue(all(t.startswith("platform_") for t in self.repo.tables["calling"]))

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


if __name__ == "__main__":
    unittest.main()

"""Mutation tests of the migration naming / ordering guard (DATA-014)."""
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_migration_order as guard  # noqa: E402

LIB = """
mod m20260101_000001_first;
mod m20260102_000002_second;
pub struct Migrator;
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260101_000001_first::Migration),
            // Box::new(m20250101_000000_commented_out::Migration),
            Box::new(m20260102_000002_second::Migration),
            %EXTRA%
        ]
    }
}
pub struct OtherMigrator;
impl MigratorTrait for OtherMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Named)]
    }
    fn migration_table_name() -> DynIden { Alias::new("other").into_iden() }
}
struct Named;
impl MigrationName for Named {
    fn name(&self) -> &str {
        "m20260201_000001_named"
    }
}
"""


class MigrationGuard(unittest.TestCase):
    def problems(self, extra: str, files: dict[str, str] | None = None):
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp)
            (src / "lib.rs").write_text(LIB.replace("%EXTRA%", extra), encoding="utf-8")
            for name, body in (files or {}).items():
                (src / name).write_text(body, encoding="utf-8")
            found = guard.migrators(src / "lib.rs")
            return found, [p[3] for p in guard.problems_of("svc", found)]

    def test_well_formed_histories_pass_and_explicit_names_resolve(self):
        found, rules = self.problems("")
        self.assertEqual(found["Migrator"], ["m20260101_000001_first", "m20260102_000002_second"])
        self.assertEqual(found["OtherMigrator"], ["m20260201_000001_named"])
        self.assertEqual(rules, [])

    def test_a_module_file_may_override_the_history_name(self):
        override = 'pub struct Migration;\nimpl MigrationName for Migration {\n    fn name(&self) -> &str {\n        "m20260103_000003_renamed"\n    }\n}\n'
        found, rules = self.problems("Box::new(m20260103_000003_file::Migration),", {"m20260103_000003_file.rs": override})
        self.assertEqual(found["Migrator"][-1], "m20260103_000003_renamed")
        self.assertEqual(rules, [])

    def test_a_new_migration_that_sorts_earlier_is_refused(self):
        _, rules = self.problems("Box::new(m20251231_000003_generated_today::Migration),")
        self.assertIn("order", rules)

    def test_bad_names_dates_double_prefixes_and_duplicates_are_refused(self):
        for extra, rule in (
            ("Box::new(add_users::Migration),", "name"),
            ("Box::new(m20261399_000003_bad_date::Migration),", "date"),
            ("Box::new(m20260301_000003_m20260301_000001_twice::Migration),", "double-prefix"),
            ("Box::new(Named),", "duplicate"),
        ):
            with self.subTest(rule=rule):
                _, rules = self.problems(extra)
                self.assertIn(rule, rules)

    def test_the_repository_passes(self):
        self.assertEqual(guard.main(), 0)


if __name__ == "__main__":
    unittest.main()

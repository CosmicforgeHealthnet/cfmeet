"""Mutation tests for the secret scan.

* Every rule fires on exactly the material it exists for, and the published / synthetic
  fixtures (AWS documentation keys, repeated-character placeholders, ``*.example``
  templates, test sources) stay clean.
* The real repository is clean.

Run: python3 -m unittest discover -s scripts/secret-scan -v
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import scan_secrets as scan  # noqa: E402

REPO = HERE.parents[1]
# Assembled at runtime so this file never contains a literal private-key block (the
# repository's own key check scans every tracked file).
_HEADER = b"-----BEGIN " + b"PRIVATE KEY-----"
PEM = _HEADER + b"\nMC4CAQAwBQYDK2VwBCIEIHx0ZXN0b25seWtleW1hdGVyaWFs\n-----END " + b"PRIVATE KEY-----\n"


def rules(rel: str, data: bytes) -> set[str]:
    return {f.rule for f in scan.scan_bytes(rel, data)}


class RuleTests(unittest.TestCase):
    def test_pem_block_with_material_fires_but_bare_header_does_not(self) -> None:
        self.assertEqual(rules("infra/x.conf", PEM), {"PRIVATE_KEY_BLOCK"})
        self.assertEqual(rules("src/a.rs", b'assert!(s.contains("-----BEGIN PRIVATE KEY-----"));'), set())

    def test_keyring_document_fires(self) -> None:
        doc = (
            b'{"keyring_version": 1, "purpose": "CONTACT_ENCRYPTION", "active": "v1",'
            b' "keys": [{"version": "v1", "key": "Zm9vYmFyYmF6cXV4Zm9vYmFyYmF6cXV4Zm9vYmFyYmE="}]}'
        )
        self.assertEqual(rules("deploy/keyring.json", doc), {"KEYRING_DOCUMENT"})

    def test_api_credential_fires_unless_synthetic(self) -> None:
        real = b"cfmk_live_4f1e9c2b7a6d0e5f3c8b1a2d_" + b"9f8e7d6c5b4a3210e1d2c3b4a5f60718" * 2
        self.assertEqual(rules("notes.md", real), {"API_CREDENTIAL"})
        fixture = b"cfmk_live_4f1e9c2b7a6d0e5f3c8b1a2d_" + b"ab" * 32
        self.assertEqual(rules("notes.md", fixture), set())

    def test_webhook_secret_fires_unless_synthetic(self) -> None:
        self.assertEqual(rules("cfg.yml", b"whsec_k3J9x2Qm7Lp0Zt4Vb8Nc1Rw6Hy5Gd2Fs"), {"WEBHOOK_SECRET"})
        self.assertEqual(rules("cfg.yml", b"whsec_" + b"A" * 43), set())
        self.assertEqual(rules("cfg.yml", b"whsec_test-only-fixture-secret-value"), set())

    def test_cloud_access_key_fires_unless_published_example(self) -> None:
        self.assertEqual(rules("cfg.env", b"AKIAJ7Q2M9X4P1L6T3Z8"), {"CLOUD_ACCESS_KEY"})
        self.assertEqual(rules("cfg.env", b"AKIAIOSFODNN7EXAMPLE"), set())

    def test_assignment_fires_on_real_value_only(self) -> None:
        self.assertEqual(rules("cfg.env", b"TELEPHONY_ARI_PASSWORD=q7Hx2kLm9ZpR4vWd8sT1"), {"CREDENTIAL_ASSIGNMENT"})
        clean = [
            b"TELEPHONY_ARI_PASSWORD_FILE=/run/secrets/ari",
            b"secret_ref = \"carrier-primary-trunk\"",
            b"# password = q7Hx2kLm9ZpR4vWd8sT1",
            b"password = ${ARI_PASSWORD}",
            b"api_key: <your-api-key-goes-here>",
            b"let token = std::env::var(\"X_TOKEN_1234567890abcd\");",
            b"password = changeme-changeme-123",
        ]
        for line in clean:
            self.assertEqual(rules("cfg.env", line), set(), line)

    def test_templates_tests_and_certification_placeholders_are_allowed(self) -> None:
        line = b"password = q7Hx2kLm9ZpR4vWd8sT1"
        for rel in (
            "services/x/.env.example",
            "services/x/tests/common/mod.rs",
            "infra/telephony/certification/ari.conf",
            "infra/livekit/livekit.dev.yaml",
        ):
            self.assertEqual(rules(rel, line), set(), rel)
        # A PEM block is never allowed, whatever the path.
        self.assertEqual(rules("services/x/.env.example", PEM), {"PRIVATE_KEY_BLOCK"})

    def test_binary_files_skip_the_textual_rule(self) -> None:
        self.assertEqual(rules("bin/app", b"\0\0password = q7Hx2kLm9ZpR4vWd8sT1"), set())
        self.assertEqual(rules("bin/app", b"\0\0" + PEM), {"PRIVATE_KEY_BLOCK"})


class ImageTreeTests(unittest.TestCase):
    """--tree on an exported Debian root filesystem."""

    def scan_tree(self, files: dict[str, bytes], md5sums: dict[str, bytes]) -> list[str]:
        import hashlib
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for rel, data in files.items():
                (root / rel).parent.mkdir(parents=True, exist_ok=True)
                (root / rel).write_bytes(data)
            info = root / "var/lib/dpkg/info"
            info.mkdir(parents=True, exist_ok=True)
            (info / "libexample.md5sums").write_text(
                "".join(f"{hashlib.md5(data).hexdigest()}  {rel}\n" for rel, data in md5sums.items()))
            found = scan.scan_paths(root, scan.tree_files(root), scan.package_manifest(root))
            return sorted(f.path for f in found)

    def test_unmodified_package_content_is_skipped(self) -> None:
        lib = b"\0ELF" + PEM  # an upstream library carrying public self-test keys
        self.assertEqual(self.scan_tree({"usr/lib/libexample.so": lib}, {"usr/lib/libexample.so": lib}), [])

    def test_an_altered_package_file_is_scanned(self) -> None:
        shipped = b"\0ELF upstream"
        self.assertEqual(self.scan_tree({"usr/lib/libexample.so": b"\0ELF" + PEM},
                                        {"usr/lib/libexample.so": shipped}), ["usr/lib/libexample.so"])

    def test_files_added_to_the_image_are_always_scanned(self) -> None:
        lib = b"\0ELF" + PEM
        self.assertEqual(self.scan_tree({"usr/lib/libexample.so": lib, "app/service.pem": PEM},
                                        {"usr/lib/libexample.so": lib}), ["app/service.pem"])


class RepositoryTests(unittest.TestCase):
    def test_real_repository_is_clean(self) -> None:
        findings = scan.scan_paths(REPO, scan.tracked_files(REPO))
        self.assertEqual(findings, [])


if __name__ == "__main__":
    unittest.main()

"""Self-test: the RUSTSEC-2026-0381 guard accepts only the reviewed shim over a fixed parent."""
import unittest

from check_webpki_roots_shim import SHIM_CHECKSUM, check


def lock(*blocks):
    return "version = 4\n" + "".join(f"\n[[package]]\n{b}\n" for b in blocks)


def roots(version, checksum="abc", deps=None):
    d = "" if deps is None else "dependencies = [\n" + "".join(f' "{x}",\n' for x in deps) + "]\n"
    return f'name = "webpki-roots"\nversion = "{version}"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "{checksum}"\n{d}'


class Guard(unittest.TestCase):
    def test_absent_and_fixed(self):
        self.assertEqual(check(lock('name = "serde"\nversion = "1.0.0"')), [])
        self.assertEqual(check(lock(roots("1.0.10"))), [])

    def test_shim_over_fixed_parent(self):
        self.assertEqual(check(lock(roots("0.26.11", SHIM_CHECKSUM, ["webpki-roots 1.0.10"]), roots("1.0.10"))), [])

    def test_vulnerable_parent(self):
        self.assertTrue(check(lock(roots("1.0.9"))))
        self.assertTrue(check(lock(roots("0.26.11", SHIM_CHECKSUM, ["webpki-roots 1.0.9"]), roots("1.0.9"))))

    def test_unreviewed_shim(self):
        self.assertTrue(check(lock(roots("0.26.11", "0" * 64, ["webpki-roots 1.0.10"]), roots("1.0.10"))))
        self.assertTrue(check(lock(roots("0.26.10", SHIM_CHECKSUM, ["webpki-roots 1.0.10"]), roots("1.0.10"))))
        self.assertTrue(check(lock(roots("0.25.4", "x"))), "a real 0.25 root store is never accepted")
        self.assertTrue(check(lock(roots("0.26.11", SHIM_CHECKSUM, []))), "a shim without a parent is refused")


if __name__ == "__main__":
    unittest.main()

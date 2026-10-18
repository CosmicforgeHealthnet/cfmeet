"""Licence policy of check_licenses.py: permissive passes, copyleft / unknown fails, MPL-2.0
only on the listed crates (pending legal review)."""
import unittest

from check_licenses import allowed


class LicencePolicy(unittest.TestCase):
    def test_permissive_expressions_pass(self):
        for expr in ["MIT", "MIT OR Apache-2.0", "Apache-2.0/MIT", "(MIT OR Apache-2.0) AND Unicode-3.0",
                     "BSD-3-Clause", "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"]:
            self.assertTrue(allowed(expr, "any-crate"), expr)

    def test_copyleft_and_unknown_fail(self):
        for expr in ["GPL-3.0", "AGPL-3.0-only", "LGPL-2.1", "MIT AND GPL-2.0", "SSPL-1.0", "Proprietary", ""]:
            self.assertFalse(allowed(expr, "any-crate"), expr)

    def test_mpl_only_for_the_listed_crates(self):
        self.assertTrue(allowed("MPL-2.0", "hpke-rs"))
        self.assertFalse(allowed("MPL-2.0", "some-other-crate"))
        self.assertTrue(allowed("MIT OR MPL-2.0", "some-other-crate"))


if __name__ == "__main__":
    unittest.main()

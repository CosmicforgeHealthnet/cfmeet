import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import app_acceptance as accounts


class ControlledFixtureIsolation(unittest.TestCase):
    def test_run_namespaces_never_reuse_persistent_or_other_run_accounts(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder)
            for name,value in [('calling-acceptance-accounts.json','legacy'),
                    ('cfmeet-acceptance-communications-123456.json','first'),
                    ('cfmeet-acceptance-communications-123457.json','second')]:
                (root/name).write_text(json.dumps([value]))
            with patch.object(accounts.p,'SECRETS',root), patch.object(accounts,'api') as api:
                self.assertEqual(accounts.accounts(),['legacy'])
                self.assertEqual(accounts.accounts('communications-123456',count=2),['first'])
                self.assertEqual(accounts.accounts('communications-123457',count=2),['second'])
                api.assert_not_called()

    def test_fixture_namespace_cannot_select_arbitrary_secret_file(self):
        for value in ('../customer','/root','communications/name','x'*65):
            with self.subTest(value=value), self.assertRaises(AssertionError):
                accounts.accounts(value,count=2)

import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import activation_base as ab
import coordinated_release as cr
from test_coordinated_release import isolated_work

A, B = 'a' * 40, 'b' * 40


def git(repo, *args):
    return subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True, text=True).stdout.strip()


class Runner(unittest.TestCase):
    """activation_base.evaluate on a throwaway repository with one service image."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        git(self.repo, 'init', '-q', '-b', 'main')
        git(self.repo, 'config', 'user.email', 't@example.invalid')
        git(self.repo, 'config', 'user.name', 't')
        (self.repo / 'services/calling').mkdir(parents=True)
        (self.repo / 'packages/shared').mkdir(parents=True)
        (self.repo / 'services/calling/Dockerfile').write_text('FROM x AS builder\nCOPY packages/shared packages/shared\n'
                                                                'COPY services/calling services/calling\n')
        (self.repo / 'services/calling/main.rs').write_text('fn main() {}\n')
        (self.repo / 'packages/shared/lib.rs').write_text('\n')
        (self.repo / 'docs').mkdir()
        (self.repo / 'docs/a.md').write_text('a\n')
        self.base = self.commit('release')

    def tearDown(self):
        self.tmp.cleanup()

    def commit(self, message):
        git(self.repo, 'add', '-A')
        git(self.repo, 'commit', '-q', '-m', message)
        return git(self.repo, 'rev-parse', 'HEAD')

    def evaluate(self, head):
        with patch.object(ab.cr, 'IMAGES', ('calling',)):
            return ab.evaluate(self.repo, head, self.base)

    def test_docs_and_tooling_commits_are_equivalent(self):
        (self.repo / 'docs/a.md').write_text('b\n')
        doc = self.evaluate(self.commit('docs'))
        self.assertTrue(doc['equivalent'], doc)
        self.assertIn('services/calling', doc['inputs'])
        self.assertIn('packages/shared', doc['inputs'])

    def test_a_service_or_shared_package_change_requires_a_release(self):
        (self.repo / 'packages/shared/lib.rs').write_text('// changed\n')
        doc = self.evaluate(self.commit('shared'))
        self.assertFalse(doc['equivalent'])
        self.assertEqual(doc['changed'], ['packages/shared/lib.rs'])

    def test_base_must_be_an_ancestor_of_the_head(self):
        head = self.base
        git(self.repo, 'checkout', '-q', '--orphan', 'other')
        (self.repo / 'docs/a.md').write_text('x\n')
        other = self.commit('unrelated')
        with patch.object(ab.cr, 'IMAGES', ('calling',)):
            doc = ab.evaluate(self.repo, other, head)
        self.assertFalse(doc['equivalent'])
        self.assertIn('ancestor', doc['reason'])

    def test_released_head_needs_no_proof(self):
        self.assertTrue(self.evaluate(self.base)['equivalent'])
        self.assertFalse(ab.evaluate(self.repo, 'nope', self.base)['equivalent'])


class Vps(unittest.TestCase):
    def journal(self, work, *shas):
        (work / 'journal.json').write_text(json.dumps([dict(id='release:%d' % i, operation='release', sha=s, status='done')
                                                       for i, s in enumerate(shas)]))

    def proof(self, work, **over):
        doc = dict(head=B, base=A, equivalent=True, changed=[], inputs=['services/calling'])
        doc.update(over)
        path = work / 'activation-base.json'
        path.write_text(json.dumps(doc))
        return path

    def test_released_sha_is_its_own_base(self):
        with isolated_work() as work:
            self.journal(work, A, B)
            self.assertEqual(cr.activation_base(B, work / 'missing.json'), B)

    def test_unreleased_head_acts_on_the_latest_release_only_with_a_matching_proof(self):
        with isolated_work() as work:
            self.journal(work, A)
            self.assertEqual(cr.activation_base(B, self.proof(work)), A)
            with self.assertRaisesRegex(RuntimeError, 'proof'):
                cr.activation_base(B, work / 'missing.json')
            for bad in (dict(head=A), dict(base='c' * 40), dict(equivalent=False), dict(changed=['x']), dict(inputs=[])):
                with self.assertRaisesRegex(RuntimeError, 'release this SHA first'):
                    cr.activation_base(B, self.proof(work, **bad))

    def test_no_release_at_all_is_refused(self):
        with isolated_work() as work:
            with self.assertRaisesRegex(RuntimeError, 'completed coordinated release'):
                cr.activation_base(B, self.proof(work))

    def test_running_images_are_still_checked_against_the_base_release(self):
        with isolated_work() as work:
            self.journal(work, A)
            seen = []
            with patch.object(cr, 'ACTIVATION_BASE', self.proof(work)), \
                    patch.object(cr, 'registry', lambda sha: {cr.image_of(n): {'imageId': 'id-' + sha} for n in cr.ORDER}), \
                    patch.object(cr, 'verify_running', lambda c, i, s: seen.append((c, i, s))):
                self.assertEqual(cr.require_release(B), A)
            self.assertEqual(len(seen), len(cr.ORDER))
            self.assertTrue(all(i == 'id-' + A and s == A for _, i, s in seen))


if __name__ == '__main__':
    unittest.main()

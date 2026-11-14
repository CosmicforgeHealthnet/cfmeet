"""Offline tests: a release replaces only the images whose build inputs changed."""

import contextlib
import io
import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import release_scope
from test_coordinated_release import REPO, isolated_work

SHA = 'b' * 40
BASE = 'a' * 40


def git(*args):
    return subprocess.run(['git', '-C', str(REPO), *args], capture_output=True, text=True).stdout.strip()


class Scope(unittest.TestCase):
    def test_no_previous_release_is_a_full_release(self):
        doc = release_scope.evaluate(REPO, SHA, None)
        self.assertEqual(set(doc['changed']), set(cr.IMAGES))

    def test_same_sha_changes_nothing(self):
        head = git('rev-parse', 'HEAD')
        doc = release_scope.evaluate(REPO, head, head)
        self.assertEqual(doc['changed'], [])

    def test_only_images_with_changed_inputs_are_in_scope(self):
        head = git('rev-parse', 'HEAD')
        parent = git('rev-parse', 'HEAD~1')
        if not parent:
            self.skipTest('shallow clone')
        doc = release_scope.evaluate(REPO, head, parent)
        changed = set(git('diff', '--name-only', parent, head).splitlines())
        for image in doc['unchanged']:
            inputs = release_scope.report.build_inputs(REPO, image, head)
            self.assertFalse(any(c == i or c.startswith(i.rstrip('/') + '/') for c in changed for i in inputs), image)
        self.assertEqual(set(doc['changed']) | set(doc['unchanged']), set(cr.IMAGES))


class KeptServices(unittest.TestCase):
    def journal(self, kept=None):
        cr.commit(dict(id='release:1', operation='release', sha=BASE, runId='1', status='done', components=[],
                       **({'kept': kept} if kept else {})))

    def test_kept_service_must_run_the_previous_release_image(self):
        with isolated_work() as work:
            self.journal()
            (work / ('registry-%s.json' % BASE)).write_text(json.dumps({i: {'imageId': 'sha256:' + i} for i in cr.IMAGES}))
            scope = work / 'scope.json'
            scope.write_text(json.dumps({'head': SHA, 'base': BASE, 'unchanged': [
                i for i in cr.IMAGES if i not in ('meeting-service',)]}))
            running = []
            with patch.object(cr, 'container_names', lambda: [cr.SERVICES[n]['container'] for n in cr.ORDER]), \
                    patch.object(cr, 'verify_running', lambda c, image, rev: running.append((c, image, rev))):
                replaced, kept = cr.release_scope(SHA, scope)
        self.assertEqual(replaced, {'meeting-service'})
        self.assertNotIn('meeting-service', kept)
        self.assertEqual(kept['developer-platform-worker'], {'imageId': 'sha256:developer-platform', 'revision': BASE})
        self.assertIn((cr.SERVICES['calling']['container'], 'sha256:calling', BASE), running)

    def test_a_service_not_running_its_recorded_image_is_replaced(self):
        def verify(container, image, rev):
            if container == cr.SERVICES['calling']['container']:
                raise RuntimeError('differs')
        with isolated_work() as work:
            self.journal()
            (work / ('registry-%s.json' % BASE)).write_text(json.dumps({i: {'imageId': 'sha256:' + i} for i in cr.IMAGES}))
            scope = work / 'scope.json'
            scope.write_text(json.dumps({'head': SHA, 'base': BASE, 'unchanged': list(cr.IMAGES)}))
            with patch.object(cr, 'container_names', lambda: [cr.SERVICES[n]['container'] for n in cr.ORDER]), \
                    patch.object(cr, 'verify_running', verify):
                replaced, kept = cr.release_scope(SHA, scope)
        self.assertEqual(replaced, {'calling'})

    def test_mismatched_scope_document_means_a_full_release(self):
        with isolated_work() as work, contextlib.redirect_stdout(io.StringIO()):
            self.journal()
            scope = work / 'scope.json'
            scope.write_text(json.dumps({'head': SHA, 'base': 'c' * 40, 'unchanged': list(cr.IMAGES)}))
            replaced, kept = cr.release_scope(SHA, scope)
        self.assertEqual((replaced, kept), (set(cr.IMAGES), {}))

    def test_expected_image_follows_kept_services(self):
        with isolated_work() as work:
            cr.commit(dict(id='release:2', operation='release', sha=SHA, runId='2', status='done', components=[],
                           kept={'calling': {'imageId': 'sha256:old-calling', 'revision': BASE}}))
            manifest = {i: {'imageId': 'sha256:new-' + i} for i in cr.IMAGES}
            self.assertEqual(cr.expected_image('calling', SHA, manifest), ('sha256:old-calling', BASE))
            self.assertEqual(cr.expected_image('meeting-service', SHA, manifest), ('sha256:new-meeting-service', SHA))


if __name__ == '__main__':
    unittest.main()

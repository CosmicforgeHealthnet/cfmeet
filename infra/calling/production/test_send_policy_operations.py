import unittest
import uuid
from types import SimpleNamespace
from unittest.mock import Mock, patch

import send_policy_operations as sp


class SendPolicyActivation(unittest.TestCase):
    def fake(self):
        envs = {name: {} for name in ('messaging', 'communications', 'calling')}
        plans = []

        def facts(_cr, name):
            env = envs[name]
            return dict(sendPolicyProtocol=1, sendPolicyGeneration=env.get(sp.GENERATION),
                        sendPolicySynchronous=env.get(sp.SOURCE_FLAGS.get(name)) == 'true',
                        sendPolicyLocalEnabled=env.get(sp.FAST) == 'true')

        def apply(_op, _sha, _run, changes, post=None):
            plans.append(changes)
            for name, overrides in changes:
                envs[name].update(overrides)
            if post:
                post()

        cr = SimpleNamespace(env_of=lambda name: envs[name], require_release=Mock(),
                             apply_activation=apply, emit=Mock())
        return cr, envs, plans, facts

    def test_prepare_starts_with_receiver_and_does_not_enable_the_fast_path(self):
        cr, envs, plans, facts = self.fake()
        with patch.object(sp, 'facts', facts):
            sp.activate(cr, 'sha', '1', 'prepare')
        self.assertEqual([name for name, _ in plans[0]], ['messaging', 'communications', 'calling'])
        self.assertEqual(envs['messaging'][sp.FAST], 'false')
        self.assertEqual(envs['messaging'][sp.PREPARED], 'true')
        self.assertEqual(len({env[sp.GENERATION] for env in envs.values()}), 1)
        uuid.UUID(envs['messaging'][sp.GENERATION])

    def test_enable_requires_all_acks_and_consumes_the_prepared_generation(self):
        cr, envs, plans, facts = self.fake()
        with patch.object(sp, 'facts', facts):
            with self.assertRaises(RuntimeError):
                sp.activate(cr, 'sha', '1', 'enable')
            sp.activate(cr, 'sha', '2', 'prepare')
            sp.activate(cr, 'sha', '3', 'enable')
        self.assertEqual([name for name, _ in plans[-1]], ['messaging'])
        self.assertEqual(envs['messaging'][sp.FAST], 'true')
        self.assertEqual(envs['messaging'][sp.PREPARED], 'false')

    def test_generation_mismatch_and_old_receiver_refuse_before_mutation(self):
        for broken in ('generation', 'protocol', 'source'):
            cr, envs, plans, facts = self.fake()
            with patch.object(sp, 'facts', facts):
                sp.activate(cr, 'sha', '1', 'prepare')
                if broken == 'generation':
                    envs['calling'][sp.GENERATION] = str(uuid.uuid4())
                elif broken == 'source':
                    envs['calling'][sp.SOURCE_FLAGS['calling']] = 'false'
                def changed(cr, name):
                    value = facts(cr, name)
                    if broken == 'protocol' and name == 'messaging':
                        value['sendPolicyProtocol'] = None
                    return value
                with patch.object(sp, 'facts', changed), self.assertRaises(RuntimeError):
                    sp.activate(cr, 'sha', '2', 'enable')
            self.assertEqual(len(plans), 1)

    def test_disable_keeps_producers_synchronous_and_requires_a_new_prepare(self):
        cr, envs, _, facts = self.fake()
        with patch.object(sp, 'facts', facts):
            sp.activate(cr, 'sha', '1', 'prepare')
            first = envs['messaging'][sp.GENERATION]
            sp.activate(cr, 'sha', '2', 'enable')
            with self.assertRaisesRegex(RuntimeError, 'Disable'):
                sp.require_remote_path(cr)
            sp.activate(cr, 'sha', '3', 'disable')
            sp.require_remote_path(cr)
            self.assertTrue(all(envs[n][f] == 'true' for n, f in sp.SOURCE_FLAGS.items()))
            with self.assertRaises(RuntimeError):
                sp.activate(cr, 'sha', '4', 'enable')
            sp.activate(cr, 'sha', '5', 'prepare')
            self.assertNotEqual(first, envs['messaging'][sp.GENERATION])

    def test_unrecognized_confirmation_cannot_touch_host_state(self):
        cr, _, plans, _ = self.fake()
        with self.assertRaises(RuntimeError):
            sp.activate(cr, 'sha', '1', 'yes')
        cr.require_release.assert_not_called()
        self.assertEqual(plans, [])

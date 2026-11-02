import copy
import unittest
import json
import tempfile
from pathlib import Path
from unittest.mock import patch

import production as p


class ActivationSafety(unittest.TestCase):
    def evidence(self):
        return dict(schema=1,status='PRODUCTION_CERTIFIED',timestamp=1000,
            images={'calling':'sha256:original'},git_sha='a'*40,
            rollback_reference='/protected/rollback',acceptance={k:'PASS' for k in p.REQUIRED})

    def test_infrastructure_success_cannot_activate(self):
        evidence = self.evidence()
        evidence['status'] = 'INFRASTRUCTURE_ONLY_NOT_CERTIFIED'
        with self.assertRaises(AssertionError):
            p.validate_acceptance(evidence,1001,evidence['images'])

    def test_marker_and_pass_labels_without_receipts_cannot_activate(self):
        evidence = self.evidence()
        with self.assertRaises(AssertionError):
            p.validate_acceptance(evidence,1001,evidence['images'])

    def test_stale_evidence_and_changed_images_cannot_activate(self):
        evidence = self.evidence()
        for now,images in ((900,evidence['images']),(100000,evidence['images']),(1001,{'calling':'new'})):
            with self.subTest(now=now,images=images), self.assertRaises(AssertionError):
                p.validate_acceptance(evidence,now,images)

    def test_every_production_scenario_is_required(self):
        for name in p.REQUIRED:
            evidence = self.evidence()
            evidence['acceptance'][name] = 'NOT_RUN'
            with self.subTest(name=name), self.assertRaises(AssertionError):
                p.validate_acceptance(evidence,1001,evidence['images'])

    def test_real_receipt_hashes_are_required_and_tampering_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(p,'ROOT',Path(directory)):
            base = Path(directory)/'production-evidence'
            base.mkdir()
            evidence = self.evidence()
            evidence['receipts'] = {}
            for name in p.REQUIRED:
                path = base/(name+'.json')
                path.write_text(json.dumps(dict(scenario=name,result='PASS',git_sha=evidence['git_sha'],
                    images=evidence['images'],started_at=900,finished_at=999,assertions=['controlled test assertion'])))
                evidence['receipts'][name] = dict(path=str(path),sha256=p.hashlib.sha256(path.read_bytes()).hexdigest())
            p.validate_acceptance(evidence,1001,evidence['images'])
            path.write_text('{}')
            with self.assertRaises(AssertionError):
                p.validate_acceptance(evidence,1001,evidence['images'])


class RuntimePreservation(unittest.TestCase):
    def container(self):
        return {'Id':'a'*64,'Image':'sha256:original','Config':{
            'Image':'moving:tag','Env':['SECRET_FILE=/run/secret','CUSTOM=keep'],
            'Cmd':['/app/calling','--custom'],'Entrypoint':None,'User':'10001',
            'Healthcheck':{'Test':['CMD','/health']}},
            'HostConfig':{'AutoRemove':False,'PortBindings':{},'RestartPolicy':{'Name':'unless-stopped'},
                'Binds':['/source:/target:ro'],'Memory':123,'NanoCpus':456,'SecurityOpt':['no-new-privileges']},
            'Mounts':[{'Source':'/source','Destination':'/target','RW':False}],
            'NetworkSettings':{'Networks':{'private':{'Aliases':['calling','a'*12],'IPAMConfig':None},
                'ingress':{'Aliases':['calling-ingress'],'IPAMConfig':None}}}}

    def test_clone_retains_all_startup_security_network_and_mount_settings(self):
        container = self.container()
        original = copy.deepcopy(container)
        cloned = p.clone_payload(container,{'CALLING_CAPTURE_PROVIDER':'livekit-egress'})
        self.assertEqual(container,original)
        self.assertEqual(cloned['Image'],'sha256:original')
        self.assertEqual(cloned['Cmd'],container['Config']['Cmd'])
        self.assertEqual(cloned['User'],'10001')
        self.assertEqual(cloned['Healthcheck'],container['Config']['Healthcheck'])
        for k,v in container['HostConfig'].items():
            self.assertEqual(cloned['HostConfig'][k],v)
        self.assertIn('CUSTOM=keep',cloned['Env'])
        self.assertIn('SECRET_FILE=/run/secret',cloned['Env'])
        self.assertEqual(set(cloned['NetworkingConfig']['EndpointsConfig']),{'private','ingress'})
        self.assertEqual(cloned['NetworkingConfig']['EndpointsConfig']['private']['Aliases'],['calling'])
        self.assertEqual(cloned['HostConfig']['Mounts'][0]['ReadOnly'],True)

    def test_static_network_and_public_ports_require_explicit_safe_handling(self):
        for change in ('ip','port'):
            container = self.container()
            if change=='ip':
                container['NetworkSettings']['Networks']['private']['IPAMConfig']={'IPv4Address':'172.20.0.10'}
            else:
                container['HostConfig']['PortBindings']={'8090/tcp':[{'HostPort':'8090'}]}
            with self.subTest(change=change), self.assertRaises(AssertionError):
                p.clone_payload(container,{})


if __name__ == '__main__':
    unittest.main()

import copy
import unittest
from unittest.mock import patch
import communications_release as release


class CommunicationsRuntimePreservation(unittest.TestCase):
    def original(self):
        return {'Id':'a'*64,'Image':'sha256:old','Config':{'Image':'old:tag','Env':['KEEP=original','DATABASE_URL=private'],
            'Cmd':['./server'],'Entrypoint':None,'User':'10001'},
            'HostConfig':{'AutoRemove':False,'PortBindings':{'8080/tcp':[{'HostIp':'127.0.0.1','HostPort':'18081'}]},
                'Binds':['/private/key:/run/key:ro'],'Memory':1024,'RestartPolicy':{'Name':'unless-stopped'}},
            'NetworkSettings':{'Networks':{'private':{'Aliases':['cfmeet-production-backend','a'*64],'IPAMConfig':None}}}}

    def test_candidate_never_publishes_ports_or_claims_production_dns_alias(self):
        old=self.original();snapshot=copy.deepcopy(old)
        candidate=release.clone(old,'cfmeet-comm-stage-meeting-service','sha256:new',{'DATABASE_URL':'isolated'},True)
        self.assertEqual(candidate['HostConfig']['PortBindings'],{})
        self.assertEqual(candidate['NetworkingConfig']['EndpointsConfig']['private']['Aliases'],['cfmeet-comm-stage-meeting-service'])
        self.assertEqual(candidate['HostConfig']['Binds'],old['HostConfig']['Binds'])
        self.assertIn('KEEP=original',candidate['Env'])
        self.assertIn('DATABASE_URL=isolated',candidate['Env'])
        self.assertEqual(old,snapshot)

    def test_cutover_preserves_loopback_ingress_mounts_startup_and_networks(self):
        old=self.original()
        candidate=release.clone(old,'cfmeet-production-backend','sha256:new',{'NEW':'configured'},False)
        self.assertEqual(candidate['HostConfig'],old['HostConfig'])
        self.assertEqual(candidate['Cmd'],old['Config']['Cmd'])
        self.assertEqual(candidate['Entrypoint'],old['Config']['Entrypoint'])
        self.assertEqual(candidate['User'],old['Config']['User'])
        self.assertEqual(set(candidate['NetworkingConfig']['EndpointsConfig']),set(old['NetworkSettings']['Networks']))

    def test_external_backend_binding_and_static_ip_are_refused(self):
        old=self.original();old['HostConfig']['PortBindings']['8080/tcp'][0]['HostIp']='0.0.0.0'
        with self.assertRaises(AssertionError):release.clone(old,'name','sha256:new',{},False)
        old=self.original();old['NetworkSettings']['Networks']['private']['IPAMConfig']={'IPv4Address':'10.0.0.2'}
        with self.assertRaises(AssertionError):release.clone(old,'name','sha256:new',{},False)

    def test_revision_label_must_match_immutable_source_sha(self):
        with self.assertRaises(AssertionError):release.image('calling','main')
        with patch.object(release.p,'run',return_value='[{"Id":"sha256:test","Config":{"Labels":{"org.opencontainers.image.revision":"wrong"}}}]'):
            with self.assertRaises(AssertionError):release.image('calling','a'*40)

    def test_database_names_cannot_escape_feature_owned_namespace(self):
        with self.assertRaises(AssertionError):release.database('cfmeet_calling')
        with self.assertRaises(AssertionError):release.database('cfmeet_comm_a; DROP DATABASE x')


if __name__=='__main__':unittest.main()

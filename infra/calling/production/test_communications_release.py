import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import communications_release as release


class CommunicationsRuntimePreservation(unittest.TestCase):
    def test_staging_renews_expired_acceptance_credential_without_recreating_scope(self):
        with tempfile.TemporaryDirectory() as folder:
            work=Path(folder)
            cached={'tenantId':'preserved','token':'expired-fixture'}
            (work/'stage-context.private.json').write_text(json.dumps(cached))
            def sql(db, query):
                return json.dumps({'tenantId':'preserved'}) if 'json_build_object' in query else '1'
            with patch.object(release,'WORK',work), patch.object(release,'db_sql',side_effect=sql), \
                 patch.object(release,'stage_admin',return_value={'secret':'fresh-fixture'}) as admin:
                value=release.stage_scope('a'*40)
            self.assertEqual(value['tenantId'],'preserved')
            self.assertEqual(value['token'],'fresh-fixture')
            creates=[call.args[1:3] for call in admin.call_args_list if call.args[2]=='create']
            self.assertEqual(creates,[('credential','create')])
            self.assertEqual(json.loads((work/'stage-context.private.json').read_text()),value)
    def test_staging_never_writes_production_checkpoint_or_loads_its_managed_keyring(self):
        cfg={'Env':['KEEP=preserved','MESSAGING_MANAGED_KEYRING_FILE=/run/messaging/managed.ring'],
            'HostConfig':{'Mounts':[{'Type':'bind','Source':'/production/checkpoint','Target':'/var/lib/messaging-kt','ReadOnly':False},
                {'Type':'bind','Source':'/production/managed','Target':'/run/messaging/managed.ring','ReadOnly':True}]}}
        release.isolate_stage_messaging_state(cfg,'/stage/ring','/stage/checkpoint')
        self.assertIn('KEEP=preserved',cfg['Env'])
        self.assertEqual(sum(item.startswith('MESSAGING_MANAGED_KEYRING_FILE=') for item in cfg['Env']),1)
        self.assertNotIn('/production',str(cfg))
        self.assertIn('/stage/checkpoint',str(cfg['HostConfig']['Mounts']))
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

    def test_external_backend_binding_is_refused_and_static_ip_is_preserved(self):
        old=self.original();old['HostConfig']['PortBindings']['8080/tcp'][0]['HostIp']='0.0.0.0'
        with self.assertRaises(AssertionError):release.clone(old,'name','sha256:new',{},False)
        old=self.original();old['NetworkSettings']['Networks']['private']['IPAMConfig']={'IPv4Address':'10.0.0.2'}
        candidate=release.clone(old,'stage','sha256:new',{},True)
        self.assertNotIn('IPAMConfig',candidate['NetworkingConfig']['EndpointsConfig']['private'])
        production=release.clone(old,'name','sha256:new',{},False)
        self.assertEqual(production['NetworkingConfig']['EndpointsConfig']['private']['IPAMConfig'],{'IPv4Address':'10.0.0.2'})

    def test_production_network_with_null_aliases_keeps_the_service_dns_name(self):
        old=self.original();old['NetworkSettings']['Networks']['private']['Aliases']=None
        candidate=release.clone(old,'cfmeet-platform-access','sha256:new',{},False)
        self.assertEqual(candidate['NetworkingConfig']['EndpointsConfig']['private']['Aliases'],['cfmeet-platform-access'])

    def test_revision_label_must_match_immutable_source_sha(self):
        with self.assertRaises(AssertionError):release.image('calling','main')
        with patch.object(release.p,'run',return_value='[{"Id":"sha256:test","Config":{"Labels":{"org.opencontainers.image.revision":"wrong"}}}]'):
            with self.assertRaises(AssertionError):release.image('calling','a'*40)

    def test_database_names_cannot_escape_feature_owned_namespace(self):
        with self.assertRaises(AssertionError):release.database('cfmeet_calling')
        with self.assertRaises(AssertionError):release.database('cfmeet_comm_a; DROP DATABASE x')

    def test_public_ingress_denies_only_private_account_routes_and_preserves_existing_ingress(self):
        original='server {\n include /etc/nginx/snippets/cfmeet-calling-routes.conf;\n location / { proxy_pass http://127.0.0.1:18081; }\n}'
        candidate=release.public_ingress_config(original)
        self.assertIn('location ^~ /internal/v1/accounts/communications/ { return 404; }',candidate)
        self.assertIn('location / { proxy_pass http://127.0.0.1:18081; }',candidate)
        self.assertEqual(release.public_ingress_config(candidate),candidate)
        with self.assertRaises(AssertionError):release.public_ingress_config(candidate.replace('return 404','return 200'))

    def test_public_ingress_refuses_unknown_or_duplicate_site_includes(self):
        for value in ('server {}','include /etc/nginx/snippets/cfmeet-calling-routes.conf;\n'*2):
            with self.assertRaises(AssertionError):release.public_ingress_config(value)


if __name__=='__main__':unittest.main()

import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import communications_release as release


SA='11111111-1111-4111-8111-111111111111'
NOW=release.datetime(2026,11,6,tzinfo=release.timezone.utc)


def launch_view(**over):
    v=dict(id='22222222-2222-4222-8222-222222222222',publicId='ab'*12,label=release.LAUNCH_LABEL,effectiveState='ACTIVE',
        environmentKind='PRODUCTION',serviceAccountId=SA,scopes=['calls.write'],expiresAt='2027-11-06T00:00:00Z')
    v.update(over);return v


class CommunicationsLaunchCredential(unittest.TestCase):
    def test_only_an_active_dedicated_calls_write_credential_of_the_scope_account_is_reused(self):
        self.assertTrue(release.usable_launch_credential(launch_view(),SA,NOW))
        self.assertTrue(release.usable_launch_credential(launch_view(expiresAt='2027-11-06T04:41:50.123456789Z'),SA,NOW))
        for bad in (dict(label='Sections 4-7 native policy acceptance'),dict(effectiveState='REVOKED'),
                dict(effectiveState='SUSPENDED'),dict(environmentKind='TEST'),
                dict(serviceAccountId='33333333-3333-4333-8333-333333333333'),
                dict(scopes=['calls.write','messages.send']),dict(scopes=['calls.read']),
                dict(expiresAt='2026-11-20T00:00:00Z')):
            self.assertFalse(release.usable_launch_credential(launch_view(**bad),SA,NOW),bad)

    def admin_double(self,listed):
        issued=[]
        def admin(*args):
            if args[:2]==('credential','list'):return listed
            if args[:2]==('credential','create'):
                issued.append(args)
                return {'credential':launch_view(id='44444444-4444-4444-8444-444444444444'),'secret':'never-kept'}
            return {}
        return admin,issued

    def run_launch(self,listed):
        import api_acceptance
        admin,issued=self.admin_double(listed)
        with tempfile.TemporaryDirectory() as folder, patch.object(release,'WORK',Path(folder)), \
             patch.object(api_acceptance,'sql',return_value=json.dumps({'tenant':'t','application':'personal','account':'hub'})), \
             patch.object(api_acceptance,'admin',side_effect=admin), patch.object(release.os,'chown'):
            value=release.launch_credential({'serviceAccountId':SA},NOW)
            stored=(Path(folder)/'launch-credential.json').read_text()
        self.assertNotIn('never-kept',stored)
        return value,issued,json.loads(stored)

    def test_existing_launch_credential_is_reused_without_issuing(self):
        value,issued,stored=self.run_launch([launch_view(),launch_view(label='other')])
        self.assertEqual(value,'22222222-2222-4222-8222-222222222222')
        self.assertEqual(issued,[])
        self.assertEqual(stored['scopes'],['calls.write'])

    def test_missing_or_expiring_launch_credential_is_issued_with_calls_write_only(self):
        value,issued,stored=self.run_launch([launch_view(expiresAt='2026-11-10T00:00:00Z')])
        self.assertEqual(value,'44444444-4444-4444-8444-444444444444')
        self.assertEqual(len(issued),1)
        args=list(issued[0])
        self.assertEqual([args[i+1] for i,a in enumerate(args) if a=='--scope'],['calls.write'])
        self.assertIn('365',args)
        self.assertEqual(stored['credentialId'],value)

    def test_calling_receives_only_the_launch_credential_id(self):
        scope={'tenantId':'t','applicationId':'a','environmentId':'e','launchCredentialId':'22222222-2222-4222-8222-222222222222'}
        original={'Id':'a'*64,'Config':{'Env':['KEEP=1']},'HostConfig':{'AutoRemove':False},'NetworkSettings':{'Networks':{}}}
        with patch.object(release.p,'inspect',return_value=original), patch.object(release,'image',return_value='sha256:new'):
            _,cfg=release.configured('calling','a'*40,False,scope)
            _,messaging=release.configured('messaging','a'*40,False,scope)
            del scope['launchCredentialId']
            _,unset=release.configured('calling','a'*40,False,scope)
        self.assertIn('CALLING_COMMUNICATIONS_LAUNCH_CREDENTIAL_ID=22222222-2222-4222-8222-222222222222',cfg['Env'])
        self.assertFalse(any('LAUNCH_CREDENTIAL' in x for x in messaging['Env']+unset['Env']))


class LaunchCredentialMonitoring(unittest.TestCase):
    RECORD={'credentialId':'22222222-2222-4222-8222-222222222222','scopes':['calls.write'],'expiresAt':'2027-11-06T04:41:50.123456789Z'}

    def test_status_reports_days_left_and_asks_for_renewal_in_time(self):
        ok=release.launch_credential_status(NOW,self.RECORD)
        self.assertEqual((ok['remainingDays'],ok['status']),(365,'OK'))
        soon=release.launch_credential_status(release.datetime(2027,10,1,tzinfo=release.timezone.utc),self.RECORD)
        self.assertEqual(soon['status'],'RENEW')

    def test_daily_monitor_is_valid_and_alerts_below_the_threshold(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(release,'WORK',Path(folder)):
            for expires,code in (('2099-01-01T00:00:00Z',0),('2026-01-01T00:00:00.5Z',1)):
                (Path(folder)/'launch-credential.json').write_text(json.dumps(dict(self.RECORD,expiresAt=expires)))
                script=Path(folder)/'monitor.py';script.write_text(release.launch_monitor_source())
                import subprocess
                r=subprocess.run(['python3',str(script)],capture_output=True,text=True)
                self.assertEqual(r.returncode,code,r.stderr)
                self.assertNotIn('secret',r.stdout.lower())


class CommunicationsRuntimePreservation(unittest.TestCase):
    def test_staging_renews_expired_acceptance_credential_without_recreating_scope(self):
        with tempfile.TemporaryDirectory() as folder:
            work=Path(folder)
            cached={'tenantId':'preserved','token':'expired-fixture'}
            (work/'stage-context.private.json').write_text(json.dumps(cached))
            def sql(db, query):
                return json.dumps({'tenantId':'preserved'}) if 'json_build_object' in query else '1'
            with patch.object(release,'WORK',work), patch.object(release,'db_sql',side_effect=sql), \
                 patch.object(release,'stage_admin',return_value={'secret':'fresh-fixture','credential':{'id':'00000000-0000-4000-8000-000000000001'}}) as admin:
                value=release.stage_scope('a'*40)
            self.assertEqual(value['tenantId'],'preserved')
            self.assertEqual(value['token'],'fresh-fixture')
            self.assertEqual(value['launchCredentialId'],'00000000-0000-4000-8000-000000000001')
            creates=[call.args[1:3] for call in admin.call_args_list if call.args[2]=='create']
            self.assertEqual(creates,[('credential','create')])
            self.assertEqual(json.loads((work/'stage-context.private.json').read_text()),value)
    def test_staging_never_writes_production_checkpoint_or_loads_its_managed_keyring(self):
        cfg={'Env':['KEEP=preserved','MESSAGING_MANAGED_KEYRING_FILE=/run/messaging/managed.ring',
                'MESSAGING_REALTIME_REDIS_URL=redis://private-production',
                'MESSAGING_ATTACHMENTS_S3_SECRET_ACCESS_KEY_FILE=/run/production-storage',
                'MESSAGING_PUSH_PROVIDER=fcm','MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE=/run/production-push'],
            'HostConfig':{'Binds':None,'Mounts':[{'Type':'bind','Source':'/production/checkpoint','Target':'/var/lib/messaging-kt','ReadOnly':False},
                {'Type':'bind','Source':'/production/storage','Target':'/run/production-storage','ReadOnly':True},
                {'Type':'bind','Source':'/production/push','Target':'/run/production-push','ReadOnly':True},
                {'Type':'bind','Source':'/production/managed','Target':'/run/messaging/managed.ring','ReadOnly':True}]}}
        release.isolate_stage_messaging_state(cfg,'/stage/ring','/stage/checkpoint')
        self.assertIn('KEEP=preserved',cfg['Env'])
        self.assertEqual(sum(item.startswith('MESSAGING_MANAGED_KEYRING_FILE=') for item in cfg['Env']),1)
        self.assertNotIn('/production',str(cfg))
        self.assertNotIn('redis://',str(cfg))
        self.assertNotIn('MESSAGING_ATTACHMENTS_S3_',str(cfg))
        self.assertIn('MESSAGING_PUSH_PROVIDER=none',cfg['Env'])
        self.assertIn('MESSAGING_KT_LOG_ID=cfmeet.messaging:isolated-release-test',cfg['Env'])
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
        host=dict(candidate['HostConfig']);log=host.pop('LogConfig')
        self.assertEqual(host,{k:v for k,v in old['HostConfig'].items() if k!='LogConfig'})
        self.assertEqual(log,{'Type':'json-file','Config':{'max-size':'20m','max-file':'5'}})
        old['HostConfig']['LogConfig']={'Type':'json-file','Config':{'max-size':'50m'}}
        self.assertEqual(release.clone(old,'n','sha256:new',{},False)['HostConfig']['LogConfig'],old['HostConfig']['LogConfig'])
        old['HostConfig']['LogConfig']={'Type':'journald','Config':{}}
        self.assertEqual(release.clone(old,'n','sha256:new',{},False)['HostConfig']['LogConfig'],{'Type':'journald','Config':{}})
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

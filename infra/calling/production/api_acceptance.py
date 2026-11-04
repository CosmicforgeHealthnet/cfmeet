"""Prepare isolated PRODUCTION application identities; exercise the existing Calling API."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.request
import urllib.error
import sys
import uuid
import production as p
import app_acceptance

SCOPES=['calls.read','calls.write','enterprise.calls.use','enterprise.config.read','enterprise.config.write',
    'enterprise.voicemail.read','enterprise.voicemail.manage','enterprise.recording.read','enterprise.recording.manage']


def admin(*args):
    image=p.inspect('cfmeet-platform-access')['Image']
    return json.loads(p.run('docker','run','--rm','--network',p.NET,'--env-file',str(p.ROOT/'platform-access.runtime.env'),
        '--mount',f'type=bind,src={p.SECRETS}/platform-access-pepper,dst=/run/secrets/platform-pepper,readonly',
        '--entrypoint','/app/platform-access-admin',image,'--operator','cfmeet-production-acceptance',*args))


def sql(query):
    return p.run('docker','exec','-u','postgres','cfmeet-core-db','psql','-XAtq','-v','ON_ERROR_STOP=1',
        '-U','postgres','-d','cfmeet_platform_access','-c',query)


def context():
    file=p.SECRETS/'calling-api-acceptance.json'
    if file.exists():
        value=json.loads(file.read_text())
        expired=value.get('credentialExpiresAt',0)<time.time()+900
        if expired:
            # A paused release may outlive the one-day fixture credentials.
            # Renew only the two explicitly owned test service accounts; retain
            # their principal mappings, scopes and customer-independent history.
            for key,tenant in (('actors','cfmeet-calling-acceptance'),('other','cfmeet-calling-isolation')):
                assert sql("SELECT id FROM platform_tenants WHERE slug='"+tenant+"'")==str(uuid.UUID(value[key]['tenantId']))
                credential=admin('credential','create','--tenant',tenant,'--application','suite',
                    '--environment','production','--service-account','acceptance','--expires-in-days','1',
                    *[v for scope in SCOPES for v in ('--scope',scope)],'--label','Renewed controlled production acceptance')
                value[key]['token']=credential['secret']
        if expired or 'restrictedToken' not in value:
            credential=admin('credential','create','--tenant','cfmeet-calling-acceptance','--application','suite',
                '--environment','production','--service-account','acceptance','--expires-in-days','1',
                '--scope','calls.read','--label','Controlled scope denial acceptance')
            value['restrictedToken']=credential['secret']
        if expired:value['credentialExpiresAt']=time.time()+86400
        p.write_private(file,json.dumps(value))
        return value
    users=app_acceptance.accounts()
    tenants=['cfmeet-calling-acceptance','cfmeet-calling-isolation']
    records=[]
    for tenant in tenants:
        if sql("SELECT count(*) FROM platform_tenants WHERE slug='"+tenant+"'")=='0':
            admin('tenant','create','--slug',tenant,'--name','CF Meet Calling acceptance','--classification','first-party')
        if sql("SELECT count(*) FROM platform_applications a JOIN platform_tenants t ON t.id=a.tenant_id WHERE t.slug='"+tenant+"' AND a.slug='suite'")=='0':
            admin('application','create','--tenant',tenant,'--slug','suite','--name','Controlled production acceptance')
        path=['--tenant',tenant,'--application','suite','--environment','production']
        if sql("SELECT count(*) FROM platform_environments e JOIN platform_applications a ON a.id=e.application_id JOIN platform_tenants t ON t.id=a.tenant_id WHERE t.slug='"+tenant+"' AND e.kind='PRODUCTION'")=='0':
            admin('environment','create',*path)
        if sql("SELECT count(*) FROM platform_service_accounts s JOIN platform_tenants t ON t.id=s.tenant_id WHERE t.slug='"+tenant+"' AND s.name='acceptance'")=='0':
            admin('service-account','create',*path,'--name','acceptance')
        for scope in SCOPES:
            admin('scope','grant','--tenant',tenant,'--application','suite','--scope',scope,'--environment-kind','production')
        credential=admin('credential','create',*path,'--service-account','acceptance','--expires-in-days','1',
            *[v for scope in SCOPES for v in ('--scope',scope)],'--label','Controlled production acceptance')
        principals=[admin('principal','register',*path,'--kind','native-cf-user','--reference',u['userId'])['principal'] for u in users]
        records.append(dict(token=credential['secret'],principals=[x['id'] for x in principals],
            tenantId=principals[0]['tenantId'],applicationId=principals[0]['applicationId'],environmentId=principals[0]['environmentId']))
    value={'actors':records[0],'other':records[1],'credentialExpiresAt':time.time()+86400}
    p.write_private(file,json.dumps(value));return context()


def compatibility():
    users=app_acceptance.accounts()
    token=app_acceptance.api('login',{'email':users[0]['email'],'password':users[0]['password']})['access_token']
    def fetch(path,body=None,authorized=True,expected=200):
        headers={'Content-Type':'application/json'}
        if authorized:headers['Authorization']='Bearer '+token
        req=urllib.request.Request('https://api.cfmeet.com'+path,headers=headers,
            data=json.dumps(body).encode() if body is not None else None)
        try:
            with urllib.request.urlopen(req,timeout=20) as response:status=response.status;value=json.load(response)
        except urllib.error.HTTPError as error:status=error.code;value={}
        assert status==expected,'Compatibility check failed: '+path+' HTTP '+str(status)
        return value
    fetch('/api/v1/users/me')
    identity=fetch('/api/v2/messaging/me')
    assert all(identity.get(k) for k in ('principalId','tenantId','applicationId','environmentId'))
    directory=fetch('/api/v2/messaging/directory')
    assert isinstance(directory['people'],list)
    fetch('/api/v2/messaging/directory/resolve',{'userIds':[users[1]['userId']]},expected=404)
    fetch('/api/v2/messaging/relay/v1/conversations')
    fetch('/api/v1/meetings')
    for path in ('/api/v2/messaging/me','/api/v1/meetings'):
        fetch(path,authorized=False,expected=401)
    return {'native_identity':'PASS','messaging_authenticated_api':'PASS','contacts_directory_isolation':'PASS',
        'meetings_authenticated_api':'PASS','anonymous_backend_access_denied':'PASS'}


def expire_fixture(value,request):
    """Accelerate only a READY artifact of the isolated acceptance tenant.

    The production worker must perform the real audited retention transition and
    storage deletion. No customer policy, clock, state or object key is changed.
    """
    table={'recording':'call_recordings','voicemail':'voicemail_messages'}[request['kind']]
    actor=value['actors']
    ids={k:str(uuid.UUID(actor[k])) for k in ('tenantId','applicationId','environmentId')}
    artifact=str(uuid.UUID(request['artifactId']));call=str(uuid.UUID(request['callId']))
    assert sql("SELECT id FROM platform_tenants WHERE slug='cfmeet-calling-acceptance'")==ids['tenantId']
    query=(f"UPDATE {table} SET retention_until=now()-interval '1 second',version=version+1,updated_at=GREATEST(updated_at,now()) WHERE id='{artifact}' AND call_id='{call}' "
        f"AND tenant_id='{ids['tenantId']}' AND application_id='{ids['applicationId']}' "
        f"AND environment_id='{ids['environmentId']}' AND state='READY' RETURNING id")
    changed=p.run('docker','exec','-u','postgres','cfmeet-core-db','psql','-XAtq','-v','ON_ERROR_STOP=1',
        '-U','postgres','-d','cfmeet_calling','-c',query)
    assert changed==artifact,'Retention acceleration refused outside the READY isolated fixture'
    print('RETENTION_FIXTURE: isolated artifact deadline accelerated; production worker remains authoritative')


def cleanup_fixture_calls(value):
    """End abandoned controlled calls through the API, never through SQL state updates."""
    actor=value['actors'];ids={k:str(uuid.UUID(actor[k])) for k in ('tenantId','applicationId','environmentId')}
    assert sql("SELECT id FROM platform_tenants WHERE slug='cfmeet-calling-acceptance'")==ids['tenantId']
    query=(f"SELECT id FROM calls WHERE tenant_id='{ids['tenantId']}' AND application_id='{ids['applicationId']}' "
        f"AND environment_id='{ids['environmentId']}' AND current_state NOT IN ('ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED')")
    def fetch(path,body=None):
        req=urllib.request.Request('https://api.cfmeet.com'+path,
            data=json.dumps(body).encode() if body is not None else None,
            headers={'Authorization':'Bearer '+actor['token'],'Content-Type':'application/json','Idempotency-Key':str(uuid.uuid4())})
        try:
            with urllib.request.urlopen(req,timeout=20) as response:return response.status,json.load(response)
        except urllib.error.HTTPError as error:return error.code,{}
    for _ in range(40):
        calls=p.run('docker','exec','-u','postgres','cfmeet-core-db','psql','-XAtq','-v','ON_ERROR_STOP=1',
            '-U','postgres','-d','cfmeet_calling','-c',query).splitlines()
        if not calls:return
        for call in calls:
            call=str(uuid.UUID(call));status,view=fetch('/v1/calls/'+call)
            assert status==200,'Controlled call cleanup could not read its own call'
            if view['state']=='ENDING':continue
            verb='end' if view['state'] in ('CONNECTED','HELD','ANSWERED','TRANSFERRING') else 'cancel'
            status,_=fetch('/v1/calls/'+call+'/'+verb,{'expectedVersion':view['version']})
            assert status in (200,409),'Controlled call cleanup was refused'
        time.sleep(.5)
    raise AssertionError('Abandoned controlled calls did not terminate')


def retire_abandoned_probe():
    try:container=p.inspect('cfmeet-calling-api-probe')
    except RuntimeError:return
    assert container['Config']['Image'].startswith('cfmeet-calling-api-probe:')
    root=Path(next(m['Source'] for m in container['Mounts'] if m['Destination']=='/result')).resolve()
    assert root.parent==(p.ROOT/'production-evidence').resolve() and root.name.startswith('api-')
    assert '/api_client.py' in container['Config']['Cmd']
    p.run('docker','stop','--time','10','cfmeet-calling-api-probe')
    print('ABANDONED_PROBE: verified isolated acceptance container stopped')


def run(run_id,feature='enterprise'):
    original=p.inspect('cfmeet-calling')['Id']
    try:
        _run(run_id,feature)
    except BaseException:
        try:cleanup_fixture_calls(context())
        finally:
            if p.inspect('cfmeet-calling')['Id']!=original:
                journal=json.loads((p.ROOT/'calling-activation-rollback.json').read_text())
                if journal['original']['Id']==original:
                    p.rollback_calling()
        raise


def _run(run_id,feature='enterprise'):
    p.identity(); value=context()
    retire_abandoned_probe()
    # Surface only known-safe assertions left by an interrupted SSH session.
    previous=sorted((p.ROOT/'production-evidence').glob('api-*/api-result.json'),key=lambda f:f.stat().st_mtime)[-1:]
    for file in previous:
        report=json.loads(file.read_text())
        print('PREVIOUS_ACCEPTANCE:',json.dumps({k:report[k] for k in ('phase','checks','error_type','failure_message') if k in report}))
    cleanup_fixture_calls(value)
    # Enabling an independent carrier-free feature never depends on postponed backups.
    active=p.run('docker','exec','-u','postgres','cfmeet-core-db','psql','-XAtq','-U','postgres','-d','cfmeet_calling',
        '-c',"SELECT count(*) FROM calls WHERE current_state NOT IN ('ENDED','DECLINED','BUSY','NO_ANSWER','FAILED','CANCELLED')")
    assert active=='0', 'Active calls exist; runtime restart safely deferred'
    before=p.env_of(p.inspect('cfmeet-calling'))
    overrides={'enterprise':{'ENTERPRISE_CALLING_ENABLED':'true'},'recording':{'CALL_RECORDING_ENABLED':'true'},
        'voicemail':{'VOICEMAIL_ENABLED':'true','ENTERPRISE_CALLING_ENABLED':'true'}}[feature]
    old_id=p.inspect('cfmeet-calling')['Id']
    if feature!='enterprise':
        p.run('docker','exec','cfmeet-calling','/app/platform_admin','--operator','cfmeet-production-acceptance',
            'enterprise-capabilities','set','--tenant','cfmeet-calling-acceptance','--application','suite','--environment','production',
            '--capability','manage-routing','--capability','manage-voicemail','--capability','manage-recording')
    p.recreate_calling(overrides,require_acceptance=False)
    root=p.ROOT/'production-evidence'/('api-'+run_id);root.mkdir(parents=True,exist_ok=False,mode=0o700);os.chown(root,10001,10001)
    private=root/'context.private.json';p.write_private(private,json.dumps(value));os.chown(private,10001,10001)
    source=Path(__file__).parent
    p.run('docker','pull','python:3.12-slim')
    base=json.loads(p.run('docker','image','inspect','python:3.12-slim'))[0]['RepoDigests'][0]
    tag='cfmeet-calling-api-probe:'+hashlib.sha256((source/'Probe.Dockerfile').read_bytes()+base.encode()).hexdigest()[:20]
    p.run('docker','build','--network','default','--build-arg','PYTHON_IMAGE='+base,'-f',str(source/'Probe.Dockerfile'),'-t',tag,str(source))
    command=['docker','run','--rm','--name','cfmeet-calling-api-probe','--network','bridge','--memory','768m','--cpus','1',
        '--tmpfs','/tmp:rw,nosuid,size=256m,uid=10001,gid=10001','--mount',f'type=bind,src={root},dst=/result',
        '--mount',f'type=bind,src={source}/api_client.py,dst=/api_client.py,readonly',
        '--mount',f'type=bind,src={source}/rtc_client.py,dst=/rtc_client.py,readonly',
        '--mount',f'type=bind,src={source}/capture_scenarios.py,dst=/capture_scenarios.py,readonly',tag,'python','/api_client.py',feature]
    with (root/'execution.private.log').open('w') as log:
        process=subprocess.Popen(command,stdout=log,stderr=subprocess.STDOUT)
        deadline=time.monotonic()+900
        next_progress=time.monotonic()+30
        try:
            while process.poll() is None:
                pending=root/'retention-request.json'
                if pending.exists():
                    expire_fixture(value,json.loads(pending.read_text()))
                    pending.rename(root/'retention-request.handled.json')
                assert time.monotonic()<deadline,'Production acceptance timed out'
                if time.monotonic()>=next_progress:
                    print('API_ACCEPTANCE_RUNNING:',feature,'real media/API assertions in progress',flush=True)
                    next_progress=time.monotonic()+30
                time.sleep(.25)
        finally:
            if process.poll() is None:
                subprocess.run(['docker','stop','--time','10','cfmeet-calling-api-probe'],capture_output=True,timeout=25)
                process.wait(timeout=15)
    report=root/'api-result.json'
    if report.exists():
        record=json.loads(report.read_text())
        record.update(git_sha=sys.argv[2],feature=feature,workflow_run_id=run_id,
            images={n:p.inspect(n)['Image'] for n in ('cfmeet-calling','lk-server',p.EGRESS,'cfmeet-calling-artifacts')},
            effective_flags={k:p.env_of(p.inspect('cfmeet-calling')).get(k,'false') for k in sorted(p.SAFE_ENV) if k.endswith('_ENABLED')},
            offserver_backups='POSTPONED_BY_OWNER')
        p.write_private(report,json.dumps(record,indent=2));print(report.read_text())
    if process.returncode:
        if p.inspect('cfmeet-calling')['Id']!=old_id:p.rollback_calling()
        raise RuntimeError('Production '+feature+' acceptance failed; changed feature activation rolled back')
    p.regression();print(feature.upper()+': enabled after production API acceptance')

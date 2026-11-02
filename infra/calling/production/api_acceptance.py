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
        if 'restrictedToken' not in value:
            credential=admin('credential','create','--tenant','cfmeet-calling-acceptance','--application','suite',
                '--environment','production','--service-account','acceptance','--expires-in-days','1',
                '--scope','calls.read','--label','Controlled scope denial acceptance')
            value['restrictedToken']=credential['secret'];p.write_private(file,json.dumps(value))
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
    value={'actors':records[0],'other':records[1]}
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
    fetch('/api/v1/meetings/')
    for path in ('/api/v2/messaging/me','/api/v1/meetings/'):
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


def run(run_id,feature='enterprise'):
    original=p.inspect('cfmeet-calling')['Id']
    try:
        _run(run_id,feature)
    except BaseException:
        if p.inspect('cfmeet-calling')['Id']!=original:
            journal=json.loads((p.ROOT/'calling-activation-rollback.json').read_text())
            if journal['original']['Id']==original:
                p.rollback_calling()
        raise


def _run(run_id,feature='enterprise'):
    p.identity(); value=context()
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
        try:
            while process.poll() is None:
                pending=root/'retention-request.json'
                if pending.exists():
                    expire_fixture(value,json.loads(pending.read_text()))
                    pending.rename(root/'retention-request.handled.json')
                assert time.monotonic()<deadline,'Production acceptance timed out'
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

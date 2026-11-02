"""Prepare isolated PRODUCTION application identities; exercise the existing Calling API."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
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
    if file.exists(): return json.loads(file.read_text())
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
    p.write_private(file,json.dumps(value));return value


def run(run_id,feature='enterprise'):
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
    result=subprocess.run(['docker','run','--rm','--name','cfmeet-calling-api-probe','--network','bridge','--memory','768m','--cpus','1',
        '--tmpfs','/tmp:rw,nosuid,size=256m,uid=10001,gid=10001','--mount',f'type=bind,src={root},dst=/result',
        '--mount',f'type=bind,src={source}/api_client.py,dst=/api_client.py,readonly',
        '--mount',f'type=bind,src={source}/rtc_client.py,dst=/rtc_client.py,readonly',
        '--mount',f'type=bind,src={source}/capture_scenarios.py,dst=/capture_scenarios.py,readonly',tag,'python','/api_client.py',feature],
        capture_output=True,text=True,timeout=900)
    p.write_private(root/'execution.private.log',result.stdout+'\n'+result.stderr)
    report=root/'api-result.json'
    if report.exists(): print(report.read_text())
    if result.returncode:
        if p.inspect('cfmeet-calling')['Id']!=old_id:p.rollback_calling()
        raise RuntimeError('Production '+feature+' acceptance failed; changed feature activation rolled back')
    p.regression();print(feature.upper()+': enabled after production API acceptance')

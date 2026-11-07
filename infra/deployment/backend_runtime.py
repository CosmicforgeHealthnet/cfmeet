"""Release Account from its running configuration; retain a verified rollback.

Invoked by the existing Account deployment workflow after its CI and image build.
Secret-bearing Docker inspections and migration environment files stay on the VPS.
"""
import argparse
import json
from pathlib import Path
import os
import sys
import time
import urllib.request

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'calling'/'production'))
import production as p
from communications_release import clone, env_file, meeting_migration_url

NAME='cfmeet-production-backend'


def ready(name):
    code="import urllib.request; urllib.request.urlopen('http://"+name+":8080/health',timeout=4).read()"
    for attempt in range(40):
        try:
            p.run('docker','run','--rm','--network','container:'+name,'python:3.12-slim','python','-c',code.replace(name,'127.0.0.1'))
            return
        except RuntimeError:
            if attempt%10==0:print('Account candidate readiness pending',flush=True)
            time.sleep(2)
    raise RuntimeError('Account candidate failed readiness')


def verify_public():
    base='http://127.0.0.1:18081'
    for family in ('calling','messaging','communications','numbering','sms-gateway','developer-platform','api-gateway'):
        with urllib.request.urlopen(base+'/api-docs/'+family+'.json',timeout=8) as response:
            paths=json.load(response)['paths']
        assert paths and not any('/internal/' in x for x in paths),'Public Swagger regression'
    with urllib.request.urlopen(base+'/api-docs/swagger-config.json',timeout=8) as response:
        assert 'Communications' in response.read().decode()
    if p.env_of(p.inspect(NAME)).get('CF_COMMUNICATIONS_BASE_URL'):
        # Existing controlled accounts only; no customer credentials or account scans.
        import app_acceptance
        users=json.loads((p.SECRETS/'calling-acceptance-accounts.json').read_text())
        token=app_acceptance.api('login',{'email':users[0]['email'],'password':users[0]['password']})['access_token']
        request=urllib.request.Request(base+'/api/v2/communications/me',headers={'Authorization':'Bearer '+token})
        with urllib.request.urlopen(request,timeout=15) as response:profile=json.load(response)
        assert profile['activationState']=='ACTIVE' and profile['cfMeetId'].startswith('CF-')


def release(image,run_id):
    assert run_id.isdigit()
    c=p.inspect(NAME);assert c['State']['Running']
    immutable=json.loads(p.run('docker','image','inspect',image))[0]['Id']
    folder=p.ROOT/'account-releases'/run_id;folder.mkdir(parents=True,exist_ok=False,mode=0o700)
    p.write_private(folder/'original.private.json',json.dumps(c))
    # SEC-018: the migrator role's URL when provisioned (opt-in); the runtime one otherwise.
    migration_env=env_file(folder/'migration.private.env',dict(p.env_of(c),DATABASE_URL=meeting_migration_url(p.env_of(c))))
    try:
        for binary in (['migration','account-security-migration','meetings-calendar-migration','profile-notifications-migration','account-communications-migration'] if p.env_of(c).get('CF_COMMUNICATIONS_BASE_URL') else ['migration','account-security-migration','meetings-calendar-migration','profile-notifications-migration']):
            p.run('docker','run','--rm','--network','container:'+NAME,'--env-file',str(migration_env),
                '--entrypoint','/app/'+binary,immutable,'up')
    finally:migration_env.unlink(missing_ok=True)
    candidate=NAME+'-candidate-'+run_id
    cfg=clone(c,candidate,immutable,{},True)
    # Clone only removes candidate host bindings and production DNS aliases.
    p.docker_api('POST','/containers/create?name='+candidate,cfg)
    p.docker_api('POST','/containers/'+candidate+'/start')
    ready(candidate)
    p.run('docker','stop','--time','20',candidate)
    previous=NAME+'-rollback-'+run_id
    p.run('docker','rename',NAME,previous)
    try:
        p.run('docker','stop','--time','30',previous)
        cfg=clone(c,NAME,immutable,{},False)
        p.docker_api('POST','/containers/create?name='+NAME,cfg)
        p.docker_api('POST','/containers/'+NAME+'/start')
        ready(NAME);verify_public()
        deployed=p.inspect(NAME)
        assert set(deployed['NetworkSettings']['Networks'])==set(c['NetworkSettings']['Networks'])
        assert deployed['HostConfig']['PortBindings']==c['HostConfig']['PortBindings']
        assert all(p.env_of(deployed).get(k)==v for k,v in p.env_of(c).items())
    except Exception:
        try:p.run('docker','stop','--time','15',NAME);p.run('docker','rename',NAME,NAME+'-failed-'+run_id)
        except RuntimeError:pass
        p.run('docker','rename',previous,NAME);p.run('docker','start',NAME)
        raise
    p.write_private(folder/'verified.json',json.dumps({'imageId':immutable,'rollbackContainer':previous,'verified':True}))
    print(json.dumps({'accountImageId':immutable,'runtimeConfigurationPreserved':True,'rollbackContainer':previous,'publicSwagger':'PASS'}))


if __name__=='__main__':
    os.umask(0o077)
    parser=argparse.ArgumentParser();parser.add_argument('--image',required=True);parser.add_argument('--run-id',required=True)
    args=parser.parse_args()
    try:release(args.image,args.run_id)
    except Exception as error:
        print('ACCOUNT_RELEASE_FAILED:',type(error).__name__)
        sys.exit(1)

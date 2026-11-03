"""Sections 4–7 release operations through the existing GitHub-controlled VPS access.
Secret-bearing inspection and rollback files never leave the VPS.
"""
import json
from pathlib import Path
from urllib.parse import urlsplit
import production as p

SERVICES = ('cfmeet-platform-access', 'cfmeet-production-backend', 'cfmeet-calling', 'cfmeet-messaging', 'cfmeet-core-tls')

def inspect_runtime():
    p.identity()
    for name in SERVICES:
        c=p.inspect(name); env=p.env_of(c)
        db=urlsplit(env.get('DATABASE_URL',''))
        print(json.dumps(dict(service=name,image=c['Config']['Image'],image_id=c['Image'],
            database=dict(host=db.hostname,port=db.port,name=db.path.lstrip('/'),role=db.username),
            networks=list(c['NetworkSettings']['Networks']),ports=c['HostConfig']['PortBindings'],
            mounts=[dict(source=m['Source'],target=m['Destination'],readonly=not m['RW']) for m in c['Mounts']],
            configuration_names=sorted(env),
            personal_scope={k:v for k,v in env.items() if k in ('CFMEET_COMMUNICATION_TENANT_ID','CFMEET_COMMUNICATION_APPLICATION_ID','CFMEET_COMMUNICATION_ENVIRONMENT_ID','CFMEET_COMMUNICATION_ENVIRONMENT_KIND','CFMEET_COMMUNICATION_SERVICE_ACCOUNT_ID')},
            flags={k:v for k,v in env.items() if k in p.SAFE_ENV})))
    print(json.dumps({'configuration_files':[str(x) for x in p.ROOT.glob('*.env')], 'secret_file_names':[x.name for x in p.SECRETS.iterdir() if x.is_file()], 'container_names':p.run('docker','ps','--format','{{.Names}}').splitlines()}))
    tls=p.inspect('cfmeet-core-tls')
    for m in tls['Mounts']:
        path=Path(m['Source'])
        if path.is_file() and path.suffix in ('.conf',):
            # nginx routing has no credential directives in this existing proxy.
            text=path.read_text()
            assert not any(x in text.lower() for x in ('authorization','password','secret','token'))
            print(json.dumps({'private_proxy_config':str(path),'routing':text}))


# The release uses the existing private network, certificate, PostgreSQL and keys.
# Candidate services use fresh isolated owner databases, never customer data copies.
import base64
import copy
import os
import re
import secrets
import subprocess
import time
from urllib.parse import quote

OWNERS = {'platform-access':'cfmeet-platform-access','meeting-service':'cfmeet-production-backend',
          'calling':'cfmeet-calling','messaging':'cfmeet-messaging'}
PORTS = {'platform-access':8094,'meeting-service':8080,'calling':8090,'messaging':8097,'communications':8080}
STAGE_TLS = {'platform-access':8543,'messaging':8544,'calling':8545,'communications':8547,'meeting-service':8548}
PROD_TLS = {'platform-access':8443,'messaging':8444,'calling':8445,'communications':8447,'meeting-service':8448}
WORK = p.ROOT/'communications'

def save(path, value, secret=False):
    path.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
    path.write_text(value)
    os.chmod(path,0o400 if secret else 0o444)
    if secret: os.chown(path,10001,10001)

def env_file(path, values):
    assert all('\n' not in v and '\r' not in v for v in values.values())
    p.write_private(path,'\n'.join(k+'='+v for k,v in values.items())+'\n')
    return path

def mount_source(c,target):
    found=[m['Source'] for m in c['Mounts'] if m['Destination']==target]
    assert len(found)==1,'Expected private service key mount missing'
    return found[0]

def public_key(path,kid):
    raw=subprocess.run(['openssl','pkey','-in',str(path),'-pubout','-outform','DER'],capture_output=True)
    assert raw.returncode==0,'Public service key derivation failed'
    return dict(kty='OKP',crv='Ed25519',kid=kid,x=base64.urlsafe_b64encode(raw.stdout[-32:]).decode().rstrip('='))

def grant(bundle,issuer,key,permissions):
    assert not any(x['issuer']==issuer for x in bundle['trusted_issuers']),'Unexpected existing Communications trust issuer'
    bundle['trusted_issuers'].append(dict(issuer=issuer,audience=bundle['receiver'],
        runtime_environment=bundle['runtime_environment'],keys=[key],permissions=permissions))

def prepare_trust():
    WORK.mkdir(exist_ok=True,mode=0o700)
    keyfile=WORK/'communications.pem'
    if not keyfile.exists():
        p.run('openssl','genpkey','-algorithm','ED25519','-out',str(keyfile))
        os.chmod(keyfile,0o400);os.chown(keyfile,10001,10001)
    if not (WORK/'invite-digest.key').exists():save(WORK/'invite-digest.key',secrets.token_hex(32),True)
    # A same-host recovery copy is protected, and explicitly not disaster recovery.
    recovery=p.ROOT/'key-recovery'/'communications'
    recovery.mkdir(parents=True,exist_ok=True,mode=0o700)
    for name in ('communications.pem','invite-digest.key'):
        if not (recovery/name).exists():p.write_private(recovery/name,(WORK/name).read_text())
    hubkey=public_key(keyfile,'communications-20261103')
    hub=dict(bundle_version=1,receiver='cfmeet.communications',runtime_environment='production',trusted_issuers=[])
    perms={'platform-access':['platform-identity.read','platform-principal.resolve'],
        'meeting-service':['communications-account.read'],
        'calling':['calling-communications.read','calling-communications.apply'],
        'messaging':['messaging-communications.read','messaging-communications.apply']}
    for service,name in OWNERS.items():
        c=p.inspect(name);e=p.env_of(c)
        key=public_key(mount_source(c,e['CF_SERVICE_AUTH_PRIVATE_KEY_FILE']),e['CF_SERVICE_AUTH_KEY_ID'])
        incoming={'meeting-service':['communications-user.forward','communications-account.apply'],
                  'calling':['communications-policy.check','communications-block.apply'],
                  'messaging':['communications-policy.check','communications-block.apply']}
        if service in incoming:grant(hub,'cfmeet.'+service,key,incoming[service])
        old=e.get('CF_SERVICE_TRUST_BUNDLE_FILE')
        bundle=json.loads(Path(mount_source(c,old)).read_text()) if old else dict(bundle_version=1,receiver='cfmeet.'+service,runtime_environment='production',trusted_issuers=[])
        # Repeated release preparation reads the original bundle or our already
        # installed bundle; retain existing grants and replace only the Hub entry.
        bundle['trusted_issuers']=[x for x in bundle['trusted_issuers'] if x['issuer']!='cfmeet.communications']
        grant(bundle,'cfmeet.communications',hubkey,perms[service])
        save(WORK/(service+'.trust.json'),json.dumps(bundle))
    save(WORK/'communications.trust.json',json.dumps(hub))

def image(service,sha):
    assert re.fullmatch('[a-f0-9]{40}',sha)
    ref='ghcr.io/cosmicforgehealthnet/cfmeet-'+service+':'+sha
    meta=json.loads(p.run('docker','image','inspect',ref))[0]
    assert meta['Config']['Labels']['org.opencontainers.image.revision']==sha,'Image source revision mismatch'
    return meta['Id']

def db_sql(database,sql):
    return p.run('docker','exec','-i','-u','postgres','cfmeet-core-db','psql','-XAtq','-v','ON_ERROR_STOP=1','-U','postgres','-d',database,input=sql)

def database(name):
    assert re.fullmatch('cfmeet_comm_[a-z_]+',name)
    config=WORK/(name+'.database.private.json')
    if config.exists():return json.loads(config.read_text())
    runtime=name+'_rt';owner=name+'_migrator'
    rp,mp=secrets.token_hex(32),secrets.token_hex(32)
    # These names belong solely to this feature; refuse a collision rather than
    # adopting, clearing, or recredentialing an existing database.
    assert db_sql('postgres',"SELECT count(*) FROM pg_database WHERE datname='"+name+"';")=='0','Unowned Communications database collision'
    db_sql('postgres',f"CREATE ROLE {owner} LOGIN NOINHERIT PASSWORD '{mp}'; CREATE ROLE {runtime} LOGIN NOINHERIT PASSWORD '{rp}';")
    db_sql('postgres',f'CREATE DATABASE {name} OWNER {owner};')
    db_sql('postgres',f'REVOKE ALL ON DATABASE {name} FROM PUBLIC; GRANT CONNECT ON DATABASE {name} TO {runtime};')
    db_sql(name,f"REVOKE CREATE ON SCHEMA public FROM PUBLIC; GRANT USAGE ON SCHEMA public TO {runtime}; ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA public GRANT SELECT,INSERT,UPDATE,DELETE ON TABLES TO {runtime}; ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA public GRANT USAGE,SELECT,UPDATE ON SEQUENCES TO {runtime}; ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA public GRANT EXECUTE ON FUNCTIONS TO {runtime};")
    value={'runtime':f'postgres://{runtime}:{rp}@cfmeet-core-db:5432/{name}','migrator':f'postgres://{owner}:{mp}@cfmeet-core-db:5432/{name}'}
    p.write_private(config,json.dumps(value));return value

def migrate(service,sha,url,feature_only=False):
    binaries={'platform-access':['platform-access-migration'],'meeting-service':['migration','account-communications-migration'],
              'calling':['calling-migration','calling-communications-migration'],'messaging':['messaging-migration','messaging-communications-migration'],
              'communications':['communications-migration']}[service]
    if feature_only:binaries=binaries[-1:]
    path=env_file(WORK/(service+'.migration.private.env'),{'DATABASE_URL':url})
    for binary in binaries:
        p.run('docker','run','--rm','--network',p.NET,'--env-file',str(path),'--entrypoint','/app/'+binary,image(service,sha),'up')
    path.unlink()

def clone(c,name,img,overrides,stage):
    cfg=copy.deepcopy(c['Config']);cfg['Image']=img;cfg['Hostname']=name
    e=p.env_of(c);e.update(overrides);cfg['Env']=[k+'='+v for k,v in e.items()]
    cfg['HostConfig']=copy.deepcopy(c['HostConfig'])
    assert not cfg['HostConfig'].get('AutoRemove'),'Rollback requires persistent container'
    if stage:
        cfg['HostConfig']['PortBindings']={}
        cfg['HostConfig']['RestartPolicy']={'Name':'unless-stopped','MaximumRetryCount':0}
        cfg['HostConfig']['Memory']=768*1024*1024
    else:
        for bindings in (cfg['HostConfig'].get('PortBindings') or {}).values():
            assert all(x['HostIp'] in ('127.0.0.1','::1') for x in bindings),'Existing backend ingress must stay loopback'
    endpoints={}
    for net,ep in c['NetworkSettings']['Networks'].items():
        assert not ep.get('IPAMConfig'),'Static IP deployment needs explicit preservation'
        aliases=[name] if stage else [a for a in ep.get('Aliases',[]) if a not in (c['Id'],c['Id'][:12])]
        endpoints[net]={'Aliases':aliases}
    cfg['NetworkingConfig']={'EndpointsConfig':endpoints}
    return cfg

def add_mount(cfg,source,target):
    cfg['HostConfig'].setdefault('Mounts',[]).append(dict(Type='bind',Source=str(source),Target=target,ReadOnly=True))

def configured(service,sha,stage,scope,db=None):
    name=('cfmeet-comm-stage-' if stage else 'cfmeet-')+service
    if not stage and service in OWNERS:name=OWNERS[service]
    routes=STAGE_TLS if stage else PROD_TLS
    e={'CF_COMMUNICATIONS_BASE_URL':'https://api.cfmeet.com:'+str(routes['communications']),
       'CF_PLATFORM_ACCESS_BASE_URL':'https://api.cfmeet.com:'+str(routes['platform-access']),
       'CF_SERVICE_TRUST_BUNDLE_FILE':'/run/communications/trust.json'}
    if service in ('calling','messaging'):
        e.update({'CF_COMMUNICATIONS_PERSONAL_'+k+'_ID':scope[k.lower()+'Id'] for k in ('TENANT','APPLICATION','ENVIRONMENT')})
    if db:e['DATABASE_URL']=db
    if service=='communications':
        e.update({'CF_RUNTIME_ENVIRONMENT':'production','CF_SERVICE_AUTH_KEY_ID':'communications-20261103',
            'CF_SERVICE_AUTH_PRIVATE_KEY_FILE':'/run/communications/service.pem',
            'COMMUNICATIONS_INVITE_DIGEST_KEY_FILE':'/run/communications/invite.key',
            'CFMEET_PUBLIC_APP_URL':'https://cfmeet.com',
            'CF_MEETING_SERVICE_BASE_URL':'https://api.cfmeet.com:'+str(routes['meeting-service']),
            'CF_CALLING_BASE_URL':'https://api.cfmeet.com:'+str(routes['calling']),
            'CF_MESSAGING_BASE_URL':'https://api.cfmeet.com:'+str(routes['messaging'])})
        cfg={'Image':image(service,sha),'Env':[k+'='+v for k,v in e.items()],
             'HostConfig':dict(NetworkMode=p.NET,PortBindings={},ReadonlyRootfs=True,Tmpfs={'/tmp':'rw,noexec,nosuid,size=16m'},
                 RestartPolicy={'Name':'unless-stopped','MaximumRetryCount':0},Memory=512*1024*1024,CapDrop=['ALL'],SecurityOpt=['no-new-privileges']),
             'NetworkingConfig':{'EndpointsConfig':{p.NET:{'Aliases':[name]}}}}
        add_mount(cfg,WORK/'communications.pem','/run/communications/service.pem')
        add_mount(cfg,WORK/'invite-digest.key','/run/communications/invite.key')
    else:
        if service=='platform-access':e.pop('CF_COMMUNICATIONS_BASE_URL')
        if service=='meeting-service':
            e.update({'CFMEET_COMMUNICATION_'+k+'_ID':scope[k.lower()+'Id'] for k in ('TENANT','APPLICATION','ENVIRONMENT')})
            e.update(CFMEET_COMMUNICATION_SERVICE_ACCOUNT_ID=scope['serviceAccountId'],CFMEET_COMMUNICATION_ENVIRONMENT_KIND=scope['kind'],
                     CF_MESSAGING_BASE_URL='https://api.cfmeet.com:'+str(routes['messaging']))
            if stage:e.update(JWT_SECRET=secrets.token_hex(32),SMTP_HOST='',EMAIL_VERIFICATION_REQUIRED='false')
        if stage and service=='calling':
            e.update(VOICEMAIL_ENABLED='false',CALL_RECORDING_ENABLED='false',CALLING_MEDIA_GATEWAY_URL='wss://api.cfmeet.com:'+str(routes['calling'])+'/v1/media/rtc')
        cfg=clone(p.inspect(OWNERS[service]),name,image(service,sha),e,stage)
        if stage and service=='messaging':
            ring=WORK/'stage-managed.ring'
            if not ring.exists():save(ring,'stage-1 '+base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip('=')+'\n',True)
            cfg['Env'].append('MESSAGING_MANAGED_KEYRING_FILE=/run/communications/managed.ring')
            add_mount(cfg,ring,'/run/communications/managed.ring')
    add_mount(cfg,WORK/(service+'.trust.json'),'/run/communications/trust.json')
    return name,cfg

def proxy_routes():
    path=p.ROOT/'tls-proxy'/'nginx.conf'
    text=path.read_text();marker='  # CFMEET COMMUNICATIONS PRIVATE ROUTES'
    if marker in text:return
    routes=[]
    for stage,ports in ((False,PROD_TLS),(True,STAGE_TLS)):
        for service,port in ports.items():
            if not stage and service not in ('communications','meeting-service'):continue
            name=('cfmeet-comm-stage-' if stage else 'cfmeet-')+service
            if not stage and service in OWNERS:name=OWNERS[service]
            routes.append(f'  server {{ listen {port} ssl; server_name api.cfmeet.com; location / {{ set $peer {name}:{PORTS[service]}; proxy_pass http://$peer$request_uri; proxy_http_version 1.1; proxy_set_header Host $host; proxy_set_header X-Forwarded-Proto https; proxy_set_header Upgrade $http_upgrade; proxy_set_header Connection "upgrade"; proxy_read_timeout 3600; }} }}')
    candidate=text.rstrip().removesuffix('}')+'\n'+marker+'\n'+'\n'.join(routes)+'\n}\n'
    p.write_private(WORK/'tls-before.conf',text)
    path.write_text(candidate) # existing readonly bind mount requires same inode
    try:p.run('docker','exec','cfmeet-core-tls','nginx','-t');p.run('docker','exec','cfmeet-core-tls','nginx','-s','reload')
    except Exception:path.write_text(text);raise
    assert not p.inspect('cfmeet-core-tls')['HostConfig']['PortBindings']

def wait(name,service):
    probe="import urllib.request; urllib.request.urlopen('http://"+name+':'+str(PORTS[service])+('/health' if service=='meeting-service' else '/ready')+"',timeout=3).read()"
    for attempt in range(45):
        try:p.run('docker','run','--rm','--network',p.NET,'python:3.12-slim','python','-c',probe);return
        except RuntimeError:
            if attempt%10==0:print('COMMUNICATIONS_READINESS_WAIT:',service,flush=True)
            time.sleep(2)
    raise RuntimeError('Communications candidate readiness failed: '+service)

def start(name,cfg):
    assert name not in p.run('docker','ps','-a','--format','{{.Names}}').splitlines(),'Existing candidate must be retained or explicitly reconciled'
    p.docker_api('POST','/containers/create?name='+name,cfg);p.docker_api('POST','/containers/'+name+'/start')

def stage_admin(sha,*args):
    c=p.inspect('cfmeet-comm-stage-platform-access')
    f=env_file(WORK/'stage-pa.private.env',p.env_of(c))
    mount=c['Mounts'];cmd=['docker','run','--rm','--network',p.NET,'--env-file',str(f)]
    for m in mount:cmd+=['--mount',f"type=bind,src={m['Source']},dst={m['Destination']},readonly"]
    return json.loads(p.run(*cmd,'--entrypoint','/app/platform-access-admin',image('platform-access',sha),'--operator','communications-release',*args))

def stage_scope(sha):
    file=WORK/'stage-context.private.json'
    if file.exists():return json.loads(file.read_text())
    stage_admin(sha,'tenant','create','--slug','communications-stage','--name','Isolated Communications acceptance','--classification','first-party')
    stage_admin(sha,'application','create','--tenant','communications-stage','--slug','personal','--name','Sections 4-7')
    args=['--tenant','communications-stage','--application','personal','--environment','test']
    ev=stage_admin(sha,'environment','create',*args)
    sa=stage_admin(sha,'service-account','create',*args,'--name','acceptance')
    scopes=['calls.read','calls.write','messages.read','messages.send','conversations.create','conversations.manage']
    for scope in scopes:stage_admin(sha,'scope','grant','--tenant','communications-stage','--application','personal','--scope',scope,'--environment-kind','test')
    credential=stage_admin(sha,'credential','create',*args,'--service-account','acceptance','--expires-in-days','1',*[v for scope in scopes for v in ('--scope',scope)],'--label','Isolated Sections 4-7 acceptance')
    # CLI views are typed owner responses, never consumer discovery data.
    ev=ev.get('environment',ev);sa=sa.get('serviceAccount',sa)
    value={'tenantId':str(ev['tenantId']),'applicationId':str(ev['applicationId']),'environmentId':str(ev['id']),'serviceAccountId':str(sa['id']),'kind':'TEST','token':credential['secret']}
    p.write_private(file,json.dumps(value));return value

def stage_release(sha,run_id):
    p.identity();prepare_trust();proxy_routes()
    for service in PORTS:image(service,sha)
    # Fresh databases and additive owner migrations are tested before a production
    # database is touched. Candidates are private and have no published ports.
    dbs={service:database('cfmeet_comm_stage_'+service.replace('-','_')) for service in PORTS}
    for service in PORTS:
        migrate(service,sha,dbs[service]['migrator'])
    empty={'tenantId':'','applicationId':'','environmentId':'','serviceAccountId':'','kind':'TEST'}
    name,cfg=configured('platform-access',sha,True,empty,dbs['platform-access']['runtime'])
    start(name,cfg);wait(name,'platform-access')
    scope=stage_scope(sha)
    for service in ('meeting-service','calling','messaging','communications'):
        name,cfg=configured(service,sha,True,scope,dbs[service]['runtime'])
        start(name,cfg);wait(name,service)
    # Actual HTTP journey is a separate client on the same private TLS network.
    import communications_acceptance
    report=communications_acceptance.execute(sha,run_id,scope,stage=True)
    p.write_private(WORK/'staging-evidence.json',json.dumps(report,indent=2))
    print(json.dumps(report),flush=True)

def run(operation, sha, run_id):
    if operation=='communications-inspect':inspect_runtime()
    elif operation=='communications-stage':stage_release(sha,run_id)
    else:raise RuntimeError('Production Communications cutover awaits successful isolated acceptance')

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
    print(json.dumps({'configuration_files':[str(x) for x in p.ROOT.glob('*.env')]}))
    tls=p.inspect('cfmeet-core-tls')
    for m in tls['Mounts']:
        path=Path(m['Source'])
        if path.is_file() and path.suffix in ('.conf',):
            # nginx routing has no credential directives in this existing proxy.
            text=path.read_text()
            assert not any(x in text.lower() for x in ('authorization','password','secret','token'))
            print(json.dumps({'private_proxy_config':str(path),'routing':text}))

def run(operation, sha, run_id):
    if operation=='communications-inspect':
        inspect_runtime()
    else:
        raise RuntimeError('Communications release operation is not implemented yet; running deployment preserved')

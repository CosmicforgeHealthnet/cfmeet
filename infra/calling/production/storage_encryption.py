"""Native MinIO KMS migration. The original data and container stay untouched."""
import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import time
import production as p

NAME = 'cfmeet-calling-artifacts'


def mc(name, *args, data=None, admin=True):
    ip = p.inspect(name)['NetworkSettings']['Networks'][p.NET]['IPAddress']
    user = p.secret('minio-admin-user' if admin else 'minio-service-access-key')
    password = p.secret('minio-admin-password' if admin else 'calling-artifact-store-secret')
    env = dict(os.environ,MC_HOST_kms='http://'+user+':'+password+'@'+ip+':9000')
    result = subprocess.run([str(p.ROOT/'storage'/'mc'),*args],env=env,input=data,text=True,capture_output=True,timeout=40)
    assert result.returncode==0, 'MinIO validation failed: '+args[0]
    return result.stdout


def ready(name):
    for _ in range(35):
        try:
            ip = p.inspect(name)['NetworkSettings']['Networks'][p.NET]['IPAddress']
            p.run('curl','-fsS','--max-time','3','http://'+ip+':9000/minio/health/ready')
            return
        except Exception:
            time.sleep(1)
    raise RuntimeError('MinIO readiness failed')


def migrate(run_id):
    p.identity()
    env = p.env_of(p.inspect('cfmeet-calling'))
    assert all(env.get(k,'false')=='false' for k in ('VOICEMAIL_ENABLED','CALL_RECORDING_ENABLED'))
    assert not p.lk_request('Egress/ListEgress',{'active':True},p.secret('calling-livekit-api-key'),p.secret('calling-livekit-api-secret'),{'roomRecord':True}).get('items',[])
    old = p.inspect(NAME)
    assert not old['HostConfig']['PortBindings']
    print('MINIO_VERSION:',p.run('docker','exec',NAME,'minio','--version').splitlines()[0])
    if any(m['Destination']=='/run/secrets/calling-minio-kms' for m in old['Mounts']):
        print('MINIO_KMS: existing installation preserved')
        return
    source = Path(next(m['Source'] for m in old['Mounts'] if m['Destination']=='/data')).resolve()
    assert str(source).startswith('/var/lib/cfmeet-artifacts') and source.is_dir()
    size = int(p.run('du','-sb',str(source)).split()[0])
    assert size<2*1024**3 and shutil.disk_usage('/').free-size>25*1024**3
    key = p.SECRETS/'calling-minio-kms'
    if not key.exists():
        p.write_private(key,'cfmeet-capture-v1:'+base64.b64encode(secrets.token_bytes(32)).decode()+'\n')
    assert len(base64.b64decode(key.read_text().strip().split(':',1)[1],validate=True))==32
    os.chown(key,10001,10001); key.chmod(0o400)
    recovery = p.ROOT/'key-recovery'/'calling-minio-kms'
    if recovery.exists():
        assert recovery.read_bytes()==key.read_bytes()
    else:
        p.write_private(recovery,key.read_text())
    os.chown(recovery,10001,10001); recovery.chmod(0o400)
    destination = Path('/var/lib/cfmeet-artifacts-kms-'+run_id)
    assert not destination.exists()
    p.run('docker','stop','--time','30',NAME)
    try:
        shutil.copytree(source,destination,copy_function=shutil.copy2)
        for root,dirs,files in os.walk(destination):
            os.chown(root,10001,10001)
            for file in files: os.chown(Path(root)/file,10001,10001)
    finally:
        p.run('docker','start',NAME)
    ready(NAME)
    payload = copy.deepcopy(old['Config']); payload['Image']=old['Image']
    settings=p.env_of(old); settings['MINIO_KMS_SECRET_KEY_FILE']='/run/secrets/calling-minio-kms'
    payload['Env']=[k+'='+v for k,v in settings.items()]
    host=copy.deepcopy(old['HostConfig'])
    for mount in host['Mounts']:
        if mount['Target']=='/data': mount['Source']=str(destination)
    host['Mounts'].append(dict(Type='bind',Source=str(key),Target='/run/secrets/calling-minio-kms',ReadOnly=True))
    payload['HostConfig']=host
    payload['NetworkingConfig']={'EndpointsConfig':{n:{} for n in old['NetworkSettings']['Networks']}}
    candidate=NAME+'-kms-'+run_id; backup=NAME+'-pre-kms-'+run_id
    evidence=p.ROOT/'production-evidence'/('kms-'+run_id)
    evidence.mkdir(parents=True,exist_ok=False,mode=0o700)
    p.write_private(evidence/'container.before.json',json.dumps(old))
    probe='capture-staging/kms-recovery-'+run_id
    swapped=False
    try:
        p.docker_api('POST','/containers/create?name='+candidate,payload)
        p.docker_api('POST','/containers/'+candidate+'/start'); ready(candidate)
        mc(candidate,'encrypt','set','sse-s3','kms/'+NAME)
        mc(candidate,'pipe','kms/'+NAME+'/'+probe,data='controlled encrypted recovery probe',admin=False)
        assert 'AES256' in mc(candidate,'stat','--json','kms/'+NAME+'/'+probe,admin=False)
        assert mc(candidate,'cat','kms/'+NAME+'/'+probe,admin=False)=='controlled encrypted recovery probe'
        print('KMS_ENCRYPTED_UPLOAD_DOWNLOAD: PASS')
        p.run('docker','stop','--time','30',candidate)
        restore=copy.deepcopy(payload)
        for mount in restore['HostConfig']['Mounts']:
            if mount['Target']=='/run/secrets/calling-minio-kms': mount['Source']=str(recovery)
        recovery_name=NAME+'-key-recovery-'+run_id
        p.docker_api('POST','/containers/create?name='+recovery_name,restore)
        p.docker_api('POST','/containers/'+recovery_name+'/start')
        try:
            ready(recovery_name)
            assert mc(recovery_name,'cat','kms/'+NAME+'/'+probe,admin=False)=='controlled encrypted recovery probe'
            print('KMS_RECOVERY_KEY_RESTART_AND_DECRYPT: PASS')
        finally:
            p.run('docker','stop','--time','20',recovery_name)
        p.run('docker','start',candidate); ready(candidate)
        mc(candidate,'rm','kms/'+NAME+'/'+probe,admin=False)
        p.run('docker','rename',NAME,backup); swapped=True
        p.run('docker','stop','--time','30',backup)
        p.run('docker','rename',candidate,NAME)
        ready(NAME)
        print('KMS_CUTOVER: updating authenticated public ingress')
        p.secure_storage_ingress(run_id); p.regression()
    except Exception:
        if swapped:
            if subprocess.run(['docker','inspect',NAME],capture_output=True).returncode==0:
                p.run('docker','stop','--time','15',NAME)
                p.run('docker','rename',NAME,NAME+'-failed-kms-'+run_id)
            p.run('docker','rename',backup,NAME); p.run('docker','start',NAME)
            p.secure_storage_ingress(run_id+'-rollback')
        elif subprocess.run(['docker','inspect',candidate],capture_output=True).returncode==0:
            p.run('docker','stop','--time','15',candidate)
        raise
    result=dict(status='SSE_S3_AND_SAME_HOST_KEY_RECOVERY_PASS',image=old['Image'],original_data=str(source),
        encrypted_data=str(destination),rollback_container=backup,key_file=str(key),recovery_file=str(recovery),
        key_sha256=hashlib.sha256(key.read_bytes()).hexdigest(),offserver_backup='POSTPONED_BY_OWNER')
    p.write_private(evidence/'result.json',json.dumps(result,indent=2)); print(json.dumps(result))

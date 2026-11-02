"""Real, isolated CF Meet accounts and deployed web acceptance. Secrets stay on VPS."""
import json
import os
from pathlib import Path
import secrets
import subprocess
import urllib.error
import urllib.request
import uuid
import production as p


def api(path,body):
    request=urllib.request.Request('https://api.cfmeet.com/api/v1/auth/'+path,
        data=json.dumps(body).encode(),headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(request,timeout=30) as response: return json.load(response)


def accounts():
    path=p.SECRETS/'calling-acceptance-accounts.json'
    if path.exists(): return json.loads(path.read_text())
    values=[]
    suffix=uuid.uuid4().hex[:8]
    for letter in ('a','b','c'):
        name='cfmeet_accept_'+letter+'_'+suffix
        values.append(dict(username=name,email=name+'@example.invalid',password='Aa1!'+secrets.token_urlsafe(28)))
    p.write_private(path,json.dumps(values))
    redis_url=p.env_of(p.inspect('cfmeet-production-backend'))['REDIS_URL']
    redis_reader='''import json,socket,sys,urllib.parse
x=json.load(sys.stdin); u=urllib.parse.urlsplit(x['url']); s=socket.create_connection((u.hostname,u.port or 6379),timeout=10); f=s.makefile('rb')
def cmd(*parts):
 b=[str(v).encode() for v in parts]; s.sendall(b'*'+str(len(b)).encode()+b'\\r\\n'+b''.join(b'$'+str(len(v)).encode()+b'\\r\\n'+v+b'\\r\\n' for v in b)); h=f.readline(); n=h[1:].strip()
 if h[:1]==b'$':
  size=int(n)
  if size<0:return None
  v=f.read(size); f.read(2);return v.decode()
 if h[:1]==b'-':raise RuntimeError('Redis rejected controlled OTP lookup')
 return n.decode()
if u.password:cmd('AUTH',urllib.parse.unquote(u.password))
if u.path.strip('/'):cmd('SELECT',int(u.path.strip('/')))
print(cmd('GET',x['key']) or '')
'''
    for account in values:
        account['userId']=api('register',dict(username=account['username'],email=account['email'],
            password=account['password'],confirm_password=account['password']))['user_id']
        # Only retrieve the verification code for this newly created controlled account.
        code=p.run('docker','run','--rm','-i','--network','container:cfmeet-production-backend',
            'python:3.12-slim','python','-c',redis_reader,
            input=json.dumps({'url':redis_url,'key':'otp:verify:'+account['email']}))
        assert code.isdigit() and len(code)==6
        api('verify-email',dict(email=account['email'],token=code))
        assert api('login',dict(email=account['email'],password=account['password']))['user']['id']==account['userId']
        p.write_private(path,json.dumps(values))
    return values


def run(run_id):
    p.identity()
    values=accounts()
    assert all(x.get('userId') for x in values), 'Controlled account setup incomplete; preserve credentials for recovery'
    root=p.ROOT/'production-evidence'/('web-'+run_id)
    root.mkdir(parents=True,exist_ok=False,mode=0o700); os.chown(root,1000,1000)
    private=root/'accounts.private.json'; p.write_private(private,json.dumps(values)); os.chown(private,1000,1000)
    source=Path(__file__).parent
    p.run('docker','pull','mcr.microsoft.com/playwright:v1.56.1-noble')
    base=json.loads(p.run('docker','image','inspect','mcr.microsoft.com/playwright:v1.56.1-noble'))[0]['RepoDigests'][0]
    image='cfmeet-calling-web-probe:1.56.1'
    p.run('docker','build','--network','default','--build-arg','PLAYWRIGHT_IMAGE='+base,
        '-f',str(source/'WebProbe.Dockerfile'),'-t',image,str(source))
    result=subprocess.run(['docker','run','--rm','--name','cfmeet-calling-web-probe','--network','bridge',
        '--memory','1536m','--cpus','2','--shm-size','256m','--user','1000:1000',
        '--tmpfs','/tmp:rw,nosuid,size=512m,uid=1000,gid=1000',
        '--mount',f'type=bind,src={root},dst=/result',
        '--mount',f'type=bind,src={source}/web_acceptance.mjs,dst=/probe/test.mjs,readonly',
        image,'node','/probe/test.mjs'],capture_output=True,text=True,timeout=600)
    p.write_private(root/'execution.private.log',result.stdout+'\n'+result.stderr)
    report=root/'web-result.json'
    assert report.exists(), 'Browser probe failed before producing evidence'
    print(report.read_text())
    assert result.returncode==0, 'Production web Calling acceptance failed'

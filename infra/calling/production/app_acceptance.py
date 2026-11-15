"""Real, isolated CF Meet API test accounts. Secrets stay on VPS."""
import json
import re
import secrets
import urllib.request
import uuid
import controlled_otp
import production as p


def api(path,body):
    request=urllib.request.Request('https://api.cfmeet.com/api/v1/auth/'+path,
        data=json.dumps(body).encode(),headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(request,timeout=30) as response: return json.load(response)


def controlled_input(redis_url,email):
    """Input of the in-container reader: issued-code replacement (N0-8) or legacy read."""
    code=controlled_otp.new_code()
    return {'url':redis_url,'hashed':controlled_otp.hashed_key(email),'legacy':controlled_otp.legacy_key(email),
            'code':code,'sealed':controlled_otp.sealed(code)}


def accounts(namespace=None,count=3):
    assert count in (2,3)
    if namespace is not None:
        assert re.fullmatch(r'[a-z0-9-]{1,64}',namespace), 'Invalid controlled fixture namespace'
    path=p.SECRETS/('cfmeet-acceptance-'+namespace+'.json' if namespace else 'calling-acceptance-accounts.json')
    if path.exists(): return json.loads(path.read_text())
    values=[]
    suffix=uuid.uuid4().hex[:8]
    for letter in ('a','b','c')[:count]:
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
# N0-8: a hashed code was issued -> replace it with the controlled one; else (previous
# image) read the plaintext code.
if cmd('EXISTS',x['hashed'])=='1':print(x['code'] if cmd('SET',x['hashed'],x['sealed'],'EX','86400','XX')=='OK' else '')
else:print(cmd('GET',x['legacy']) or '')
'''
    for account in values:
        account['userId']=api('register',dict(username=account['username'],email=account['email'],
            password=account['password'],confirm_password=account['password']))['user_id']
        # Only retrieve the verification code for this newly created controlled account.
        code=p.run('docker','run','--rm','-i','--network','container:cfmeet-production-backend',
            'python:3.12-slim','python','-c',redis_reader,
            input=json.dumps(controlled_input(redis_url,account['email'])))
        assert code.isdigit() and len(code)==6
        api('verify-email',dict(email=account['email'],token=code))
        assert api('login',dict(email=account['email'],password=account['password']))['user']['id']==account['userId']
        p.write_private(path,json.dumps(values))
    return values

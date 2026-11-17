"""Real, isolated CF Meet API test accounts. Secrets stay on VPS.

Sign-in is passwordless: an account signs in by requesting a code (`POST
/api/v1/auth/otp/request`) and verifying it (`POST /api/v1/auth/otp/verify`); the first
verification of a new address creates the account. For its own controlled accounts the
tooling replaces the issued code with a controlled one in the Meeting Service's Redis
(`controlled_otp`), from the VPS only. Clients that run in a container and cannot reach Redis
receive a refresh token of a session opened here instead (`refresh_token_for`).
"""
import json
import re
import urllib.request
import uuid
import controlled_otp
import production as p


def api(path,body):
    request=urllib.request.Request('https://api.cfmeet.com/api/v1/auth/'+path,
        data=json.dumps(body).encode(),headers={'Content-Type':'application/json'})
    with urllib.request.urlopen(request,timeout=30) as response: return json.load(response)


def controlled_input(redis_url,key):
    """Input of the in-container helper: replace the issued code under `key` (SET XX)."""
    code=controlled_otp.new_code()
    return {'url':redis_url,'hashed':key,'code':code,'sealed':controlled_otp.sealed(code)}


REDIS_HELPER='''import json,socket,sys,urllib.parse
x=json.load(sys.stdin); u=urllib.parse.urlsplit(x['url']); s=socket.create_connection((u.hostname,u.port or 6379),timeout=10); f=s.makefile('rb')
def cmd(*parts):
 b=[str(v).encode() for v in parts]; s.sendall(b'*'+str(len(b)).encode()+b'\\r\\n'+b''.join(b'$'+str(len(v)).encode()+b'\\r\\n'+v+b'\\r\\n' for v in b)); h=f.readline(); n=h[1:].strip()
 if h[:1]==b'$':
  size=int(n)
  if size<0:return None
  v=f.read(size); f.read(2);return v.decode()
 if h[:1]==b'-':raise RuntimeError('Redis rejected controlled OTP replacement')
 return n.decode()
if u.password:cmd('AUTH',urllib.parse.unquote(u.password))
if u.path.strip('/'):cmd('SELECT',int(u.path.strip('/')))
# Only a code that was really issued is replaced (XX); its TTL is kept.
print(x['code'] if cmd('SET',x['hashed'],x['sealed'],'XX','KEEPTTL')=='OK' else '')
'''


def redis_url():
    return p.env_of(p.inspect('cfmeet-production-backend'))['REDIS_URL']


def controlled_code(key,url=None):
    """Replace the issued code under `key` with a controlled one and return it (VPS only)."""
    code=p.run('docker','run','--rm','-i','--network','container:cfmeet-production-backend',
        'python:3.12-slim','python','-c',REDIS_HELPER,
        input=json.dumps(controlled_input(url or redis_url(),key)))
    assert code.isdigit() and len(code)==6, 'No sign-in code was issued for the controlled account'
    return code


def sign_in_via(post,account,url=None):
    """A passwordless sign-in of a controlled account through the caller's own HTTP client:
    `post(path, body)` returns the JSON body. The answer of `otp/verify` (tokens, user,
    is_new_account)."""
    post('/api/v1/auth/otp/request',{'email':account['email']})
    code=controlled_code(controlled_otp.hashed_key(account['email']),url)
    return post('/api/v1/auth/otp/verify',{'email':account['email'],'code':code})


def sign_in(account,url=None):
    prefix='/api/v1/auth/'
    return sign_in_via(lambda path,body: api(path[len(prefix):],body),account,url)


def refresh_token_for(account,url=None):
    """A fresh session's refresh token for a client that cannot reach Redis (it calls
    `POST /api/v1/auth/refresh` once). Never printed."""
    return sign_in(account,url)['refresh_token']


def accounts(namespace=None,count=3,security_checks=False,keep_sessions=False):
    """Fresh controlled accounts of a namespace (created once, then reused from the 0600 file).
    With `keep_sessions`, newly created accounts also carry the refresh token of their first
    session (`refreshToken`) in memory only, never in the file: sign-in codes are limited per
    address, so a run that needs many sessions does not waste this one."""
    assert count in (2,3)
    assert not security_checks or (count == 3 and namespace and namespace.startswith('organizations-'))
    if namespace is not None:
        assert re.fullmatch(r'[a-z0-9-]{1,64}',namespace), 'Invalid controlled fixture namespace'
    path=p.SECRETS/('cfmeet-acceptance-'+namespace+'.json' if namespace else 'calling-acceptance-accounts.json')
    if path.exists(): return json.loads(path.read_text())
    values=[]
    suffix=uuid.uuid4().hex[:8]
    for letter in ('a','b','c')[:count]:
        name='cfmeet_accept_'+letter+'_'+suffix
        values.append(dict(username=name,email=name+'@example.invalid'))
    def save():
        p.write_private(path,json.dumps([{k:v for k,v in a.items() if k!='refreshToken'} for a in values]))
    save()
    url=redis_url()
    for account in values:
        if security_checks and account is values[0]:
            import notification_auth_acceptance
            try:
                notification_auth_acceptance.run_pending(account,url)
            finally:
                save()
            continue
        # The first verification creates the controlled account.
        login=sign_in(account,url)
        assert login['is_new_account'] is True, 'A controlled fixture address already had an account'
        account['userId']=login['user']['id']
        if keep_sessions:
            account['refreshToken']=login['refresh_token']
        save()
    return values

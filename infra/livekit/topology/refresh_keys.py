"""Which LiveKit key signs refreshed tokens on a server holding two keys?

Mirrors Calling: join-only token signed with the Calling key, then a server-side
UpdateParticipant permission grant (Calling's admission). Records the key NAME (iss)
of every refreshed token LiveKit sends. Production Calling's gateway can only verify
its own key, so a refresh signed with the other key used to drop the connection.
"""
import asyncio, base64, collections, hashlib, hmac, json, sys, time, urllib.request
from pathlib import Path

KEYS = {'calling-topology': 'calling-topology-only-key-not-a-secret-0123456789',
        'meetings-topology': 'meetings-topology-only-key-not-a-secret-0123456789'}
JOIN_KEY = 'calling-topology'
CLIENT = Path(__file__).resolve().parents[3] / 'services/calling/tests/rtc/rtc_client.py'


def jwt(key, claims):
    b64 = lambda b: base64.urlsafe_b64encode(b).decode().rstrip('=')
    now = int(time.time())
    claims = dict(iss=key, nbf=now - 5, exp=now + 600, **claims)
    head = b64(b'{"alg":"HS256","typ":"JWT"}') + '.' + b64(json.dumps(claims).encode())
    return head + '.' + b64(hmac.new(KEYS[key].encode(), head.encode(), hashlib.sha256).digest())


def update_participant(room, identity):
    token = jwt(JOIN_KEY, {'sub': 'admission', 'video': {'roomAdmin': True, 'room': room}})
    body = {'room': room, 'identity': identity,
            'permission': {'canSubscribe': True, 'canPublish': True, 'canPublishData': True}}
    request = urllib.request.Request('http://127.0.0.1:7880/twirp/livekit.RoomService/UpdateParticipant',
        data=json.dumps(body).encode(), headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    urllib.request.urlopen(request, timeout=10).read()


async def main(trials, report):
    counts = collections.Counter(); per_trial = []
    with open(report + '.clients.log', 'wb') as log:
        for index in range(trials):
            room, identity = f'refresh-{index}', f'caller-{index}'
            join = jwt(JOIN_KEY, {'sub': identity, 'video': {'roomJoin': True, 'room': room,
                       'canPublish': False, 'canSubscribe': False, 'canPublishData': False}})
            p = await asyncio.create_subprocess_exec(sys.executable, str(CLIENT), stdin=asyncio.subprocess.PIPE,
                                                     stdout=asyncio.subprocess.PIPE, stderr=log)
            async def cmd(c, **body):
                p.stdin.write((json.dumps({'cmd': c, **body}) + '\n').encode()); await p.stdin.drain()
                return json.loads(await asyncio.wait_for(p.stdout.readline(), 60))
            joined = await cmd('connect', url='ws://127.0.0.1:7880', token=join)
            issuers = []
            if joined.get('ok'):
                update_participant(room, identity)
                await asyncio.sleep(3)
                issuers = (await cmd('refresh_issuers'))['issuers']
            counts.update(issuers)
            per_trial.append({'trial': index, 'joined': bool(joined.get('ok')), 'refresh_issuers': issuers})
            await cmd('quit'); await p.wait()
    other = sum(n for key, n in counts.items() if key != JOIN_KEY)
    summary = {'trials': trials, 'joined': sum(t['joined'] for t in per_trial),
               'refreshes_by_key': dict(counts), 'refreshes_signed_with_other_key': other,
               'trials_with_other_key_refresh': sum(any(i != JOIN_KEY for i in t['refresh_issuers']) for t in per_trial)}
    Path(report).write_text(json.dumps({'summary': summary, 'trials': per_trial}, indent=2))
    print(json.dumps(summary, indent=2))


asyncio.run(main(int(sys.argv[1]), sys.argv[2]))

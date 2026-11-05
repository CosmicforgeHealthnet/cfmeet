"""Real two-party WebRTC connect + bidirectional audio trials against one LiveKit.

Uses the same rtc_client.py as production acceptance (same SDK, same 20 s connect
budget). Prints and writes a JSON report; assertions live in check.py.
"""
import argparse, asyncio, base64, hashlib, hmac, json, statistics, sys, time
from pathlib import Path

KEY, SECRET = 'topology-test', 'topology-test-only-key-not-a-secret-0123456789'
CLIENT = Path(__file__).resolve().parents[3] / 'services/calling/tests/rtc/rtc_client.py'


def token(room, identity):
    b64 = lambda b: base64.urlsafe_b64encode(b).decode().rstrip('=')
    now = int(time.time())
    claims = dict(iss=KEY, sub=identity, nbf=now - 5, exp=now + 600,
                  video=dict(roomJoin=True, room=room, canPublish=True, canSubscribe=True))
    head = b64(b'{"alg":"HS256","typ":"JWT"}') + '.' + b64(json.dumps(claims).encode())
    return head + '.' + b64(hmac.new(SECRET.encode(), head.encode(), hashlib.sha256).digest())


class Client:
    async def start(self, log):
        self.p = await asyncio.create_subprocess_exec(sys.executable, str(CLIENT), stdin=asyncio.subprocess.PIPE,
                                                      stdout=asyncio.subprocess.PIPE, stderr=log)
        return self

    async def cmd(self, cmd, **body):
        self.p.stdin.write((json.dumps({'cmd': cmd, **body}) + '\n').encode()); await self.p.stdin.drain()
        line = await asyncio.wait_for(self.p.stdout.readline(), 60)
        return json.loads(line) if line else {'ok': False, 'error': 'client exited'}

    async def close(self):
        try:
            await self.cmd('quit')
        except Exception:
            pass
        try:
            await asyncio.wait_for(self.p.wait(), 10)
        except asyncio.TimeoutError:
            self.p.kill()


async def trial(url, index, log):
    room = f'trial-{index}'
    clients = [await Client().start(log), await Client().start(log)]
    result = {'trial': index}
    try:
        started = time.monotonic()
        joined = await asyncio.gather(*(c.cmd('connect', url=url, token=token(room, f'p{n}')) for n, c in enumerate(clients)))
        result['connect_seconds'] = round(time.monotonic() - started, 2)
        result['connected'] = all(j.get('ok') for j in joined)
        result['connect_errors'] = [str(j.get('error', ''))[:40].split(':')[0] for j in joined if not j.get('ok')]
        if result['connected']:
            for c in clients:
                await c.cmd('publish', kind='audio')
            heard = await asyncio.gather(*(c.cmd('verify_received', kind='audio', timeout=12) for c in clients))
            result['bidirectional_audio'] = all(h.get('ok') for h in heard)
    finally:
        for c in clients:
            await c.close()
    return result


async def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--url', required=True); parser.add_argument('--trials', type=int, default=30)
    parser.add_argument('--report', required=True); parser.add_argument('--topology', required=True)
    args = parser.parse_args()
    results = []
    with open(args.report + '.clients.log', 'wb') as log:
        for index in range(args.trials):
            results.append(await trial(args.url, index, log))
            print(json.dumps(results[-1]), flush=True)
    durations = [r['connect_seconds'] for r in results if r.get('connected')]
    report = {'topology': args.topology, 'trials': len(results),
              'connected': sum(bool(r.get('connected')) for r in results),
              'bidirectional_audio': sum(bool(r.get('bidirectional_audio')) for r in results),
              'connect_seconds_p50': statistics.median(durations) if durations else None,
              'connect_seconds_max': max(durations) if durations else None, 'results': results}
    Path(args.report).write_text(json.dumps(report, indent=2))
    print(json.dumps({k: v for k, v in report.items() if k != 'results'}))


asyncio.run(main())

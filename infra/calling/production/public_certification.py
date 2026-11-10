"""Read-only public-surface certification of api.cfmeet.com.

Runs anywhere with outbound HTTPS (the GitHub runner of the `certify` operation, or a
workstation). Sends only unauthenticated GET / OPTIONS requests: no credentials, no request
bodies, no writes. Placeholder identifiers are random-looking UUIDs that cannot match data.

    python3 public_certification.py <repository root> <output.json>

Checks
  infrastructure   /health, Swagger configuration and every listed OpenAPI document, the
                   CORS preflight from the web origin, TLS certificate lifetime.
  public routes    every GET route of infra/api-gateway/route-manifest.json, without
                   credentials: it must reject (401/403) or be unrouted (404/405); a 2xx
                   on an authenticated route or any 5xx fails. Routes of a gateway family
                   activated by `activate-gateway-routing` must be routed (401/403).
  internal routes  every INTERNAL_SERVICE / OPERATOR_ADMIN / PROVIDER_CALLBACK GET operation
                   of the source contracts must not answer 2xx on the public host.
Exit status 0 when no check failed.
"""
import datetime
import json
import re
import socket
import ssl
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HOST = 'api.cfmeet.com'
PUBLIC = 'https://' + HOST
WEB_ORIGIN = 'https://cfmeet.com'
PLACEHOLDER = '00000000-0000-4000-8000-00000000c0de'
# Must match coordinated_release.GATEWAY_PREFIXES (asserted by the tests).
GATEWAY_PREFIXES = ('/v1/commercial-plans', '/v1/developer-platform', '/v1/environments', '/v1/invitations',
                    '/v1/organizations', '/v1/production-access-requests', '/v1/projects', '/v1/scopes')
INTERNAL_CLASSES = ('INTERNAL_SERVICE', 'OPERATOR_ADMIN', 'PROVIDER_CALLBACK')
MIN_CERT_DAYS = 14
PACE_SECONDS = 0.2
USER_AGENT = 'cfmeet-production-certification/1 (read-only)'


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def request(path, method='GET', headers=None, attempts=4, limit=400):
    """(status, headers, first `limit` bytes of the body). 0 = no HTTP response."""
    if method not in ('GET', 'OPTIONS', 'HEAD'):
        raise ValueError('certification sends read-only methods only')
    headers = dict(headers or {})
    headers.setdefault('User-Agent', USER_AGENT)
    for attempt in range(attempts):
        req = urllib.request.Request(PUBLIC + path, method=method, headers=headers)
        try:
            with OPENER.open(req, timeout=20) as response:
                status, hdrs, body = response.status, dict(response.headers), response.read(limit)
        except urllib.error.HTTPError as error:
            status, hdrs, body = error.code, dict(error.headers or {}), error.read(limit) if error.fp else b''
        except (urllib.error.URLError, socket.timeout, ConnectionError, ssl.SSLError):
            status, hdrs, body = 0, {}, b''
        if status in (0, 429) and attempt + 1 < attempts:
            time.sleep(2 ** (attempt + 1))
            continue
        return status, {k.lower(): v for k, v in hdrs.items()}, body.decode('utf-8', 'replace')
    return status, {}, ''


def concrete(pattern):
    return re.sub(r'\{[^}/]+\}', PLACEHOLDER, pattern)


def in_gateway_family(path):
    return any(path == prefix or path.startswith(prefix + '/') for prefix in GATEWAY_PREFIXES)


def classify(status):
    if status in (401, 403):
        return 'AUTH_ENFORCED'
    if status in (404, 405):
        return 'NOT_ROUTED'
    if status in (400, 422):
        return 'REJECTED'
    if status == 429:
        return 'RATE_LIMITED'
    if 200 <= status < 400:
        return 'OPEN'
    if status == 0:
        return 'NO_RESPONSE'
    return 'ERROR'


def judge_public(route, status):
    """PASS / FAIL / WARN for one public-manifest GET route answered with `status`."""
    outcome = classify(status)
    if outcome in ('ERROR', 'NO_RESPONSE'):
        return 'FAIL', outcome
    if outcome == 'OPEN' and route.get('auth'):
        return 'FAIL', 'authenticated route answered without credentials'
    if in_gateway_family(route['pattern']) and outcome == 'NOT_ROUTED':
        return 'FAIL', 'activated gateway family route is not routed'
    if outcome in ('REJECTED', 'RATE_LIMITED'):
        return 'WARN', outcome
    return 'PASS', outcome


def judge_internal(status):
    outcome = classify(status)
    if outcome == 'OPEN':
        return 'FAIL', 'internal/operator/callback route answers on the public host'
    if outcome in ('ERROR', 'NO_RESPONSE'):
        return 'WARN', outcome
    return 'PASS', outcome


def public_routes(repo):
    routes = json.loads((repo / 'infra/api-gateway/route-manifest.json').read_text())['routes']
    return [r for r in routes if r['method'] == 'GET' and r.get('protocol', 'REST') == 'REST']


def internal_routes(repo):
    sys.path.insert(0, str(repo / 'scripts/contracts'))
    try:
        import public_api
    finally:
        sys.path.pop(0)
    found = {}
    for domain, _service, _rel, _cid, method, path, op, _doc in public_api.operations():
        cls = public_api.auto_class(domain, path, op)
        if method == 'GET' and cls in INTERNAL_CLASSES:
            found.setdefault(path, dict(domain=domain, path=path, cls=cls))
    return sorted(found.values(), key=lambda r: r['path'])


def certificate_days(host=HOST):
    context = ssl.create_default_context()
    with socket.create_connection((host, 443), timeout=15) as sock:
        with context.wrap_socket(sock, server_hostname=host) as tls:
            cert = tls.getpeercert()
    expires = datetime.datetime.strptime(cert['notAfter'], '%b %d %H:%M:%S %Y %Z').replace(tzinfo=datetime.timezone.utc)
    return (expires - datetime.datetime.now(datetime.timezone.utc)).days


def infrastructure():
    checks = {}
    status, _, body = request('/health')
    checks['health'] = dict(status=status, result='PASS' if status == 200 and '"healthy"' in body else 'FAIL')
    status, _, body = request('/api-docs/swagger-config.json', limit=65536)
    checks['swagger_config'] = dict(status=status, result='PASS' if status == 200 else 'FAIL')
    try:
        urls = [u['url'] for u in json.loads(body).get('urls', []) if u.get('url', '').startswith('/')]
    except ValueError:
        urls = []
    for url in urls:
        status, _, _ = request(url)
        checks['openapi ' + url] = dict(status=status, result='PASS' if status == 200 else 'FAIL')
    status, headers, _ = request('/api/v2/me/profile', 'OPTIONS', {
        'Origin': WEB_ORIGIN, 'Access-Control-Request-Method': 'GET', 'Access-Control-Request-Headers': 'authorization'})
    allowed = headers.get('access-control-allow-origin')
    checks['cors_preflight_from_web'] = dict(status=status, allowOrigin=allowed,
                                             result='PASS' if status < 400 and allowed in (WEB_ORIGIN, '*') else 'FAIL')
    status, _, body = request('/api/v2/me/profile')
    checks['user_api_requires_session'] = dict(status=status, result='PASS' if status == 401 else 'FAIL')
    try:
        days = certificate_days()
        checks['tls_certificate'] = dict(daysRemaining=days, result='PASS' if days >= MIN_CERT_DAYS else 'FAIL')
    except (OSError, ssl.SSLError, ValueError) as error:
        checks['tls_certificate'] = dict(error=type(error).__name__, result='FAIL')
    return checks


def certify(repo, pace=PACE_SECONDS):
    started = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
    result = dict(startedAt=started, host=HOST, infrastructure=infrastructure(), publicRoutes=[], internalRoutes=[])
    for route in public_routes(repo):
        status, _, body = request(concrete(route['pattern']))
        verdict, detail = judge_public(route, status)
        code = re.search(r'"code"\s*:\s*"([A-Z_]+)"', body)
        result['publicRoutes'].append(dict(id=route['id'], domain=route['domain'], path=route['pattern'],
                                           upstream=route.get('upstream'), gatewayFamily=in_gateway_family(route['pattern']),
                                           status=status, errorCode=code.group(1) if code else None,
                                           result=verdict, detail=detail))
        time.sleep(pace)
    for route in internal_routes(repo):
        status, _, _ = request(concrete(route['path']))
        verdict, detail = judge_internal(status)
        result['internalRoutes'].append(dict(route, status=status, result=verdict, detail=detail))
        time.sleep(pace)
    result['failed'] = summary_failures(result)
    result['finishedAt'] = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')
    return result


def summary_failures(result):
    failed = ['infrastructure ' + k for k, v in result['infrastructure'].items() if v['result'] == 'FAIL']
    failed += ['public %s %s' % (r['path'], r['detail']) for r in result['publicRoutes'] if r['result'] == 'FAIL']
    failed += ['internal %s %s' % (r['path'], r['detail']) for r in result['internalRoutes'] if r['result'] == 'FAIL']
    return failed


def main(argv):
    if len(argv) != 2:
        print('usage: public_certification.py <repository root> <output.json>')
        return 2
    result = certify(Path(argv[0]).resolve())
    Path(argv[1]).write_text(json.dumps(result, indent=1, sort_keys=True))
    print(json.dumps(dict(failed=result['failed'],
                          publicRoutes=len(result['publicRoutes']), internalRoutes=len(result['internalRoutes'])), indent=1))
    return 1 if result['failed'] else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

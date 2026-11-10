"""Turns a `certify` run into the production certification verdict and report.

Runs on the GitHub runner with full git history (no production access needed):

    python3 certification_report.py <repo> <main head sha> <vps log> <public.json> <out.md> <out.json> [run url]

Adds, per service, the checks that need the repository:
  revision_on_main     the running image revision is a commit on main (ancestor of the head);
  source_matches_main  none of the image's build inputs (Dockerfile COPY sources) changed
                       between the running revision and the main head;
  migrations_complete  every migration defined at the running revision is applied.
Exit status 0 only for CERTIFIED_READ_ONLY (every applicable read-only check passed).
"""
import json
import re
import subprocess
import sys
from pathlib import Path

import coordinated_release as cr

MIGRATION_NAME = re.compile(r'm\d{8}_\d{6}_[a-z0-9_]+')
SERVICE_CHECKS = ('container_state', 'restart_stability', 'image', 'revision_on_main', 'source_matches_main',
                  'readiness', 'database', 'migrations', 'migrations_complete', 'authentication',
                  'service_auth', 'upstreams')
CHECK_TITLES = {
    'container_state': 'Container running', 'restart_stability': 'No restarts during certification',
    'image': 'Image revision + registry tag', 'revision_on_main': 'Revision on main',
    'source_matches_main': 'Source equals main head', 'readiness': 'Readiness',
    'database': 'Database connectivity (service credentials)', 'migrations': 'Migration histories present',
    'migrations_complete': 'All migrations of the revision applied', 'authentication': 'Rejects anonymous requests',
    'service_auth': 'Service-auth identity and grants', 'upstreams': 'Service-to-service reachability (TLS)',
}
OUTSTANDING_ALWAYS = (
    'Authenticated end-to-end feature journeys (sign-in, meetings, messaging, calling, communications) were not '
    're-executed: they create sessions, audit rows and messages, which this read-only certification must not write. '
    'Full production readiness additionally requires a current, owner-approved run of the acceptance suites with '
    'dedicated acceptance accounts.',
)


def git(repo, *args):
    out = subprocess.run(['git', '-C', str(repo), *args], capture_output=True, text=True)
    return out.returncode, out.stdout


def parse_log(path):
    services, platform, collected, failure, started = [], None, False, None, None
    for line in Path(path).read_text().splitlines():
        if line.startswith('COORDINATED_RELEASE_FAILED'):
            failure = line.strip()
            continue
        try:
            item = json.loads(line)
        except ValueError:
            continue
        if not isinstance(item, dict):
            continue
        if 'certifyService' in item:
            services.append(item['certifyService'])
        elif 'certifyPlatform' in item:
            platform = item['certifyPlatform']
        elif 'certifyCollected' in item:
            collected = True
        elif 'certifyStarted' in item:
            started = item['certifyStarted']
    return dict(services=services, platform=platform, collected=collected, failure=failure, started=started)


def build_inputs(repo, image, head):
    """Repository paths the image is built from (COPY sources without --from), plus its build files."""
    folder = 'services/%s' % image
    code, dockerfile = git(repo, 'show', '%s:%s/Dockerfile' % (head, folder))
    paths = {folder + '/Dockerfile', folder + '/Dockerfile.dockerignore'}
    if code:
        return sorted(paths | {folder})
    for line in dockerfile.splitlines():
        parts = line.split()
        if not parts or parts[0].upper() != 'COPY' or any(x.startswith('--from') for x in parts):
            continue
        sources = [x for x in parts[1:-1] if not x.startswith('--')]
        paths.update(s.rstrip('/') for s in sources if s not in ('.', './'))
    return sorted(paths)


def expected_migrations(repo, image, revision):
    folder = 'services/%s/migration/src' % image
    code, listing = git(repo, 'ls-tree', '-r', '--name-only', revision, folder)
    if code:
        return None
    names = set()
    for path in listing.splitlines():
        stem = Path(path).stem
        if MIGRATION_NAME.fullmatch(stem):
            names.add(stem)
        if path.endswith('.rs'):
            _, text = git(repo, 'show', '%s:%s' % (revision, path))
            names.update(re.findall(r'"(%s)"' % MIGRATION_NAME.pattern, text))
    return names


def repository_checks(repo, head, service):
    name = service['service']
    image = cr.image_of(name)
    revision = (service.get('image') or {}).get('imageRevision')
    out = {}
    if not revision or not cr.SHA_RE.fullmatch(revision) or git(repo, 'cat-file', '-e', revision + '^{commit}')[0]:
        reason = 'running image revision unknown to the repository'
        return {k: dict(result='FAIL', reason=reason) for k in ('revision_on_main', 'source_matches_main', 'migrations_complete')}
    on_main = git(repo, 'merge-base', '--is-ancestor', revision, head)[0] == 0
    out['revision_on_main'] = dict(result='PASS' if on_main else 'FAIL', revision=revision)
    inputs = build_inputs(repo, image, head)
    _, changed = git(repo, 'diff', '--name-only', revision, head, '--', *inputs)
    changed = [c for c in changed.splitlines() if c]
    out['source_matches_main'] = dict(result='PASS' if not changed else 'FAIL', buildInputs=inputs,
                                      changedFiles=changed[:40], changedCount=len(changed))
    migrations = service.get('migrations') or {}
    if migrations.get('result') == 'N/A':
        out['migrations_complete'] = dict(result='N/A', reason=migrations.get('reason'))
    else:
        expected = expected_migrations(repo, image, revision)
        applied = {v for versions in (migrations.get('applied') or {}).values() for v in versions}
        if expected is None:
            out['migrations_complete'] = dict(result='N/A', reason='image has no migration crate')
        else:
            missing = sorted(expected - applied)
            unknown = sorted(v for v in applied - expected if MIGRATION_NAME.fullmatch(v))
            out['migrations_complete'] = dict(result='PASS' if not missing and migrations.get('result') == 'PASS' else 'FAIL',
                                              expected=len(expected), applied=len(applied), missing=missing,
                                              appliedNotInRevision=unknown)
    return out


def service_verdict(service):
    results = [service.get(k, {}).get('result', 'FAIL') for k in SERVICE_CHECKS]
    return 'FAIL' if 'FAIL' in results else 'PASS'


def platform_failures(platform, public):
    failed = []
    if not platform:
        failed.append('platform checks were not collected')
    else:
        for key in ('providerSafeDefaults', 'organizationsEventDelivery', 'coreIdentity'):
            if (platform.get(key) or {}).get('result') != 'PASS':
                failed.append(key)
        failed += ['vps public smoke ' + k for k, v in (platform.get('publicSmokeFromVps') or {}).items() if v != 'PASS']
    if public is None:
        failed.append('public certification did not run')
    else:
        failed += public.get('failed', [])
    return failed


def evaluate(repo, head, log, public):
    data = parse_log(log)
    services = data['services']
    for service in services:
        service.update(repository_checks(repo, head, service))
        service['verdict'] = service_verdict(service)
    expected = list(cr.ORDER)
    seen = [s['service'] for s in services]
    missing = [n for n in expected if n not in seen]
    failures = []
    if data['failure']:
        failures.append('collection failed: ' + data['failure'])
    if not data['collected']:
        failures.append('certification collection did not complete')
    failures += ['service not certified: ' + n for n in missing]
    failures += ['%s: %s' % (s['service'], k) for s in services for k in SERVICE_CHECKS
                 if s.get(k, {}).get('result', 'FAIL') == 'FAIL']
    failures += platform_failures(data['platform'], public)
    if data['failure'] or not data['collected'] or missing:
        verdict = 'INCOMPLETE'
    elif failures:
        verdict = 'NOT_CERTIFIED'
    else:
        verdict = 'CERTIFIED_READ_ONLY'
    revisions = {}
    for s in services:
        revisions.setdefault((s.get('image') or {}).get('imageRevision'), []).append(s['service'])
    return dict(verdict=verdict, head=head, failures=failures, services=services, platform=data['platform'],
                public=public, started=data['started'], revisions=revisions,
                outstanding=list(OUTSTANDING_ALWAYS) + [f for f in failures])


# --- Markdown ----------------------------------------------------------------------------------

def mark(result):
    return {'PASS': 'PASS', 'FAIL': '**FAIL**', 'N/A': 'n/a', 'WARN': 'WARN'}.get(result, str(result))


def render(report, run_url=None):
    lines = ['# CF Meet live-production certification (`certify`)', '']
    lines += ['Verdict: **%s**' % report['verdict'], '',
              'Reference main head: `%s`' % report['head']]
    if run_url:
        lines.append('Workflow run: ' + run_url)
    lines += ['', '## Deployed topology', '', '| Image revision | Services |', '|---|---|']
    for revision, names in sorted(report['revisions'].items(), key=lambda x: str(x[0])):
        lines.append('| `%s` | %s |' % (revision, ', '.join(names)))
    lines += ['', '## Per-service results', '',
              '| Service | ' + ' | '.join(CHECK_TITLES[k] for k in SERVICE_CHECKS) + ' | Verdict |',
              '|---' * (len(SERVICE_CHECKS) + 2) + '|']
    for s in report['services']:
        lines.append('| %s | %s | %s |' % (s['service'], ' | '.join(mark(s.get(k, {}).get('result', 'FAIL')) for k in SERVICE_CHECKS),
                                           mark(s['verdict'])))
    lines += ['', '## Service details', '']
    for s in report['services']:
        image = s.get('image') or {}
        lines.append('### %s (`%s`)' % (s['service'], s['container']))
        lines.append('- image `%s`, revision `%s`, registry tag matches: %s, container label revision `%s`%s, '
                     'matches journaled release: %s' % (image.get('imageId'), image.get('imageRevision'),
                     image.get('registryTagMatches'), image.get('containerLabelRevision'),
                     ' (stale)' if image.get('containerLabelStale') else '', image.get('matchesJournaledRelease')))
        r = s.get('readiness') or {}
        lines.append('- readiness `%s` -> %s (proves database: %s)' % (r.get('kind'), r.get('status', r.get('result')), r.get('provesDatabase')))
        d = s.get('database') or {}
        lines.append('- database: %s role `%s` db `%s` read-only session %s' % (mark(d.get('result')), d.get('role'), d.get('database'), d.get('sessionReadOnly')))
        m = s.get('migrations_complete') or {}
        lines.append('- migrations: expected %s, applied %s, missing %s' % (m.get('expected'), m.get('applied'), m.get('missing') or 'none'))
        a = s.get('authentication') or {}
        lines.append('- anonymous probe `%s` -> %s %s' % (a.get('probe'), a.get('status'), a.get('errorCode') or ''))
        t = s.get('service_auth') or {}
        lines.append('- service auth: key `%s`, issuers %s, activated grants %s, problems %s' % (
            t.get('signingKeyId'), sorted((t.get('issuers') or {}).keys()),
            ['%s<-%s:%s' % (g['activation'], g['issuer'], 'ok' if g['present'] and g['issuerKeyCurrent'] else 'MISSING')
             for g in t.get('activatedGrants') or []], t.get('problems') or 'none'))
        u = s.get('upstreams') or {}
        lines.append('- upstreams: %s' % (', '.join('%s=%s' % (c.get('upstream', c['variable']), c.get('status', c['result']))
                                                    for c in u.get('checks') or []) or 'none'))
        src = s.get('source_matches_main') or {}
        if src.get('changedCount'):
            lines.append('- build inputs changed since revision: %s' % src.get('changedFiles'))
        failed = [k for k in SERVICE_CHECKS if s.get(k, {}).get('result') == 'FAIL']
        if failed:
            lines.append('- **failed:** %s' % ', '.join('%s %s' % (k, json.dumps({x: y for x, y in s[k].items() if x not in ('applied',)})[:300]) for k in failed))
        lines.append('')
    platform = report['platform'] or {}
    lines += ['## Platform checks', '']
    for key in ('providerSafeDefaults', 'organizationsEventDelivery', 'coreIdentity'):
        lines.append('- %s: %s' % (key, json.dumps(platform.get(key))))
    lines.append('- publicSmokeFromVps: %s' % json.dumps(platform.get('publicSmokeFromVps')))
    lines += ['', '## External integrations', '', '| Service | Integration | Switch | State |', '|---|---|---|---|']
    for i in platform.get('integrations') or []:
        lines.append('| %s | %s | `%s`=%s | %s |' % (i['service'], i['integration'], i['variable'], i['value'], i['state']))
    public = report['public'] or {}
    if public:
        lines += ['', '## Public API and gateway', '']
        for k, v in sorted((public.get('infrastructure') or {}).items()):
            lines.append('- %s: %s' % (k, json.dumps(v)))
        counts = {}
        for r in public.get('publicRoutes', []):
            counts.setdefault(r['domain'], {}).setdefault(r['detail'], 0)
            counts[r['domain']][r['detail']] += 1
        lines += ['', '| Domain | Outcome counts (unauthenticated GET) |', '|---|---|']
        for domain, c in sorted(counts.items()):
            lines.append('| %s | %s |' % (domain, ', '.join('%s %d' % kv for kv in sorted(c.items()))))
        internal = public.get('internalRoutes', [])
        lines.append('')
        lines.append('Internal/operator/callback GET routes probed on the public host: %d, answering 2xx: %d.' % (
            len(internal), sum(1 for r in internal if r['detail'] == 'internal/operator/callback route answers on the public host')))
    lines += ['', '## Failures', '']
    lines += ['- ' + f for f in report['failures']] or ['- none']
    lines += ['', '## Outstanding requirements', '']
    lines += ['- ' + o for o in report['outstanding']]
    return '\n'.join(lines) + '\n'


def main(argv):
    if len(argv) not in (6, 7):
        print(__doc__)
        return 2
    repo, head, log, public_path, out_md, out_json = argv[:6]
    public = json.loads(Path(public_path).read_text()) if Path(public_path).exists() else None
    report = evaluate(Path(repo), head, log, public)
    Path(out_json).write_text(json.dumps(report, indent=1, sort_keys=True, default=str))
    Path(out_md).write_text(render(report, argv[6] if len(argv) == 7 else None))
    print('CERTIFICATION VERDICT:', report['verdict'])
    for failure in report['failures']:
        print('  -', failure)
    return 0 if report['verdict'] == 'CERTIFIED_READ_ONLY' else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

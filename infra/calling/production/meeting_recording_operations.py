"""M7: private encrypted recording storage, then a real API/media acceptance gate."""
import json
import copy
import os
from pathlib import Path
import secrets
import shutil
import subprocess

import production as p
import provider_operations as providers
import storage_encryption as storage
import storage_relay as relay
import livekit_webhook as media_webhook

OPERATION = 'activate-meeting-recording'
BUCKET = 'cfmeet-meeting-artifacts'
SNIPPET = Path('/etc/nginx/snippets/cfmeet-meeting-artifacts.conf')
SITE = Path('/etc/nginx/sites-available/api.cfmeet.com')
INCLUDE = 'include /etc/nginx/snippets/cfmeet-meeting-artifacts.conf;'
ANCHOR = 'include /etc/nginx/snippets/cfmeet-calling-routes.conf;'
ACCESS = '/run/secrets/meeting-artifact-access-key'
SECRET = '/run/secrets/meeting-artifact-secret-key'
FLAG = 'FEATURE_RECORDING_V2_ENABLED'


def route_text():
    return ('# Meeting recordings: signed reads only; query signatures never enter logs.\n'
            'location ^~ /' + BUCKET + '/ {\n'
            '    limit_except GET HEAD { deny all; }\n'
            '    access_log off;\n    proxy_buffering off;\n'
            '    proxy_set_header Host $http_host;\n'
            '    proxy_set_header X-Forwarded-Proto https;\n'
            '    add_header Cache-Control "private, no-store" always;\n'
            '    proxy_pass http://' + relay.ADDRESS + ';\n}\n')


def site_text(before):
    if before.count(INCLUDE) == 1:
        return before
    if INCLUDE in before or before.count(ANCHOR) != 1:
        raise RuntimeError('Meeting storage needs the known CF Meet TLS site layout')
    return before.replace(ANCHOR, ANCHOR + '\n    ' + INCLUDE)


def identity():
    """Keys stay in protected VPS files and the existing key-recovery directory."""
    paths = []
    for name, make in (
        ('meeting-artifact-access-key', lambda: 'cfmeet-meeting-' + secrets.token_hex(10)),
        ('meeting-artifact-secret-key', lambda: secrets.token_urlsafe(40)),
    ):
        path = p.SECRETS / name
        recovery = p.ROOT / 'key-recovery' / 'meeting-service' / name
        if not path.exists():
            p.write_private(path, recovery.read_text() if recovery.exists() else make())
        path, value = providers.root_file(name)
        if recovery.exists() and recovery.read_text().strip() != value:
            raise RuntimeError('Meeting storage recovery material differs; keys preserved')
        if not recovery.exists():
            p.write_private(recovery, value)
        paths.append(path)
    user, password = (path.read_text().strip() for path in paths)
    storage.mc(storage.NAME, 'mb', '--ignore-existing', 'kms/' + BUCKET)
    storage.mc(storage.NAME, 'anonymous', 'set', 'none', 'kms/' + BUCKET)
    storage.mc(storage.NAME, 'encrypt', 'set', 'sse-s3', 'kms/' + BUCKET)
    storage.mc(storage.NAME, 'quota', 'set', 'kms/' + BUCKET, '--size', '10GiB')
    policy = {'Version': '2012-10-17', 'Statement': [
        {'Effect': 'Allow', 'Action': ['s3:GetBucketLocation', 's3:ListBucket'],
         'Resource': ['arn:aws:s3:::' + BUCKET]},
        {'Effect': 'Allow', 'Action': ['s3:GetObject', 's3:PutObject', 's3:DeleteObject',
                                     's3:AbortMultipartUpload', 's3:ListMultipartUploadParts'],
         'Resource': ['arn:aws:s3:::' + BUCKET + '/artifacts/*']}]}
    policy_path = p.ROOT / 'meeting-recording' / 'storage-policy.json'
    p.write_private(policy_path, json.dumps(policy))
    ip = p.inspect(storage.NAME)['NetworkSettings']['Networks'][p.NET]['IPAddress']
    env = dict(os.environ, MC_HOST_kms='http://' + p.secret('minio-admin-user') + ':' +
               p.secret('minio-admin-password') + '@' + ip + ':9000')
    result = subprocess.run([str(p.ROOT / 'storage' / 'mc'), '--json', 'admin', 'user', 'info', 'kms', user],
                            env=env, capture_output=True, text=True, timeout=40)
    if result.returncode:
        if 'XMinioAdminNoSuchUser' not in result.stdout + result.stderr:
            raise RuntimeError('Cannot verify the Meeting storage identity')
        storage.mc(storage.NAME, 'admin', 'user', 'add', 'kms', user, password)
    elif json.loads(result.stdout).get('userStatus', '').lower() != 'enabled':
        raise RuntimeError('Meeting storage identity is disabled; preserved')
    storage.mc(storage.NAME, 'admin', 'policy', 'create', 'kms', 'cfmeet-meeting-storage', str(policy_path))
    storage.mc(storage.NAME, 'admin', 'policy', 'attach', 'kms', 'cfmeet-meeting-storage', '--user', user)
    return paths, user, password


def facts(cr):
    s = cr.SERVICES['meeting-service']
    return json.loads(p.run('docker', 'run', '--rm', '--network', 'container:' + s['container'],
                           cr.PROBE_IMAGE, '-fsS', '--max-time', '5', 'http://127.0.0.1:%d/health' % s['port']))


def activate(cr, sha, run_id, confirm):
    if confirm not in ('prepare', 'enable-30-days'):
        raise RuntimeError('confirm must be prepare or enable-30-days')
    cr.require_release(sha)
    if facts(cr).get('recordingStorageProtocol') != 1:
        raise RuntimeError('Release explicit Meeting recording storage support first')
    if confirm == 'enable-30-days':
        env = cr.env_of('meeting-service')
        if env.get('ARTIFACT_STORAGE_BUCKET') != BUCKET or env.get('MEETING_RECORDING_STORAGE_PREPARED') != 'true':
            raise RuntimeError('Prepare the private Meeting recording store first')
        if cr.mem_available_mib() < 2048:
            raise RuntimeError('Recording acceptance needs 2 GiB available memory for media and the disposable client')
        ensure_callback(cr, run_id)
        import meeting_recording_acceptance
        def post():
            report = meeting_recording_acceptance.run(cr, run_id)
            cr.emit('meetingRecordingAcceptance', report)
        cr.apply_activation(OPERATION, sha, run_id, [('meeting-service', {FLAG: 'true'})], post=post)
        return
    if shutil.disk_usage(p.ROOT).free < 12 * 2**30:
        raise RuntimeError('Meeting recording needs 12 GiB free for its 10 GiB bucket quota and reserve')
    if not p.inspect(storage.NAME)['State']['Running'] or p.inspect(storage.NAME)['HostConfig'].get('PortBindings'):
        raise RuntimeError('The encrypted object store must remain running and private')
    relay.wait_for_relay()
    before = SITE.read_text()
    after = site_text(before)
    old = SNIPPET.read_text() if SNIPPET.exists() else None
    if old is not None and old != route_text():
        raise RuntimeError('Unexpected existing Meeting storage route; preserved')
    paths, user, password = identity()
    try:
        SNIPPET.write_text(route_text())
        SITE.write_text(after)
        p.reload_host_nginx()
        if relay.anonymous_status(BUCKET) != 403:
            raise RuntimeError('Unsigned recording storage reads were not refused')
        relay.signed_round_trip(BUCKET, user, password, 'artifacts/probes', run_id)
        overrides = {FLAG: 'false', 'MEETING_RECORDING_STORAGE_PREPARED': 'true',
                     'ARTIFACT_STORAGE_ENDPOINT': 'http://' + storage.NAME + ':9000',
                     'ARTIFACT_STORAGE_PUBLIC_ENDPOINT': 'https://api.cfmeet.com',
                     'ARTIFACT_STORAGE_BUCKET': BUCKET, 'ARTIFACT_STORAGE_REGION': 'us-east-1',
                     'ARTIFACT_STORAGE_PATH_STYLE': 'true', 'ARTIFACT_STORAGE_PREFIX': 'artifacts',
                     'ARTIFACT_STORAGE_ACCESS_KEY': '', 'ARTIFACT_STORAGE_SECRET_KEY': '',
                     'ARTIFACT_STORAGE_ACCESS_KEY_FILE': ACCESS, 'ARTIFACT_STORAGE_SECRET_KEY_FILE': SECRET,
                     'RECORDING_ENCODING': 'h264-opus'}
        def post():
            status = facts(cr)
            if status.get('recordingStorageConfigured') is not True or status.get('recordingEnabled') is not False:
                raise RuntimeError('Prepared Meeting storage is not ready with capture disabled')
        cr.apply_activation(OPERATION, sha, run_id,
                            [('meeting-service', overrides, {'mounts': list(zip(paths, (ACCESS, SECRET)))})], post=post)
    except Exception:
        SITE.write_text(before)
        if old is None:
            SNIPPET.unlink(missing_ok=True)
        else:
            SNIPPET.write_text(old)
        p.reload_host_nginx()
        raise
    # Storage and the read-only signed route persist across image rollback, like schema
    # additions: restoring an old application never strands existing recording bytes.
    cr.emit('meetingRecordingStorage', dict(encrypted=True, anonymousRead='denied',
            quotaGiB=10, defaultRetentionDays=30, captureEnabled=False))


def provider_identity(cr):
    """Compare provider identities on the host; never emit either key or secret."""
    env = cr.env_of('meeting-service')
    configured = bool(env.get('LIVEKIT_API_KEY') and env.get('LIVEKIT_API_SECRET'))
    matches = None
    if configured:
        try:
            matches = (env['LIVEKIT_API_KEY'] == p.secret('calling-livekit-api-key')
                       and env['LIVEKIT_API_SECRET'] == p.secret('calling-livekit-api-secret'))
        except OSError:
            pass
    cr.emit('meetingRecordingProviderIdentity', dict(meetingSignerConfigured=configured,
                                                    sharesCallingSigner=matches))


def callback_plan(config, calling_url, meeting_url, key, secret):
    """Extend exactly the known Calling webhook; preserve every provider key and other setting."""
    current = config.get('webhook')
    if (config.get('keys') or {}).get(key) != secret:
        raise RuntimeError('Meeting callback signer does not match a configured provider key')
    expected = dict(api_key=key, urls=[calling_url])
    combined = dict(api_key=key, urls=[calling_url, meeting_url])
    if current == combined:
        return config, False
    if current != expected:
        raise RuntimeError('Unknown existing provider callback configuration; preserved')
    after = copy.deepcopy(config)
    after['webhook'] = combined
    return after, True


def ensure_callback(cr, run_id):
    """Prepare signed internal callbacks while capture is still disabled; never interrupt live media."""
    import yaml
    env = cr.env_of('meeting-service')
    key, secret = media_webhook.calling_key()
    if env.get('LIVEKIT_API_KEY') != key or env.get('LIVEKIT_API_SECRET') != secret:
        raise RuntimeError('Meeting and provider webhook signers differ; preserve keys and configure callback verification first')
    container = p.inspect('lk-server')
    calling_url = media_webhook.target_url(container)
    if p.NET not in (container.get('NetworkSettings', {}).get('Networks') or {}) or not calling_url:
        raise RuntimeError('Meeting callback requires the existing private media network')
    spec = cr.SERVICES['meeting-service']
    meeting_url = 'http://%s:%d/internal/rtc/livekit/webhook' % (spec['container'], spec['port'])
    if media_webhook.route_status(meeting_url) != 401:
        raise RuntimeError('Meeting callback must reject an unsigned request before activation')
    if media_webhook.CONFIG.is_symlink():
        raise RuntimeError('Unknown provider configuration path; preserved')
    before = media_webhook.CONFIG.read_text()
    after, change = callback_plan(yaml.safe_load(before) or {}, calling_url, meeting_url, key, secret)
    if not change:
        cr.emit('meetingRecordingCallback', dict(configured=True, callingPreserved=True, keysPreserved=True))
        return
    if media_webhook.participants():
        raise RuntimeError('Active media participants prevent the callback configuration restart')
    active = p.lk_request('Egress/ListEgress', {'active': True}, key, secret, {'roomRecord': True})
    if active.get('items'):
        raise RuntimeError('Active captures prevent the callback configuration restart')
    changed = yaml.safe_dump(after, sort_keys=False)
    if yaml.safe_load(changed) != after:
        raise RuntimeError('Provider configuration did not round-trip; preserved')
    p.write_private(p.ROOT / 'meeting-recording' / ('livekit-webhook.before.' + run_id + '.yaml'), before)
    try:
        # Keep the inode used by the media server's existing file bind mount.
        media_webhook.CONFIG.write_text(changed)
        media_webhook.restart_and_wait()
        if media_webhook.route_status(meeting_url) != 401 or media_webhook.route_status(calling_url) != 401:
            raise RuntimeError('Provider callback route verification failed')
        p.regression()
    except Exception:
        media_webhook.CONFIG.write_text(before)
        media_webhook.restart_and_wait()
        raise
    cr.emit('meetingRecordingCallback', dict(configured=True, callingPreserved=True, keysPreserved=True,
                                           idleMediaRestart=True))

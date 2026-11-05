"""Section 8 operations in the existing VPS workflow. Never emit secrets/content."""
import json
from urllib.parse import urlsplit
import production as p

SAFE_FLAGS = ('CF_RUNTIME_ENVIRONMENT', 'MESSAGING_E2EE_DEVICES_ENABLED',
    'MESSAGING_PUSH_PROVIDER', 'MESSAGING_REALTIME_REQUIRE_TLS', 'MESSAGING_KT_LOG_ID',
    'MESSAGING_ATTACHMENTS_S3_SERVER_SIDE_ENCRYPTION')
DEPENDENCIES = {
    'realtime_signing': ('MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE',),
    'transparency_signing': ('MESSAGING_KT_SIGNING_KEY_FILE',),
    'transparency_checkpoint': ('MESSAGING_KT_CHECKPOINT_FILE',),
    'transparency_witnesses': ('MESSAGING_KT_WITNESSES_FILE',),
    'managed_encryption': ('MESSAGING_MANAGED_KEYRING_FILE',),
    'redis': ('MESSAGING_REALTIME_REDIS_URL',),
    'attachment_storage': ('MESSAGING_ATTACHMENTS_S3_ENDPOINT', 'MESSAGING_ATTACHMENTS_S3_BUCKET'),
    'malware_scanner': ('MESSAGING_CLAMAV_ADDRESS',),
    'metrics_listener': ('MESSAGING_METRICS_ADDRESS',),
    'apns': ('MESSAGING_PUSH_APNS_KEY_FILE', 'MESSAGING_PUSH_APNS_KEY_ID', 'MESSAGING_PUSH_APNS_TEAM_ID', 'MESSAGING_PUSH_APNS_TOPIC'),
    'fcm': ('MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE',),
    'web_push': ('MESSAGING_PUSH_WEB_VAPID_KEY_FILE', 'MESSAGING_PUSH_WEB_VAPID_SUBJECT'),
    'communications_policy': ('CF_COMMUNICATIONS_BASE_URL', 'CF_COMMUNICATIONS_PERSONAL_TENANT_ID', 'CF_COMMUNICATIONS_PERSONAL_APPLICATION_ID', 'CF_COMMUNICATIONS_PERSONAL_ENVIRONMENT_ID'),
}


def configuration_summary(env):
    return {'flags': {key: env.get(key) for key in SAFE_FLAGS},
            'dependencies_configured': {name: all(bool(env.get(k)) for k in keys)
                                        for name, keys in DEPENDENCIES.items()}}


def database_summary():
    # Messaging's OWN database; aggregates/migration names, never content/device keys.
    query = """SELECT json_build_object(
      'migrations', (SELECT json_agg(version ORDER BY version) FROM messaging_seaql_migrations),
      'tables', (SELECT count(*) FROM pg_tables WHERE schemaname='public'),
      'devices', (SELECT count(*) FROM messaging_devices),
      'conversations', (SELECT count(*) FROM messaging_conversations),
      'pending_outbox', (SELECT count(*) FROM messaging_outbox_events WHERE status='PENDING'),
      'dead_outbox', (SELECT count(*) FROM messaging_outbox_events WHERE status='DEAD_LETTER'),
      'oldest_pending_seconds', (SELECT COALESCE(EXTRACT(EPOCH FROM now()-min(occurred_at)),0)::bigint
                                FROM messaging_outbox_events WHERE status='PENDING'),
      'push_statuses', (SELECT json_object_agg(status,n) FROM
                       (SELECT status,count(*) n FROM messaging_push_jobs GROUP BY status) s)
    )"""
    return json.loads(p.run('docker', 'exec', '-u', 'postgres', 'cfmeet-core-db',
                            'psql', '-U', 'postgres', '-d', 'cfmeet_messaging', '-tAc', query))


def inspect_runtime(sha, run_id):
    p.identity()
    c = p.inspect('cfmeet-messaging')
    env = p.env_of(c)
    image = json.loads(p.run('docker', 'image', 'inspect', c['Image']))[0]
    db = urlsplit(env['DATABASE_URL'])
    receipt = {'schemaVersion': 1, 'operation': 'messaging-inspect', 'workflowSha': sha,
        'runId': run_id, 'imageId': c['Image'],
        'imageSource': image['Config'].get('Labels', {}).get('org.opencontainers.image.revision'),
        'registryDigests': image.get('RepoDigests', []),
        'database': {'host': db.hostname, 'name': db.path.lstrip('/'), 'role': db.username},
        'configuration': configuration_summary(env),
        'limits': {key: c['HostConfig'].get(key) for key in ('Memory', 'NanoCpus', 'LogConfig')},
        'publishedPorts': c['HostConfig']['PortBindings'],
        'databaseSummary': database_summary(), 'offServerRecovery': 'DEFERRED_BY_OWNER',
        'independentCryptographicAssessment': 'NOT_ESTABLISHED_BY_RUNTIME_INSPECTION'}
    source = receipt['imageSource']
    if source and len(source) == 40 and all(x in '0123456789abcdef' for x in source):
        manifest = p.ROOT / 'communications' / ('registry-' + source + '.json')
        if manifest.is_file():
            receipt['registryEvidence'] = json.loads(manifest.read_text()).get('messaging')
    for path in ('/health', '/ready', '/openapi.json'):
        code = p.run('docker', 'run', '--rm', '--network', p.NET, 'curlimages/curl:8.12.1',
                     '-sS', '--max-time', '15', '-o', '/dev/null', '-w', '%{http_code}',
                     'http://cfmeet-messaging:8097' + path)
        receipt.setdefault('privateProbes', {})[path] = int(code)
    target = p.ROOT / 'communications' / ('messaging-baseline-' + run_id + '.json')
    p.write_private(target, json.dumps(receipt, sort_keys=True))
    print(json.dumps(receipt, sort_keys=True))
    return receipt


def run(operation, sha, run_id):
    if operation == 'messaging-inspect':
        inspect_runtime(sha, run_id)
    elif operation == 'messaging-configure':
        import messaging_runtime
        messaging_runtime.configure(sha, run_id)
    elif operation == 'messaging-verify':
        import messaging_acceptance
        messaging_acceptance.execute(sha, run_id)
        receipt = inspect_runtime(sha, run_id)
        assert all(code == 200 for code in receipt['privateProbes'].values()), 'Messaging probes failed'
        p.regression()
    else:
        raise AssertionError('Unsupported Messaging operation')

"""B2: prepare every synchronous producer before enabling the local send path."""
import json
import uuid

OPERATION = 'activate-local-send-policy'
GENERATION = 'COMMUNICATIONS_SEND_POLICY_GENERATION'
FAST = 'MESSAGING_LOCAL_SEND_POLICY_ENABLED'
PREPARED = 'MESSAGING_LOCAL_SEND_POLICY_PREPARED'
SOURCE_FLAGS = {'communications': 'COMMUNICATIONS_SYNCHRONOUS_POLICY_EVENTS_ENABLED',
                'calling': 'CALLING_SYNCHRONOUS_BLOCK_PROJECTION_ENABLED'}
OWNERS = ('messaging', 'communications', 'calling')


def pause_for_release(cr, sha, run_id):
    """Fail closed on the verified deployed images, before a release swaps any source.

    The release already holds its main/CI/production gates. Its new image inputs may
    differ, so verify the actual old source images against their completed release;
    this operation changes only Messaging's existing runtime flags, never an image.
    """
    base = cr.last_release()
    if not base:
        raise RuntimeError('Cannot pause policy without a completed source release')
    manifest = cr.registry(base['sha'])
    for name in OWNERS:
        cr.verify_running(cr.SERVICES[name]['container'], *cr.expected_image(name, base['sha'], manifest))
    def post():
        if facts(cr, 'messaging').get('sendPolicyLocalEnabled') is not False:
            raise RuntimeError('Messaging must confirm authoritative send checks before source upgrades')
    cr.apply_activation(OPERATION, sha, run_id,
        [('messaging', {FAST: 'false', PREPARED: 'false'})], post=post)
    cr.emit('sendPolicyReleasePause', dict(deployedRelease=base['sha'], localEnabled=False))


def facts(cr, name):
    service = cr.SERVICES[name]
    body = cr.p.run('docker', 'run', '--rm', '--network', 'container:' + service['container'],
                    cr.PROBE_IMAGE, '-fsS', '--max-time', '4',
                    'http://127.0.0.1:%d%s' % (service['port'], service['health']))
    if len(body) > 65536:
        raise RuntimeError('Unexpected send-policy readiness response')
    return json.loads(body)


def enabled(cr):
    return cr.env_of('messaging').get(FAST) == 'true'


def require_remote_path(cr):
    """Keep a previous source image from confirming an unprojected block during rollback."""
    if enabled(cr):
        raise RuntimeError('Disable the local send policy before releasing or rolling back its sources')


def validate_generation(envs, states, prepared=False):
    generation = envs['messaging'].get(GENERATION)
    try:
        uuid.UUID(generation or '')
    except (ValueError, TypeError):
        raise RuntimeError('Prepare a fresh send-policy generation first') from None
    if prepared and envs['messaging'].get(PREPARED) != 'true':
        raise RuntimeError('Prepare a fresh send-policy generation before enabling')
    for name in ('messaging', 'communications', 'calling'):
        state = states[name]
        if state.get('sendPolicyProtocol') != 1 or state.get('sendPolicyGeneration') != generation \
                or envs[name].get(GENERATION) != generation:
            raise RuntimeError('Send-policy receiver/source generations do not match')
        if name in SOURCE_FLAGS and (envs[name].get(SOURCE_FLAGS[name]) != 'true'
                                     or state.get('sendPolicySynchronous') is not True):
            raise RuntimeError('Every block producer must synchronously acknowledge Messaging')
    return generation


def activate(cr, sha, run_id, confirm):
    if confirm not in ('prepare', 'enable', 'disable'):
        raise RuntimeError('confirm must be prepare, enable or disable')
    cr.require_release(sha)
    if confirm == 'disable':
        cr.apply_activation(OPERATION, sha, run_id,
                            [('messaging', {FAST: 'false', PREPARED: 'false'})])
        return
    names = ('messaging', 'communications', 'calling')
    before = {name: facts(cr, name) for name in names}
    if any(value.get('sendPolicyProtocol') != 1 for value in before.values()):
        raise RuntimeError('Release the send-policy protocol and migration on every owner first')
    envs = {name: cr.env_of(name) for name in names}
    if confirm == 'prepare':
        require_remote_path(cr)
        generation = str(uuid.uuid4())
        changes = [('messaging', {FAST: 'false', PREPARED: 'true', GENERATION: generation})]
        changes += [(name, {flag: 'true', GENERATION: generation}) for name, flag in SOURCE_FLAGS.items()]
    else:
        generation = validate_generation(envs, before, prepared=True)
        changes = [('messaging', {FAST: 'true', PREPARED: 'false'})]

    def post():
        states = {name: facts(cr, name) for name in names}
        validate_generation({name: cr.env_of(name) for name in names}, states)
        if states['messaging'].get('sendPolicyLocalEnabled') != (confirm == 'enable'):
            raise RuntimeError('Messaging did not apply the requested send-policy mode')
        cr.emit('localSendPolicy', dict(stage=confirm, generation=generation,
                                       coldEligibilityCheck='remote', warmSendChecks='local'))

    cr.apply_activation(OPERATION, sha, run_id, changes, post=post)

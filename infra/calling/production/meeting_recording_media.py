"""Prepare the shared, known LiveKit SFU's missing UDP ingress, only while idle.

This narrow media operation preserves its immutable image, keys, bind mounts,
TCP/TURN ports and Docker settings. It never uses the core release's ownership
mechanism to replace an arbitrary foreign container.
"""
import copy
import json
import time

import production as p
import livekit_webhook as media

NAME = 'lk-server'
PORT = 7882
BINDING = [{'HostIp': '0.0.0.0', 'HostPort': str(PORT)}]


def plan(config, container):
    rtc = config.get('rtc') or {}
    host = container['HostConfig']
    if (host.get('NetworkMode') != 'bridge' or host.get('AutoRemove')
            or set(container['NetworkSettings']['Networks']) != {'bridge', p.NET}
            or any(m.get('Type') != 'bind' for m in container.get('Mounts', []))
            or any((e.get('IPAMConfig') or {}).get(k) for e in container['NetworkSettings']['Networks'].values()
                   for k in ('IPv4Address', 'IPv6Address'))
            or any(k.startswith('com.docker.compose.') for k in (container['Config'].get('Labels') or {}))):
        raise RuntimeError('Unknown shared media container topology; preserved')
    if (rtc.get('tcp_port') != 7881 or rtc.get('udp_port') not in (None, PORT)
            or rtc.get('port_range_start', 50000) != 50000
            or rtc.get('port_range_end', 60000) != 60000):
        raise RuntimeError('Unknown shared media port configuration; preserved')
    bindings = host.get('PortBindings') or {}
    if str(PORT) + '/udp' in bindings and bindings[str(PORT) + '/udp'] != BINDING:
        raise RuntimeError('Unknown existing media UDP binding; preserved')
    after = copy.deepcopy(config)
    after.setdefault('rtc', {}).update(udp_port=PORT)
    for key in ('port_range_start', 'port_range_end'):
        after['rtc'].pop(key, None)
    cfg = copy.deepcopy(container['Config'])
    cfg['Image'] = container['Image']
    cfg['ExposedPorts'] = copy.deepcopy(cfg.get('ExposedPorts') or {})
    cfg['ExposedPorts'][str(PORT) + '/udp'] = {}
    cfg['HostConfig'] = copy.deepcopy(host)
    cfg['HostConfig'].setdefault('PortBindings', {})[str(PORT) + '/udp'] = copy.deepcopy(BINDING)
    endpoints = {}
    for network, endpoint in container['NetworkSettings']['Networks'].items():
        # Default bridge does not accept network aliases. Retain user-network aliases
        # and explicit options, omitting only the old container's generated aliases.
        value = {}
        for key in ('IPAMConfig', 'Links', 'DriverOpts'):
            if endpoint.get(key):
                value[key] = copy.deepcopy(endpoint[key])
        if network != 'bridge':
            value['Aliases'] = [a for a in (endpoint.get('Aliases') or [NAME])
                                if a not in (container['Id'], container['Id'][:12])]
        endpoints[network] = value
    cfg['NetworkingConfig'] = {'EndpointsConfig': endpoints}
    return after, cfg, after != config or bindings.get(str(PORT) + '/udp') != BINDING


def known_identity(config, container):
    if not p.env_of(container).keys().isdisjoint({'LIVEKIT_CONFIG', 'LIVEKIT_KEYS', 'REDIS_HOST', 'REDIS_PASSWORD'}):
        raise RuntimeError('Alternate shared SFU configuration environment; preserved')
    key, secret = media.calling_key()
    if (config.get('keys') or {}).get(key) != secret:
        raise RuntimeError('Shared SFU identity does not match Calling; preserved')
    if not any(m.get('Source') == str(media.CONFIG) and m.get('Destination') == '/etc/livekit.yaml'
               for m in container.get('Mounts', [])) or '/etc/livekit.yaml' not in (container['Config'].get('Cmd') or []):
        raise RuntimeError('Unknown shared SFU configuration mount; preserved')


def prepared():
    import yaml
    if media.CONFIG.is_symlink():
        return False
    config = yaml.safe_load(media.CONFIG.read_text()) or {}
    container = p.inspect(NAME)
    known_identity(config, container)
    _, _, change = plan(config, container)
    return not change and container['State']['Running']


def idle():
    key, secret = media.calling_key()
    if media.participants() or p.lk_request('Egress/ListEgress', {'active': True}, key, secret,
                                           {'roomRecord': True}).get('items'):
        raise RuntimeError('Active media prevents UDP ingress preparation')


def wait_ready(mux):
    for _ in range(30):
        try:
            p.current_rooms()
            if mux and not prepared():
                raise RuntimeError('Media UDP ingress is not prepared')
            p.regression()
            return
        except (RuntimeError, OSError, AssertionError):
            time.sleep(2)
    raise RuntimeError('Shared SFU did not recover its expected media configuration')


def ensure(cr, run_id):
    import yaml
    if media.CONFIG.is_symlink():
        raise RuntimeError('Unknown shared SFU configuration path; preserved')
    original = media.CONFIG.read_text()
    before = yaml.safe_load(original) or {}
    old = p.inspect(NAME)
    known_identity(before, old)
    after, cfg, change = plan(before, old)
    if not change:
        if not old['State']['Running']:
            raise RuntimeError('Prepared shared SFU is not running; preserved')
        cr.emit('meetingMediaIngress', dict(udpMuxPort=PORT, prepared=True, reused=True))
        return
    idle()
    names = p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()
    previous = NAME + '-udp-rollback-' + run_id
    if previous in names:
        raise RuntimeError('Existing media rollback container; inspect it before retrying')
    others = [name for name in names if name != NAME]
    ports = p.run('docker', 'inspect', '--format', '{{json .HostConfig.PortBindings}}', *others) if others else ''
    for line in ports.splitlines():
        if any(binding.get('HostPort') == str(PORT)
               for bindings in (json.loads(line) or {}).values() for binding in (bindings or [])):
            raise RuntimeError('Media UDP port is already reserved; preserved')
    saved = p.ROOT/'meeting-recording'/('media-ingress-' + run_id)
    saved.mkdir(parents=True, exist_ok=False, mode=0o700)
    p.write_private(saved/'container.before.json', json.dumps(old))
    p.write_private(saved/'livekit.before.yaml', original)
    changed = yaml.safe_dump(after, sort_keys=False)
    if yaml.safe_load(changed) != after:
        raise RuntimeError('Media configuration did not round-trip; preserved')
    cfg['Labels'] = {**(cfg.get('Labels') or {}), 'cfmeet.media.udp-mux-run': run_id}
    idle()  # Recheck immediately before stopping; inventory/backup can take time.
    p.run('docker', 'rename', NAME, previous)
    try:
        p.run('docker', 'stop', '--time', '30', previous)
        media.CONFIG.write_text(changed)  # preserve the existing file bind's inode
        p.docker_api('POST', '/containers/create?name=' + NAME, cfg)
        p.docker_api('POST', '/containers/' + NAME + '/start')
        wait_ready(True)
    except Exception:
        # A local Docker API request can create the container before its response
        # is lost. Remove only our exact labelled replacement in that case.
        exists = NAME in p.run('docker', 'ps', '-a', '--format', '{{.Names}}').splitlines()
        if exists:
            current = p.inspect(NAME)
            if (current['Image'] != old['Image']
                    or (current['Config'].get('Labels') or {}).get('cfmeet.media.udp-mux-run') != run_id):
                raise RuntimeError('Unknown container occupies shared media name; protected rollback retained') from None
            p.run('docker', 'stop', '--time', '30', NAME)
            p.run('docker', 'rm', '-f', NAME)
        media.CONFIG.write_text(original)
        p.run('docker', 'rename', previous, NAME)
        p.run('docker', 'start', NAME)
        wait_ready(False)
        raise
    cr.emit('meetingMediaIngress', dict(udpMuxPort=PORT, prepared=True, keysPreserved=True,
                                       imagePreserved=True, tcpTurnPortsPreserved=True, idleMediaReplacement=True))

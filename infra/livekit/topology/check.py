"""Assertions for the topology reproduction. Server connection types come from
LiveKit's own 'participant active' log fields (no secrets in this CI-only config)."""
import collections, json, re, sys

report = json.load(open(sys.argv[1])); logs = open(sys.argv[2], errors='replace').read().splitlines()
types = collections.Counter()
for line in logs:
    if 'participant active' in line and '{' in line:
        types[json.loads(line[line.index('{'):]).get('connectionType', 'unknown')] += 1
never = sum('without connection' in line for line in logs)
summary = {k: v for k, v in report.items() if k != 'results'}
summary.update(server_connection_types=dict(types), server_participants_never_connected=never)
print(json.dumps(summary, indent=2))
with open(sys.argv[3], 'a') as out:
    out.write(f"### {report['topology']}\n\n```json\n{json.dumps(summary, indent=2)}\n```\n")
if report['topology'] == 'udp-mux-bridge':
    # The corrected topology must be fully reliable over UDP: no fallback, no retries.
    assert report['connected'] == report['trials'], 'corrected topology had connect failures'
    assert report['bidirectional_audio'] == report['trials'], 'corrected topology lost media'
    assert types and set(types) == {'udp'}, f'corrected topology did not use UDP: {dict(types)}'
else:
    # Reproduction of production: WebRTC UDP is never usable in this topology.
    assert 'udp' not in types, f'production replica unexpectedly connected over UDP: {dict(types)}'

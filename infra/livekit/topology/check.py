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
# Both topologies must connect every trial with bidirectional audio. The inspected
# production bridge topology falls back to ICE-TCP; that is recorded, not failed:
# run 42009530386 showed it connects reliably, so it is NOT the timeout root cause.
assert report['connected'] == report['trials'], f"{report['topology']}: connect failures"
assert report['bidirectional_audio'] == report['trials'], f"{report['topology']}: media lost"
assert never == 0, f"{report['topology']}: participants never connected"

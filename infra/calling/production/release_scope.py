"""Release scope: which images a coordinated release must actually replace.

Runs on the GitHub runner with full git history before `release` is shipped to the VPS:

    python3 release_scope.py <repo> <release sha> <latest completed release sha | -> <out.json>

An image whose build inputs (the Dockerfile COPY sources certify already uses) did not
change between the latest completed release and the release SHA is byte-for-byte the
same software: the VPS keeps that service's running container (after checking it runs
exactly the image recorded for the previous release) instead of replacing it. Only the
changed services are migrated and swapped, so a Meeting-only change no longer restarts
all 13 services. Without a previous release, or when the previous release is not an
ancestor of the release SHA, every image is in scope (the former behaviour).
"""
import json
import sys
from pathlib import Path

import certification_report as report
import coordinated_release as cr


def evaluate(repo, head, base):
    doc = dict(head=head, base=base, changed=list(cr.IMAGES), unchanged=[], reason='full release')
    if not cr.SHA_RE.fullmatch(head) or not base or not cr.SHA_RE.fullmatch(base):
        doc['reason'] = 'no previous completed release: full release'
        return doc
    if base == head:
        doc.update(changed=[], unchanged=list(cr.IMAGES), reason='release SHA already released')
        return doc
    if report.git(repo, 'cat-file', '-e', base + '^{commit}')[0] or \
            report.git(repo, 'merge-base', '--is-ancestor', base, head)[0] != 0:
        doc['reason'] = 'previous release is not an ancestor: full release'
        return doc
    changed, unchanged, paths = [], [], {}
    for image in cr.IMAGES:
        inputs = report.build_inputs(repo, image, head)
        code, diff = report.git(repo, 'diff', '--name-only', base, head, '--', *inputs)
        if code:
            changed.append(image)
            continue
        files = [f for f in diff.splitlines() if f]
        (changed if files else unchanged).append(image)
        if files:
            paths[image] = files[:50]
    doc.update(changed=changed, unchanged=unchanged, changedInputs=paths,
               reason='only images with changed build inputs are replaced')
    return doc


def main(argv):
    if len(argv) != 4:
        print('usage: release_scope.py <repo> <release sha> <base sha|-> <out.json>')
        return 2
    repo, head, base, out = argv
    doc = evaluate(Path(repo), head, None if base == '-' else base)
    Path(out).write_text(json.dumps(doc, indent=1, sort_keys=True))
    print(json.dumps({k: doc[k] for k in ('head', 'base', 'changed', 'unchanged', 'reason')}, sort_keys=True))
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

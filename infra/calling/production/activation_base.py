"""Activation base: may an activation run on the deployed release without a new release?

Runs on the GitHub runner with full git history, before an activation step is shipped to
the VPS:

    python3 activation_base.py <repo> <dispatched main head> <latest completed release sha> <out.json>

Activations change flags, trust bundles and host routing; they never change images. They
used to require that the dispatched main head itself had a completed coordinated release,
so every docs or tooling commit on main forced a redeploy of all 13 services before the next
activation. This module states the narrower condition that actually matters: the deployed
release is an ancestor of the dispatched head, and none of the build inputs of any of the
13 images (the Dockerfile COPY sources certify already uses) changed between the two. The
VPS side (`coordinated_release.require_release`) then still checks that every container runs
exactly the recorded image of that release. `verify` stays strictly same-SHA.

Exit status 0 when the head is equivalent to the base (the document says so); 1 otherwise.
"""
import json
import sys
from pathlib import Path

import certification_report as report
import coordinated_release as cr


def evaluate(repo, head, base):
    doc = dict(head=head, base=base, equivalent=False, changed=[], inputs=[])
    if not (cr.SHA_RE.fullmatch(head) and cr.SHA_RE.fullmatch(base)):
        doc['reason'] = 'invalid SHA'
        return doc
    if base == head:
        doc.update(equivalent=True, reason='head is the released SHA')
        return doc
    if report.git(repo, 'cat-file', '-e', base + '^{commit}')[0]:
        doc['reason'] = 'released SHA unknown to the repository'
        return doc
    if report.git(repo, 'merge-base', '--is-ancestor', base, head)[0] != 0:
        doc['reason'] = 'released SHA is not an ancestor of the dispatched head'
        return doc
    inputs = sorted({path for image in cr.IMAGES for path in report.build_inputs(repo, image, head)})
    code, changed = report.git(repo, 'diff', '--name-only', base, head, '--', *inputs)
    if code:
        doc['reason'] = 'git diff failed'
        return doc
    changed = [c for c in changed.splitlines() if c]
    doc.update(inputs=inputs, changed=changed[:200], changedCount=len(changed), equivalent=not changed,
               reason='no image build input changed' if not changed else 'image build inputs changed: release first')
    return doc


def main(argv):
    if len(argv) != 4:
        print('usage: activation_base.py <repo> <head> <base> <out.json>')
        return 2
    repo, head, base, out = argv
    doc = evaluate(Path(repo), head, base)
    Path(out).write_text(json.dumps(doc, indent=1, sort_keys=True))
    print(json.dumps({k: doc[k] for k in ('head', 'base', 'equivalent', 'reason')}, sort_keys=True))
    return 0 if doc['equivalent'] else 1


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

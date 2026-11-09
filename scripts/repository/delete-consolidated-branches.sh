#!/usr/bin/env bash
# Delete the temporary branches consolidated into `staging` by the final closure (2026-11-09).
# Each deletion is pinned to the tip recorded in docs/implementation/cfmeet-branch-consolidation-report.md
# (--force-with-lease): a branch that moved since the audit is NOT deleted and is reported instead.
# Never touches `main` or `staging`. Run from a clone with push rights:
#   bash scripts/repository/delete-consolidated-branches.sh            # dry run (default)
#   bash scripts/repository/delete-consolidated-branches.sh --apply    # delete
set -euo pipefail
apply=0; [ "${1:-}" = "--apply" ] && apply=1
git fetch --prune origin
# Safety: staging must contain every recorded tip (ancestor or patch-identical).
while read -r branch sha; do
  case "$branch" in main|staging) echo "refusing to touch $branch"; exit 2 ;; esac
  current="$(git ls-remote --heads origin "refs/heads/$branch" | cut -f1)"
  if [ -z "$current" ]; then echo "gone      $branch"; continue; fi
  # The closure's own working branch is not pinned (its tip is the last closure commit):
  # it is deleted only when fully contained in staging.
  if [ "$sha" = ANCESTOR_OF_STAGING ]; then
    if git merge-base --is-ancestor "$current" origin/staging; then sha="$current"; else echo "UNIQUE    $branch — not deleted"; continue; fi
  fi
  if [ "$current" != "$sha" ]; then echo "MOVED     $branch ($current != recorded $sha) — not deleted"; continue; fi
  if ! git merge-base --is-ancestor "$sha" origin/staging && [ -n "$(git cherry origin/staging "$sha" | grep '^+' || true)" ]; then
    echo "UNIQUE    $branch has commits not in staging — not deleted"; continue
  fi
  if [ "$apply" = 1 ]; then
    git push --force-with-lease="refs/heads/$branch:$sha" origin ":refs/heads/$branch" && echo "deleted   $branch"
  else
    echo "would delete $branch $sha"
  fi
done <<'LIST'
blueprint/final-org-events b148ca9b143733a7cdf2629398423c93b95f6a48
blueprint/s02-account-security dcfcf1a32afd98df9cd318f7498db97e891a3cea
blueprint/s02-s03-hardening fb4900032e57935dc81501684485f6b2ed73159c
blueprint/s02-s03-sec015-guard 57240b27b585c7b606799c658df9b2ed1d9ac993
blueprint/s02-s03-sec016-alerts 7284c3fc179542ecb5e85176375020bab9f077b4
blueprint/s02-s03-sec018-roles 29b9c7163d35c49d5a1d8d0d502d7eeff95303e1
blueprint/s04-meeting-organizations 4ed16e3b8e2161bd40d3587507aff0ba2f5db71d
blueprint/s04-organizations-service 5062b6afd14a00fbd73931cb3dcd677989c83b6b
blueprint/s06-07-org-teams-provisioning e5e96153f802ff6b1106c052dde80c1991a7b67c
blueprint/s08-workspaces 010018245687a6e2e7bd654a017ab1e5354ac475
blueprint/s09-s11-profile-notifications a167a5f7252f2d03d663653e7b7d547eb4021d52
blueprint/s10-meetings-calendar 10959b7fa35c545b7c9ac1749b53e9d092d90f76
blueprint/s12-calling-lifecycle bc85daf2922ec6dd654a9467a0e4a9f3c582c900
blueprint/s12-org-policy-calling-scope 736605fc4ae5bf7d248af1475bfe5f0ce285b293
blueprint/s13-messaging 4c25700443ca4c9fdfbff2b7c12f16fae75f48ad
blueprint/s14-hub-revocation 9592564ca1d06ea64002f81b0ae0a993380db99b
blueprint/s15-developer-platform c53b60847fd851e598fbad0016342254cc401345
blueprint/s16-retirement-tooling 3a90d07766e7f8939764e343fc0008ee2261497c
blueprint/s17-developer-portal 977b792aa2ec5f48604d05b28865d57497a9b1f7
blueprint/s17-integration ccb7caa923c6f0ac48ca638920df7a20ac2500d4
blueprint/s17-integration-r2 3ebaac24a2b25c75b8c00c6aa03679a8284843bd
blueprint/s17-integration-r3 3ebaac24a2b25c75b8c00c6aa03679a8284843bd
blueprint/s17-mobile f00feef4e2acc18cd50534e99dde7fd258bf3ff5
blueprint/s17-web-account 2b3e2fb134ae4d421f4afab3d13f6f672d31554f
blueprint/s17-web-calling 24bd189f2b491645fa03a24dc84c8c8a1dc782cf
blueprint/s17-web-meetings afce090f552637cccf2cdd59dedfdbd2e5dc2440
blueprint/s17-web-messaging c0fcedcefd96101f4e95073f042287c7074ea1dd
blueprint/s18-certification 361585a205476e5c8e3437e1c30d14529433630e
blueprint/s18-integration ab349836fa9d34cbb1da3526483becfb5c83ee37
blueprint/s18-sip-race 0ee026e9b939b84d1c33be744533d58986047d48
blueprint/s18-sip-race-r2 0ee026e9b939b84d1c33be744533d58986047d48
blueprint/s18-sip-race-r3 0ee026e9b939b84d1c33be744533d58986047d48
blueprint/s18-sip-race-r4 0ee026e9b939b84d1c33be744533d58986047d48
claude/cosmicforge-shared-scan-gateway-pawadp 18eebeba5ee26858e048eac3edeb7db1eea0dda7
claude/messaging-realtime-ingress 53e26d809e58bd47745e68410f1983176ceb2651
feature/sections-9-23 71eedfebcdac788c80a2f3743527cc953b07e257
fix/rtc-quarantine-wait 6d708f04598a70fe094d111e90f41abdf7ffd7ed
fix/webhook-marker-false-positive 379c8740aaf8ccde6d86fc5809dc315b21ea492f
claude/cfmeet-consolidation-closure-vk2pe3 ANCESTOR_OF_STAGING
LIST
git fetch --prune origin
echo "remote heads now:"; git ls-remote --heads origin

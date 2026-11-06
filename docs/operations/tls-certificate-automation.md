# api.cfmeet.com TLS certificate automation — operations runbook

## What is automated

| Step | Mechanism |
|---|---|
| Renewal | Certbot's existing timer and renewal configuration for the `api.cfmeet.com` lineage (unchanged; it renews at 30 days left). |
| Activation | Certbot deploy hook `/etc/letsencrypt/renewal-hooks/deploy/cfmeet-core-tls` runs `certificate-monitor.py --reload` for this lineage only. |
| Daily check and self-repair | systemd timer `cfmeet-core-tls-certificate.timer` (journal tag `cfmeet-certificate`), plus the scheduled GitHub workflow `cfmeet-certificate-monitor.yml`. |
| Operator notification | Comments on the open issue "Operations: api.cfmeet.com TLS certificate monitoring". Its body mentions the repository owner account, so GitHub notifies them. Each comment is read back by id before the run reports `NOTIFICATION_DELIVERED`. A scheduled run that ends in ALERT also fails (red), which GitHub reports separately. |

Targets: the private proxy container `cfmeet-core-tls` (mounts `live/` and `archive/` read-only at their own paths; no numbered file is pinned). Host nginx is also a target when it serves the hostname from the managed `live/` lineage. Program and config: `/opt/cfmeet-core/tls-proxy/certificate-monitor.py` and `certificate-monitor.json`, installed by the `private-tls-durable` operation from `infra/calling/production/certificate_monitor.py`. The TCP relays (`cfmeet-messaging-ingress`, `cfmeet-storage-ingress`) do not terminate TLS and hold no certificate.

## What "successful" means

A renewal or check is successful only when all of these hold:

1. The lineage is valid: chain with intermediates, not expired, verifies to the system roots for `api.cfmeet.com` as a TLS server, private key matches, and the key is not group- or world-readable.
2. Every target presents that certificate on **5 consecutive independent connections**, each verified for chain, hostname and SNI.
3. After a reload, every accepting nginx worker belongs to the new generation. A shutting-down worker retitles itself and stops accepting, so it is not counted.

Invalid material or a failing `nginx -t` never reloads anything; the running certificate keeps serving. A reload is attempted up to 3 times with backoff. A stopped proxy container is started again. The previous proxy container from the durable switch is kept, stopped, for rollback.

## Status and bands

| Status | When | Notified |
|---|---|---|
| OK | valid, presented everywhere, more than 45 days left | only when the state changes |
| WARN | 45 or 30 days left (certbot normally renews at 30) | on change |
| ALERT | 14 or 7 days left, expired, invalid lineage, a target not presenting the managed certificate, or the public certificate differs | every day while it lasts; the run fails |

## Operations (GitHub Actions → *CF Meet Calling production infrastructure and acceptance*)

| Operation | Effect |
|---|---|
| `private-tls-inspect` | Read only: mounts, managed vs presented certificate, certificate consumers (containers with letsencrypt mounts, host nginx certificate paths, certbot renewal method, deploy hooks, timers), last check. |
| `private-tls-durable` | Real-container proof first. Then either the one-time durable switch, or a reinstall of monitor, hook and timer when the proxy is already durable. Runs the hook, the daily check and `certbot renew --dry-run`, and compares every private route before and after. |
| `private-tls-rollback` | Restores the previous proxy container and nginx configuration exactly. |

Delivery test: run `CF Meet TLS certificate monitor` with `delivery_test: true`.

## When an ALERT arrives

1. Read the comment: which target, which problem, how many days are left.
2. Run `private-tls-inspect`.
3. Pick the case:
   - **Lineage problem** (renewal produced bad material): fix the certbot renewal. Nothing was loaded and the previous certificate is still served.
   - **Target not presenting the managed certificate after 3 attempts**: check `nginx -t` in that proxy (`docker exec cfmeet-core-tls nginx -t`), then re-run `private-tls-durable`.
   - **14/7 days left with no renewal**: certbot renewal failed. Run `certbot renew --cert-name api.cfmeet.com` on the host through the normal access path, then re-run `private-tls-durable` to activate and verify.
4. Never disable TLS verification, never pin a numbered archive file, and never expose a port to work around an alert.

## Tests

- `infra/calling/production/test_certificate_monitor.py` covers:
  - lineage validation: expired, wrong hostname, foreign root, mismatched key, open key, truncated chain
  - bands
  - stable presentation
  - bounded activation and refusal
  - recovery of a stopped proxy
  - atomic status
- `test_certificate_notify.py` covers when operators are notified and read-back delivery.
- `infra/calling/ingress-ci/private_tls_renewal.py` runs with the production nginx image and a certbot-shaped lineage. It runs before every `private-tls-durable` and covers:
  - the pinned-inode defect, reproduced
  - durable switch
  - container restart
  - stopped-proxy recovery
  - hook renewal confirmed by 10 independent connections
  - missed reload repaired
  - renewal not happening in time (14-day ALERT while still serving)
  - wrong-hostname renewal refused
  - mismatched key refused
  - exact rollback

# Security deployment checklist

Part H §32, §35, §43, §46, §50. What a deployment must guarantee **outside** application
code: network segmentation, secret delivery, backups and keyrings, administrative
interfaces. Application code fails closed where it can (refusing public ARI URLs,
refusing inline secrets, refusing weak keys); it cannot enforce cloud network topology —
this page is the contract the operator signs off.

## 1. Network segmentation (least exposure)

| Plane | Component | Listener | Allowed sources | Never |
|---|---|---|---|---|
| Customer API | Calling `/v1/*`, media connection gateway, signaling WebSocket | public ingress (TLS at the edge) | internet | `/internal/*`, `/metrics` on the public listener |
| Internal service API | Calling / Privacy Relay / Telephony `/internal/v1/*` | cluster-internal only | the other CF Meet services (service-auth tokens) | exposure through the public ingress |
| Provider callbacks | Telephony `POST /provider-callbacks/{provider}[/{token}]` | public ingress **path-filtered to this prefix only** (it shares Telephony's listener with `/internal/v1`) | the provider's published egress ranges (IP allowlist at the ingress) | any other Telephony path |
| SIP signalling | Asterisk `5061/tls`, `5060/udp` on the public SIP IP | carrier signalling CIDRs only (firewall) + PJSIP `identify` by IP; `endpoint_identifier_order = ip` (never `username`) | anonymous / guest endpoints |
| Media (RTP) | Asterisk `10000–20000/udp` | carrier media CIDRs + the RTC media gateway | application containers |
| Internal media peer | Asterisk `5070/udp` on the private media IP | the RTC media gateway CIDR | public exposure |
| SIP control (ARI) | Asterisk HTTP `8088` on the private control IP | Telephony instances only; TLS (`https`/`wss`) outside single-host deployments | public exposure (Telephony refuses a public ARI URL in staging/production) |
| Metrics | every service's `*_METRICS_ADDRESS` | monitoring network | the public or internal API listeners |
| Databases | one PostgreSQL per service | that service only | cross-service access, read replicas for other services |
| Object storage | Calling's private artifact bucket | Calling only (SigV4), server-side encryption on | public bucket policy, permanent public URLs |
| LiveKit | signalling + RTC ports | clients reach RTC ports; **signalling only through Calling's gateway**; the API key/secret only on Calling and the LiveKit server | the admin key in any client, bundle or mobile config |
| Operator CLIs | `platform_admin`, `privacy-relay-admin`, `telephony-admin` | an operator bastion with the service's database credentials | a network-reachable "admin API" (none exists by design) |

Security groups / ACLs must be written per row above; CI cannot verify them. Record the
rule set with the environment's change ticket.

## 2. Secret delivery

* Mount every secret as a file and name it with the `*_FILE` variable (see
  `secret-rotation-runbook.md` for the full list). Inline values are accepted only where a
  service documents it, never for private keys or the ARI password.
* Keyring files (Contact Vault), service-auth private keys and provider secrets: mode
  0600, owned by the service user, on a tmpfs / secret volume — never on the image layer
  (`Dockerfile.dockerignore` excludes `.env*`, `*.pem`, `*.key`; CI greps the image).
* Each service receives **only** its own secrets: Calling never gets SIP / ARI / provider /
  carrier secrets or the Contact Vault keyrings; Privacy Relay never gets carrier or
  LiveKit secrets; Telephony never gets Contact Vault keyrings or object-storage
  credentials (enforced by the architecture tests and the service-boundary guard).
* Verify before go-live: start each service with its production configuration and an
  intentionally missing mandatory secret — it must exit 2 (the CI "startup refusal" jobs
  do the same with placeholder values).

## 3. Backups and keyrings

* **Privacy Relay** backups contain AES-256-GCM contact ciphertext, keyed fingerprints and
  the contact access audit: sensitive even though encrypted. Keyrings are **never**
  stored in the same backup set, bucket or account; backup access and keyring access are
  granted to different roles. A retired keyring version must be kept offline until every
  backup that references it has aged out (runbook §4).
* **Calling** backups contain webhook signing secrets sealed under
  `CALLING_WEBHOOK_SECRET_KEY`, credential verifiers under the pepper, recording /
  voicemail metadata and operator audit: treat them as sensitive; store the secret-box key
  and the pepper separately from backups.
* **Telephony** backups contain provider event evidence (restricted detail, no raw
  bodies), legs by ContactReference and incident evidence — no numbers, no credentials.
* Backup restores are rehearsed per service with the retention worker **disabled** until
  the restore is verified (a restored database with an old clock would otherwise be
  purged at startup — workers are bounded, but a rehearsal must not delete evidence).

## 4. Administrative interfaces

* There is no network admin API. Platform, vault and telecom administration happen through
  the three CLIs with the service's database credentials and a required
  `--operator <id>` (or `CF_OPERATOR_ID`); every mutation writes an append-only operator
  audit row with a correlation id. No "internal network bypass" exists in any service.
* ARI (`/ari/*`) is an edge control interface for Telephony only; never customer-facing.
* Asterisk CLI / AMI are not enabled in the shipped configuration.

## 5. Go-live gates (per environment)

1. `make secret-scan`, `make service-boundaries`, per-service CI green on the deployed SHA.
2. Image scans (CI) on the deployed images.
3. Startup-refusal rehearsal (§2) and public-ARI refusal (`TELEPHONY_ARI_URL` must be
   private in staging/production).
4. Firewall / security-group rules recorded against §1.
5. Public-network flags (`live_telecom_allowed`, provider / route `ENABLED`,
   compliance eligibility) stay **off** until the real carrier certification (ADR 0016)
   has been run for that environment.

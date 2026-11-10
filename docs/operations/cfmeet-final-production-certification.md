# CF Meet final production certification (backend, read-only)

Date: 2026-11-10 · Environment: production (Contabo VPS, `api.cfmeet.com`)

## Verdict

**Live production: CERTIFIED (read-only scope).** All 13 backend containers passed every
applicable check. The public API and the gateway also passed every check. Run:
[42909534672](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42909534672),
verdict `CERTIFIED_READ_ONLY`, 0 failures.

**Full production readiness: NOT declared.** This certification is read-only by mandate.
It did not re-run the authenticated end-to-end journeys: sign-in, meetings, messaging, calling
and communications create sessions, audit rows and messages. Those journeys are an applicable
requirement that this certification could not evidence. See [Outstanding requirements](#outstanding-requirements).

## Evidence

| Run | Operation | Ref | Result |
|---|---|---|---|
| [42882978566](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42882978566) | `verify` | `982d72cd` | Failed (correctly): *No completed coordinated release of this SHA is recorded* |
| [42884861657](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42884861657) | `inspect` | `982d72cd` | Passed; showed apparently mixed revisions (see audit) |
| [42898830797](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42898830797) | `certify` (first) | `d2bc8ff5` | `INCOMPLETE`: probe image `curlimages/curl:8.12.1` absent on the host; certify refuses to pull. Public half passed. |
| [42909534672](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42909534672) | `certify` | `d2deb862` | **`CERTIFIED_READ_ONLY`**, 0 failures |

Artifact of the final run: `coordinated-release-certify-42909534672-1` contains:
- `coordinated-release.log` (raw VPS output)
- `certification.json` / `certification.md` (verdict and per-check data)
- `certification-public.json` (every public probe)

Code: [#48](https://github.com/CosmicforgeHealthnet/cfmeet/pull/48) /
[#49](https://github.com/CosmicforgeHealthnet/cfmeet/pull/49) add `certify`;
[#50](https://github.com/CosmicforgeHealthnet/cfmeet/pull/50) /
[#51](https://github.com/CosmicforgeHealthnet/cfmeet/pull/51) remove the probe-image
dependency. CI was green on every PR. The 216 offline production-tooling tests pass, 31 of them new.

## Audit findings

1. **`verify` failed by design, not by defect.** `verify <sha>` requires a completed
   coordinated release of exactly `<sha>` in the journal
   (`/opt/cfmeet-core/coordinated/journal.json`). The latest completed release is `4904ee33`
   (run 42723081476). The commits on `main` after it changed only release tooling and docs;
   no service build inputs changed. Strict same-SHA verification is preserved unchanged. Its
   refusal message now names the latest completed release and points to `certify`.
2. **The "mixed revisions" were stale container labels, not a mixed deployment.** A release
   swaps an existing service with `communications_release.clone()`, which copies the previous
   container's `Config.Labels`. Docker lets explicit container labels override image labels,
   so each swapped container kept the `org.opencontainers.image.revision` of the image it was
   first created from: `787bb9a5`, `08729298` or `bde67a8b`. `inspect` reported that label.
   The image's own label shows that **all 13 containers run the `4904ee33` images**. Each
   running image id equals the image id that run 42723081476 recorded in
   `registry-4904ee33….json`.
   Fixed for the future:
   - `inspect` now reports the image revision (and the container label separately);
   - a release no longer copies the previous container's OCI labels.
   Existing containers keep the stale label until their next release. That is harmless now
   that every reader uses the image label.
3. **The release journal is consistent with production.**
   - Completed: `release` (`bde67a8b`), `activate-provisioning`, `activate-org-events`,
     `org-cutover-export`, `activate-developer-platform`, `activate-gateway-routing`,
     `release` (`4904ee33`) and `activate-first-party-features`.
   - Not run: `org-cutover-import`, `-shadow`, `-authoritative` and `activate-enterprise-calling`.

## What `certify` is

`certify` is a separate operation of `cfmeet-coordinated-release.yml`. It is main-only, uses
the dispatched head as its reference, and shares the production lock with every other
operation. It certifies the topology that is actually deployed and does not weaken `verify`.

It is read-only by construction:
- No container lifecycle or configuration change.
- No file is written on the host. Database URLs reach `psql` only through the docker CLI's
  environment.
- Every SQL session is read-only.
- No image pull (`--pull never`).
- `psql` runs in `--rm --read-only` containers. HTTP probes run the host's `curl` inside each
  service's network namespace (`nsenter -n`), with names pinned to Docker's own addresses, so
  TLS is verified.
- No journal entry is written, and no confirmation is accepted.

The only file activity on the VPS is the usual upload of the release tooling to
`/opt/cfmeet-core/coordinated-releases/<sha>/`, which every operation does.

Runbook: [coordinated-production-release.md](coordinated-production-release.md#live-production-certification-certify).

## Per-service results (run 42909534672)

All containers run image revision `4904ee33c2defdb37dac680119962d62c7ed9ca5`. That revision is
on `main`, and no build input of any image changed between it and the reference head `d2deb862`.
Every image is the local `ghcr.io/cosmicforgehealthnet/cfmeet-<svc>:4904ee33…` tag with this
repository as its source. Restart count was 0 for every container, and none restarted during
the run.

| Service | Readiness | Database (own credentials, read-only) | Migrations applied / defined at revision | Anonymous request | Service auth | Upstreams (private TLS) | Result |
|---|---|---|---|---|---|---|---|
| platform-access | `/ready` 200 | `platform_access` @ `cfmeet_platform_access` | 6 / 6 | `GET /internal/v1/scopes` → 401 | key `platform-access-production-1`; grants from Organizations, API Gateway, Developer Platform present with current keys | platform-access 200 | PASS |
| organizations | `/ready` 200 | `organizations` @ `cfmeet_organizations` | 4 / 4 | `GET /internal/v1/admin/event-delivery` → 401 | key present; bundle valid (no activated grants it receives) | meeting-service 200, platform-access 200 | PASS |
| messaging | `/ready` 200 | `messaging` @ `cfmeet_messaging` | 31 / 31 (+ communications marker) | `GET /v1/messaging/sms-bridge/disclosure` → 401 | key present; issuers Communications, Meeting Service | communications 200, platform-access 200 | PASS |
| calling | `/health` 200 (DB query) | `calling` @ `cfmeet_calling` | 47 / 47 (+ communications marker) | `GET /internal/v1/webhook-migration/inventory` → 401 | key present; issuer Communications | communications 200, platform-access 200 | PASS |
| communications | `/ready` 200 | `cfmeet_comm_production_hub_rt` @ `cfmeet_comm_production_hub` | 4 / 4 | `GET /internal/v1/communications/metrics` → 401 | key `communications-20261103`; issuers Calling, Meeting Service, Messaging, operator console | calling, communications, meeting-service, messaging, platform-access 200 | PASS |
| numbering | `/ready` 200 | `numbering` @ `cfmeet_numbering` | 10 / 10 | `GET /internal/v1/number-resources/{nil}` → 401 | key present; bundle valid | platform-access 200 | PASS |
| privacy-relay | `/health` 200 (DB query) | `privacy_relay` @ `cfmeet_privacy_relay` | 17 / 17 | `GET /internal/v1/contexts/{nil}` → 401 | key present; bundle valid | platform-access 200 | PASS |
| sms-gateway | `/ready` 200 | `sms_gateway` @ `cfmeet_sms_gateway` | 4 / 4 | `GET /internal/v1/sms-bridge/messages/{nil}` → 401 | key present; bundle valid | platform-access 200 | PASS |
| telephony | `/health` 200 (DB query) | `telephony` @ `cfmeet_telephony` | 17 / 17 | `GET /internal/v1/routes/availability` → 401 | key present; bundle valid | platform-access 200 | PASS |
| meeting-service | `/health` 200 (liveness only) | `cosmicforge` @ `rtc_db` | 33 / 33 across 8 histories | `GET /api/v1/users/me` → 401 | key present; Organizations event grant present with current key | communications, messaging, platform-access 200 | PASS |
| developer-platform | `/ready` 200 | `developer_platform` @ `cfmeet_developer_platform` | 10 / 10 | `GET /internal/v1/operator/activity` → 401 | key present; API Gateway grant present with current key | platform-access 200 | PASS |
| developer-platform-worker | stays running (no listener) | `developer_platform` @ `cfmeet_developer_platform` | n/a (shares the API's DB) | n/a (no listener) | key present; bundle valid | platform-access 200 | PASS |
| api-gateway | `/ready` 200 | n/a (no database) | n/a | `GET /v1/calls` → 401 (public routing on) | key present (no inbound bundle by design) | developer-platform 200, platform-access 200 | PASS |

The meeting service's `/health` does not touch its database. Its database was proven
separately by the service-credential query.

## Platform checks

- Provider-dependent features at safe defaults: PASS (no violations).
- Organizations outbox: PASS; no `DEAD` events (the outbox is empty).
- Core identity (Calling, Meeting, Messaging, Platform Access, LiveKit, Redis, artifact store running): PASS.
- VPS public smoke: Swagger, CORS preflight, `/internal/*` not public: PASS.
- Resources: 5,316 MiB available memory, 151.2 GiB free disk.

## Public API and gateway (from the GitHub runner, unauthenticated GET/OPTIONS only)

- `/health` 200; Swagger configuration 200. All 10 listed OpenAPI documents return 200:
  api-gateway, calling, communications, developer-platform, messaging, numbering, openapi
  (meetings), privacy-relay, sms-gateway.
- CORS preflight from `https://cfmeet.com` is allowed. `/api/v2/me/profile` without a session returns 401.
- The TLS certificate has 40 days remaining (minimum 14).
- All 236 public GET routes of `infra/api-gateway/route-manifest.json` were probed with placeholder IDs:

| Domain | Auth enforced (401/403) | Not publicly routed (404) |
|---|---|---|
| calling | 44 | 7 |
| communications | 37 | 0 |
| developer-platform | 33 | 0 |
| gateway | 1 | 0 |
| meetings | 26 | 12 |
| messaging | 0 | 40 |
| numbering | 0 | 26 |
| privacy-relay | 0 | 6 |
| sms | 0 | 4 |

  - No route answered 2xx without credentials, and none answered 5xx.
  - All 34 routes of the gateway families activated by `activate-gateway-routing` are routed
    and reject anonymous requests (401 from the gateway).
  - Messaging, numbering, privacy-relay and SMS customer routes are not publicly routed. Their
    gateway families have not been activated; this is by design.
- All 25 internal, operator and provider-callback GET routes of the source contracts were probed
  on the public host. None answered 2xx: 19 returned 404, 4 returned 401 and 2 returned 403.

## Enabled features and external integrations

| Integration | State | Switch |
|---|---|---|
| SMS carrier provider | **disabled** | `SMS_GATEWAY_PROVIDER` unset |
| Number inventory providers / porting | **disabled** | `NUMBERING_PROVIDERS` unset, `NUMBERING_PORTING_ENABLED=false` |
| PSTN telephony, public outbound/inbound PSTN, callback relay, enterprise SIP, SIP edge | **disabled** | `TELEPHONY_ENABLED=false`, `PUBLIC_*_CALLING_ENABLED=false`, `CALLBACK_RELAY_ENABLED=false`, `ENTERPRISE_SIP_ENABLED=false`, `TELEPHONY_SIP_EDGE=none` |
| Privacy relay, contact vault, relay number pool | **disabled** | all `false` |
| Mobile push provider | **disabled** | `MESSAGING_PUSH_PROVIDER=none` |
| End-to-end encrypted messaging devices | **disabled** | `MESSAGING_E2EE_DEVICES_ENABLED=false` |
| Calling public outbound/inbound (PSTN) | **disabled** | `CALLING_PUBLIC_*_ENABLED=false` |
| Customer webhooks / delivery, production-access workflow | **disabled** | `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED=false`, delivery mode unset (shadow), workflow `false` |
| API gateway WebSockets | **disabled** | `API_GATEWAY_WEBSOCKETS_ENABLED=false` |
| App calling, new calls | enabled | `CALLING_APP_CALLING_ENABLED=true`, `CALLING_NEW_CALLS_ENABLED=true` |
| Media (self-hosted LiveKit), capture via LiveKit egress | enabled | `CALLING_MEDIA_PROVIDER=livekit`, `CALLING_CAPTURE_PROVIDER=livekit-egress` |
| Call recording, voicemail (self-hosted artifact store, AES256) | enabled | `CALL_RECORDING_ENABLED=true`, `VOICEMAIL_ENABLED=true` |
| Enterprise calling (Calling service flag) | enabled | `ENTERPRISE_CALLING_ENABLED=true` (see owner note) |
| Developer gateway API, gateway public routing | enabled | journaled `activate-developer-platform`, `activate-gateway-routing` |
| Organization provisioning | enabled | journaled `activate-provisioning` |
| Transactional email (SMTP) | configured | host and credentials present (values not read) |
| Google sign-in | configured | client id present |

## Failures

Final run: **none**.

The first run (42898830797) was `INCOMPLETE`. `curlimages/curl:8.12.1` is not on the host, and
`certify` refuses to pull images. Earlier `verify` runs pulled that image implicitly, and it has
since been removed from the host. The fix was not to allow a pull. HTTP probes now use the
host's `curl` in each service's network namespace (#50 / #51).

## Outstanding requirements

1. **Authenticated end-to-end journeys.** This read-only certification did not run sign-in,
   meetings, messaging, calling, communications, developer-platform and gateway flows with a
   credential: they write sessions, audit rows and messages. Full production readiness needs a
   current, owner-approved run of the acceptance suites with dedicated acceptance accounts. The
   most recent recorded evidence is in the backend closure report (Communications 20/20 live
   acceptance). That evidence predates this certification and was not re-validated here.
2. **Owner note: enterprise calling flag versus coordinated activation.** Calling runs with
   `ENTERPRISE_CALLING_ENABLED=true`, recording and voicemail on. These were set by the earlier
   Calling production path. The coordinated `activate-enterprise-calling` step has not run (it
   follows the Organizations authoritative cutover). As a result, Calling's trust bundle does not
   grant Meeting Service `calling-request.act-for-organization`. Organization-scoped calling
   requests from Meeting Service are therefore not authorized yet. Decide whether that is the
   intended state.
3. **Not yet activated (by design, owner decisions):**
   - the Organizations cutover (`org-cutover-import` → `-shadow` → `-authoritative`);
   - gateway families for messaging, numbering, privacy-relay and SMS;
   - every disabled external integration above, which needs provider contracts and credentials.
4. **`verify` on the current main head** keeps failing until a coordinated `release` of that head
   runs. The head contains no service-code change, so this is optional. Releasing would also
   refresh the stale container labels.
5. **TLS certificate** for `api.cfmeet.com` expires in 40 days. Renewal is automated (see
   [tls-certificate-automation.md](tls-certificate-automation.md)); confirm the next renewal.

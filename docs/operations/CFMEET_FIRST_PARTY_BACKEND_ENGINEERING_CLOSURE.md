# CF Meet — first-party backend engineering closure and automatic Messaging

> **Superseded (2026-11-16).** Current production state, feature activation and the remaining blockers are in [CFMEET_PRODUCTION_ACTIVATION_AND_BACKEND_CLOSURE.md](CFMEET_PRODUCTION_ACTIVATION_AND_BACKEND_CLOSURE.md). This report is kept as history; statements here about switches being off or Organizations not being authoritative may be out of date.

**Date:** 2026-11-15 · **Environment:** production (`https://api.cfmeet.com`, `https://www.cfmeet.com`)
**Baseline:** `main` `cbcbdded` (certified 13-service release `c6de964c` + E2EE activation)
**Deployed backend release:** `6f3743f806434a55bad8297f104d28a55e350e2e`. Coordinated release [43696469230](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43696469230).
**Web:** Vercel production build of `main` (live bundle verified, §6).

## 0. Verdict

**`CFMEET_FIRST_PARTY_BACKEND_ENGINEERING_CLOSED`**

* Every in-scope first-party engineering item is implemented, tested, deployed and verified in production (§1).
* No first-party backend defect remains open (§2).
* Production activation that still waits on something external is listed separately (§4), as are optional enhancements (§3) and excluded services (§5).
* **Not claimed:**
  * an independent cryptographic assessment: `NOT_PERFORMED`;
  * key-transparency witnesses: `UNWITNESSED`;
  * delivery through any external push provider.

* **Final `certify`:** [43711424389](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43711424389), **`CERTIFIED_READ_ONLY`**. All 13 services pass, provider safe defaults pass with no violations, core identity passes, and public API and gateway certification pass.

## 1. Implemented, deployed and production-verified

### 1.1 Messaging security (Section 1)

**Audit:** witnesses and quorum are already optional everywhere.
* Messaging accepts an unwitnessed log when the owner's release record accepts `KEY_TRANSPARENCY_UNWITNESSED`.
* The tree head reports `UNWITNESSED` and never refuses.
* No client requires witnesses.
* No gate wrongly blocks the approved Messaging service.

**Kept unchanged:** MLS/OpenMLS, device-held keys, signed tree heads, consistency and rollback checks, device trust, revocation, replacement and rekey, anti-replay, and organization isolation.

**Corrected record:** the served Messaging Swagger said "production enrollment remains blocked pending independent assessment". It now states the actual record:
* the owner's release decision;
* `NOT_PERFORMED`;
* `UNWITNESSED`;
* no certification claimed.

**Tests:**
* Messaging full suite: 302 passed. 12 load tests are `#[ignore]` by design.
* Production `messaging-encrypted-verify` [43702913188](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43702913188): **PASS 16/16** with real OpenMLS clients. Server storage: 6 messages, 0 not opaque, 0 containing a phrase.

### 1.2 Organizations events (Section 2)

**Audit:** every event writes one `cfmeet.reconciliation` record. This is event history: it is never dispatched, so it stays `PENDING` with 0 attempts by design. Each event also writes one row per subscribed consumer; only `cfmeet.meeting-service` subscribes, to 13 event types.

**Production classification** (release verify, after the release):

| Destination | Rows | State |
|---|---|---|
| `cfmeet.reconciliation` | **67** (the "59 pending" have grown with new events) | all `PENDING`, 0 attempts, **0 dispatched** |
| `cfmeet.meeting-service` | **40** | all `DELIVERED`; 0 pending, 0 dead, 0 blocked; oldest undelivered 0 s |

Of the 67 records:
* 27 are event types no consumer subscribes to: `organization.provisioning_changed` 18, invitations 4, teams 5;
* 40 are subscribed types, each with a delivered consumer twin.

There is no undelivered consumer event and no dispatch defect.

**Fixes:**
* `/ready` always said `delivery: disabled`. It now reports the configured destinations and `enabled`/`disabled`.
* Monitoring classifies by destination and event type: `verify`, `certify` and `activate-org-events` use `classify_event_delivery`.
  * Reconciliation records are reported as history.
  * The check fails only on a real delivery defect: a dead consumer row, one undelivered for 15 minutes, one blocked behind a dead row, a missing record twin, or a reconciliation record that was dispatched.
* No history was deleted.

**Tests:**
* Organizations: 86 passed, including the readiness test.
* SQL checked on PostgreSQL 16.
* Offline classifier tests.
* Production check `organizations_event_delivery`: PASS.

### 1.3 Developer Platform production access (Section 3)

The backend is complete:
* operator-console identity;
* `developer-platform.administer` granted to that identity only;
* named reviewers in the `github-<login>=<CF Meet user id>` format;
* manual approve and reject, audited, with self-approval refused;
* customer submissions enabled only once reviewers exist;
* the applicant is emailed.

The only remaining step is the owner naming reviewers (§4).

### 1.4 Push (Section 4)

**Verified internal behaviour:**
* adapters for APNs, FCM and Web Push (RFC 8291/8292);
* a durable job queue with leases, backoff and terminal states;
* recipient authorization at send time;
* invalidation of handles on 404/410;
* revocation on device revoke and sign-out everywhere;
* Communications notification preferences and mutes;
* payloads limited to metadata: allowlisted keys, enforced again by a database CHECK. No plaintext, no ciphertext and no sender is ever sent.

**New:**
* `GET /v1/messaging/push/configuration`, relayed for first-party clients: the platforms this deployment delivers to and the Web Push application-server key.
* The `activate-web-push` operation:
  * generates a P-256 VAPID key on the VPS (PKCS#8, owned by Messaging, mode 0400, with a recovery copy);
  * mounts it and sets the two adapter variables, using subject `https://cfmeet.com`;
  * widens the safe defaults for exactly those values only. APNs and FCM stay refused.

**Tests:**
* `push_adapters` (real fake push services, RFC 8291 decryption);
* an openssl-generated key driven through the real environment parser;
* the configuration endpoint, including its scope.

**Production activation:** `activate-web-push` [43706099426](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43706099426).
* Key generated, `publicKeyPresent: true`.
* Messaging restarted and reports the `WEB_PUSH` adapter.
* The regression check passed.
* `certify` lists "Web Push (VAPID, no provider account): enabled".

A first attempt ([43702218850](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43702218850)) failed before any change: DER output was decoded as text. That was fixed in `de7b863f`.

**External delivery to a real browser was not verified**: no client subscribes yet (§4).

### 1.5 Device security and recovery (Section 5)

**Defects found and fixed:**
* An unclassified ("legacy") sign-in survived a primary-mobile replacement or a device revocation.
* Nothing ever bounded unclassified sign-ins.

**Changes:**
* **Always on:** replacing the primary mobile, or revoking any device, also ends the account's unclassified sign-ins, because they cannot be attributed to a device. They are fanned out like any ended sign-in. Classified web devices are untouched.
* **Migration switch `ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM`** (an owner-set date):
  * from that date, a sign-in that did not declare its device within its declaration window is refused on requests and on refresh with `403 DEVICE_DECLARATION_REQUIRED`;
  * the device endpoints still answer, so the client can explain what to do;
  * it is unset until the mobile app declares at sign-in (§4).
* The web client now declares at every sign-in (§1.6).

**Preserved:** multiple web devices, independent Meetings and Calling, device proofs and session verification.

**Tests:** 12 device tests on real PostgreSQL and Redis with real Ed25519 proofs, including:
* eviction on replacement and revocation;
* enforcement before and after the date;
* the previously untested claim that an hour-old undeclared sign-in cannot declare (`409`).

The production multi-device suite also passed: replacement, fan-out and rekey (§1.1).

### 1.6 Messaging works automatically (follow-up assignment)

**Root causes:**
1. **"Live updates unavailable."** The production web build had no `NEXT_PUBLIC_MESSAGING_URL`. The live bundle read it at runtime as empty, so the engine never opened a socket. This had been listed on 2026-11-13 as an owner Vercel action and never done. **Fix:** the realtime origin falls back to the API origin, where production serves `/v1/realtime`, and the CSP is aligned. Device tokens are now renewed on the open socket before they expire.
2. **Manual "Turn on".** The browser was registered only when that button was clicked. **Fix:**
   * every sign-in declares the browser's account device, reusing its key; this never blocks sign-in;
   * the engine enrolls on its own: trusted → MLS device, KeyPackages, realtime;
   * an untrusted browser starts a link request automatically and shows only the comparison code, with an email-code alternative;
   * a sign-in that predates the device flow goes through the standard login page (`reason=secure_messaging`);
   * transient failures retry.

   The panel and buttons are removed; the layout is unchanged.
3. **Contact without a device.** Messaging refused the DIRECT encrypted chat (`409 COUNTERPART_HAS_NO_DEVICE`), and the web client threw `NO_DEVICE_TO_ADD` after creating it half-way. **Fix:**
   * the chat is created `PRIVATE_E2EE`; only the creator needs a device; it is never MANAGED and no key is generated on the server;
   * messages written meanwhile are held sealed in the crypto worker, state `pending-setup` ("Waiting for secure setup"), and never shown as sent;
   * a member device adds the contact's devices when they enroll, then the held messages are encrypted and sent.
4. **Browser crypto artifact.** The committed wasm was not what `packages/messaging-crypto` builds; the "Web crypto artifact" check had failed since 2026-11-04. It is rebuilt reproducibly (rustc 1.97.0, wasm-bindgen 0.2.129), byte-identical to CI's build.

**Production:**
* `messaging-web-verify` [43705489163](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43705489163) drives the real www.cfmeet.com in Chromium: real sign-in page, real accounts, nothing injected. **11/11 PASS**:

  | Check | Result |
  |---|---|
  | `fresh_accounts_ready_automatically_web_only` | PASS |
  | `live_updates_connected_authenticated_websocket` | PASS |
  | `contact_chat_send_receive_encrypted` | PASS |
  | `offline_recipient_receives_queued_messages` | PASS |
  | `returning_trusted_browser_starts_automatically` | PASS |
  | `reconnect_after_network_interruption` | PASS |
  | `chat_with_unenrolled_contact_is_pending_not_failed` | PASS |
  | `pending_message_delivered_after_recipient_setup` | PASS |
  | `new_untrusted_browser_verification_only` | PASS |
  | `two_trusted_browsers_synchronize` | PASS |
  | `no_exposed_encryption_errors` (no page errors either) | PASS |

* The live bundle contains no "Turn on private messaging" and no activation control.
* CI's real-services Chromium journey (Unified Messaging integration journeys) passed with automatic setup.

## 2. Remaining first-party backend defects

**None open.** Defects found during this work and fixed:
* readiness misreporting delivery;
* legacy sign-ins surviving replacement;
* the stale E2EE statement in Swagger;
* the three Messaging and web defects in §1.6;
* the stale crypto artifact;
* `activate-web-push` decoding DER output as text (my own bug; it failed before any change; fixed and retried);
* a test race in the realtime inbox test under load.

## 3. Optional security enhancements (not blockers)

* Key-transparency witnesses: not deployed, by owner decision.
* Independent cryptographic assessment: not performed.
* Passkey (WebAuthn) recovery. Email step-up is the recovery path.
* Platform attestation of the device class (App Attest / Play Integrity).

## 4. External-provider or owner-configuration dependencies

| Item | What is needed | State |
|---|---|---|
| Developer production-access review | The owner sets `CFMEET_PRODUCTION_ACCESS_REVIEWERS` (`github-<login>=<CF Meet user id>`), then runs `activate-production-access-review` | backend complete. Activation attempt [43708560267](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43708560267) stopped at "Set the CFMEET_PRODUCTION_ACCESS_REVIEWERS variable" before reaching the VPS: no reviewer is named, nothing changed. **Owner configuration requirement** |
| Enforce device declaration | The mobile app adopts the device flow (handover §3.1/§3.5); then the owner sets `ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM` | backend implemented and tested; unset in production (compatibility) |
| Web Push in browsers | A client service worker subscribes with the published key. This is web client work; no layout change was in scope | server **activated** (§1.4); **external delivery not verified** |
| APNs / FCM | Apple and Google provider credentials | not configured |
| Email provider | Resend owner setup; SMTP in use | excluded from this assignment |

## 5. Deliberately excluded

* Witness deployment and second hosting.
* Independent assessment.
* Paid services.
* Backup and restore gate.
* Resend.
* External telecom: SMS, PSTN, numbering, privacy relay, all still disabled.

## 6. Commits, CI and deployment

| Commit | Content |
|---|---|
| `42b509ce` | Organizations classification and readiness, push configuration route, Web Push activation, legacy sign-in migration |
| `74e5600c` | Automatic Messaging, pending-setup chats, live-updates fix, production web journey |
| `5aa3952d` | Generated contract types; source-built browser crypto core |
| `6f3743f8` | Messaging authorization-matrix and mirror rows; consumer guide; inbox test publish order (**released**) |
| `de7b863f` | `activate-web-push` PEM fix |
| `37633921`, `7f70ec73` | Production journey diagnostics and step order |

**CI on the released head:** all required workflows green, including:
* Messaging, Meeting Service, Organizations, API Gateway;
* Contracts, Service boundaries, Part VI cross-service certification;
* Unified Messaging integration journeys, Enterprise Calling;
* Frontend, Mobile, Web crypto artifact.

**Release [43696469230](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43696469230):**
* replaced api-gateway, calling, meeting-service, messaging and organizations;
* kept communications, developer-platform (and worker), numbering, platform-access, privacy-relay, sms-gateway and telephony;
* every verify check passed (running, migrations, provider safe defaults, public smoke, Organizations event delivery); violations `[]`.

**Web:** Vercel built `main`. The live site serves `crypto-worker.33ee2cd68f0ec412.js`; the old worker answers 404.

## 7. Production verification (Section 6)

| Area | Run | Result |
|---|---|---|
| Release verify | [43696469230](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43696469230) | PASS (all checks) |
| Personal E2EE, multi-device, one-primary-mobile, web-only | [43702913188](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43702913188) | **16/16** |
| Real-website Messaging journey | [43705489163](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43705489163) | **11/11** |
| Enterprise Calling, real media | [43702221217](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43702221217) | **21/21** |
| Organizations and Messaging | [43708792961](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43708792961) | **18/18** |
| Developer Platform and API Gateway | [43709293595](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43709293595) | **30/30**, cleanup with 0 problems |
| Personal Calling and Communications, real voice and video | [43709750585](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43709750585) | **24/24** |
| Meetings, real LiveKit room | [43710752927](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43710752927) | **22/22** |
| Call recording, real media | [43713116133](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43713116133) | **12/12**. The first attempt ([43712045458](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43712045458)) was refused by the runtime's active-calls guard, which defers a restart while calls are live; nothing changed |
| Web Push activation | [43706099426](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43706099426) | activated; `WEB_PUSH` adapter running |
| Production-access review activation | [43708560267](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43708560267) | refused before the VPS: no reviewers named (owner) |
| **Final `certify`** | [43711424389](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43711424389) | **`CERTIFIED_READ_ONLY`** |

**Final certify details:**
* **13/13 services:** each is running at its revision, ready, connected to its database, has every migration applied, refuses anonymous requests, and holds its service identity. `N/A` is recorded where a check does not apply (the worker has no listener; the gateway has no database).
* **Provider safe defaults:** PASS, violations `[]`. E2EE devices and Web Push are accepted only because their activations are journaled with their exact settings. APNs, FCM, SMS and PSTN stay off.
* **Organizations event delivery:** PASS. 96 reconciliation records, 0 dispatched; 58 consumer rows, all `DELIVERED` (the acceptance runs above added events).
* **Core identity:** PASS.

**Section 6 journey coverage:**

| Journey | Evidence |
|---|---|
| Registration and login | every suite creates fresh, email-verified accounts; the web journey uses the real login page |
| One-primary-mobile replacement | E2EE 16/16; device tests |
| Multiple linked web devices | E2EE 16/16; web journey (second browser verified, both synchronized) |
| Web-only accounts | E2EE 16/16; web journey |
| Personal E2EE Messaging | E2EE 16/16; web journey 11/11 |
| Calling and Enterprise Calling | 24/24; 21/21 |
| Meetings | 22/22 |
| Recording | 12/12 on the released Calling image: consent and non-participant denial, notice evidence, retention deadline, encrypted real-media playback, cross-user and cross-tenant denial, access audit, deletion revokes access, physical object deletion, retention worker |
| Organizations and Messaging | 18/18 |
| Developer Platform and API Gateway | 30/30 |
| Cross-service authorization and revocation | E2EE (fan-out revocation, sign-out everywhere), Organizations (revocation, suspension), Developer (revoked or forged credentials) |

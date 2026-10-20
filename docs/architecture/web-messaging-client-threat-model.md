# CF Meet web Messaging client — threat model and key handling

Scope: the shipped web client of Unified Messaging (`apps/web`, route `/dashboard/messages`):
`src/lib/messaging/{engine,api,crypto-client}.ts`, the device worker
`messaging-crypto/worker.src.js` (served as `public/messaging-crypto/crypto-worker.*.js`), the
browser crypto core `packages/messaging-crypto/wasm` and the Meeting Service client relay
(`services/meeting-service/src/services/unified_messaging/relay.rs`). Part V §104, Appendix L;
Part K gates 20, 24, 25.

This document states what the design protects, against whom, and what it does **not** protect.
It is an engineering threat model, not an independent cryptographic assessment (Part K gate 21
remains open).

## 1. Architecture

```
signed-in page (React) ── login bearer ──▶ Meeting Service ── signed service token ──▶ Messaging
      │  postMessage (ciphertext, public keys,                (principal SIGNED, allowlisted
      │  decrypted text for display)                           end-user operations only)
      ▼
dedicated Worker (one device of one account)            Messaging realtime gateway
  WebAssembly crypto core (OpenMLS 0.9.1)  ◀─ client token (device-bound, short-lived) ─┘
  IndexedDB: AES-256-GCM under a NON-EXTRACTABLE key
```

* The page never sees a device private key, an MLS epoch secret, an attachment content key or
  the at-rest wrapping key. It sends MLS messages / ciphertext into the worker and receives
  public keys, MLS messages / ciphertext, and decrypted text and files for display.
* The browser never holds a Messaging or Platform Access credential. Messaging REST is reached
  only through the Meeting Service relay, which verifies the CF Meet login, resolves the
  account's own principal and signs it into its service token; the acting principal is never
  taken from the request. Only an explicit allowlist of end-user operations is relayed
  (no policy, legal hold, export, audit, moderation, SMS bridge or raw content route).
* The only Messaging token in the browser is the device-bound client token (minted through the
  Meeting identity bridge), used once to obtain a single-use realtime ticket.

## 2. Assets

| Asset | Where it lives | Protection |
|---|---|---|
| Device signature key, KeyPackage private keys, MLS group secrets | worker memory; IndexedDB `devices` (sealed) | never posted out of the worker; at rest AES-GCM under the wrapping key, AAD = account |
| Attachment content keys | worker memory until sent; inside MLS messages; IndexedDB `records` (sealed) | never posted out of the worker (the page holds an opaque handle) |
| Decrypted history | IndexedDB `records` (sealed), page memory while shown | at rest AES-GCM, AAD = account + conversation + sequence (a record cannot be moved) |
| Wrapping key | IndexedDB `keys` as a `CryptoKey` | generated non-extractable; script cannot export its bytes |
| Login bearer / refresh token | JavaScript-readable cookies (pre-existing CF Meet design) | see §5 residual risk R4 |

## 3. Threats and controls

| # | Threat | Control | Verified by |
|---|---|---|---|
| T1 | Server (or anyone with database / storage access) reads PRIVATE_E2EE content | MLS end-to-end encryption on the device; attachments encrypted before upload, file name and type only inside the MLS-protected descriptor | `apps/web/tests/messaging-e2e.browser.mjs` + the Rust driver: the exchanged phrase is absent from Messaging's database; E2EE attachment rows are `application/octet-stream`, nameless |
| T2 | Silent downgrade to server-readable messaging | the engine refuses a conversation whose granted profile differs from the requested one; no MANAGED fallback for PRIVATE_E2EE sends; the UI labels MANAGED as readable by CF Meet servers | engine (`createConversation`, `sendEncrypted`); browser journey step 9 |
| T3 | Message bound to another conversation / tenant / sender | the CF Meet AAD (tenant, application, environment, conversation, client message id, sender device) is recomputed from the envelope and must equal what MLS authenticated (`AAD_MISMATCH` otherwise) | wasm `WebDevice::process(expected_aad)` |
| T4 | Tampered or replayed ciphertext, tampered attachment | OpenMLS authentication and replay refusal; attachment AEAD + digest | crypto core tests; Chromium harness; browser journey |
| T5 | Key substitution by the server (a fake device) | each claimed KeyPackage is validated and its device id checked against the claim; with a pinned log key (`NEXT_PUBLIC_MESSAGING_KT_LOG_KEY` / `_LOG_ID`) the device and exact key must be in the signed transparency log, not revoked, and the head must extend every head seen before (rollback / equivocation refused) | worker `verifyTransparency` |
| T6 | DOM XSS in rendered content | React text rendering only (no HTML sink in `src/`, static guard); nonce CSP with `strict-dynamic`; Trusted Types enforced with a default policy that admits only same-origin `/_next/static/*.js` script URLs | `tests/csp.test.mjs`, `tests/browser-security.test.mjs`, journey step 4 |
| T7 | Malicious crypto module | the worker fetches the module and compares its SHA-384 with the value pinned at build time before compiling; the worker is content-addressed; the artifact is reproducibly rebuilt and compared in CI | journey step 13; `.github/workflows/web-crypto-artifact.yml` |
| T8 | WebAssembly / eval in pages | pages have no `'wasm-unsafe-eval'` and no `'unsafe-eval'`; WebAssembly compiles only under the worker's own CSP (`default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self'`) | `tests/csp.test.mjs`; journey step 1 |
| T9 | Stolen copy of the browser's IndexedDB files used elsewhere | everything stored is sealed under a key that only this browser profile's Web Crypto can use | journey step 5 (no plaintext in IndexedDB, wrapping key `extractable: false`) |
| T10 | Two tabs forking one MLS state | one tab per device (Web Locks); a second tab shows "open in another tab" | worker `acquireLock` |
| T11 | Group change refused by the Delivery Service leaves the device ahead of the group | snapshot before create / add / remove; `confirm` after the server accepted, `rollback` otherwise | engine `addPrincipalsToGroup`, `commitPendingRemovals` |
| T12 | Revoked or removed device keeps reading | revocation removes the server's authorization; member devices commit the MLS removal of revoked devices; a revoked device erases its local keys and history | journey step 11 |
| T13 | History recovery to an attacker's device | device-to-device only: the target's request is signed by its registered device key and verified by the source; both screens show the same comparison code and the user confirms; HPKE sealed to a single-use key; no server key or escrow | journey step 10; `history_transfer` tests |
| T14 | Acting as another user through the relay | identity comes only from the verified login; allowlist; Messaging re-authorizes every operation | `tests/unified_messaging_api.rs` |

## 4. Lifecycle of local key material

| Event | What happens |
|---|---|
| Device set-up | keys generated in the worker; nothing stored until the server accepted the registration (`adopt`) |
| New device of a member | added by one member device (the active in-tree device with the smallest id; others after a grace period) only after the new device's listed key verifies against the transparency log, so failed attempts never consume its KeyPackages; every member's Welcomes carry the ratchet tree, whichever member adds (Part K final evidence §C); a device still waiting tops up its KeyPackages |
| Every state change | the whole device state is exported, sealed and written together with the affected records in ONE IndexedDB transaction (no torn state) |
| Encrypt | state persisted before the ciphertext leaves the worker (a crash cannot re-use a ratchet generation) |
| Browser refresh / crash | the worker re-opens the sealed state; unpersisted work is redone from the Delivery Service |
| Logout (`engine.signOut`) | background work (polling, realtime) stops first; then the device is revoked and every key and record of the account in this browser is erased; the Meeting Service also revokes all Messaging client sessions at logout |
| Device removal / revocation detected | local keys and history erased; the browser must set up a new device |

## 5. What this does NOT protect (accepted residual risk)

* **R1 — A compromised CF Meet web origin.** Code served by the CF Meet origin (a malicious
  deployment, a compromised build, an XSS that bypasses the controls above) runs with the
  page's authority: it can ask the worker to decrypt, read what is displayed and use the
  non-extractable key while the user is signed in. Web Workers and non-extractable keys raise
  the bar (no key bytes can be exfiltrated); they do not stop an attacker who controls the
  origin. This is inherent to web-delivered end-to-end encryption.
* **R2 — A compromised browser profile or operating system** can read process memory and use
  the profile's keys.
* **R3 — Script integrity of first-party chunks** is enforced by the first-party origin, the
  nonce CSP and the release manifest (`scripts/verify-build-assets.mjs`), not by
  browser-enforced Subresource Integrity on every chunk (Next.js does not provide complete
  SRI; Part K evidence §D). The crypto module itself IS integrity-checked by the worker.
* **R4 — Login tokens in JavaScript-readable cookies** (pre-existing CF Meet authentication):
  an XSS could steal the login bearer and act as the user through the relay (MANAGED content,
  metadata); it still cannot read PRIVATE_E2EE content without a device. Moving the refresh
  token into an HttpOnly cookie is a recommended follow-up (it changes the login flow of the
  live product and needs its own rollout).
* **R5 — Metadata.** The server sees who talks to whom, when, message sizes and membership.
* **R6 — Without a pinned key-transparency log key** (the variables above unset), a malicious
  server could substitute a device key; the UI says "Contact keys not verified against a pinned
  transparency log". Witness independence (gate 22) is a separate, external requirement.
* **R7 — No independent cryptographic review** of this client yet (gate 21).

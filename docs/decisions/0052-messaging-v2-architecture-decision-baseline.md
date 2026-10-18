# ADR 0052 — CF Meet Part V v2.0 architecture decision baseline and production gates

**Date:** 2026-10-18  
**Status:** Selected engineering direction for continued development; production cryptographic security approval and independent certification **NOT GRANTED**.  
**Branch:** staging only.  
**Authority:** CF Meet Part V — Secure Unified Messaging & E2EE, Version 2.0, Appendix G.1 (AD-01 through AD-10), G.2 and Appendix N.  
**Scope:** Design policy for Part V implementation; does not enable any feature flag or authorize production deployment.

## Purpose and interpretation

This record consolidates the ten architecture positions in the Version 2.0 specification into the development baseline requested by the owner. "Selected for development" means engineers may implement and test against this architecture while respecting existing production-off controls. It does **not** mean an independent reviewer approved the cryptography, that an external security audit is complete, or that the product may advertise security equivalence with other messengers.

No proposed solution may silently provide server access to PRIVATE_E2EE plaintext or keys. Existing PRIVATE_E2EE and MANAGED conversation security profiles remain distinct and immutable.

## AD-01 through AD-10 — engineering positions

| ID | Engineering direction | Mandatory boundary / outstanding release gate |
|---|---|---|
| **AD-01** | Adopt **IETF MLS 1.0 (RFC 9420)** for both direct and group protected conversations. A direct conversation is a two-principal MLS group with separate authorized-device leaves. | Validate membership/epoch changes, multi-device operation, offline ordering, fork resistance and interop; no Signal-Protocol equivalence claim. |
| **AD-02** | Use **OpenMLS** behind the CF Meet crypto interface, currently pinned in the implementation to OpenMLS 0.9.1 and suite 0x0001. Do not implement custom cryptography. | Review upgrades, dependencies, key lifecycle and target-specific interoperability; require expert cryptographic assessment before production. |
| **AD-03** | **Do not link libsignal as a production dependency** in Version 2.0; Signal specifications can inform comparative threat modeling. | No claims of Signal interoperability or Signal Protocol implementation. Revisit licensing/support if the choice changes. |
| **AD-04** | **No experimental post-quantum MLS combination in v2**. Preserve protocol agility for a future standardized/audited hybrid profile. | No post-quantum confidentiality claims; document current classical forward secrecy/post-compromise-security scope. |
| **AD-05** | Use draft-aligned, append-only **key transparency** with client proof verification and cached signed tree heads. | Existing operator-signed log without independent witnesses is **UNWITNESSED**; pin/review a KEYTRANS profile and operate external witness/auditor or equivalent anti-equivocation mechanism before strong production claims. |
| **AD-06** | Support Web E2EE only with explicit disclosure of **malicious JavaScript/code-delivery risk**. Favor signed/reproducible native clients and strengthen Web delivery controls. | Browser runtime certification, CSP, Trusted Types, supply-chain/code-delivery review and truthful security disclosures are still required. Web E2EE cannot protect against a compromised origin serving hostile client code. |
| **AD-07** | A newly linked device receives **future messages by default**. Historical plaintext is available only via a user-authorized encrypted backup or authenticated device-to-device transfer. | Never introduce server master recovery keys or server-side history-key escrow. Recovery UX and device-transfer testing are incomplete. |
| **AD-08** | **Encrypt E2EE attachments on the sender device before upload**. The backend stores protected ciphertext objects; attachment keys travel only inside E2EE-protected descriptors. | Server plaintext malware scanning is impossible for E2EE content. Document client-side safeguards; scanning-required workflows must use an explicitly disclosed MANAGED profile. |
| **AD-09** | **SMS is never E2EE**. E2EE-to-SMS delivery requires explicit per-message/per-channel downgrade, consent/disclosure and a separately identified non-E2EE copy. | No implicit fallback and no conversion of an existing PRIVATE_E2EE conversation into MANAGED or SMS transport. No claim that SMS preserves MLS security. |
| **AD-10** | Require **independent cryptographic and application-security assessment** before public production E2EE parity/security claims. | Assessment not completed. Critical/high findings must be fixed; residual risks formally accepted and production release explicitly authorized by the owner. |

## Engineering gates versus production gates

### Permitted on staging

- Implement, review, test and benchmark MLS group lifecycle, client SDKs, Messaging APIs, realtime transport, attachments and compliance controls against the selected engineering design.
- Run proof-of-concept clients and isolated integration tests against real development infrastructure.
- Keep compatibility and data-migration plans versioned; avoid breaking the established ciphertext format and AAD v2 binding.
- Keep architectural risks and incomplete features documented instead of fabricating production evidence.

### Not permitted without a separate release authorization

- Set production or staging cryptographic enablement flags to bypass pending gates.
- Deploy production E2EE services, activate key material publication to production users or distribute sensitive client keys to the server.
- Claim that operator-signed key-transparency heads provide independent anti-equivocation protection.
- Market the system as independently audited or as equivalent to WhatsApp/Signal security.
- Treat Android/iOS/WASM **compile checks** as actual device/browser runtime certification.

## Required client runtime acceptance plan

| Gate | Minimum empirical evidence |
|---|---|
| **Protocol/interop** | Real native Android and iOS devices plus browser and Rust reference where supported; compatible MLS suite, KeyPackages, welcomes, commit ordering, epoch transition, message decrypt and tamper rejection. Record versions/devices. |
| **Group and device lifecycle** | Two principals / multiple authorized devices, add/remove/update leaf, concurrent membership change conflicts, revoked-device future-message exclusion, offline members, rekey and recovery; no server-held E2EE secrets. |
| **Realtime** | Reconnect/offline replay, cursor gaps, duplicated/out-of-order frames, authorization revocation while connected, Redis outage and multi-gateway fanout, bounded backpressure. |
| **Key transparency** | Pinned, signed heads; inclusion and consistency verification; forged key substitution; independent witness or equivalent split-view/equivocation detection, with documented trust assumptions. |
| **Attachments/backups** | Client encrypt/decrypt of real image, document, voice and video bytes; integrity failure, chunk/order/nonce misuse tests, expired signed-grant and removed-member tests, backup/transfer consent and lost-key UX. |
| **Privacy/abuse** | Inspect DB, Redis, logs, metrics, push, traces and exported artifacts for E2EE plaintext/keys; local-only E2EE search; voluntary reports only; legally held E2EE content remains ciphertext. |
| **Client hardening** | Keychain/Keystore behavior, device compromise/loss, browser CSP/Trusted Types and build integrity, safe update/rollback, dependency/license review, mobile app backgrounding and notification behavior. |
| **Scale and resilience** | Repeated load tests on stated hardware, p50/p95/p99, no test-harness distortions, chaos/restart/failover, leakage/queue/gauge stability, rollback and backup/restore tests. |
| **Independent release assessment** | External expert scope includes OpenMLS integration, registration proof, key transparency, media crypto, protocol downgrade, clients and service authorization; all critical/high findings closed or accepted by a documented owner security exception where appropriate. |

## Tracked production blockers

1. External cryptographic/application-security assessment (AD-10).
2. Key-transparency witness/draft alignment (AD-05).
3. Real Android/iOS/Web runtime certification and Web code-delivery limitations (AD-06).
4. Client-controlled history recovery and device-transfer design/UX (AD-07).
5. Independent legal review for MPL-2.0 transitive dependencies and tracked advisories.
6. Production credentials and actual third-party notification delivery tests when those functions are offered (APNs/FCM/Web Push).
7. Explicit owner go/no-go for enabling production cryptographic operation, separate from design selection.

## Repository workflow

All new work occurs directly on **staging**, subject to tests and normal pushes. **main** receives only explicitly approved releases. No additional permanent remote branches, history rewrites, direct main development or accidental production deployment. This record changes no runtime configuration and does not supersede the safety and audit evidence in ADRs 0045–0051.

## Follow-up

Do not mark the ten decisions as production-certification PASS by conflating their selected architectural direction with deployed or independently verified security controls. Record versioned owner choices if any of the ten design positions change, and require explicit sign-off on the final production risk acceptance.

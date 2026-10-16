# ADR 0035 — Numbering: KYC and eligibility

Status: Accepted (Part IV Workstream F). Extends ADR 0033 and ADR 0034.

## Context

Numbers of some countries, types and ownership classes may be assigned only to a verified
subject. Workstream D left an `eligibility_decision_ref` slot on assignments that accepted
any id. Workstream F supplies the requirements, the cases that satisfy them, protected
evidence, verification, and an eligibility decision that is explainable, scoped and
fail-closed — without an external KYC provider and without reserving or provisioning
anything.

## Decisions

1. **Versioned requirements.** `number_kyc_requirements` holds one CURRENT version per
   selector (country, inventory class, number type, ownership class, tenant classification
   and, for `PROVIDER_EXTENSION` sets, the provider). Resolution takes the most specific
   BASE set and merges the serving provider's extensions (stricter wins: mandatory is
   OR-ed, accepted evidence intersected, validity minimized). No matching BASE set is
   `KYC_REQUIREMENTS_UNKNOWN` — never "no requirements". Synthetic sets are
   `SYNTHETIC_TEST` only and marked `nonProduction`; real sets can never be synthetic.
2. **Cases with a frozen snapshot.** `number_kyc_cases` belongs to the trusted scope that
   opened it (other scopes see NOT_FOUND) and stores the exact requirement snapshot it was
   opened against. The lifecycle `DRAFT → EVIDENCE_REQUIRED → SUBMITTED → UNDER_REVIEW →
   APPROVED` with `MORE_EVIDENCE_REQUIRED`, `REJECTED`, `EXPIRED`, `REVOKED` is one edge set
   in Rust (`kyc_allowed`) and SQL (`number_kyc_transition_allowed`, compared by a test);
   `APPROVED → APPROVED` is a revalidation renewal. A trigger refuses any change outside a
   flagged domain transition that advances the revision by exactly one, and keeps scope,
   subject and snapshot immutable. Every transition appends one `number_kyc_decisions` row
   (unique per case revision, append-only; a MANUAL_REVIEW row must be MANUAL by an
   OPERATOR) and one `number.kyc-case.changed` event in the same transaction.
3. **Protected evidence.** Content goes through `packages/object-storage` sealed with
   AES-256-GCM (`cf_webhooks::secret_box`, associated data = the evidence id, so a blob moved
   to another reference does not open), plus server-side encryption required in
   staging / production. The database keeps a reference, type, size, SHA-256 and key id.
   PDF / JPEG / PNG only, ≤ 5 MiB, magic bytes checked. Without a configured key AND store
   evidence is refused (`EVIDENCE_STORAGE_UNAVAILABLE`). Reads are operator-only and every
   attempt (granted, denied, purged) is appended to `number_kyc_evidence_access`.
   Retention purges content and keeps the reference (`PURGED`, one-way). Content never
   appears in logs, events, decisions, errors or non-privileged responses (tested).
4. **One verifier contract, two adapters.** `KycVerifier::verify` / `status` receive
   evidence summaries, never content. An `Unknown` outcome leaves the verification
   UNKNOWN and the job polls `status` for the same request — never a second check, also
   across a worker restart. Responses are applied once (unique `response_ref`); a late
   response for a case that moved on is SUPERSEDED. The deterministic adapter scripts
   approval, rejection, missing / more evidence, short validity, unknown-then-resolved,
   duplicates and revalidation outcomes and is refused in staging / production
   configuration. The manual adapter queues every case for a `numbers.admin` operator whose
   explicit, reason-coded decision must cite the ACTIVE evidence it examined; an approval
   never bypasses a mandatory requirement and may shorten, never extend, the validity.
5. **Lifecycle operations.** Approval expiry and abandonment, revalidation scheduling and
   evidence retention are bounded sweeps; revocation is operator-only; an appeal of a
   rejection or revocation reopens review, is linked to the decision it contests and is
   resolved (UPHELD / OVERTURNED) by the next final manual decision.
6. **Eligibility.** `evaluate_eligibility` reads the trusted standing (FULL authority,
   tenant classification, the environment's `liveTelecomAllowed` from Platform Access),
   feature flags, the E catalog of the caller's inventory class, the specific number's
   resolved capabilities, recorded risk inputs and restriction lists, the environment's
   number limit, the requirements (with provider extension) and the subject's latest case.
   Restriction flags the requirements do not cover are refused. Every reason is recorded
   append-only in `number_eligibility_decisions` with the catalog revision, requirement set
   ids, KYC case and revision; precedence DENIED > UNSUPPORTED_CAPABILITY >
   RISK_RESTRICTED > LIMIT_EXCEEDED > ADDITIONAL_EVIDENCE_REQUIRED > KYC_REQUIRED >
   PENDING_REVIEW > ELIGIBLE. An ELIGIBLE decision is valid until the earlier of its TTL
   and the approval's expiry. Revocation, expiry and requirement changes alter the next
   evaluation immediately. It never reserves or provisions; the allocation-risk engine is
   Workstream T.
7. **Assignments need a real decision.** `record_eligibility` now admits only an ELIGIBLE,
   in-force decision of the assignment's own scope (and number, when the decision names
   one). Internal consumers read `GET /internal/v1/assignments/{id}/kyc-status`.

## Consequences

* KYC works end to end in software (deterministic or manual); external KYC providers are
  future adapters of the same contract.
* Actual grant revocation and release orchestration on expiry / revocation remain later
  workstreams; they consume the eligibility and KYC-status views defined here.

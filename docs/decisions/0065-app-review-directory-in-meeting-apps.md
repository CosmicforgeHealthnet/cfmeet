# ADR 0065 — App review, the app directory and in-meeting apps

**Status:** Accepted (owner decision, 2026-11-18); delivered in phases I6–I7
**Date:** 2026-11-18
**Extends:** [ADR 0064](0064-cf-meet-apps-oauth-installs-delegated-access.md).

## Decisions

1. **Review.** An app moves DRAFT → SUBMITTED → IN_REVIEW → APPROVED or REJECTED → PUBLISHED, and can be SUSPENDED.
   * The state machine is copied from the production-access workflow.
   * Reviewers are named (`DEVELOPER_PLATFORM_APP_REVIEWERS`), self-approval is impossible, and every decision is audited.
   * The operator CLI drives it.
   * A draft app can be installed only by its own developers' CF Meet accounts, with a limited number of test users.
2. **Directory.** `GET /v1/app-directory` is anonymous and lists only PUBLISHED, public apps. Workspace admins can list, allow and remove apps.
3. **In-meeting apps.** An app declares a meeting surface: its URL and origins.
   * The Meeting Service lists the apps installed for a meeting's workspace or owner.
   * It mints a 5-minute app-context token for the app. The token carries the conference, a pairwise participant id and the role; the user's principal is included only if that user granted the app.
   * The token is signed with keys published as a JWKS at `GET /api/v2/app-context-keys`.
   * The sandboxed iframe panel and its messaging bridge are frontend work.
4. **Suspension.** An operator can suspend an app, which revokes every grant.

## Consequences

* The review policy is an open owner decision: which scopes need review, and how many test users a draft app may have.

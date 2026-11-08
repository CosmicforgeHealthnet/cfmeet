/**
 * Account status rules (ACC-012). Pure; runs under `node --test`.
 *
 * `POST /api/v1/auth/login` and `GET /api/v1/users/me` report `status` as
 * `pending_verification`, `active`, `inactive` or `suspended`; until the deployment sets
 * `ACCOUNT_STATUS_FORMAT=canonical` the unverified state is spelled `pendingverification`
 * (historical v1). `GET /api/v2/me/profile` always uses the canonical spelling.
 * The login body carries no email address, so the verify screen needs the email the user typed.
 */

export type AccountStatusClass = "active" | "pending_verification" | "inactive" | "unknown";

export function classifyAccountStatus(status: string | null | undefined): AccountStatusClass {
  const s = (status ?? "").trim().toLowerCase().replace(/[\s-]/g, "_");
  if (s === "active") return "active";
  if (s === "pending_verification" || s === "pendingverification" || s === "pending") {
    return "pending_verification";
  }
  if (s === "inactive" || s === "suspended" || s === "deactivated" || s === "deleted") return "inactive";
  return "unknown";
}

export type PostSignInRoute =
  | { kind: "home" }
  | { kind: "verify"; email: string }
  | { kind: "blocked" };

/** Where to send a user right after a successful sign-in. */
export function routeAfterSignIn(status: string | null | undefined, typedEmail: string): PostSignInRoute {
  switch (classifyAccountStatus(status)) {
    case "pending_verification":
      return { kind: "verify", email: typedEmail.trim().toLowerCase() };
    case "inactive":
      return { kind: "blocked" };
    default:
      return { kind: "home" };
  }
}

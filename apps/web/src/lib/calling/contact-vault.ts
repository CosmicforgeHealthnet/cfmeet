import { callingFetch, CallingHttpError } from "@/lib/calling/server";

/**
 * Privacy Relay Contact Vault access through Calling's delegated grant (Part D §31).
 *
 * The Contact Vault paths (`/v1/privacy-relay/contacts…`) are served by Privacy Relay, not by
 * Calling (Calling answers 404 for them; they are CONTACT_VAULT_PRIVATE on the public gateway).
 * The adapter therefore:
 *   1. mints a short-lived Contact Vault token with the platform credential:
 *      `POST /v1/privacy-relay/contact-access-grants` `{ scopes: [<one privacy.contacts.* scope>] }`
 *      (served by Calling; the credential must itself hold that scope);
 *   2. calls Privacy Relay's Contact Vault directly with `X-CF-Contact-Authorization: Bearer
 *      <token>` and NO `Authorization` header, within the token's lifetime (≤ 300 s).
 * The token never leaves the server; a new one is minted per request.
 */

export type ContactScope = "privacy.contacts.read" | "privacy.contacts.write" | "privacy.contacts.verify";

const CONTACT_REFERENCE = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";

/** Contact Vault operations of packages/contracts/privacy-relay/http/v1.openapi.json and their scope. */
const CONTACT_VAULT_RULES: Array<[RegExp, string, ContactScope]> = [
  [/^v1\/privacy-relay\/contacts$/, "POST", "privacy.contacts.write"],
  [new RegExp(`^v1/privacy-relay/contacts/${CONTACT_REFERENCE}$`), "GET", "privacy.contacts.read"],
  [new RegExp(`^v1/privacy-relay/contacts/${CONTACT_REFERENCE}$`), "PATCH", "privacy.contacts.write"],
  [new RegExp(`^v1/privacy-relay/contacts/${CONTACT_REFERENCE}/disable$`), "POST", "privacy.contacts.write"],
  [new RegExp(`^v1/privacy-relay/contacts/${CONTACT_REFERENCE}/verification$`), "POST", "privacy.contacts.verify"],
];

/** Whether `route` is under the Contact Vault prefix (served by Privacy Relay, never by Calling). */
export function isContactVaultRoute(route: string): boolean {
  return route === "v1/privacy-relay/contacts" || route.startsWith("v1/privacy-relay/contacts/");
}

/** The scope a Contact Vault operation needs, or null when it is not an allowlisted operation. */
export function contactVaultScope(route: string, method: string): ContactScope | null {
  const rule = CONTACT_VAULT_RULES.find(([pattern, m]) => m === method && pattern.test(route));
  return rule ? rule[2] : null;
}

/** Privacy Relay's origin; production requires an explicit https:// URL without credentials. */
export function privacyRelayBase(env: Record<string, string | undefined> = process.env): string {
  const configured = (env.PRIVACY_RELAY_API_URL || "").replace(/\/$/, "");
  const production = env.NODE_ENV === "production";
  let url: URL;
  try {
    url = new URL(configured);
  } catch {
    throw new CallingHttpError(503, "CONTACT_VAULT_NOT_CONFIGURED", "Protected contacts are not configured for this deployment.");
  }
  if ((production && url.protocol !== "https:") || !["https:", "http:"].includes(url.protocol) || url.username || url.password) {
    throw new CallingHttpError(503, "CONTACT_VAULT_NOT_CONFIGURED", "Protected contacts are not securely configured.");
  }
  return configured;
}

async function mintContactToken(scope: ContactScope): Promise<string> {
  const response = await callingFetch("/v1/privacy-relay/contact-access-grants", {
    method: "POST",
    body: JSON.stringify({ scopes: [scope] }),
  });
  if (response.status === 401) {
    throw new CallingHttpError(503, "CALLING_NOT_CONFIGURED", "The calling connection is not available for this account.");
  }
  if (response.status === 403) {
    throw new CallingHttpError(403, "INSUFFICIENT_SCOPE", "This deployment's credential may not use protected contacts.");
  }
  if (response.status !== 201 && response.status !== 200) {
    throw new CallingHttpError(503, "CONTACT_VAULT_UNAVAILABLE", "Protected contacts are temporarily unavailable.");
  }
  const payload = await response.json().catch(() => null) as { grant?: { accessToken?: unknown; authorizationHeader?: unknown } } | null;
  const token = payload?.grant?.accessToken;
  if (typeof token !== "string" || !/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token) ||
      payload?.grant?.authorizationHeader !== "X-CF-Contact-Authorization") {
    throw new CallingHttpError(502, "UPSTREAM_ERROR", "Calling returned an invalid contact access grant.");
  }
  return token;
}

/** Calls the Contact Vault for `route` (already allowlisted) with a freshly minted grant. */
export async function contactVaultFetch(route: string, scope: ContactScope, init: RequestInit & { search?: string } = {}) {
  const base = privacyRelayBase();
  const token = await mintContactToken(scope);
  const headers = new Headers(init.headers);
  headers.delete("Authorization");
  headers.set("X-CF-Contact-Authorization", `Bearer ${token}`);
  headers.set("Content-Type", "application/json");
  const { search = "", ...rest } = init;
  return fetch(`${base}/${route}${search}`, {
    ...rest, headers, cache: "no-store", redirect: "error", signal: AbortSignal.timeout(20_000),
  });
}

/**
 * ADR 0068 — owner-delivered exports reach the person they belong to (pure helpers, no I/O).
 *
 * CF Meet support (HQ) can ask for a copy of someone's data, but HQ never sees it: the owning
 * service builds it and the person downloads it themselves, signed in to CF Meet.
 *
 * - Account data: an email link `/settings/data-export/download?export=<id>&token=<token>`;
 *   the page calls `GET /api/v2/me/data-export?exportId=<id>` with the token in the
 *   `X-CF-Data-Export-Token` header, after a fresh sign-in.
 * - Messaging: the export waits in the person's own console
 *   (`GET /api/v2/messaging/export-deliveries`); claiming it
 *   (`POST /api/v2/messaging/exports/{id}/claim`) returns one short-lived download link,
 *   used straight away.
 * - Organization: an email link `/organizations/exports/download#org=<id>&export=<id>&token=<token>`
 *   (fragment: never sent to a server); the page posts the token to
 *   `POST /api/v2/organizations/{id}/exports/{exportId}/download`.
 *
 * Tokens are read once, removed from the address bar at once, kept in memory (and, while the
 * person signs in, in this tab's sessionStorage for at most 30 minutes), sent only in a header
 * or a request body, and never logged.
 */

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
/** The account-export token: 32 random bytes, base64url (43 characters). */
const ACCOUNT_TOKEN = /^[A-Za-z0-9_-]{20,128}$/;
/** The organization-export token (Organizations accepts 16–512 visible characters). */
const ORGANIZATION_TOKEN = /^[A-Za-z0-9_\-.~]{16,512}$/;

export interface AccountExportLink {
  exportId: string;
  token: string;
}

export interface OrganizationExportLink {
  organizationId: string;
  exportId: string;
  token: string;
}

function uuid(value: string | null | undefined): string | null {
  const v = (value ?? "").trim().toLowerCase();
  return UUID.test(v) ? v : null;
}

/** `?export=<uuid>&token=<token>` of the account-export email link, or null. */
export function accountExportLinkFrom(search: string | null | undefined): AccountExportLink | null {
  const params = new URLSearchParams((search ?? "").replace(/^\?/, ""));
  const exportId = uuid(params.get("export"));
  const token = (params.get("token") ?? "").trim();
  if (!exportId || !ACCOUNT_TOKEN.test(token)) return null;
  return { exportId, token };
}

/** `#org=<uuid>&export=<uuid>&token=<token>` of the organization-export email link, or null. */
export function organizationExportLinkFrom(hash: string | null | undefined): OrganizationExportLink | null {
  const raw = (hash ?? "").replace(/^#/, "");
  if (!raw || raw.length > 1024) return null;
  const params = new URLSearchParams(raw);
  const organizationId = uuid(params.get("org"));
  const exportId = uuid(params.get("export"));
  const token = (params.get("token") ?? "").trim();
  if (!organizationId || !exportId || !ORGANIZATION_TOKEN.test(token)) return null;
  return { organizationId, exportId, token };
}

/** The minimal storage the pending-link helpers need (sessionStorage in the browser). */
export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

export const PENDING_TTL_MS = 30 * 60_000;
export const ACCOUNT_EXPORT_PENDING_KEY = "cfmeet.account-export-link";
export const ORGANIZATION_EXPORT_PENDING_KEY = "cfmeet.organization-export-link";

/** Keeps a link while the person signs in (at most 30 minutes). Storage errors are ignored. */
export function stashLink(store: KeyValueStore | null, key: string, link: object, now: number): void {
  try {
    store?.setItem(key, JSON.stringify({ link, until: now + PENDING_TTL_MS }));
  } catch {
    /* storage disabled: the person opens the email link again after signing in */
  }
}

/** The stashed link if it is still valid (an expired or malformed one is erased). */
export function stashedLink<T>(
  store: KeyValueStore | null,
  key: string,
  now: number,
  parse: (value: unknown) => T | null,
): T | null {
  try {
    const raw = store?.getItem(key);
    if (!raw) return null;
    const value = JSON.parse(raw) as { link?: unknown; until?: unknown };
    const link = typeof value.until === "number" && value.until >= now ? parse(value.link) : null;
    if (!link) store?.removeItem(key);
    return link;
  } catch {
    return null;
  }
}

/** Erases a stashed link (after a download, or when it can never succeed). */
export function forgetLink(store: KeyValueStore | null, key: string): void {
  try {
    store?.removeItem(key);
  } catch {
    /* nothing stored */
  }
}

/** Re-validates a stashed account-export link. */
export function asAccountExportLink(value: unknown): AccountExportLink | null {
  const v = value as Partial<AccountExportLink> | null;
  if (!v || typeof v.exportId !== "string" || typeof v.token !== "string") return null;
  return accountExportLinkFrom(`?export=${encodeURIComponent(v.exportId)}&token=${encodeURIComponent(v.token)}`);
}

/** Re-validates a stashed organization-export link. */
export function asOrganizationExportLink(value: unknown): OrganizationExportLink | null {
  const v = value as Partial<OrganizationExportLink> | null;
  if (!v || typeof v.organizationId !== "string" || typeof v.exportId !== "string" || typeof v.token !== "string") return null;
  return organizationExportLinkFrom(
    `#org=${encodeURIComponent(v.organizationId)}&export=${encodeURIComponent(v.exportId)}&token=${encodeURIComponent(v.token)}`,
  );
}

/** `{status, code}` of an axios error whose body may be JSON text (text / blob responses). */
export function exportErrorOf(error: unknown): { status: number; code: string } {
  const e = error as { response?: { status?: unknown; data?: unknown } } | null;
  const status = typeof e?.response?.status === "number" ? e.response.status : 0;
  let data = e?.response?.data;
  if (typeof data === "string") {
    try {
      data = JSON.parse(data);
    } catch {
      data = null;
    }
  }
  const code = (data as { error?: { code?: unknown } } | null)?.error?.code;
  return { status, code: typeof code === "string" ? code : status ? `HTTP_${status}` : "NETWORK_UNAVAILABLE" };
}

export type DownloadOutcome = "reauthenticate" | "sign_in" | "invalid" | "expired" | "already_delivered" | "unavailable" | "failed";

/** What the page tells the person after a refused download. */
export function downloadOutcome(error: unknown): DownloadOutcome {
  const { status, code } = exportErrorOf(error);
  if (code === "REAUTHENTICATION_REQUIRED") return "reauthenticate";
  if (status === 401 || code === "AUTHENTICATION_REQUIRED") return "sign_in";
  if (code === "EXPORT_ALREADY_DELIVERED") return "already_delivered";
  if (code === "EXPORT_LINK_EXPIRED" || code === "EXPORT_EXPIRED" || status === 410) return "expired";
  if (status === 404 || status === 400 || code === "EXPORT_LINK_INVALID") return "invalid";
  if (status === 0 || status >= 500 || code === "FEATURE_DISABLED") return "unavailable";
  return "failed";
}

/** Whether a refused link can never succeed (it is then forgotten). */
export function isFinalOutcome(outcome: DownloadOutcome): boolean {
  return outcome === "invalid" || outcome === "expired" || outcome === "already_delivered";
}

export function downloadOutcomeText(outcome: DownloadOutcome): string {
  switch (outcome) {
    case "reauthenticate":
      return "For your security, sign in again to download your data.";
    case "sign_in":
      return "Sign in to CF Meet to download your data.";
    case "invalid":
      return "This download link is invalid, already used, or not for this account.";
    case "expired":
      return "This download link has expired. Ask CF Meet support to request your data again.";
    case "already_delivered":
      return "This export was already downloaded. Ask CF Meet support if you need a new copy.";
    case "unavailable":
      return "Downloads are temporarily unavailable. Try again in a few minutes.";
    default:
      return "That didn't work. Try again.";
  }
}

/** A safe file name for a downloaded export. */
export function exportFileName(kind: "account" | "organization" | "messaging", exportId: string, now: Date): string {
  const day = now.toISOString().slice(0, 10);
  const id = (uuid(exportId) ?? "export").slice(0, 8);
  return `cfmeet-${kind}-data-${day}-${id}.json`;
}

/** One Messaging export delivery as the console lists it (references only). */
export interface ExportDelivery {
  exportId: string;
  exportState: "REQUESTED" | "READY" | "DELIVERED" | "EXPIRED" | "FAILED";
  size: number | null;
  sha256: string | null;
  createdAt: string;
  completedAt: string | null;
  deliveredAt: string | null;
  expiresAt: string;
}

/** The deliveries the person can still act on (READY and unexpired) first, newest first. */
export function orderedDeliveries(items: ExportDelivery[], now: number): ExportDelivery[] {
  const waiting = (d: ExportDelivery) => d.exportState === "READY" && Date.parse(d.expiresAt) > now;
  return [...items].sort((a, b) => {
    const w = Number(waiting(b)) - Number(waiting(a));
    return w !== 0 ? w : Date.parse(b.createdAt) - Date.parse(a.createdAt);
  });
}

export function canClaim(d: ExportDelivery, now: number): boolean {
  return d.exportState === "READY" && Date.parse(d.expiresAt) > now;
}

export function deliveryStateText(d: ExportDelivery, now: number): string {
  if (canClaim(d, now)) return "Ready to download (once)";
  switch (d.exportState) {
    case "REQUESTED":
      return "Being prepared";
    case "DELIVERED":
      return "Downloaded";
    case "FAILED":
      return "Could not be prepared";
    default:
      return "Expired";
  }
}

/** Only an https (or same-origin http on localhost) download URL is followed. */
export function safeDownloadUrl(url: unknown): string | null {
  if (typeof url !== "string" || url.length > 4096) return null;
  try {
    const u = new URL(url);
    if (u.protocol === "https:") return u.toString();
    if (u.protocol === "http:" && (u.hostname === "localhost" || u.hostname === "127.0.0.1")) return u.toString();
    return null;
  } catch {
    return null;
  }
}

export function humanSize(bytes: number | null): string {
  if (bytes === null || !Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

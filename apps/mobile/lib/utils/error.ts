import { decodeApiError } from "@/lib/api/errors";

/**
 * A user-facing message for any failure (FE-019): the v2 error envelope's message, a
 * classified fallback (expired session, forbidden, offline, …) or `fallback`.
 */
export function getErrorMessage(err: unknown, fallback = "An error occurred"): string {
  const decoded = decodeApiError(err);
  return decoded.message || fallback;
}

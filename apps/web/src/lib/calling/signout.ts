/**
 * Logout from CF Meet: this browser's Calling endpoint is revoked so a signed-out browser is
 * never rung (S-10). Uses the same revoke the Calling devices page and "Turn off" use
 * (`DELETE /v1/calling/endpoints/{id}` through the same-origin calling adapter, with the
 * account session). Best effort and bounded; must run before the session is cleared.
 */
import { endpointsService } from "@/services/calling/endpoints.service";
import { ENDPOINT_SIGNOUT_TIMEOUT_MS, clearSigningOut, signOutCallingEndpoint, type KeyValueStorage } from "./endpoint-signout";

function browserStorage(): KeyValueStorage | null {
  if (typeof window === "undefined") return null;
  try { return window.localStorage ?? null; } catch { return null; }
}

export async function signOutCalling(timeoutMs = ENDPOINT_SIGNOUT_TIMEOUT_MS): Promise<void> {
  try {
    await signOutCallingEndpoint({ storage: browserStorage(), revoke: (id) => endpointsService.revoke(id), timeoutMs });
  } catch { /* logout never fails because of Calling */ }
}

/** After a fresh sign-in: this browser registers its Calling endpoint again. */
export function allowCallingRegistration(): void {
  clearSigningOut(browserStorage());
}

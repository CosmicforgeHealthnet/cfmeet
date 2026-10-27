/**
 * Idempotency keys for Calling mutations.
 *
 * Every mutating Calling route accepts an `Idempotency-Key`; call creation and
 * transfer require one. A replay with the same key returns the recorded response.
 */

export function newIdempotencyKey(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

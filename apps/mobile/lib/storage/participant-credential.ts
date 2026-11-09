import * as SecureStore from "expo-secure-store";
import { STORAGE_KEYS } from "@/lib/constants";

/**
 * The conference-scoped participant credential (FE-005).
 *
 * `POST /api/v2/conferences/{id}/join` returns `participantToken` once, when it creates a
 * participant for a caller without an account. The token is the guest's identity in that
 * conference: it must be sent as `X-Participant-Token` on that conference's calls
 * (status polling, chat, leave, re-join) or the backend cannot recognise the guest and a
 * re-join would create a duplicate participant. It is kept in memory and in SecureStore
 * (so an app restart can resume), bound to exactly one conference.
 */
export interface ParticipantCredential {
  conferenceId: string;
  token: string;
}

let cached: ParticipantCredential | null | undefined;

export const participantCredentialStore = {
  async get(): Promise<ParticipantCredential | null> {
    if (cached !== undefined) return cached;
    try {
      const raw = await SecureStore.getItemAsync(STORAGE_KEYS.PARTICIPANT_CREDENTIAL);
      const parsed = raw ? (JSON.parse(raw) as Partial<ParticipantCredential>) : null;
      cached =
        parsed && typeof parsed.conferenceId === "string" && typeof parsed.token === "string"
          ? { conferenceId: parsed.conferenceId, token: parsed.token }
          : null;
    } catch {
      cached = null;
    }
    return cached;
  },

  /** The credential for `conferenceId`, or null when none is held for it. */
  async getFor(conferenceId: string): Promise<string | null> {
    const credential = await this.get();
    return credential && credential.conferenceId === conferenceId ? credential.token : null;
  },

  async set(conferenceId: string, token: string): Promise<void> {
    cached = { conferenceId, token };
    try {
      await SecureStore.setItemAsync(
        STORAGE_KEYS.PARTICIPANT_CREDENTIAL,
        JSON.stringify(cached)
      );
    } catch {
      // Memory copy still serves this session.
    }
  },

  async clear(): Promise<void> {
    cached = null;
    try {
      await SecureStore.deleteItemAsync(STORAGE_KEYS.PARTICIPANT_CREDENTIAL);
    } catch {
      // Nothing else to do.
    }
  },
};

/** The conference id of a conference-scoped v2 path (`/conferences/{id}/…`), if any. */
export function conferenceIdFromPath(url: string | undefined): string | null {
  const match = url?.match(/^\/?conferences\/([^/?#]+)/);
  return match ? decodeURIComponent(match[1]) : null;
}

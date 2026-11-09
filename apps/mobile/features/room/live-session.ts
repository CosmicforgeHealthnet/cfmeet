/**
 * The current live-room session, in memory only (FE-004 / FE-005).
 *
 * The RTC credential (LiveKit token) is never put in a route parameter, a log or persistent
 * storage: the join flow writes it here and the live screen reads it. Leaving or a refused
 * admission clears it, together with a guest's conference-scoped participant credential.
 */
import { create } from "zustand";
import type { LiveMedia } from "@/lib/meeting/admission";
import type { Role } from "@/lib/api/generated/meeting-service";
import { participantCredentialStore } from "@/lib/storage/participant-credential";

export interface LiveSessionState {
  media: LiveMedia | null;
  /** The caller's role in the meeting space (`viewerRole`), when known. */
  spaceRole: Role | null;
  title: string | null;
  meetingCode: string | null;
  isGuest: boolean;
  setLive: (s: { media: LiveMedia; spaceRole: Role | null; title: string | null; meetingCode: string | null; isGuest: boolean }) => void;
  /** Replaces the RTC credential after a reconnect (`POST /join` with `previousSessionId`). */
  replaceMedia: (media: LiveMedia) => void;
  clear: () => Promise<void>;
}

export const useLiveSession = create<LiveSessionState>((set) => ({
  media: null,
  spaceRole: null,
  title: null,
  meetingCode: null,
  isGuest: false,
  setLive: ({ media, spaceRole, title, meetingCode, isGuest }) => set({ media, spaceRole, title, meetingCode, isGuest }),
  replaceMedia: (media) => set({ media }),
  clear: async () => {
    set({ media: null, spaceRole: null, title: null, meetingCode: null, isGuest: false });
    await participantCredentialStore.clear();
  },
}));

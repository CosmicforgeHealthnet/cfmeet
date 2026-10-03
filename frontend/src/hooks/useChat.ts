import { useChatContext } from "@/context/ChatContext";

/**
 * Hook to access chat functionality.
 * Must be used within a ChatProvider (which is mounted at the room level).
 *
 * The meetingId and participantId parameters are kept for backward compatibility
 * but are no longer used - the ChatProvider handles these.
 */
export function useChat(_meetingId?: string, _participantId?: string) {
  return useChatContext();
}

/**
 * Which conferences have a healthy realtime connection in this tab. While one does, every
 * change reaches the room as an event (RealtimeRoomSync refreshes the affected queries), so
 * the room's REST polling only needs to be a slow safety net; when the connection drops, the
 * polls go back to their fast interval on their next tick.
 */
const connected = new Set<string>();

export function markRealtime(conferenceId: string, open: boolean): void {
  if (open) connected.add(conferenceId);
  else connected.delete(conferenceId);
}

export function realtimeHealthy(conferenceId: string): boolean {
  return connected.has(conferenceId);
}

/** A react-query `refetchInterval`: `fastMs` without realtime, a slow safety net with it. */
export function roomPoll(conferenceId: string, fastMs: number, safetyNetMs = 15_000): () => number {
  return () => (connected.has(conferenceId) ? Math.max(fastMs, safetyNetMs) : fastMs);
}

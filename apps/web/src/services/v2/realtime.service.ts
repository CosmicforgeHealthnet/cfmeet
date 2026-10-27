/**
 * V2 Realtime Service
 * Handles WebSocket subscriptions and event replay
 */

import apiV2 from "./api";
import type {
  RealtimeTicketResponse,
  EventReplayQuery,
  EventReplayResponse,
} from "@/types/v2";

export const realtimeService = {
  /**
   * Get a one-time WebSocket ticket
   */
  getTicket: async (conferenceId: string) => {
    const response = await apiV2.post<RealtimeTicketResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/realtime/ticket`,
    );
    return response.data;
  },

  /**
   * Get missed events (REST fallback)
   */
  getEvents: async (conferenceId: string, query?: EventReplayQuery) => {
    const response = await apiV2.get<EventReplayResponse>(
      `/conferences/${encodeURIComponent(conferenceId)}/events`,
      { params: query },
    );
    return response.data;
  },

  /**
   * Create a WebSocket connection for realtime events
   */
  createWebSocket: (
    websocketPath: string,
    ticket: string,
    cursor?: number,
  ): WebSocket => {
    const baseUrl = apiV2.defaults.baseURL || "";
    const wsUrl = new URL(websocketPath, baseUrl);
    wsUrl.protocol = wsUrl.protocol === "https:" ? "wss:" : "ws:";
    wsUrl.searchParams.set("ticket", ticket);
    if (cursor !== undefined) wsUrl.searchParams.set("cursor", cursor.toString());
    return new WebSocket(wsUrl);
  },
};

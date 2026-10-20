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
      `/conferences/${conferenceId}/realtime/ticket`,
    );
    return response.data;
  },

  /**
   * Get missed events (REST fallback)
   */
  getEvents: async (conferenceId: string, query?: EventReplayQuery) => {
    const response = await apiV2.get<EventReplayResponse>(
      `/conferences/${conferenceId}/events`,
      { params: query },
    );
    return response.data;
  },

  /**
   * Create a WebSocket connection for realtime events
   */
  createWebSocket: (
    conferenceId: string,
    ticket: string,
    cursor?: number,
  ): WebSocket => {
    const baseUrl = apiV2.defaults.baseURL || "";
    const wsUrl = baseUrl
      .replace("https://", "wss://")
      .replace("http://", "ws://");

    const params = new URLSearchParams({ ticket });
    if (cursor !== undefined) {
      params.set("cursor", cursor.toString());
    }

    return new WebSocket(`${wsUrl}/conferences/${conferenceId}/realtime?${params}`);
  },
};

/**
 * V2 Realtime Hooks
 * React hooks for WebSocket subscriptions and event handling
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { realtimeService } from "@/services/v2";
import type { RealtimeEnvelope, EventReplayQuery } from "@/types/v2";

// Query Keys
export const realtimeKeys = {
  all: ["v2", "realtime"] as const,
  ticket: (conferenceId: string) => [...realtimeKeys.all, "ticket", conferenceId] as const,
  events: (conferenceId: string, query?: EventReplayQuery) =>
    [...realtimeKeys.all, "events", conferenceId, query] as const,
};

// -------------------------------------------------------------------------
// Queries
// -------------------------------------------------------------------------

/**
 * Get missed events (REST fallback)
 */
export function useEvents(conferenceId: string, query?: EventReplayQuery) {
  return useQuery({
    queryKey: realtimeKeys.events(conferenceId, query),
    queryFn: () => realtimeService.getEvents(conferenceId, query),
    enabled: !!conferenceId,
  });
}

// -------------------------------------------------------------------------
// Mutations
// -------------------------------------------------------------------------

/**
 * Get a WebSocket ticket
 */
export function useRealtimeTicket(conferenceId: string) {
  return useMutation({
    mutationFn: () => realtimeService.getTicket(conferenceId),
  });
}

// -------------------------------------------------------------------------
// WebSocket Hook
// -------------------------------------------------------------------------

export interface UseRealtimeOptions {
  /** Conference ID to subscribe to */
  conferenceId: string;
  /** Callback for each event received */
  onEvent?: (event: RealtimeEnvelope) => void;
  /** Callback for connection open */
  onOpen?: () => void;
  /** Callback for connection close */
  onClose?: (event: CloseEvent) => void;
  /** Callback for errors */
  onError?: (error: Event) => void;
  /** Auto-reconnect on disconnect (default: true) */
  autoReconnect?: boolean;
  /** Max reconnect attempts (default: 5) */
  maxReconnectAttempts?: number;
  /** Reconnect delay in ms (default: 1000) */
  reconnectDelay?: number;
  /** Enable the connection (default: true) */
  enabled?: boolean;
}

export interface UseRealtimeReturn {
  /** Current WebSocket connection state */
  readyState: number;
  /** Whether the connection is open */
  isConnected: boolean;
  /** Whether we're attempting to reconnect */
  isReconnecting: boolean;
  /** Number of reconnect attempts made */
  reconnectAttempts: number;
  /** Last cursor received */
  cursor: number;
  /** Manually close the connection */
  disconnect: () => void;
  /** Manually reconnect */
  reconnect: () => void;
}

/**
 * Hook for managing a realtime WebSocket connection to a conference
 */
export function useRealtime(options: UseRealtimeOptions): UseRealtimeReturn {
  const {
    conferenceId,
    onEvent,
    onOpen,
    onClose,
    onError,
    autoReconnect = true,
    maxReconnectAttempts = 5,
    reconnectDelay = 1000,
    enabled = true,
  } = options;

  const wsRef = useRef<WebSocket | null>(null);
  const cursorRef = useRef<number>(0);
  const reconnectAttemptsRef = useRef(0);
  const reconnectTimeoutRef = useRef<NodeJS.Timeout | null>(null);

  const [readyState, setReadyState] = useState<number>(WebSocket.CLOSED);
  const [isReconnecting, setIsReconnecting] = useState(false);

  const getTicket = useRealtimeTicket(conferenceId);

  const connect = useCallback(async () => {
    if (!enabled || !conferenceId) return;

    try {
      const ticketData = await getTicket.mutateAsync();
      cursorRef.current = ticketData.cursor;

      const ws = realtimeService.createWebSocket(
        conferenceId,
        ticketData.ticket,
        cursorRef.current,
      );

      ws.onopen = () => {
        setReadyState(WebSocket.OPEN);
        setIsReconnecting(false);
        reconnectAttemptsRef.current = 0;
        onOpen?.();
      };

      ws.onmessage = (event) => {
        try {
          const envelope: RealtimeEnvelope = JSON.parse(event.data);
          cursorRef.current = envelope.sequence;
          onEvent?.(envelope);
        } catch (e) {
          console.error("Failed to parse realtime event:", e);
        }
      };

      ws.onclose = (event) => {
        setReadyState(WebSocket.CLOSED);
        onClose?.(event);

        // Auto-reconnect if enabled and not a clean close
        if (
          autoReconnect &&
          !event.wasClean &&
          reconnectAttemptsRef.current < maxReconnectAttempts
        ) {
          setIsReconnecting(true);
          reconnectAttemptsRef.current++;
          reconnectTimeoutRef.current = setTimeout(() => {
            connect();
          }, reconnectDelay * reconnectAttemptsRef.current);
        }
      };

      ws.onerror = (error) => {
        onError?.(error);
      };

      wsRef.current = ws;
      setReadyState(WebSocket.CONNECTING);
    } catch (error) {
      console.error("Failed to get realtime ticket:", error);
      onError?.(error as Event);
    }
  }, [
    conferenceId,
    enabled,
    getTicket,
    onEvent,
    onOpen,
    onClose,
    onError,
    autoReconnect,
    maxReconnectAttempts,
    reconnectDelay,
  ]);

  const disconnect = useCallback(() => {
    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current);
      reconnectTimeoutRef.current = null;
    }
    if (wsRef.current) {
      wsRef.current.close();
      wsRef.current = null;
    }
    setReadyState(WebSocket.CLOSED);
    setIsReconnecting(false);
    reconnectAttemptsRef.current = 0;
  }, []);

  const reconnect = useCallback(() => {
    disconnect();
    reconnectAttemptsRef.current = 0;
    connect();
  }, [disconnect, connect]);

  // Connect on mount, disconnect on unmount
  useEffect(() => {
    if (enabled) {
      connect();
    }
    return () => {
      disconnect();
    };
  }, [enabled, connect, disconnect]);

  return {
    readyState,
    isConnected: readyState === WebSocket.OPEN,
    isReconnecting,
    reconnectAttempts: reconnectAttemptsRef.current,
    cursor: cursorRef.current,
    disconnect,
    reconnect,
  };
}

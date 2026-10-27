/**
 * V2 Realtime Hooks
 * React hooks for WebSocket subscriptions and event recovery
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { conferencesService } from "@/services/v2/conferences.service";
import { realtimeService } from "@/services/v2";
import type {
  ConferenceResponse,
  RealtimeEnvelope,
  EventReplayQuery,
} from "@/types/v2";

const CLOSED = 3;
const OPEN = 1;
const CONNECTING = 0;

interface RealtimeFrame {
  type: "subscribed" | "event" | "resync_required" | "degraded" | "revoked" | "closed" | "pong";
  event?: RealtimeEnvelope;
  transient?: boolean;
  cursor?: number;
  reason?: string;
  canonical?: string;
}

// Query Keys
export const realtimeKeys = {
  all: ["v2", "realtime"] as const,
  ticket: (conferenceId: string) => [...realtimeKeys.all, "ticket", conferenceId] as const,
  events: (conferenceId: string, query?: EventReplayQuery) =>
    [...realtimeKeys.all, "events", conferenceId, query] as const,
};

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

/**
 * Get a WebSocket ticket
 */
export function useRealtimeTicket(conferenceId: string) {
  return useMutation({
    mutationFn: () => realtimeService.getTicket(conferenceId),
  });
}

export interface UseRealtimeOptions {
  /** Conference ID to subscribe to */
  conferenceId: string;
  /** Callback for each committed event received or replayed */
  onEvent?: (event: RealtimeEnvelope) => void;
  /** Callback after the canonical conference state is fetched for recovery */
  onCanonicalState?: (conference: ConferenceResponse) => void;
  /** Callback for connection open */
  onOpen?: () => void;
  /** Callback for connection close */
  onClose?: (event: CloseEvent) => void;
  /** Callback for errors */
  onError?: (error: unknown) => void;
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
  /** Last committed event cursor received */
  cursor: number;
  /** Manually close the connection */
  disconnect: () => void;
  /** Manually reconnect */
  reconnect: () => void;
}

/**
 * Subscribe to authorized conference events and recover missed events from REST
 * replay, refetching canonical conference state when the cursor can no longer be replayed.
 */
export function useRealtime(options: UseRealtimeOptions): UseRealtimeReturn {
  const {
    conferenceId,
    onEvent,
    onCanonicalState,
    onOpen,
    onClose,
    onError,
    autoReconnect = true,
    maxReconnectAttempts = 5,
    reconnectDelay = 1000,
    enabled = true,
  } = options;

  const callbacks = useRef({ onEvent, onCanonicalState, onOpen, onClose, onError });
  const wsRef = useRef<WebSocket | null>(null);
  const cursorRef = useRef(0);
  const hasCursorRef = useRef(false);
  const reconnectAttemptsRef = useRef(0);
  const reconnectTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const generationRef = useRef(0);
  const connectRef = useRef<() => void>(() => undefined);
  const [readyState, setReadyState] = useState(CLOSED);
  const [isReconnecting, setIsReconnecting] = useState(false);
  const [reconnectAttempts, setReconnectAttempts] = useState(0);
  const [cursor, setCursor] = useState(0);
  const requestTicket = useCallback(
    () => realtimeService.getTicket(conferenceId),
    [conferenceId],
  );

  const advanceCursor = useCallback((value: number) => {
    cursorRef.current = Math.max(cursorRef.current, value);
    hasCursorRef.current = true;
    setCursor(cursorRef.current);
  }, []);

  useEffect(() => {
    callbacks.current = { onEvent, onCanonicalState, onOpen, onClose, onError };
  }, [onEvent, onCanonicalState, onOpen, onClose, onError]);

  const scheduleReconnect = useCallback(() => {
    if (!autoReconnect || reconnectAttemptsRef.current >= maxReconnectAttempts) return;
    reconnectAttemptsRef.current += 1;
    setReconnectAttempts(reconnectAttemptsRef.current);
    setIsReconnecting(true);
    reconnectTimeoutRef.current = setTimeout(() => {
      reconnectTimeoutRef.current = null;
      connectRef.current();
    }, reconnectDelay * reconnectAttemptsRef.current);
  }, [autoReconnect, maxReconnectAttempts, reconnectDelay]);

  const recover = useCallback(async () => {
    try {
      const replay = await realtimeService.getEvents(conferenceId, { after: cursorRef.current });
      if (replay.resyncRequired) {
        const conference = await conferencesService.get(conferenceId);
        callbacks.current.onCanonicalState?.(conference);
        advanceCursor(replay.cursor);
        return;
      }
      for (const event of replay.events) {
        advanceCursor(event.sequence);
        callbacks.current.onEvent?.(event);
      }
      advanceCursor(replay.cursor);
    } catch (error) {
      callbacks.current.onError?.(error);
    }
  }, [advanceCursor, conferenceId]);

  const connect = useCallback(async () => {
    if (!enabled || !conferenceId) return;
    const generation = generationRef.current;
    try {
      const ticketData = await requestTicket();
      if (generation !== generationRef.current) return;

      if (!hasCursorRef.current) {
        advanceCursor(ticketData.cursor);
      }
      const ws = realtimeService.createWebSocket(
        ticketData.websocketPath,
        ticketData.ticket,
        cursorRef.current,
      );
      wsRef.current = ws;
      setReadyState(CONNECTING);

      ws.onopen = () => {
        if (generation !== generationRef.current) {
          ws.close();
          return;
        }
        setReadyState(OPEN);
        setIsReconnecting(false);
        reconnectAttemptsRef.current = 0;
        setReconnectAttempts(0);
        callbacks.current.onOpen?.();
      };

      ws.onmessage = (message) => {
        try {
          const frame = JSON.parse(message.data) as RealtimeFrame;
          if (frame.type === "event" && frame.event) {
            if (!frame.transient && Number.isFinite(frame.event.sequence)) {
              advanceCursor(frame.event.sequence);
            }
            callbacks.current.onEvent?.(frame.event);
          } else if (frame.type === "resync_required") {
            void recover();
          } else if (frame.type === "revoked") {
            ws.close(4403, "revoked");
          } else if (frame.type === "closed") {
            ws.close(1000, frame.reason || "conference closed");
          }
        } catch (error) {
          console.error("Failed to parse realtime frame:", error);
          callbacks.current.onError?.(error);
        }
      };

      ws.onclose = (event) => {
        if (generation !== generationRef.current) return;
        wsRef.current = null;
        setReadyState(CLOSED);
        callbacks.current.onClose?.(event);
        if (!event.wasClean && event.code !== 4403) scheduleReconnect();
        else setIsReconnecting(false);
      };

      ws.onerror = (error) => callbacks.current.onError?.(error);
    } catch (error) {
      if (generation !== generationRef.current) return;
      callbacks.current.onError?.(error);
      scheduleReconnect();
    }
  }, [advanceCursor, conferenceId, enabled, recover, requestTicket, scheduleReconnect]);

  useEffect(() => {
    connectRef.current = () => void connect();
  }, [connect]);

  const disconnect = useCallback(() => {
    generationRef.current += 1;
    if (reconnectTimeoutRef.current) {
      clearTimeout(reconnectTimeoutRef.current);
      reconnectTimeoutRef.current = null;
    }
    wsRef.current?.close();
    wsRef.current = null;
    setReadyState(CLOSED);
    setIsReconnecting(false);
    reconnectAttemptsRef.current = 0;
    setReconnectAttempts(0);
  }, []);

  const reconnect = useCallback(() => {
    disconnect();
    if (enabled) {
      connectRef.current();
    }
  }, [disconnect, enabled]);

  useEffect(() => {
    if (enabled) connectRef.current();
    return disconnect;
  }, [enabled, conferenceId, connect, disconnect]);

  return {
    readyState,
    isConnected: readyState === OPEN,
    isReconnecting,
    reconnectAttempts,
    cursor,
    disconnect,
    reconnect,
  };
}

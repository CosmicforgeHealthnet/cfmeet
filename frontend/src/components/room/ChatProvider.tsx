"use client";

import { useState, useEffect, useMemo, type ReactNode } from "react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { useRoomContext } from "@livekit/components-react";
import { RoomEvent } from "livekit-client";
import { meetingService } from "@/services/meeting.service";
import { ChatContext } from "@/context/ChatContext";
import type { ChatMessage } from "@/types/meeting";

interface ChatProviderProps {
  children: ReactNode;
  meetingId: string;
  participantId: string;
}

export function ChatProvider({ children, meetingId, participantId }: ChatProviderProps) {
  const room = useRoomContext();
  const [realtimeMessages, setRealtimeMessages] = useState<ChatMessage[]>([]);

  // Load history from backend
  const { data: historyData, isLoading } = useQuery({
    queryKey: ["chat", meetingId],
    queryFn: () => meetingService.getChatMessages(meetingId),
    refetchOnWindowFocus: false,
  });

  // Listen for incoming realtime messages at room level (always active)
  useEffect(() => {
    if (!room) return;

    const handleDataReceived = (
      payload: Uint8Array,
      _participant?: unknown,
      _kind?: unknown,
      _topic?: string,
    ) => {
      try {
        const strData = new TextDecoder().decode(payload);
        const data = JSON.parse(strData);

        if (data.type === "chat_message") {
          setRealtimeMessages((prev) => {
            // Dedupe by id
            if (prev.some((m) => m.id === data.id)) return prev;
            return [...prev, data as ChatMessage];
          });
        }
      } catch {
        // Ignore non-chat data packets
      }
    };

    room.on(RoomEvent.DataReceived, handleDataReceived);
    return () => {
      room.off(RoomEvent.DataReceived, handleDataReceived);
    };
  }, [room]);

  // Send message mutation
  const sendMessage = useMutation({
    mutationFn: (message: string) =>
      meetingService.sendChatMessage(meetingId, participantId, message),
    onSuccess: async (response) => {
      if (response.success && response.data) {
        const messageData = response.data;

        // Add to local realtime state immediately
        setRealtimeMessages((prev) => {
          if (prev.some((m) => m.id === messageData.id)) return prev;
          return [...prev, messageData];
        });

        // Broadcast to others via LiveKit
        if (room) {
          const payload = JSON.stringify({
            type: "chat_message",
            ...messageData,
          });
          const encoder = new TextEncoder();
          await room.localParticipant.publishData(encoder.encode(payload), {
            reliable: true,
          });
        }
      }
    },
  });

  // Merge history and realtime messages, deduping by ID
  const messages = useMemo(() => {
    const history = historyData?.data || [];
    const combined = [...history, ...realtimeMessages];

    // Unique by ID
    const uniqueMap = new Map();
    combined.forEach((m) => uniqueMap.set(m.id, m));

    return Array.from(uniqueMap.values()).sort(
      (a: ChatMessage, b: ChatMessage) =>
        new Date(a.created_at).getTime() - new Date(b.created_at).getTime(),
    );
  }, [historyData, realtimeMessages]);

  return (
    <ChatContext.Provider value={{ messages, isLoading, sendMessage }}>
      {children}
    </ChatContext.Provider>
  );
}

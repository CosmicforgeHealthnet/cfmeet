import { createContext, useContext, useState, useCallback, useMemo } from "react";
import type { JoinMeetingData, Participant, ChatMessage, WaitingParticipant } from "@/features/meetings/types";
import type { RoomContextType } from "../types";

const RoomContext = createContext<RoomContextType | undefined>(undefined);

interface RoomProviderProps {
  children: React.ReactNode;
  initialJoinData?: JoinMeetingData;
}

export function RoomProvider({ children, initialJoinData }: RoomProviderProps) {
  const [joinData, setJoinData] = useState<JoinMeetingData | null>(initialJoinData ?? null);
  const [participants, setParticipants] = useState<Participant[]>([]);
  const [waitingParticipants, setWaitingParticipants] = useState<WaitingParticipant[]>([]);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [isAudioEnabled, setIsAudioEnabled] = useState(true);
  const [isVideoEnabled, setIsVideoEnabled] = useState(true);
  const [isScreenSharing, setIsScreenSharing] = useState(false);
  const [isSidebarOpen, setIsSidebarOpen] = useState(false);
  const [activeSidebarTab, setActiveSidebarTab] = useState<"participants" | "chat">("participants");

  const addMessage = useCallback((message: ChatMessage) => {
    setMessages((prev) => {
      if (prev.some((m) => m.id === message.id)) {
        return prev;
      }
      return [...prev, message];
    });
  }, []);

  const toggleAudio = useCallback(() => {
    setIsAudioEnabled((prev) => !prev);
  }, []);

  const toggleVideo = useCallback(() => {
    setIsVideoEnabled((prev) => !prev);
  }, []);

  const toggleScreenShare = useCallback(() => {
    setIsScreenSharing((prev) => !prev);
  }, []);

  const toggleSidebar = useCallback(() => {
    setIsSidebarOpen((prev) => !prev);
  }, []);

  const setSidebarTab = useCallback((tab: "participants" | "chat") => {
    setActiveSidebarTab(tab);
    if (!isSidebarOpen) {
      setIsSidebarOpen(true);
    }
  }, [isSidebarOpen]);

  const value = useMemo<RoomContextType>(
    () => ({
      joinData,
      participants,
      waitingParticipants,
      messages,
      isAudioEnabled,
      isVideoEnabled,
      isScreenSharing,
      isSidebarOpen,
      activeSidebarTab,
      setJoinData,
      setParticipants,
      setWaitingParticipants,
      addMessage,
      setMessages,
      toggleAudio,
      toggleVideo,
      toggleScreenShare,
      toggleSidebar,
      setSidebarTab,
    }),
    [
      joinData,
      participants,
      waitingParticipants,
      messages,
      isAudioEnabled,
      isVideoEnabled,
      isScreenSharing,
      isSidebarOpen,
      activeSidebarTab,
      addMessage,
      toggleAudio,
      toggleVideo,
      toggleScreenShare,
      toggleSidebar,
      setSidebarTab,
    ]
  );

  return <RoomContext.Provider value={value}>{children}</RoomContext.Provider>;
}

export function useRoom(): RoomContextType {
  const context = useContext(RoomContext);
  if (!context) {
    throw new Error("useRoom must be used within a RoomProvider");
  }
  return context;
}

import type { JoinMeetingData, Participant, ChatMessage, WaitingParticipant } from "@/features/meetings/types";

export interface RoomState {
  joinData: JoinMeetingData | null;
  participants: Participant[];
  waitingParticipants: WaitingParticipant[];
  messages: ChatMessage[];
  isAudioEnabled: boolean;
  isVideoEnabled: boolean;
  isScreenSharing: boolean;
  isSidebarOpen: boolean;
  activeSidebarTab: "participants" | "chat";
}

export interface RoomContextType extends RoomState {
  setJoinData: (data: JoinMeetingData | null) => void;
  setParticipants: (participants: Participant[]) => void;
  setWaitingParticipants: (participants: WaitingParticipant[]) => void;
  addMessage: (message: ChatMessage) => void;
  setMessages: (messages: ChatMessage[]) => void;
  toggleAudio: () => void;
  toggleVideo: () => void;
  toggleScreenShare: () => void;
  toggleSidebar: () => void;
  setSidebarTab: (tab: "participants" | "chat") => void;
}

export interface PreJoinState {
  displayName: string;
  isAudioEnabled: boolean;
  isVideoEnabled: boolean;
}

export interface LocalMediaState {
  hasPermissions: boolean;
  isCameraReady: boolean;
  isMicReady: boolean;
}

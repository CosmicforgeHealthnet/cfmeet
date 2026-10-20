"use client";

import { useState } from "react";
import {
  Mic,
  MicOff,
  Video,
  Send,
  Check,
  X,
  Clock,
  Crown,
  Shield,
  Presentation,
  Eye,
} from "lucide-react";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import {
  useParticipantsV2,
  useWaitingRoomV2,
  useAdmitParticipants,
  useDenyParticipants,
  useMessages,
  useSendMessage,
} from "@/hooks/v2";
import type { ParticipantResponse, MessageView, Role } from "@/types/v2";

interface SidebarProps {
  conferenceId?: string;
  meetingId?: string; // V1 compatibility
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

export default function Sidebar({
  conferenceId,
  meetingId,
  isHost,
  participantId,
  onClose,
}: SidebarProps) {
  // Use conferenceId for V2, fallback to meetingId for V1 compatibility
  const roomId = conferenceId || meetingId || "";
  const [newMessage, setNewMessage] = useState("");

  // V2 Chat hooks
  const { data: messagesData, isLoading: isChatLoading } = useMessages(roomId);
  const sendMessage = useSendMessage(roomId);
  const messages = messagesData?.messages || [];

  const handleSendMessage = () => {
    if (!newMessage.trim()) return;
    sendMessage.mutate({ content: newMessage.trim() });
    setNewMessage("");
  };

  // V2 Participants hooks - returns ParticipantResponse[] directly
  const { data: participants = [] } = useParticipantsV2(roomId);

  // V2 Waiting room (only for host) - returns WaitingRoomResponse with .participants
  const { data: waitingData } = useWaitingRoomV2(roomId, isHost);
  const waitingParticipants = waitingData?.participants || [];

  return (
    <div className="flex flex-col h-full px-6 py-6 bg-card rounded-3xl shadow-sm">
      {/* Fixed Header with Close Button */}
      <div className="flex items-center justify-between mb-3 shrink-0">
        <h3 className="text-sm font-semibold text-foreground">
          Participants{" "}
          <span className="text-muted-foreground">({participants.length})</span>
        </h3>
        <button
          onClick={onClose}
          className="p-1.5 rounded-full hover:bg-muted transition-colors"
        >
          <X className="w-4 h-4 text-foreground" />
        </button>
      </div>

      {/* Scrollable Participants & Waiting Room Section */}
      <div className="shrink-0 max-h-[35%] overflow-y-auto overflow-x-hidden mb-4">
        {/* Participants List */}
        <div>
          <div className="space-y-3 py-2">
            {participants.length === 0 ? (
              <p className="text-sm text-muted-foreground text-center py-2">
                No participants yet
              </p>
            ) : (
              participants.map((participant) => (
                <ParticipantItem
                  key={participant.id}
                  participant={participant}
                  isCurrentUser={participant.id === participantId}
                />
              ))
            )}
          </div>
        </div>

        {/* Waiting Room - Only visible to host */}
        {isHost && (
          <WaitingRoom
            roomId={roomId}
            waitingParticipants={waitingParticipants}
          />
        )}
      </div>

      {/* Chat Room - Static container with scrollable messages */}
      <div className="flex-1 min-h-0 flex flex-col rounded-xl shadow-[0_8px_24px_rgba(2,156,212,0.08)] bg-card overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 bg-card rounded-t-2xl border-b shrink-0">
          <span className="text-sm font-medium text-muted-foreground">Chat Room</span>
        </div>

        {/* Messages */}
        <div className="flex-1 px-4 py-3 space-y-3 overflow-y-auto bg-muted/50">
          {isChatLoading ? (
            <div className="text-center text-xs text-muted-foreground py-4">
              Loading chat...
            </div>
          ) : messages.length === 0 ? (
            <div className="text-center text-xs text-muted-foreground italic mt-4">
              No messages yet
            </div>
          ) : (
            messages.map((msg: MessageView) => (
              <div key={msg.id} className="flex flex-col gap-1">
                <div
                  className={`flex flex-col ${
                    msg.senderId === participantId
                      ? "items-end"
                      : "items-start"
                  }`}
                >
                  <span className="text-[10px] text-muted-foreground font-bold px-1 mb-0.5">
                    {msg.senderDisplayName || "User"}
                  </span>
                  <div
                    className={`p-2.5 rounded-2xl text-sm max-w-[90%] break-words shadow-sm ${
                      msg.senderId === participantId
                        ? "bg-primary/10 text-primary rounded-tr-none"
                        : "bg-card border border-border text-foreground rounded-tl-none"
                    }`}
                  >
                    {msg.content}
                  </div>
                </div>
              </div>
            ))
          )}
        </div>

        {/* Input - Fixed at bottom */}
        <div className="px-3 py-3 bg-card rounded-b-2xl border-t shrink-0">
          <div className="flex items-center gap-2">
            <div className="relative flex-1">
              <input
                placeholder="Type here..."
                className="w-full h-9 bg-muted rounded-full text-sm px-4 text-foreground focus:outline-none focus:ring-1 focus:ring-[var(--primary)]"
                value={newMessage}
                onChange={(e) => setNewMessage(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleSendMessage()}
              />
            </div>
            <button
              className="w-9 h-9 rounded-full flex items-center justify-center text-white bg-primary hover:bg-primary/80 active:scale-95 transition disabled:opacity-50 disabled:cursor-not-allowed"
              onClick={handleSendMessage}
              disabled={!newMessage.trim() || sendMessage.isPending}
            >
              <Send className="w-4 h-4 ml-0.5" />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

interface WaitingRoomProps {
  roomId: string;
  waitingParticipants: ParticipantResponse[];
}

function WaitingRoom({ roomId, waitingParticipants }: WaitingRoomProps) {
  const admitMutation = useAdmitParticipants(roomId);
  const denyMutation = useDenyParticipants(roomId);

  const handleAdmit = (participantId: string) => {
    admitMutation.mutate({ participantIds: [participantId] });
  };

  const handleDeny = (participantId: string) => {
    denyMutation.mutate({ participantIds: [participantId] });
  };

  const handleAdmitAll = () => {
    admitMutation.mutate({ all: true });
  };

  if (waitingParticipants.length === 0) {
    return null;
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <h3 className="text-sm font-semibold text-foreground flex items-center gap-1.5">
          <Clock className="w-3.5 h-3.5 text-primary" />
          Waiting Room{" "}
          <span className="text-muted-foreground">
            ({waitingParticipants.length})
          </span>
        </h3>
        <button
          className="text-xs text-primary font-medium hover:underline disabled:opacity-50"
          onClick={handleAdmitAll}
          disabled={admitMutation.isPending}
        >
          Admit All
        </button>
      </div>

      <div className="space-y-2 py-2">
        {waitingParticipants.map((participant) => (
          <WaitingRoomItem
            key={participant.id}
            participant={participant}
            onAdmit={handleAdmit}
            onDeny={handleDeny}
            isAdmitting={admitMutation.isPending}
            isDenying={denyMutation.isPending}
          />
        ))}
      </div>
    </div>
  );
}

interface WaitingRoomItemProps {
  participant: ParticipantResponse;
  onAdmit: (id: string) => void;
  onDeny: (id: string) => void;
  isAdmitting: boolean;
  isDenying: boolean;
}

function WaitingRoomItem({
  participant,
  onAdmit,
  onDeny,
  isAdmitting,
  isDenying,
}: WaitingRoomItemProps) {
  return (
    <div className="flex items-center justify-between">
      <div className="flex items-center gap-2">
        <Avatar className="h-8 w-8">
          <AvatarImage
            src={`https://api.dicebear.com/7.x/avataaars/svg?seed=${participant.displayName}`}
          />
          <AvatarFallback>{participant.displayName[0]}</AvatarFallback>
        </Avatar>
        <span className="text-sm font-medium text-muted-foreground">
          {participant.displayName.length > 4
            ? `${participant.displayName.slice(0, 4)}...`
            : participant.displayName}
        </span>
      </div>

      <div className="flex gap-1.5 items-center">
        <button
          className="w-7 h-7 rounded-full flex items-center justify-center bg-primary hover:bg-primary/80 text-white transition disabled:opacity-50"
          onClick={() => onAdmit(participant.id)}
          disabled={isAdmitting || isDenying}
          title="Admit"
        >
          <Check className="w-3.5 h-3.5" />
        </button>
        <button
          className="w-7 h-7 rounded-full flex items-center justify-center bg-muted hover:bg-red-100 text-muted-foreground hover:text-red-500 transition disabled:opacity-50"
          onClick={() => onDeny(participant.id)}
          disabled={isAdmitting || isDenying}
          title="Deny"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  );
}

// Role badge component
function RoleBadge({ role }: { role: Role }) {
  const config: Record<Role, { icon: React.ReactNode; label: string; className: string }> = {
    HOST: {
      icon: <Crown className="w-3 h-3" />,
      label: "Host",
      className: "text-primary",
    },
    CO_HOST: {
      icon: <Shield className="w-3 h-3" />,
      label: "Co-Host",
      className: "text-blue-500",
    },
    PRESENTER: {
      icon: <Presentation className="w-3 h-3" />,
      label: "Presenter",
      className: "text-green-500",
    },
    PARTICIPANT: {
      icon: null,
      label: "",
      className: "",
    },
    VIEWER: {
      icon: <Eye className="w-3 h-3" />,
      label: "Viewer",
      className: "text-muted-foreground",
    },
  };

  const { icon, label, className } = config[role] || config.PARTICIPANT;
  if (!label) return null;

  return (
    <span className={`flex items-center gap-1 text-xs font-medium ${className}`}>
      {icon}
      {label}
    </span>
  );
}

interface ParticipantItemProps {
  participant: ParticipantResponse;
  isCurrentUser: boolean;
}

function ParticipantItem({ participant, isCurrentUser }: ParticipantItemProps) {
  return (
    <div className="flex items-center justify-between">
      <div className="flex items-center gap-2">
        <Avatar className="h-8 w-8">
          <AvatarImage
            src={`https://api.dicebear.com/7.x/avataaars/svg?seed=${participant.displayName}`}
          />
          <AvatarFallback>{participant.displayName[0]}</AvatarFallback>
        </Avatar>

        <div className="flex flex-col">
          <span className="text-sm font-medium text-foreground">
            {participant.displayName.length > 8
              ? `${participant.displayName.slice(0, 8)}...`
              : participant.displayName}
            {isCurrentUser && " (You)"}
          </span>
          <RoleBadge role={participant.role} />
        </div>
      </div>

      <div className="flex gap-2 items-center">
        <Mic className="w-4 h-4 text-primary" />
        <Video className="w-4 h-4 text-primary" />
      </div>
    </div>
  );
}

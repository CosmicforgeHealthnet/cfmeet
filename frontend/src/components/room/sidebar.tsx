"use client";

import { useState } from "react";
import { Mic, MicOff, Video, Send, Check, X, Clock } from "lucide-react";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import {
  useWaitingParticipants,
  useAdmitParticipant,
  useDenyParticipant,
  useParticipants,
  useChat,
} from "@/hooks";
import type {
  WaitingParticipant,
  Participant,
  ChatMessage,
} from "@/types/meeting";

interface SidebarProps {
  meetingId: string;
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

export default function Sidebar({
  meetingId,
  isHost,
  participantId,
  onClose,
}: SidebarProps) {
  const [newMessage, setNewMessage] = useState("");
  const {
    messages,
    isLoading: isChatLoading,
    sendMessage,
  } = useChat(meetingId, participantId);

  const handleSendMessage = () => {
    if (!newMessage.trim()) return;
    sendMessage.mutate(newMessage);
    setNewMessage("");
  };

  // Fetch participants
  const { data: participantsData } = useParticipants(meetingId);
  const participants = participantsData?.data || [];

  // Fetch waiting room (only for host)
  const { data: waitingData } = useWaitingParticipants(meetingId, isHost);
  const waitingParticipants = waitingData?.data || [];

  return (
    <div className="flex flex-col h-full px-6 py-6 bg-white rounded-3xl shadow-sm">
      {/* Fixed Header with Close Button */}
      <div className="flex items-center justify-between mb-3 shrink-0">
        <h3 className="text-sm font-semibold text-[#343434]">
          Participants{" "}
          <span className="text-[#00000080]">({participants.length})</span>
        </h3>
        <button
          onClick={onClose}
          className="p-1.5 rounded-full hover:bg-gray-100 transition-colors"
        >
          <X className="w-4 h-4 text-[#0F0F0F]" />
        </button>
      </div>

      {/* Scrollable Participants & Waiting Room Section */}
      <div className="shrink-0 max-h-[35%] overflow-y-auto overflow-x-hidden mb-4">
        {/* Participants List */}
        <div>
          <div className="space-y-3 py-2">
            {participants.length === 0 ? (
              <p className="text-sm text-gray-400 text-center py-2">
                No participants yet
              </p>
            ) : (
              participants.map((participant) => (
                <ParticipantItem
                  key={participant.participant_id}
                  participant={participant}
                  isCurrentUser={participant.participant_id === participantId}
                />
              ))
            )}
          </div>
        </div>

        {/* Waiting Room - Only visible to host */}
        {isHost && (
          <WaitingRoom
            meetingId={meetingId}
            waitingParticipants={waitingParticipants}
          />
        )}
      </div>

      {/* Chat Room - Static container with scrollable messages */}
      <div className="flex-1 min-h-0 flex flex-col rounded-xl shadow-[0_8px_24px_rgba(2,156,212,0.08)] bg-[#FFFFFF] overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 bg-white rounded-t-2xl border-b shrink-0">
          <span className="text-sm font-medium text-gray-600">Chat Room</span>
        </div>

        {/* Messages */}
        <div className="flex-1 px-4 py-3 space-y-3 overflow-y-auto bg-gray-50/50">
          {isChatLoading ? (
            <div className="text-center text-xs text-gray-400 py-4">
              Loading chat...
            </div>
          ) : messages.length === 0 ? (
            <div className="text-center text-xs text-gray-400 italic mt-4">
              No messages yet
            </div>
          ) : (
            messages.map((msg: ChatMessage, i: number) => (
              <div key={i} className="flex flex-col gap-1">
                <div
                  className={`flex flex-col ${
                    msg.participant_id === participantId
                      ? "items-end"
                      : "items-start"
                  }`}
                >
                  <span className="text-[10px] text-gray-500 font-bold px-1 mb-0.5">
                    {msg.display_name || "User"}
                  </span>
                  <div
                    className={`p-2.5 rounded-2xl text-sm max-w-[90%] break-words shadow-sm ${
                      msg.participant_id === participantId
                        ? "bg-[#E6F5FA] text-[#029CD4] rounded-tr-none"
                        : "bg-white border border-gray-100 text-gray-700 rounded-tl-none"
                    }`}
                  >
                    {msg.message}
                  </div>
                </div>
              </div>
            ))
          )}
        </div>

        {/* Input - Fixed at bottom */}
        <div className="px-3 py-3 bg-white rounded-b-2xl border-t shrink-0">
          <div className="flex items-center gap-2">
            <div className="relative flex-1">
              <input
                placeholder="Type here..."
                className="w-full h-9 bg-gray-100 rounded-full text-sm px-4 text-black focus:outline-none focus:ring-1 focus:ring-[#029CD4]"
                value={newMessage}
                onChange={(e) => setNewMessage(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleSendMessage()}
              />
            </div>
            <button
              className="w-9 h-9 rounded-full flex items-center justify-center text-white bg-[#029CD4] hover:bg-[#028AC0] active:scale-95 transition disabled:opacity-50 disabled:cursor-not-allowed"
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
  meetingId: string;
  waitingParticipants: WaitingParticipant[];
}

function WaitingRoom({ meetingId, waitingParticipants }: WaitingRoomProps) {
  const admitMutation = useAdmitParticipant();
  const denyMutation = useDenyParticipant();

  const handleAdmit = (participantId: string) => {
    admitMutation.mutate({ meetingId, participantId });
  };

  const handleDeny = (participantId: string) => {
    denyMutation.mutate({ meetingId, participantId });
  };

  const handleAdmitAll = () => {
    waitingParticipants.forEach((p) => {
      admitMutation.mutate({ meetingId, participantId: p.participant_id });
    });
  };

  if (waitingParticipants.length === 0) {
    return null;
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-2">
        <h3 className="text-sm font-semibold text-[#343434] flex items-center gap-1.5">
          <Clock className="w-3.5 h-3.5 text-[#029CD4]" />
          Waiting Room{" "}
          <span className="text-[#00000080]">
            ({waitingParticipants.length})
          </span>
        </h3>
        <button
          className="text-xs text-[#029CD4] font-medium hover:underline disabled:opacity-50"
          onClick={handleAdmitAll}
          disabled={admitMutation.isPending}
        >
          Admit All
        </button>
      </div>

      <div className="space-y-2 py-2">
        {waitingParticipants.map((participant) => (
          <WaitingRoomItem
            key={participant.participant_id}
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
  participant: WaitingParticipant;
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
            src={`https://api.dicebear.com/7.x/avataaars/svg?seed=${participant.display_name}`}
          />
          <AvatarFallback>{participant.display_name[0]}</AvatarFallback>
        </Avatar>
        <span className="text-sm font-medium text-[#00000080]">
          {participant.display_name.length > 4
            ? `${participant.display_name.slice(0, 4)}...`
            : participant.display_name}
        </span>
      </div>

      <div className="flex gap-1.5 items-center">
        <button
          className="w-7 h-7 rounded-full flex items-center justify-center bg-[#029CD4] hover:bg-[#028AC0] text-white transition disabled:opacity-50"
          onClick={() => onAdmit(participant.participant_id)}
          disabled={isAdmitting || isDenying}
          title="Admit"
        >
          <Check className="w-3.5 h-3.5" />
        </button>
        <button
          className="w-7 h-7 rounded-full flex items-center justify-center bg-gray-100 hover:bg-red-100 text-gray-400 hover:text-red-500 transition disabled:opacity-50"
          onClick={() => onDeny(participant.participant_id)}
          disabled={isAdmitting || isDenying}
          title="Deny"
        >
          <X className="w-3.5 h-3.5" />
        </button>
      </div>
    </div>
  );
}

interface ParticipantItemProps {
  participant: Participant;
  isCurrentUser: boolean;
}

function ParticipantItem({ participant, isCurrentUser }: ParticipantItemProps) {
  const isHost = participant.role === "host";

  return (
    <div className="flex items-center justify-between">
      <div className="flex items-center gap-2">
        <Avatar className="h-8 w-8">
          <AvatarImage
            src={`https://api.dicebear.com/7.x/avataaars/svg?seed=${participant.display_name}`}
          />
          <AvatarFallback>{participant.display_name[0]}</AvatarFallback>
        </Avatar>

        <span className="text-sm font-medium text-[#00000080]">
          {participant.display_name.length > 5
            ? `${participant.display_name.slice(0, 5)}...`
            : participant.display_name}
          {isCurrentUser && " (You)"}
        </span>
      </div>

      <div className="flex gap-2 items-center">
        {isHost && <span className="text-base text-[#029CD4] ml-1">Host</span>}
        {participant.is_muted ? (
          <MicOff className="w-4 h-4 text-[#DCDFE3]" />
        ) : (
          <Mic className="w-4 h-4 text-[#029CD4]" />
        )}
        {participant.is_video_on ? (
          <Video className="w-4 h-4 text-[#029CD4]" />
        ) : (
          <Video className="w-4 h-4 text-[#DCDFE3]" />
        )}
      </div>
    </div>
  );
}

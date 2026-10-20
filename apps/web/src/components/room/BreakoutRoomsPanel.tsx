"use client";

import { useState } from "react";
import {
  Users,
  Plus,
  Play,
  Square,
  Megaphone,
  ArrowRight,
  X,
  Shuffle,
  Loader2,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import {
  useBreakouts,
  useCreateBreakouts,
  useAssignBreakouts,
  useOpenBreakout,
  useCloseAllBreakouts,
  useAnnounceAll,
  useEnterBreakout,
  useLeaveBreakout,
} from "@/hooks/v2";
import type { BreakoutView } from "@/types/v2";

interface BreakoutRoomsPanelProps {
  conferenceId: string;
  isHost: boolean;
  onClose: () => void;
}

export default function BreakoutRoomsPanel({
  conferenceId,
  isHost,
  onClose,
}: BreakoutRoomsPanelProps) {
  const { data, isLoading } = useBreakouts(conferenceId);
  const breakouts = data?.breakouts || [];
  const myContext = data?.myContext;

  const [showCreateModal, setShowCreateModal] = useState(false);
  const [showAnnounceModal, setShowAnnounceModal] = useState(false);

  const closeAll = useCloseAllBreakouts(conferenceId);
  const enterBreakout = useEnterBreakout(conferenceId);
  const leaveBreakout = useLeaveBreakout(conferenceId);

  const hasOpenRooms = breakouts.some((b) => b.status === "OPEN");
  const isInBreakout = myContext?.roomType === "BREAKOUT";

  const handleCloseAll = () => {
    closeAll.mutate({ countdownSeconds: 30 });
  };

  const handleEnterRoom = (breakoutId: string) => {
    enterBreakout.mutate(breakoutId);
  };

  const handleLeaveRoom = () => {
    leaveBreakout.mutate();
  };

  return (
    <div className="flex flex-col h-full bg-card rounded-3xl shadow-sm">
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-4 border-b">
        <h3 className="text-lg font-semibold text-foreground flex items-center gap-2">
          <Users className="w-5 h-5 text-primary" />
          Breakout Rooms
        </h3>
        <button
          onClick={onClose}
          className="p-1.5 rounded-full hover:bg-muted transition-colors"
        >
          <X className="w-4 h-4 text-foreground" />
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-6 py-4">
        {isLoading ? (
          <div className="flex items-center justify-center py-10">
            <Loader2 className="w-6 h-6 text-primary animate-spin" />
          </div>
        ) : breakouts.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-10 text-center">
            <Users className="w-12 h-12 text-muted-foreground mb-3" />
            <p className="text-foreground font-medium">No breakout rooms</p>
            <p className="text-sm text-muted-foreground mt-1">
              {isHost
                ? "Create breakout rooms to split participants into smaller groups."
                : "The host hasn't created any breakout rooms yet."}
            </p>
            {isHost && (
              <Button
                className="mt-4"
                onClick={() => setShowCreateModal(true)}
              >
                <Plus className="w-4 h-4 mr-2" />
                Create Rooms
              </Button>
            )}
          </div>
        ) : (
          <div className="space-y-4">
            {/* Current room indicator */}
            {isInBreakout && myContext?.breakoutName && (
              <div className="bg-primary/10 rounded-xl p-4 flex items-center justify-between">
                <div>
                  <p className="text-sm text-muted-foreground">You are in:</p>
                  <p className="font-semibold text-primary">
                    {myContext.breakoutName}
                  </p>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={handleLeaveRoom}
                  disabled={leaveBreakout.isPending}
                >
                  Leave Room
                </Button>
              </div>
            )}

            {/* Room list */}
            {breakouts.map((room) => (
              <BreakoutRoomCard
                key={room.id}
                room={room}
                isHost={isHost}
                isCurrentRoom={myContext?.breakoutId === room.id}
                onEnter={() => handleEnterRoom(room.id)}
                conferenceId={conferenceId}
              />
            ))}
          </div>
        )}
      </div>

      {/* Footer Actions (Host only) */}
      {isHost && breakouts.length > 0 && (
        <div className="px-6 py-4 border-t space-y-3">
          <div className="flex gap-2">
            {!hasOpenRooms && (
              <Button
                variant="outline"
                size="sm"
                className="flex-1"
                onClick={() => setShowCreateModal(true)}
              >
                <Plus className="w-4 h-4 mr-1" />
                Add Rooms
              </Button>
            )}
            <Button
              variant="outline"
              size="sm"
              className="flex-1"
              onClick={() => setShowAnnounceModal(true)}
            >
              <Megaphone className="w-4 h-4 mr-1" />
              Announce
            </Button>
          </div>
          {hasOpenRooms && (
            <Button
              variant="destructive"
              size="sm"
              className="w-full"
              onClick={handleCloseAll}
              disabled={closeAll.isPending}
            >
              <Square className="w-4 h-4 mr-2" />
              Close All Rooms
            </Button>
          )}
        </div>
      )}

      {/* Create Modal */}
      {showCreateModal && (
        <CreateBreakoutsModal
          conferenceId={conferenceId}
          onClose={() => setShowCreateModal(false)}
        />
      )}

      {/* Announce Modal */}
      {showAnnounceModal && (
        <AnnounceModal
          conferenceId={conferenceId}
          onClose={() => setShowAnnounceModal(false)}
        />
      )}
    </div>
  );
}

// ─── Room Card ────────────────────────────────────────────────────────────────

interface BreakoutRoomCardProps {
  room: BreakoutView;
  isHost: boolean;
  isCurrentRoom: boolean;
  onEnter: () => void;
  conferenceId: string;
}

function BreakoutRoomCard({
  room,
  isHost,
  isCurrentRoom,
  onEnter,
  conferenceId,
}: BreakoutRoomCardProps) {
  const openBreakout = useOpenBreakout(conferenceId);

  const statusColors: Record<string, string> = {
    PENDING: "bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400",
    OPEN: "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400",
    CLOSING: "bg-orange-100 text-orange-700 dark:bg-orange-900/30 dark:text-orange-400",
    CLOSED: "bg-muted text-muted-foreground",
  };

  const handleOpen = () => {
    openBreakout.mutate(room.id);
  };

  return (
    <div
      className={`rounded-xl border p-4 transition-all ${
        isCurrentRoom
          ? "border-primary bg-primary/5"
          : "border-border hover:border-primary/50"
      }`}
    >
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-2">
          <h4 className="font-semibold text-foreground">{room.name}</h4>
          <span
            className={`text-[10px] px-1.5 py-0.5 rounded-full font-medium ${statusColors[room.status]}`}
          >
            {room.status}
          </span>
        </div>
        <span className="text-sm text-muted-foreground flex items-center gap-1">
          <Users className="w-3.5 h-3.5" />
          {room.participantCount}
        </span>
      </div>

      {/* Assigned participants preview */}
      {room.assignedParticipants && room.assignedParticipants.length > 0 && (
        <div className="flex -space-x-2 mb-3">
          {room.assignedParticipants.slice(0, 5).map((id, i) => (
            <Avatar key={id} className="w-7 h-7 border-2 border-card">
              <AvatarImage src={`https://api.dicebear.com/7.x/avataaars/svg?seed=${id}`} />
              <AvatarFallback>{i + 1}</AvatarFallback>
            </Avatar>
          ))}
          {room.assignedParticipants.length > 5 && (
            <div className="w-7 h-7 rounded-full bg-muted flex items-center justify-center text-xs text-muted-foreground border-2 border-card">
              +{room.assignedParticipants.length - 5}
            </div>
          )}
        </div>
      )}

      {/* Actions */}
      <div className="flex gap-2">
        {isHost && room.status === "PENDING" && (
          <Button
            size="sm"
            variant="outline"
            className="flex-1"
            onClick={handleOpen}
            disabled={openBreakout.isPending}
          >
            <Play className="w-3.5 h-3.5 mr-1" />
            Open
          </Button>
        )}
        {room.status === "OPEN" && !isCurrentRoom && (
          <Button size="sm" className="flex-1" onClick={onEnter}>
            <ArrowRight className="w-3.5 h-3.5 mr-1" />
            Join
          </Button>
        )}
        {isCurrentRoom && (
          <span className="text-sm text-primary font-medium">
            You are here
          </span>
        )}
      </div>
    </div>
  );
}

// ─── Create Modal ─────────────────────────────────────────────────────────────

interface CreateBreakoutsModalProps {
  conferenceId: string;
  onClose: () => void;
}

function CreateBreakoutsModal({ conferenceId, onClose }: CreateBreakoutsModalProps) {
  const [roomCount, setRoomCount] = useState(3);
  const [autoOpen, setAutoOpen] = useState(false);
  const createBreakouts = useCreateBreakouts(conferenceId);
  const assignBreakouts = useAssignBreakouts(conferenceId);

  const handleCreate = () => {
    createBreakouts.mutate(
      { count: roomCount, autoOpen },
      {
        onSuccess: () => {
          onClose();
        },
      }
    );
  };

  const handleRandomAssign = () => {
    assignBreakouts.mutate(
      { mode: "RANDOM" },
      {
        onSuccess: () => {
          onClose();
        },
      }
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/40" onClick={onClose} />
      <div className="relative bg-card rounded-2xl shadow-xl w-full max-w-sm mx-4 p-6">
        <h3 className="text-lg font-bold text-foreground mb-4">
          Create Breakout Rooms
        </h3>

        <div className="space-y-4">
          <div>
            <label className="text-sm font-medium text-foreground">
              Number of Rooms
            </label>
            <Input
              type="number"
              min={2}
              max={50}
              value={roomCount}
              onChange={(e) => setRoomCount(parseInt(e.target.value) || 2)}
              className="mt-1.5"
            />
          </div>

          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={autoOpen}
              onChange={(e) => setAutoOpen(e.target.checked)}
              className="rounded border-border"
            />
            <span className="text-sm text-foreground">
              Open rooms immediately
            </span>
          </label>
        </div>

        <div className="flex gap-3 mt-6">
          <Button variant="outline" className="flex-1" onClick={onClose}>
            Cancel
          </Button>
          <Button
            className="flex-1"
            onClick={handleCreate}
            loading={createBreakouts.isPending}
          >
            Create
          </Button>
        </div>

        <div className="mt-4 pt-4 border-t">
          <Button
            variant="outline"
            size="sm"
            className="w-full"
            onClick={handleRandomAssign}
            disabled={assignBreakouts.isPending}
          >
            <Shuffle className="w-4 h-4 mr-2" />
            Randomly Assign Participants
          </Button>
        </div>
      </div>
    </div>
  );
}

// ─── Announce Modal ───────────────────────────────────────────────────────────

interface AnnounceModalProps {
  conferenceId: string;
  onClose: () => void;
}

function AnnounceModal({ conferenceId, onClose }: AnnounceModalProps) {
  const [message, setMessage] = useState("");
  const announceAll = useAnnounceAll(conferenceId);

  const handleAnnounce = () => {
    if (!message.trim()) return;
    announceAll.mutate(
      { message: message.trim() },
      {
        onSuccess: () => {
          onClose();
        },
      }
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/40" onClick={onClose} />
      <div className="relative bg-card rounded-2xl shadow-xl w-full max-w-sm mx-4 p-6">
        <div className="w-12 h-12 rounded-full bg-primary/10 flex items-center justify-center mx-auto mb-4">
          <Megaphone className="w-6 h-6 text-primary" />
        </div>
        <h3 className="text-lg font-bold text-foreground text-center mb-4">
          Announce to All Rooms
        </h3>

        <textarea
          placeholder="Type your announcement..."
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          rows={3}
          className="w-full rounded-xl border border-border bg-transparent px-4 py-3 text-sm focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary resize-none"
        />

        <div className="flex gap-3 mt-4">
          <Button variant="outline" className="flex-1" onClick={onClose}>
            Cancel
          </Button>
          <Button
            className="flex-1"
            onClick={handleAnnounce}
            disabled={!message.trim()}
            loading={announceAll.isPending}
          >
            Send
          </Button>
        </div>
      </div>
    </div>
  );
}

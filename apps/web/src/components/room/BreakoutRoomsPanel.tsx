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
  usePatchAssignments,
  useMoveParticipant,
  useAnnounceBreakout,
  useCloseBreakout,
  useParticipantsV2,
} from "@/hooks/v2";
import type { BreakoutView, ParticipantResponse } from "@/types/v2";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";

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
  const { data, isLoading, isError, error, refetch } = useBreakouts(conferenceId);
  const breakouts = data?.breakouts || [];
  const myContext = data?.myContext;
  const participants = useParticipantsV2(conferenceId);

  const [showCreateModal, setShowCreateModal] = useState(false);
  const [showAnnounceModal, setShowAnnounceModal] = useState(false);

  const closeAll = useCloseAllBreakouts(conferenceId);
  const enterBreakout = useEnterBreakout(conferenceId);
  const leaveBreakout = useLeaveBreakout(conferenceId);
  const moveParticipant = useMoveParticipant(conferenceId);
  const assignBreakouts = useAssignBreakouts(conferenceId);
  const [moveParticipantId, setMoveParticipantId] = useState("");
  const [moveTargetId, setMoveTargetId] = useState("");
  const [manualParticipantId, setManualParticipantId] = useState("");
  const [manualRoomId, setManualRoomId] = useState("");
  const [announceTargetId, setAnnounceTargetId] = useState<string>();
  const [assignmentNotice, setAssignmentNotice] = useState("");
  const [countdownSeconds, setCountdownSeconds] = useState(30);

  const hasOpenRooms = breakouts.some((b) => b.status === "OPEN");
  const isInBreakout = myContext?.kind === "BREAKOUT" || myContext?.roomType === "BREAKOUT";
  const myBreakoutName = breakouts.find((room) => room.id === myContext?.breakoutId)?.name;

  const handleCloseAll = () => {
    closeAll.mutate({ countdownSeconds });
  };
  const handleManualAssign = () => {
    if (!manualParticipantId || !manualRoomId) return;
    assignBreakouts.mutate(
      { mode: "MANUAL", assignments: [{ participantId: manualParticipantId, breakoutId: manualRoomId }] },
      { onSuccess: (result) => {
        setAssignmentNotice(`Assigned ${result.assigned.length} participant${result.assigned.length === 1 ? "" : "s"}; ${result.unassigned.length} left unassigned.`);
        setManualParticipantId("");
      } },
    );
  };

  const handleEnterRoom = (breakoutId: string) => {
    enterBreakout.mutate(breakoutId);
  };

  const handleLeaveRoom = () => {
    leaveBreakout.mutate();
  };
  const handleMoveParticipant = () => {
    if (!moveParticipantId) return;
    moveParticipant.mutate({ participantId: moveParticipantId, toBreakoutId: moveTargetId || null });
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
        {isError && <RequestError error={error} retry={() => void refetch()} />}
        {participants.isError && <RequestError error={participants.error} retry={() => void participants.refetch()} />}
        {moveParticipant.isError && <RequestError error={moveParticipant.error} />}
        {enterBreakout.isError && <RequestError error={enterBreakout.error} />}
        {assignBreakouts.isError && <RequestError error={assignBreakouts.error} />}
        {closeAll.isError && <RequestError error={closeAll.error} />}
        {leaveBreakout.isError && <RequestError error={leaveBreakout.error} />}
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
            {isInBreakout && myBreakoutName && (
              <div className="bg-primary/10 rounded-xl p-4 flex items-center justify-between">
                <div>
                  <p className="text-sm text-muted-foreground">You are in:</p>
                  <p className="font-semibold text-primary">
                    {myBreakoutName}
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
            {isHost && <section className="space-y-2 rounded-xl border border-border p-3">
              <h4 className="text-sm font-medium">Assign participant (manual)</h4>
              <select aria-label="Participant to assign manually" value={manualParticipantId} onChange={(event) => setManualParticipantId(event.target.value)} className="h-9 w-full rounded-md border border-input bg-background px-2 text-sm">
                <option value="">Select a participant</option>
                {(participants.data || []).map((person) => <option key={person.id} value={person.id}>{person.displayName}</option>)}
              </select>
              <select aria-label="Manual assignment room" value={manualRoomId} onChange={(event) => setManualRoomId(event.target.value)} className="h-9 w-full rounded-md border border-input bg-background px-2 text-sm">
                <option value="">Select a room</option>
                {breakouts.filter((room) => room.status === "CREATED" || room.status === "OPEN").map((room) => <option key={room.id} value={room.id}>{room.name}</option>)}
              </select>
              <Button size="sm" className="w-full" onClick={handleManualAssign} disabled={!manualParticipantId || !manualRoomId || assignBreakouts.isPending}>Assign</Button>
              {assignmentNotice && <p role="status" className="text-xs text-muted-foreground">{assignmentNotice}</p>}
            </section>}
            {isHost && <section className="space-y-2 rounded-xl border border-border p-3">
              <h4 className="text-sm font-medium">Move participant</h4>
              <select aria-label="Participant to move" value={moveParticipantId} onChange={(event) => setMoveParticipantId(event.target.value)} className="h-9 w-full rounded-md border border-input bg-background px-2 text-sm">
                <option value="">Select a participant</option>
                {(participants.data || []).map((person) => <option key={person.id} value={person.id}>{person.displayName}</option>)}
              </select>
              <select aria-label="Move participant to room" value={moveTargetId} onChange={(event) => setMoveTargetId(event.target.value)} className="h-9 w-full rounded-md border border-input bg-background px-2 text-sm">
                <option value="">Main conference</option>
                {breakouts.filter((room) => room.status === "OPEN").map((room) => <option key={room.id} value={room.id}>{room.name}</option>)}
              </select>
              <Button size="sm" className="w-full" onClick={handleMoveParticipant} disabled={!moveParticipantId || moveParticipant.isPending}>Move</Button>
            </section>}
            {breakouts.map((room) => (
              <BreakoutRoomCard
                key={room.id}
                room={room}
                isHost={isHost}
                isCurrentRoom={myContext?.breakoutId === room.id}
                onEnter={() => handleEnterRoom(room.id)}
                conferenceId={conferenceId}
                participants={participants.data || []}
                countdownSeconds={countdownSeconds}
                onAnnounce={() => setAnnounceTargetId(room.id)}
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
          <div className="flex gap-2">
            {(["RANDOM", "BALANCED"] as const).map((mode) => <Button key={mode} variant="outline" size="sm" className="flex-1" disabled={assignBreakouts.isPending} onClick={() => assignBreakouts.mutate({ mode }, { onSuccess: (result) => setAssignmentNotice(`Assigned ${result.assigned.length} participants; ${result.unassigned.length} left unassigned.`) })}>
              {mode === "RANDOM" ? <Shuffle className="mr-1 size-4" /> : null}{mode === "RANDOM" ? "Random assign" : "Balance rooms"}
            </Button>)}
          </div>
          <label className="grid gap-1 text-xs text-muted-foreground">Close countdown (seconds, 0 = immediate)
            <Input type="number" min={0} max={600} value={countdownSeconds} onChange={(event) => setCountdownSeconds(Math.max(0, Math.min(600, Number(event.target.value))))} />
          </label>
          {hasOpenRooms && (
            <Button
              variant="destructive"
              size="sm"
              className="w-full"
              onClick={handleCloseAll}
              disabled={closeAll.isPending}
            >
              <Square className="w-4 h-4 mr-2" />
              Close All Rooms ({countdownSeconds}s)
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
      {announceTargetId && <AnnounceRoomModal conferenceId={conferenceId} breakoutId={announceTargetId} roomName={breakouts.find((room) => room.id === announceTargetId)?.name || "Room"} onClose={() => setAnnounceTargetId(undefined)} />}
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
  participants: ParticipantResponse[];
  onAnnounce: () => void;
  countdownSeconds: number;
}

function BreakoutRoomCard({
  room,
  isHost,
  isCurrentRoom,
  onEnter,
  conferenceId,
  participants,
  onAnnounce,
  countdownSeconds,
}: BreakoutRoomCardProps) {
  const openBreakout = useOpenBreakout(conferenceId);
  const patchAssignments = usePatchAssignments(conferenceId);
  const closeBreakout = useCloseBreakout(conferenceId);
  const [selectedParticipant, setSelectedParticipant] = useState("");
  const assigned = room.participantIds || [];

  const statusColors: Record<string, string> = {
    CREATED: "bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400",
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
          {room.participantCount}{room.capacity ? ` / ${room.capacity}` : ""}
        </span>
      </div>

      {/* Assigned participants preview */}
      {assigned.length > 0 && (
        <div className="flex -space-x-2 mb-3">
          {assigned.slice(0, 5).map((id, i) => (
            <Avatar key={id} className="w-7 h-7 border-2 border-card">
              <AvatarImage src={`https://api.dicebear.com/7.x/avataaars/svg?seed=${id}`} />
              <AvatarFallback>{i + 1}</AvatarFallback>
            </Avatar>
          ))}
          {assigned.length > 5 && (
            <div className="w-7 h-7 rounded-full bg-muted flex items-center justify-center text-xs text-muted-foreground border-2 border-card">
              +{assigned.length - 5}
            </div>
          )}
        </div>
      )}

      {/* Actions */}
      <div className="flex gap-2">
        {isHost && room.status === "CREATED" && (
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
        {isHost && room.status === "OPEN" && !isCurrentRoom && (
          <Button size="sm" className="flex-1" onClick={onEnter}>
            <ArrowRight className="w-3.5 h-3.5 mr-1" />
            Visit
          </Button>
        )}
        {isCurrentRoom && (
          <span className="text-sm text-primary font-medium">
            You are here
          </span>
        )}
      </div>
      {isHost && <div className="mt-3 space-y-2 border-t border-border pt-3">
        <div className="flex gap-2">
          <select aria-label={`Participant to assign to ${room.name}`} value={selectedParticipant} onChange={(event) => setSelectedParticipant(event.target.value)} className="h-9 min-w-0 flex-1 rounded-md border border-input bg-background px-2 text-xs">
            <option value="">Assign participant…</option>
            {participants.filter((person) => !assigned.includes(person.id)).map((person) => <option key={person.id} value={person.id}>{person.displayName}</option>)}
          </select>
          <Button size="sm" variant="outline" disabled={!selectedParticipant || patchAssignments.isPending} onClick={() => patchAssignments.mutate({ breakoutId: room.id, data: { add: [selectedParticipant] } }, { onSuccess: () => setSelectedParticipant("") })}>Add</Button>
        </div>
        {assigned.map((id) => {
          const person = participants.find((candidate) => candidate.id === id);
          return <div key={id} className="flex items-center justify-between gap-2 text-xs">
            <span className="truncate">{person?.displayName || id}</span>
            <Button size="sm" variant="ghost" onClick={() => patchAssignments.mutate({ breakoutId: room.id, data: { remove: [id] } })} disabled={patchAssignments.isPending}>Remove</Button>
          </div>;
        })}
        <div className="flex gap-2">
          <Button size="sm" variant="outline" className="flex-1" onClick={onAnnounce}>Announce</Button>
          {(room.status === "OPEN" || room.status === "CLOSING") && <Button size="sm" variant="destructive" className="flex-1" onClick={() => closeBreakout.mutate({ breakoutId: room.id, data: { countdownSeconds } })} disabled={closeBreakout.isPending}>Close ({countdownSeconds}s)</Button>}
        </div>
        {openBreakout.isError && <RequestError error={openBreakout.error} />}
        {patchAssignments.isError && <RequestError error={patchAssignments.error} />}
        {closeBreakout.isError && <RequestError error={closeBreakout.error} />}
      </div>}
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
  const [useCustomNames, setUseCustomNames] = useState(false);
  const [roomNames, setRoomNames] = useState("");
  const [capacity, setCapacity] = useState("");
  const createBreakouts = useCreateBreakouts(conferenceId);

  const handleCreate = () => {
    const names = roomNames.split("\n").map((name) => name.trim()).filter(Boolean);
    createBreakouts.mutate(
      {
        ...(useCustomNames ? { names } : { count: roomCount }),
        ...(capacity ? { capacity: Number(capacity) } : {}),
      },
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
          {!useCustomNames && <div>
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
          </div>}
          {useCustomNames && <div>
            <label htmlFor="breakout-room-names" className="text-sm font-medium text-foreground">Room names (one per line)</label>
            <textarea id="breakout-room-names" value={roomNames} onChange={(event) => setRoomNames(event.target.value)} rows={4} maxLength={2000} className="mt-1.5 w-full rounded-md border border-border bg-background p-2 text-sm" placeholder={"Design\nPlanning\nReview"} />
          </div>}
          <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={useCustomNames} onChange={(event) => setUseCustomNames(event.target.checked)} />Use custom room names</label>
          <div>
            <label htmlFor="breakout-capacity" className="text-sm font-medium text-foreground">Capacity per room (optional)</label>
            <Input id="breakout-capacity" type="number" min={1} max={1000} value={capacity} onChange={(event) => setCapacity(event.target.value)} className="mt-1.5" />
          </div>
        </div>
        {createBreakouts.isError && <RequestError error={createBreakouts.error} />}

        <div className="flex gap-3 mt-6">
          <Button variant="outline" className="flex-1" onClick={onClose}>
            Cancel
          </Button>
          <Button
            className="flex-1"
            onClick={handleCreate}
            disabled={useCustomNames ? roomNames.split("\n").every((name) => !name.trim()) : roomCount < 1}
            loading={createBreakouts.isPending}
          >
            Create
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

        {announceAll.isError && <RequestError error={announceAll.error} />}
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

function AnnounceRoomModal({ conferenceId, breakoutId, roomName, onClose }: {
  conferenceId: string;
  breakoutId: string;
  roomName: string;
  onClose: () => void;
}) {
  const [message, setMessage] = useState("");
  const announce = useAnnounceBreakout(conferenceId);
  const submit = () => {
    if (!message.trim()) return;
    announce.mutate({ breakoutId, data: { message: message.trim() } }, { onSuccess: onClose });
  };
  return <div className="fixed inset-0 z-[60] flex items-center justify-center p-4">
    <button aria-label="Close announcement dialog" className="absolute inset-0 bg-black/50" onClick={onClose} />
    <div role="dialog" aria-modal="true" className="relative w-full max-w-sm space-y-3 rounded-2xl bg-card p-5 shadow-xl">
      <h3 className="font-semibold">Announce to {roomName}</h3>
      {announce.isError && <RequestError error={announce.error} />}
      <textarea value={message} onChange={(event) => setMessage(event.target.value)} rows={3} maxLength={1000} className="w-full rounded-lg border border-border bg-background p-3 text-sm" placeholder="Type your announcement…" />
      <div className="flex gap-2"><Button variant="outline" className="flex-1" onClick={onClose}>Cancel</Button><Button className="flex-1" disabled={!message.trim() || announce.isPending} onClick={submit}>Send</Button></div>
    </div>
  </div>;
}

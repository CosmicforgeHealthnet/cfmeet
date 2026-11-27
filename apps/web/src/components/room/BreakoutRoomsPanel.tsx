"use client";

import { useState } from "react";
import { ArrowRight, DoorOpen, LogOut, Megaphone, Minus, Play, Plus, Scale, Shuffle, Square, Users, X } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { NiceSelect } from "@/components/ui/nice-select";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { cn } from "@/lib/utils";
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
import { MEDIA_MOVE_EVENT } from "./RoomLiveEvents";

const switchMedia = () => window.dispatchEvent(new CustomEvent(MEDIA_MOVE_EVENT, { detail: { self: true } }));

interface BreakoutRoomsPanelProps {
  conferenceId: string;
  isHost: boolean;
  /** Kept for callers; the room sidebar owns the title and close button. */
  onClose?: () => void;
}

const STATUS: Record<string, { label: string; className: string }> = {
  CREATED: { label: "Not open", className: "bg-white/10 text-white/70" },
  OPEN: { label: "Open", className: "bg-emerald-500/15 text-emerald-300" },
  CLOSING: { label: "Closing", className: "bg-amber-400/15 text-amber-300" },
  CLOSED: { label: "Closed", className: "bg-white/5 text-white/40" },
};

const COUNTDOWNS = [
  { value: "0", label: "Close now" },
  { value: "30", label: "Close in 30 seconds" },
  { value: "60", label: "Close in 1 minute" },
  { value: "120", label: "Close in 2 minutes" },
];

const MAIN = "__main";
const AVATAR_COLORS = ["#1d3a6e", "#14503f", "#5a4216", "#3d2c6b", "#5c2236", "#164a5a"];
const colorFor = (key: string) => AVATAR_COLORS[([...key].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % AVATAR_COLORS.length];
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((part) => part[0]?.toUpperCase()).join("") || "?";

function Face({ person, size = "size-8" }: { person?: ParticipantResponse; size?: string }) {
  const name = person?.displayName || "Guest";
  return <span className={cn("flex shrink-0 items-center justify-center rounded-full text-[11px] font-semibold text-white ring-2 ring-[var(--background)]", size)} style={{ backgroundColor: colorFor(person?.id ?? name) }}>{initials(name)}</span>;
}

const pill = "inline-flex h-9 items-center justify-center gap-1.5 rounded-full px-4 text-sm font-semibold transition-colors disabled:opacity-50";

export default function BreakoutRoomsPanel({ conferenceId, isHost }: BreakoutRoomsPanelProps) {
  const { data, isLoading, isError, error, refetch } = useBreakouts(conferenceId);
  const participants = useParticipantsV2(conferenceId);
  const people = participants.data ?? [];
  const rooms = (data?.breakouts ?? []).filter((room) => room.status !== "CLOSED");
  const myContext = data?.myContext;
  const myRoom = rooms.find((room) => room.id === myContext?.breakoutId);

  const [createOpen, setCreateOpen] = useState(false);
  const [announce, setAnnounce] = useState<{ id?: string; name: string } | null>(null);
  const [closeAfter, setCloseAfter] = useState("30");
  const [notice, setNotice] = useState("");

  const assign = useAssignBreakouts(conferenceId);
  const openRoom = useOpenBreakout(conferenceId);
  const closeAll = useCloseAllBreakouts(conferenceId);
  const leave = useLeaveBreakout(conferenceId);
  const enter = useEnterBreakout(conferenceId);
  const move = useMoveParticipant(conferenceId);
  const patch = usePatchAssignments(conferenceId);

  const anyOpen = rooms.some((room) => room.status === "OPEN" || room.status === "CLOSING");
  const notOpened = rooms.filter((room) => room.status === "CREATED");
  const roomOf = (personId: string) => rooms.find((room) => room.participantIds?.includes(personId));
  const assignedCount = people.filter((person) => roomOf(person.id)).length;
  const failure = [assign, openRoom, closeAll, leave, enter, move, patch].find((m) => m.isError)?.error;

  const autoAssign = (mode: "RANDOM" | "BALANCED") => {
    setNotice("");
    assign.mutate({ mode }, { onSuccess: (result) => setNotice(`${result.assigned.length} assigned${result.unassigned.length ? `, ${result.unassigned.length} left in the main room` : ""}.`) });
  };

  const placePerson = (person: ParticipantResponse, target: string) => {
    setNotice("");
    const current = roomOf(person.id);
    const targetRoom = rooms.find((room) => room.id === target);
    if (current?.id === target || (!current && target === MAIN)) return;
    // Rooms already running: move the person live. Before opening: just change the plan.
    if (anyOpen || targetRoom?.status === "OPEN" || current?.status === "OPEN") {
      move.mutate({ participantId: person.id, toBreakoutId: target === MAIN ? null : target });
    } else if (target === MAIN && current) {
      patch.mutate({ breakoutId: current.id, data: { remove: [person.id] } });
    } else if (targetRoom) {
      assign.mutate({ mode: "MANUAL", assignments: [{ participantId: person.id, breakoutId: targetRoom.id }] });
    }
  };

  const openAll = async () => {
    for (const room of notOpened) {
      try { await openRoom.mutateAsync(room.id); } catch { return; }
    }
  };

  return (
    <div className="space-y-4">
      {isError && <RequestError error={error} retry={() => void refetch()} />}
      {participants.isError && <RequestError error={participants.error} retry={() => void participants.refetch()} />}
      {failure && <RequestError error={failure} />}

      {myRoom && (
        <div className="flex items-center gap-3 rounded-2xl bg-primary/15 p-4 ring-1 ring-primary/30">
          <span className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-primary/20 text-primary"><DoorOpen className="size-5" /></span>
          <div className="min-w-0 flex-1"><p className="text-xs text-white/60">You&apos;re in</p><p className="truncate font-semibold">{myRoom.name}</p></div>
          <button type="button" onClick={() => leave.mutate(undefined, { onSuccess: switchMedia })} disabled={leave.isPending} className={cn(pill, "bg-white/10 hover:bg-white/15")}><LogOut className="size-4" />{leave.isPending ? "Leaving…" : "Main room"}</button>
        </div>
      )}

      {isLoading && [0, 1, 2].map((i) => <div key={i} className="h-20 animate-pulse rounded-2xl bg-white/5" />)}

      {!isLoading && rooms.length === 0 && (
        <div className="flex flex-col items-center px-4 py-12 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/15 text-primary"><Users className="size-6" /></span>
          <p className="mt-4 font-semibold">No breakout rooms</p>
          <p className="mt-1 max-w-[16rem] text-sm text-white/60">{isHost ? "Split the call into small groups, then bring everyone back when time is up." : "When the host opens breakout rooms, you'll be moved to yours."}</p>
          {isHost && <button type="button" onClick={() => setCreateOpen(true)} className={cn(pill, "mt-5 h-10 bg-primary px-5 text-primary-foreground hover:bg-primary/90")}><Plus className="size-4" />Create rooms</button>}
        </div>
      )}

      {isHost && rooms.length > 0 && (
        <section className="space-y-3 rounded-2xl bg-white/[0.06] p-4 ring-1 ring-white/10">
          <div className="flex items-baseline justify-between gap-2">
            <p className="font-semibold">{anyOpen ? "Rooms are open" : "Set up rooms"}</p>
            <p className="text-xs text-white/50">{rooms.length} rooms · {assignedCount}/{people.length} placed</p>
          </div>
          {!anyOpen && (
            <>
              <div className="grid grid-cols-2 gap-2">
                <button type="button" onClick={() => autoAssign("RANDOM")} disabled={assign.isPending} className={cn(pill, "bg-white/10 hover:bg-white/15")}><Shuffle className="size-4" />Shuffle</button>
                <button type="button" onClick={() => autoAssign("BALANCED")} disabled={assign.isPending} className={cn(pill, "bg-white/10 hover:bg-white/15")}><Scale className="size-4" />Even out</button>
              </div>
              {notOpened.length > 0 && <button type="button" onClick={() => void openAll()} disabled={openRoom.isPending} className={cn(pill, "h-10 w-full bg-primary text-primary-foreground hover:bg-primary/90")}><Play className="size-4" />{openRoom.isPending ? "Opening…" : `Open ${notOpened.length === 1 ? "room" : `all ${notOpened.length} rooms`}`}</button>}
              <button type="button" onClick={() => setCreateOpen(true)} className="w-full text-center text-sm font-medium text-primary hover:underline">Add more rooms</button>
            </>
          )}
          {anyOpen && (
            <>
              <button type="button" onClick={() => setAnnounce({ name: "all rooms" })} className={cn(pill, "w-full bg-white/10 hover:bg-white/15")}><Megaphone className="size-4" />Message all rooms</button>
              <div className="flex gap-2">
                <NiceSelect className="min-w-0 flex-1" value={closeAfter} onChange={setCloseAfter} options={COUNTDOWNS} />
                <button type="button" onClick={() => closeAll.mutate({ countdownSeconds: Number(closeAfter) })} disabled={closeAll.isPending} className={cn(pill, "h-11 shrink-0 bg-destructive text-white hover:bg-destructive/90")}><Square className="size-4" />End rooms</button>
              </div>
              {notOpened.length > 0 && <button type="button" onClick={() => void openAll()} disabled={openRoom.isPending} className="w-full text-center text-sm font-medium text-primary hover:underline">Open the {notOpened.length} unopened room{notOpened.length === 1 ? "" : "s"}</button>}
            </>
          )}
          {notice && <p role="status" className="text-xs text-white/60">{notice}</p>}
        </section>
      )}

      {rooms.map((room) => (
        <RoomCard key={room.id} room={room} people={people} isHost={isHost} isMine={myRoom?.id === room.id} conferenceId={conferenceId}
          closeAfter={Number(closeAfter)} onEnter={() => enter.mutate(room.id, { onSuccess: switchMedia })} entering={enter.isPending}
          onAnnounce={() => setAnnounce({ id: room.id, name: room.name })}
          onRemove={(id) => patch.mutate({ breakoutId: room.id, data: { remove: [id] } })} />
      ))}

      {isHost && rooms.length > 0 && people.length > 0 && (
        <section className="space-y-1">
          <p className="px-1 pb-1 text-xs font-semibold uppercase tracking-wider text-white/40">Place people</p>
          {people.map((person) => (
            <div key={person.id} className="flex items-center gap-3 rounded-xl px-1 py-1.5">
              <Face person={person} />
              <span className="min-w-0 flex-1 truncate text-sm">{person.displayName || "Guest"}</span>
              <NiceSelect className="w-36 shrink-0" value={roomOf(person.id)?.id ?? MAIN} onChange={(target) => placePerson(person, target)}
                disabled={move.isPending || assign.isPending || patch.isPending}
                options={[{ value: MAIN, label: "Main room" }, ...rooms.filter((room) => room.status !== "CLOSING").map((room) => ({ value: room.id, label: room.name }))]} />
            </div>
          ))}
        </section>
      )}

      <CreateRoomsDialog open={createOpen} conferenceId={conferenceId} onClose={() => setCreateOpen(false)} />
      <AnnounceDialog target={announce} conferenceId={conferenceId} onClose={() => setAnnounce(null)} />
    </div>
  );
}

function RoomCard({ room, people, isHost, isMine, conferenceId, closeAfter, onEnter, entering, onAnnounce, onRemove }: {
  room: BreakoutView;
  people: ParticipantResponse[];
  isHost: boolean;
  isMine: boolean;
  conferenceId: string;
  closeAfter: number;
  onEnter: () => void;
  entering: boolean;
  onAnnounce: () => void;
  onRemove: (participantId: string) => void;
}) {
  const close = useCloseBreakout(conferenceId);
  const members = (room.participantIds ?? []).map((id) => people.find((person) => person.id === id) ?? ({ id, displayName: "Guest" } as ParticipantResponse));
  const status = STATUS[room.status] ?? STATUS.CREATED;
  const live = room.status === "OPEN";

  return (
    <article className={cn("rounded-2xl bg-white/[0.06] p-4 ring-1", isMine ? "ring-primary/50" : "ring-white/10")}>
      <div className="flex items-center gap-2">
        <h4 className="min-w-0 flex-1 truncate font-semibold">{room.name}</h4>
        <span className={cn("inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[11px] font-semibold", status.className)}>
          {live && <span className="size-1.5 animate-pulse rounded-full bg-emerald-400" />}{status.label}
        </span>
        <span className="inline-flex items-center gap-1 text-xs tabular-nums text-white/50"><Users className="size-3.5" />{room.participantCount}{room.capacity ? `/${room.capacity}` : ""}</span>
      </div>

      {members.length > 0 ? (
        <ul className="mt-3 space-y-1.5">
          {members.map((person) => (
            <li key={person.id} className="group flex items-center gap-2.5 text-sm">
              <Face person={person} size="size-6" />
              <span className="min-w-0 flex-1 truncate text-white/80">{person.displayName || "Guest"}</span>
              {isHost && room.status === "CREATED" && <button type="button" aria-label={`Remove ${person.displayName}`} onClick={() => onRemove(person.id)} className="flex size-6 items-center justify-center rounded-full text-white/40 hover:bg-white/10 hover:text-white"><X className="size-3.5" /></button>}
            </li>
          ))}
        </ul>
      ) : (
        <p className="mt-2 text-sm text-white/40">No one here yet</p>
      )}

      {isHost && live && (
        <div className="mt-3 flex gap-2 border-t border-white/10 pt-3">
          {!isMine && <button type="button" onClick={onEnter} disabled={entering} className={cn(pill, "flex-1 bg-primary text-primary-foreground hover:bg-primary/90")}><ArrowRight className="size-4" />Join</button>}
          <button type="button" onClick={onAnnounce} aria-label={`Message ${room.name}`} title="Send a message" className="flex size-9 items-center justify-center rounded-full bg-white/10 hover:bg-white/15"><Megaphone className="size-4" /></button>
          <button type="button" onClick={() => close.mutate({ breakoutId: room.id, data: { countdownSeconds: closeAfter } })} disabled={close.isPending} aria-label={`End ${room.name}`} title="End this room" className="flex size-9 items-center justify-center rounded-full bg-white/10 text-red-300 hover:bg-destructive hover:text-white"><Square className="size-4" /></button>
        </div>
      )}
      {close.isError && <div className="mt-2"><RequestError error={close.error} /></div>}
    </article>
  );
}

function Stepper({ value, onChange, min, max, label }: { value: number; onChange: (v: number) => void; min: number; max: number; label: string }) {
  return (
    <div className="flex items-center justify-between rounded-xl bg-white/5 px-3 py-2 ring-1 ring-white/10">
      <span className="text-sm font-medium">{label}</span>
      <div className="flex items-center gap-1">
        <button type="button" aria-label={`Fewer ${label.toLowerCase()}`} disabled={value <= min} onClick={() => onChange(value - 1)} className="flex size-9 items-center justify-center rounded-full hover:bg-white/10 disabled:opacity-30"><Minus className="size-4" /></button>
        <span className="w-8 text-center text-lg font-semibold tabular-nums">{value}</span>
        <button type="button" aria-label={`More ${label.toLowerCase()}`} disabled={value >= max} onClick={() => onChange(value + 1)} className="flex size-9 items-center justify-center rounded-full hover:bg-white/10 disabled:opacity-30"><Plus className="size-4" /></button>
      </div>
    </div>
  );
}

function CreateRoomsDialog({ open, conferenceId, onClose }: { open: boolean; conferenceId: string; onClose: () => void }) {
  const [count, setCount] = useState(3);
  const [names, setNames] = useState<string[]>([]);
  const [capacity, setCapacity] = useState(0);
  const create = useCreateBreakouts(conferenceId);
  const nameFor = (index: number) => names[index]?.trim() || `Room ${index + 1}`;

  const done = () => { setCount(3); setNames([]); setCapacity(0); create.reset(); onClose(); };
  const submit = () => {
    const custom = names.slice(0, count).some((name) => name?.trim());
    create.mutate({
      ...(custom ? { names: Array.from({ length: count }, (_, i) => nameFor(i)) } : { count }),
      ...(capacity ? { capacity } : {}),
    }, { onSuccess: done });
  };

  return (
    <Dialog open={open} onOpenChange={(v) => !v && done()}>
      <DialogContent className="dark max-h-[90dvh] overflow-y-auto border-white/10 bg-[#2D2E31] text-foreground sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Create breakout rooms</DialogTitle>
          <DialogDescription>You can place people and rename rooms before opening them.</DialogDescription>
        </DialogHeader>
        {create.isError && <RequestError error={create.error} />}
        <div className="space-y-3">
          <Stepper label="Rooms" value={count} onChange={setCount} min={1} max={50} />
          <div className="flex items-center justify-between rounded-xl bg-white/5 px-3 py-2 ring-1 ring-white/10">
            <span className="text-sm font-medium">Max per room</span>
            <div className="flex items-center gap-1">
              <button type="button" aria-label="Lower limit" disabled={capacity <= 0} onClick={() => setCapacity(Math.max(0, capacity - 1))} className="flex size-9 items-center justify-center rounded-full hover:bg-white/10 disabled:opacity-30"><Minus className="size-4" /></button>
              <span className="w-16 text-center text-sm font-semibold tabular-nums">{capacity ? capacity : "No limit"}</span>
              <button type="button" aria-label="Raise limit" disabled={capacity >= 1000} onClick={() => setCapacity(capacity + 1)} className="flex size-9 items-center justify-center rounded-full hover:bg-white/10 disabled:opacity-30"><Plus className="size-4" /></button>
            </div>
          </div>
          <div>
            <p className="mb-2 text-sm font-medium">Room names <span className="font-normal text-white/50">(optional)</span></p>
            <div className="no-scrollbar max-h-56 space-y-2 overflow-y-auto">
              {Array.from({ length: count }, (_, index) => (
                <input key={index} value={names[index] ?? ""} placeholder={`Room ${index + 1}`} maxLength={100} aria-label={`Room ${index + 1} name`}
                  onChange={(event) => setNames((current) => { const next = [...current]; next[index] = event.target.value; return next; })}
                  className="h-11 w-full rounded-xl bg-white/5 px-3 text-sm outline-none ring-1 ring-white/15 placeholder:text-white/40 focus:ring-2 focus:ring-primary" />
              ))}
            </div>
          </div>
        </div>
        <div className="flex flex-col-reverse gap-2 pt-2 sm:flex-row sm:justify-end">
          <button type="button" onClick={done} className="h-11 rounded-full px-5 text-sm font-semibold text-white/80 ring-1 ring-white/15 hover:bg-white/5">Cancel</button>
          <button type="button" onClick={submit} disabled={create.isPending} className="h-11 rounded-full bg-primary px-6 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{create.isPending ? "Creating…" : `Create ${count} room${count === 1 ? "" : "s"}`}</button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

function AnnounceDialog({ target, conferenceId, onClose }: { target: { id?: string; name: string } | null; conferenceId: string; onClose: () => void }) {
  const [message, setMessage] = useState("");
  const all = useAnnounceAll(conferenceId);
  const one = useAnnounceBreakout(conferenceId);
  const pending = all.isPending || one.isPending;
  const failure = all.isError ? all.error : one.isError ? one.error : undefined;

  const done = () => { setMessage(""); all.reset(); one.reset(); onClose(); };
  const send = () => {
    const text = message.trim();
    if (!text || !target) return;
    if (target.id) one.mutate({ breakoutId: target.id, data: { message: text } }, { onSuccess: done });
    else all.mutate({ message: text }, { onSuccess: done });
  };

  return (
    <Dialog open={target !== null} onOpenChange={(v) => !v && done()}>
      <DialogContent className="dark border-white/10 bg-[#2D2E31] text-foreground sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Message {target?.name}</DialogTitle>
          <DialogDescription>Shows as a banner for everyone in {target?.id ? "this room" : "every room"}.</DialogDescription>
        </DialogHeader>
        {failure && <RequestError error={failure} />}
        <textarea autoFocus value={message} onChange={(event) => setMessage(event.target.value)} rows={3} maxLength={1000} placeholder="e.g. 2 minutes left, start wrapping up"
          className="w-full resize-none rounded-xl bg-white/5 p-3 text-sm outline-none ring-1 ring-white/15 placeholder:text-white/40 focus:ring-2 focus:ring-primary" />
        <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
          <button type="button" onClick={done} className="h-11 rounded-full px-5 text-sm font-semibold text-white/80 ring-1 ring-white/15 hover:bg-white/5">Cancel</button>
          <button type="button" onClick={send} disabled={!message.trim() || pending} className="h-11 rounded-full bg-primary px-6 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{pending ? "Sending…" : "Send"}</button>
        </div>
      </DialogContent>
    </Dialog>
  );
}

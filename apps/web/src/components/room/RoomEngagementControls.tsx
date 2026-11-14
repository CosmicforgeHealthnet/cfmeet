"use client";

import { useEffect, useState } from "react";
import { Smile } from "lucide-react";
import { Popover } from "radix-ui";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { cn } from "@/lib/utils";
import { useCollaborationState, useLowerHand, useRaiseHand, useSendReaction } from "@/hooks/v2/useCollaboration";

/** Server reaction keys → text fallback (screen readers, notifications). */
export const REACTION_EMOJI: Record<string, string> = {
  thumbs_up: "\u{1F44D}",
  clap: "\u{1F44F}",
  heart: "❤️",
  laugh: "\u{1F602}",
  surprised: "\u{1F62E}",
  celebrate: "\u{1F389}",
};

/** Google Noto animated emoji (the set Meet uses), served from /public/emoji. */
const REACTION_FILE: Record<string, string> = {
  thumbs_up: "1f44d",
  clap: "1f44f",
  heart: "2764_fe0f",
  laugh: "1f602",
  surprised: "1f62e",
  celebrate: "1f389",
};

const LABEL: Record<string, string> = { thumbs_up: "Thumbs up", clap: "Clap", heart: "Love", laugh: "Laugh", surprised: "Wow", celebrate: "Celebrate" };

export function Emoji({ name, className }: { name: string; className?: string }) {
  const file = REACTION_FILE[name] ?? (name === "raised_hand" ? "270b" : undefined);
  if (!file) return <span className={className}>{REACTION_EMOJI[name]}</span>;
  // eslint-disable-next-line @next/next/no-img-element
  return <img src={`/emoji/${file}.webp`} alt="" draggable={false} className={cn("select-none", className)} />;
}

export interface RoomReaction {
  id: string;
  reaction: string;
  conferenceId?: string;
  senderName?: string;
}

export default function RoomEngagementControls({ conferenceId }: { conferenceId: string }) {
  const state = useCollaborationState(conferenceId);
  const raise = useRaiseHand(conferenceId);
  const lower = useLowerHand(conferenceId);
  const react = useSendReaction(conferenceId);
  const [feedback, setFeedback] = useState("");
  const raised = !!state.data?.me?.handRaisedAt;
  const policy = state.data?.policy;
  const reactionsOff = !state.data?.me || policy?.reactionsEnabled === false;
  const keys = Object.keys(REACTION_FILE).filter((key) => !policy || policy.allowedReactions.includes(key));

  const send = async (reaction: string) => {
    setFeedback("");
    try {
      const result = await react.mutateAsync({ reaction });
      if (!result.delivered) setFeedback("That reaction didn't go through. Try again.");
    } catch { /* shown below */ }
  };

  return (
    <Popover.Root>
      <div className="room-control">
        <Popover.Trigger asChild>
          <button type="button" aria-label="Reactions and raise hand" title="Reactions" className="room-control-button" data-active={raised || undefined}><Smile /></button>
        </Popover.Trigger>
        <span>Reactions</span>
      </div>
      <Popover.Portal>
        <Popover.Content side="top" sideOffset={14} collisionPadding={12} className="dark z-50 rounded-2xl bg-[#2D2E31] p-2 text-foreground shadow-2xl ring-1 ring-white/10">
          {keys.length > 0 && (
            <div className="flex gap-0.5">
              {keys.map((key) => (
                <button key={key} type="button" aria-label={`Send ${LABEL[key] ?? key}`} title={LABEL[key]} disabled={reactionsOff || react.isPending} onClick={() => void send(key)}
                  className="flex size-12 items-center justify-center rounded-xl transition-transform hover:scale-110 hover:bg-white/10 active:scale-95 disabled:opacity-30 disabled:hover:scale-100">
                  <Emoji name={key} className="size-8" />
                </button>
              ))}
            </div>
          )}
          {policy?.handRaiseEnabled !== false && (
            <button type="button" disabled={!state.data?.me || raise.isPending || lower.isPending} onClick={async () => { try { await (raised ? lower : raise).mutateAsync(); } catch { /* shown below */ } }}
              className={cn("mt-1 flex h-11 w-full items-center justify-center gap-2 rounded-xl text-sm font-semibold transition-colors disabled:opacity-40", raised ? "bg-[#a8c7fa] text-[#062e6f] hover:bg-[#c2d7fb]" : "bg-white/10 hover:bg-white/15")}>
              <Emoji name="raised_hand" className="size-5" />{raised ? "Lower hand" : "Raise hand"}
            </button>
          )}
          {(raise.isError || lower.isError || react.isError) && <div className="mt-2"><RequestError error={raise.error || lower.error || react.error} /></div>}
          {feedback && <p role="status" className="mt-2 px-1 text-xs text-white/70">{feedback}</p>}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

/** Reactions float up from the bottom of the stage, like Meet. */
export function ReactionOverlay() {
  const [reactions, setReactions] = useState<(RoomReaction & { left: number })[]>([]);
  useEffect(() => {
    const timers = new Set<ReturnType<typeof setTimeout>>();
    const receive = (event: Event) => {
      const item = (event as CustomEvent<RoomReaction>).detail;
      if (!REACTION_FILE[item.reaction]) return;
      setReactions((current) => [...current.slice(-14), { ...item, left: 4 + Math.random() * 22 }]);
      const timer = setTimeout(() => {
        setReactions((current) => current.filter((reaction) => reaction.id !== item.id));
        timers.delete(timer);
      }, 4000);
      timers.add(timer);
    };
    window.addEventListener("cfmeet:reaction", receive);
    return () => {
      window.removeEventListener("cfmeet:reaction", receive);
      timers.forEach(clearTimeout);
    };
  }, []);
  return (
    <div aria-live="polite" aria-atomic="false" className="pointer-events-none absolute inset-0 z-20 overflow-hidden">
      {reactions.map((item) => (
        <span key={item.id} className="room-floating-reaction absolute bottom-0 flex flex-col items-center gap-1" style={{ left: `${item.left}%` }}>
          <span className="sr-only">{item.senderName ?? "Someone"} reacted {LABEL[item.reaction] ?? item.reaction}</span>
          <Emoji name={item.reaction} className="size-12 drop-shadow-lg" />
          {item.senderName && <span aria-hidden className="max-w-[8rem] truncate rounded-full bg-black/60 px-2 py-0.5 text-xs font-medium text-white">{item.senderName}</span>}
        </span>
      ))}
    </div>
  );
}

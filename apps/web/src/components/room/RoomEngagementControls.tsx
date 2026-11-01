"use client";

import { useEffect, useState } from "react";
import { Smile, Hand } from "lucide-react";
import { Popover } from "radix-ui";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCollaborationState, useLowerHand, useRaiseHand, useSendReaction } from "@/hooks/v2/useCollaboration";
export const REACTION_EMOJI: Record<string, string> = {
  thumbs_up: "\u{1F44D}",
  clap: "\u{1F44F}",
  heart: "\u2764\uFE0F",
  laugh: "\u{1F604}",
  surprised: "\u{1F62E}",
  celebrate: "\u{1F389}"
};
export interface RoomReaction {
  id: string;
  reaction: string;
  conferenceId?: string;
}
export default function RoomEngagementControls({
  conferenceId
}: {
  conferenceId: string;
}) {
  const state = useCollaborationState(conferenceId),
    raise = useRaiseHand(conferenceId),
    lower = useLowerHand(conferenceId),
    react = useSendReaction(conferenceId);
  const [feedback, setFeedback] = useState("");
  const raised = !!state.data?.me?.handRaisedAt,
    policy = state.data?.policy;
  const send = async (reaction: string) => {
    setFeedback("");
    try {
      const result = await react.mutateAsync({
        reaction
      });
      if (!result.delivered) setFeedback("Reaction could not be delivered. Try again.");
    } catch {}
  };
  return <Popover.Root><div className="room-control"><Popover.Trigger asChild><button aria-label="Reactions and raise hand" className="room-control-button"><Smile /></button></Popover.Trigger><span>Reactions</span></div><Popover.Portal><Popover.Content side="top" sideOffset={18} collisionPadding={12} className="z-50 w-72 rounded-2xl border border-border bg-popover p-3 text-popover-foreground shadow-xl"><p className="mb-3 text-xs font-medium text-muted-foreground">Send a reaction</p><div className="flex flex-wrap gap-1">{Object.entries(REACTION_EMOJI).filter(([key]) => !policy || policy.allowedReactions.includes(key)).map(([key, emoji]) => <button key={key} aria-label={"Send " + key.replaceAll("_", " ")} disabled={!state.data?.me || policy?.reactionsEnabled === false || react.isPending} onClick={() => void send(key)} className="size-10 rounded-xl text-2xl transition hover:bg-muted hover:scale-110 disabled:opacity-30">{emoji}</button>)}</div><button className="mt-3 flex w-full items-center justify-center gap-2 rounded-xl border border-border p-2 text-sm hover:bg-muted disabled:opacity-40" disabled={!state.data?.me || policy?.handRaiseEnabled === false || raise.isPending || lower.isPending} onClick={async () => {
          try {
            await (raised ? lower : raise).mutateAsync();
          } catch {}
        }}><Hand className="size-4" />{raised ? "Lower hand" : "Raise hand"}</button>{(raise.isError || lower.isError || react.isError) && <RequestError error={raise.error || lower.error || react.error} />}{feedback && <p role="status" className="mt-2 text-xs">{feedback}</p>}<Popover.Arrow className="fill-popover" /></Popover.Content></Popover.Portal></Popover.Root>;
}
export function ReactionOverlay() {
  const [reactions, setReactions] = useState<RoomReaction[]>([]);
  useEffect(() => {
    const timers = new Set<ReturnType<typeof setTimeout>>();
    const receive = (event: Event) => {
      const item = (event as CustomEvent<RoomReaction>).detail;
      if (!REACTION_EMOJI[item.reaction]) return;
      setReactions(current => [...current.slice(-19), item]);
      const timer = setTimeout(() => {
        setReactions(current => current.filter(reaction => reaction.id !== item.id));
        timers.delete(timer);
      }, 3500);
      timers.add(timer);
    };
    window.addEventListener("cfmeet:reaction", receive);
    return () => {
      window.removeEventListener("cfmeet:reaction", receive);
      timers.forEach(clearTimeout);
    };
  }, []);
  return <div aria-live="polite" aria-atomic="false" className="pointer-events-none absolute inset-0 z-20 overflow-hidden">{reactions.map((item, index) => <span key={item.id} className="room-floating-reaction absolute bottom-0 text-5xl drop-shadow-lg" style={{
      left: 15 + index * 17 % 70 + "%"
    }}><span className="sr-only">Reaction: {item.reaction.replaceAll("_", " ")}</span><span aria-hidden="true">{REACTION_EMOJI[item.reaction]}</span></span>)}</div>;
}

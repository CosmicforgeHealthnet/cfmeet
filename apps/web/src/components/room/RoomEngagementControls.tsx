"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCollaborationState, useLowerHand, useRaiseHand, useSendReaction } from "@/hooks/v2/useCollaboration";

const REACTIONS = [
  { key: "thumbs_up", label: "👍" },
  { key: "clap", label: "👏" },
  { key: "heart", label: "❤️" },
  { key: "laugh", label: "😄" },
  { key: "surprised", label: "😮" },
  { key: "celebrate", label: "🎉" },
];

export default function RoomEngagementControls({ conferenceId }: { conferenceId: string }) {
  const state = useCollaborationState(conferenceId);
  const raise = useRaiseHand(conferenceId);
  const lower = useLowerHand(conferenceId);
  const react = useSendReaction(conferenceId);
  const [feedback, setFeedback] = useState("");
  const raised = !!state.data?.me?.handRaisedAt;
  const policy = state.data?.policy;

  const toggleHand = async () => {
    setFeedback("");
    try {
      const result = await (raised ? lower : raise).mutateAsync();
      setFeedback(result.raised ? "Your hand is raised." : "Your hand is lowered.");
    } catch {
      setFeedback("");
    }
  };

  const send = async (reaction: string) => {
    setFeedback("");
    try {
      const result = await react.mutateAsync({ reaction });
      setFeedback(result.delivered ? "Reaction sent." : "Reaction could not be delivered.");
    } catch {
      setFeedback("");
    }
  };

  return (
    <div className="flex flex-wrap items-center justify-center gap-2 border-b border-border bg-card px-3 py-2">
      <Button size="sm" variant={raised ? "default" : "outline"} disabled={!state.data?.me || policy?.handRaiseEnabled === false || raise.isPending || lower.isPending} onClick={() => void toggleHand()}>
        {raised ? "Lower hand" : "Raise hand"}
      </Button>
      {REACTIONS.map(item => (
        <Button key={item.key} type="button" size="sm" variant="outline" aria-label={`Send ${item.key} reaction`} title={`Send ${item.key} reaction`} disabled={policy?.reactionsEnabled === false || react.isPending} onClick={() => void send(item.key)}>
          {item.label}
        </Button>
      ))}
      {(raise.isError || lower.isError) && <RequestError error={raise.error || lower.error} />}
      {react.isError && <RequestError error={react.error} />}
      {feedback && <span role="status" className="text-xs text-muted-foreground">{feedback}</span>}
    </div>
  );
}

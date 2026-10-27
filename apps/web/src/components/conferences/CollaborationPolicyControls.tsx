"use client";

import { useRef, useState } from "react";
import type { CollaborationPolicyPatch, ChatPolicyUpdateRequest, Role, RolePolicy, ScreenSharePolicy, ChatSendMode, PrivateChatMode } from "@/types/v2";
import { useCollaborationState, useUpdateChatPolicy, useUpdateCollaborationPolicy } from "@/hooks/v2/useCollaboration";
import { meetingAllows } from "@/lib/meeting-authority";
import { ActionDialog, Field, RequestError } from "@/components/meeting-spaces/ManagementUI";

export default function CollaborationPolicyControls({
  conferenceId,
  role,
  rolePolicy,
}: {
  conferenceId: string;
  role?: Role;
  rolePolicy?: RolePolicy;
}) {
  const state = useCollaborationState(conferenceId);
  const update = useUpdateCollaborationPolicy(conferenceId);
  const updateChat = useUpdateChatPolicy(conferenceId);
  const reviewedVersion = useRef<number | undefined>(undefined);
  const reviewedChatVersion = useRef<number | undefined>(undefined);
  const [draft, setDraft] = useState<CollaborationPolicyPatch>({});
  const [chatDraft, setChatDraft] = useState<Pick<ChatPolicyUpdateRequest, "chatEnabled" | "chatSendMode" | "privateChatMode" | "breakoutChatEnabled" | "mainChatVisibleInBreakouts">>({});
  if (!meetingAllows(role, "conference.configure_live", rolePolicy)) return null;
  if (state.isError) return <RequestError error={state.error} retry={() => void state.refetch()} />;
  if (!state.data) return null;

  return (
    <>
    <ActionDialog
      label="Conference policy"
      description="Update this conference’s collaboration policy. Changes are version-checked and cannot grant permissions beyond each participant’s role."
      onOpen={() => {
        const policy = state.data?.policy;
        reviewedVersion.current = state.data?.collaborationVersion;
        if (policy) {
          setDraft({
            audioLocked: policy.audioLocked,
            videoLocked: policy.videoLocked,
            screenSharePolicy: policy.screenSharePolicy,
            reactionsEnabled: policy.reactionsEnabled,
            handRaiseEnabled: policy.handRaiseEnabled,
            maxSpotlights: policy.maxSpotlights,
          });
        }
      }}
      submit={async () => {
        if (reviewedVersion.current === undefined) throw new Error("Reload the conference policy before saving.");
        const maxSpotlights = Number(draft.maxSpotlights);
        if (!Number.isInteger(maxSpotlights) || maxSpotlights < 1 || maxSpotlights > 9) {
          throw new Error("Maximum spotlights must be between 1 and 9.");
        }
        await update.mutateAsync({
          collaborationVersion: reviewedVersion.current,
          changes: { ...draft, maxSpotlights },
        });
      }}
    >
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={draft.audioLocked === true} onChange={e => setDraft(value => ({ ...value, audioLocked: e.target.checked }))} />
        Lock microphones by default
      </label>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={draft.videoLocked === true} onChange={e => setDraft(value => ({ ...value, videoLocked: e.target.checked }))} />
        Lock cameras by default
      </label>
      <label className="grid gap-2 text-sm font-medium">Screen sharing policy
        <select className="h-10 rounded-md border border-input bg-background px-3" value={draft.screenSharePolicy} onChange={e => setDraft(value => ({ ...value, screenSharePolicy: e.target.value as ScreenSharePolicy }))}>
          <option value="HOST_ONLY">Hosts and co-hosts</option>
          <option value="PRESENTERS_ONLY">Presenters and above</option>
          <option value="ALL_INTERACTIVE">All roles with screen-share permission</option>
        </select>
      </label>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={draft.reactionsEnabled === true} onChange={e => setDraft(value => ({ ...value, reactionsEnabled: e.target.checked }))} />
        Enable reactions
      </label>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={draft.handRaiseEnabled === true} onChange={e => setDraft(value => ({ ...value, handRaiseEnabled: e.target.checked }))} />
        Enable hand raising
      </label>
      <Field label="Maximum simultaneous spotlights (1–9)" type="number" min="1" max="9" step="1" required value={String(draft.maxSpotlights ?? "")} onChange={e => setDraft(value => ({ ...value, maxSpotlights: Number(e.target.value) }))} />
    </ActionDialog>
    <ActionDialog
      label="Chat policy"
      description="Configure conference chat access and visibility. Changes are checked against the current collaboration version."
      onOpen={() => {
        const policy = state.data?.policy;
        reviewedChatVersion.current = state.data?.collaborationVersion;
        if (policy) {
          setChatDraft({
            chatEnabled: policy.chatEnabled,
            chatSendMode: policy.chatSendMode,
            privateChatMode: policy.privateChatMode,
            breakoutChatEnabled: policy.breakoutChatEnabled,
            mainChatVisibleInBreakouts: policy.mainChatVisibleInBreakouts,
          });
        }
      }}
      submit={async () => {
        if (reviewedChatVersion.current === undefined) throw new Error("Reload the conference policy before saving.");
        await updateChat.mutateAsync({ collaborationVersion: reviewedChatVersion.current, ...chatDraft });
      }}
    >
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={chatDraft.chatEnabled === true} onChange={e => setChatDraft(value => ({ ...value, chatEnabled: e.target.checked }))} />
        Enable conference chat
      </label>
      <label className="grid gap-2 text-sm font-medium">Who can send chat?
        <select className="h-10 rounded-md border border-input bg-background px-3" value={chatDraft.chatSendMode} onChange={e => setChatDraft(value => ({ ...value, chatSendMode: e.target.value as ChatSendMode }))}>
          <option value="EVERYONE">Everyone</option>
          <option value="HOSTS_ONLY">Hosts only</option>
        </select>
      </label>
      <label className="grid gap-2 text-sm font-medium">Private chat
        <select className="h-10 rounded-md border border-input bg-background px-3" value={chatDraft.privateChatMode} onChange={e => setChatDraft(value => ({ ...value, privateChatMode: e.target.value as PrivateChatMode }))}>
          <option value="EVERYONE">Everyone</option>
          <option value="HOSTS_ONLY">Hosts only</option>
          <option value="DISABLED">Disabled</option>
        </select>
      </label>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={chatDraft.breakoutChatEnabled === true} onChange={e => setChatDraft(value => ({ ...value, breakoutChatEnabled: e.target.checked }))} />
        Enable chat in breakouts
      </label>
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={chatDraft.mainChatVisibleInBreakouts === true} onChange={e => setChatDraft(value => ({ ...value, mainChatVisibleInBreakouts: e.target.checked }))} />
        Show main chat while in a breakout
      </label>
    </ActionDialog>
    </>
  );
}

"use client";

import { useState } from "react";
import { X, Send } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import ParticipantsPanel from "@/components/conferences/ParticipantsPanel";
import ConferenceRecordingsPanel from "@/components/conferences/ConferenceRecordingsPanel";
import BreakoutRoomsPanel from "@/components/room/BreakoutRoomsPanel";
import PollsPanel from "@/components/room/PollsPanel";
import QAPanel from "@/components/room/QAPanel";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCurrentParticipant, useParticipantsV2, useWaitingRoomV2, useAdmitParticipants, useDenyParticipants } from "@/hooks/v2/useParticipants";
import { useConference } from "@/hooks/v2/useConferences";
import { useSpace } from "@/hooks/v2/useSpaces";
import { useCollaborationState, useDeleteMessage, useLoadOlderMessages, useMessages, useSendMessage } from "@/hooks/v2";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import type { BulkAdmissionResponse, DenyRequest, MessageListQuery, MessageView, ParticipantResponse } from "@/types/v2";
import { ActionDialog, Field } from "@/components/meeting-spaces/ManagementUI";

export type SidebarPanel = "people" | "chat" | "breakouts" | "polls" | "qa" | "recordings";
interface SidebarProps {
 initialPanel?: SidebarPanel;
  conferenceId?: string;
  meetingId?: string;
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

export default function Sidebar({ conferenceId, meetingId, participantId, onClose, initialPanel = "people" }: SidebarProps) {
  const id = conferenceId || meetingId || "";
  const [activePanel, setActivePanel] = useState<SidebarPanel>(initialPanel);
  const me = useCurrentParticipant(id);
  const conference = useConference(id);
  const space = useSpace(conference.data?.meetingSpaceId || "");
  const effectiveRole = effectiveConferenceRole(space.data?.viewerRole, me.data?.role, me.data?.admissionState);
  const canAdmit = meetingAllows(effectiveRole, "participant.admit", space.data?.rolePolicy);
  const waiting = useWaitingRoomV2(id, canAdmit);
  const people = useParticipantsV2(id);
  const collaboration = useCollaborationState(id);
  const policy = collaboration.data?.policy;
  const inBreakout = collaboration.data?.me?.mediaContext.kind === "BREAKOUT";
  const breakoutId = collaboration.data?.me?.mediaContext.breakoutId;
  const canModerateChat = meetingAllows(effectiveRole, "chat.moderate", space.data?.rolePolicy);
  const chatRestricted = collaboration.data?.me?.chatRestricted === true;
  const chatOptions: { value: "MAIN" | "BREAKOUT" | "PRIVATE"; label: string }[] = [];
  if ((!policy || policy.chatEnabled || canModerateChat) && (!inBreakout || policy?.mainChatVisibleInBreakouts || canModerateChat)) {
    chatOptions.push({ value: "MAIN", label: "Main chat" });
  }
  if (inBreakout && breakoutId && (policy?.breakoutChatEnabled || canModerateChat)) {
    chatOptions.push({ value: "BREAKOUT", label: "Breakout chat" });
  }
  if (policy?.privateChatMode !== "DISABLED") {
    chatOptions.push({ value: "PRIVATE", label: "Private chat" });
  }
  const [chatScope, setChatScope] = useState<"MAIN" | "BREAKOUT" | "PRIVATE">("MAIN");
  const [privatePeer, setPrivatePeer] = useState("");
  const scope = chatOptions.some(option => option.value === chatScope)
    ? chatScope
    : chatOptions[0]?.value || "MAIN";
  const chatQuery: MessageListQuery = scope === "BREAKOUT"
    ? { scope, ...(breakoutId ? { breakoutId } : {}) }
    : scope === "PRIVATE"
      ? { scope, ...(privatePeer ? { peerId: privatePeer } : {}) }
      : { scope: "MAIN" };
  const messages = useMessages(id, chatQuery, chatOptions.some(option => option.value === scope) && (scope !== "PRIVATE" || !!privatePeer));
  const loadOlder = useLoadOlderMessages(id, chatQuery);
  const send = useSendMessage(id);
  const deleteMessage = useDeleteMessage(id);
  const peer = people.data?.find(person => person.id === privatePeer);
  const peerCanModerateChat = meetingAllows(peer?.role, "chat.moderate", space.data?.rolePolicy);
  const scopeAllowedByPolicy = scope === "PRIVATE"
    ? policy?.privateChatMode !== "DISABLED" &&
      (policy?.privateChatMode !== "HOSTS_ONLY" || canModerateChat || peerCanModerateChat) &&
      (!!policy?.chatEnabled || canModerateChat)
    : !!policy?.chatEnabled && policy.chatSendMode !== "HOSTS_ONLY" &&
      (scope !== "BREAKOUT" || policy.breakoutChatEnabled);
  const canSendChat = !!collaboration.data?.me && (canModerateChat || (!chatRestricted && scopeAllowedByPolicy));
  const canManageRoom = !!effectiveRole && ["OWNER", "HOST", "CO_HOST", "MODERATOR"].includes(effectiveRole);
  const canManageBreakouts = !!effectiveRole && ["OWNER", "HOST", "CO_HOST"].includes(effectiveRole);
  const canSeeRecordings = meetingAllows(effectiveRole, "artifact.list", space.data?.rolePolicy) ||
    (space.data?.recordingPolicy !== "DISABLED" && (
      meetingAllows(effectiveRole, "recording.start", space.data?.rolePolicy) ||
      meetingAllows(effectiveRole, "recording.stop", space.data?.rolePolicy)
    ));
  const [draft, setDraft] = useState("");
  const pageKey = `${scope}:${scope === "PRIVATE" ? privatePeer : ""}:${scope === "BREAKOUT" ? breakoutId : ""}`;
  const [olderState, setOlderState] = useState<{ key: string; messages: MessageView[]; cursor?: string }>({ key: "", messages: [] });
  const olderMessages = olderState.key === pageKey ? olderState.messages : [];
  const olderCursor = olderState.key === pageKey ? olderState.cursor : undefined;
  const myParticipantId = collaboration.data?.me?.participantId || participantId;
  const loadOlderMessages = async () => {
    const cursor = olderCursor || messages.data?.nextCursor;
    if (!cursor) return;
    try {
      const page = await loadOlder.mutateAsync(cursor);
      setOlderState(current => ({
        key: pageKey,
        messages: [...(current.key === pageKey ? current.messages : []), ...page.messages],
        cursor: page.nextCursor || undefined,
      }));
    } catch { /* Mutation error is displayed below. */ }
  };
  const handleSend = async () => {
    const text = draft.trim();
    if (!text || send.isPending) return;
    try {
      await send.mutateAsync({
        body: text,
        ...(scope === "BREAKOUT" && breakoutId ? { breakoutId } : {}),
        ...(scope === "PRIVATE" && privatePeer ? { recipientParticipantId: privatePeer } : {}),
      });
      setDraft(current => current.trim() === text ? "" : current);
    } catch { /* Mutation error is displayed below. */ }
  };
  return <div className="flex h-full flex-col gap-4 rounded-2xl bg-card p-4">
    <header className="flex items-center justify-between"><h2 className="font-semibold">Conference</h2><Button aria-label="Close conference sidebar" variant="ghost" size="icon" onClick={onClose}><X className="size-4" /></Button></header>
    <nav aria-label="Conference tools" className="grid grid-cols-6 gap-1 border-b border-border pb-3">
      {([
        ["people", "People"],
        ...(canSeeRecordings ? [["recordings", "Recordings"] as const] : []),
        ["chat", "Chat"],
        ...(policy?.maxBreakouts !== 0 ? [["breakouts", "Breakouts"] as const] : []),
        ...(policy?.pollsEnabled !== false ? [["polls", "Polls"] as const] : []),
        ...(policy?.qnaEnabled !== false ? [["qa", "Q&A"] as const] : []),
      ] as const).map(([panel, label]) => (
        <button key={panel} type="button" aria-pressed={activePanel === panel}
          onClick={() => setActivePanel(panel)}
          className={`rounded-md px-1 py-2 text-xs font-medium ${activePanel === panel ? "bg-primary text-primary-foreground" : "text-muted-foreground hover:bg-muted"}`}>
          {label}
        </button>
      ))}
    </nav>
    {activePanel === "people" && <div className="min-h-0 flex-1 space-y-4 overflow-y-auto"><ParticipantsPanel conferenceId={id} spaceRole={space.data?.viewerRole} policy={space.data?.rolePolicy} />
      {canAdmit && <section className="space-y-2"><h3 className="font-semibold text-sm">Waiting room</h3>{waiting.isLoading && <p role="status" className="text-sm">Loading waiting room…</p>}{waiting.isError && <RequestError error={waiting.error} retry={() => void waiting.refetch()} />}{waiting.data && <WaitingQueue id={id} people={waiting.data.participants} live={conference.data?.status === "LIVE"} canAdmit={canAdmit} canDeny={meetingAllows(effectiveRole, "participant.deny", space.data?.rolePolicy)} />}</section>}
    </div>}
    {activePanel === "recordings" && canSeeRecordings && <div className="min-h-0 flex-1 overflow-y-auto"><ConferenceRecordingsPanel
      conferenceId={id}
      spaceRole={space.data?.viewerRole}
      rolePolicy={space.data?.rolePolicy}
      recordingPolicy={space.data?.recordingPolicy}
      live={conference.data?.status === "LIVE"}
    /></div>}
    {activePanel === "chat" && <section className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-border"><div className="flex flex-wrap items-center justify-between gap-2 border-b border-border p-3"><h3 className="text-sm font-medium">Chat</h3><select aria-label="Chat scope" className="h-9 max-w-40 rounded-md border border-input bg-background px-2 text-sm" value={scope} onChange={event => setChatScope(event.target.value as typeof scope)} disabled={chatOptions.length < 2}>{chatOptions.map(option => <option key={option.value} value={option.value}>{option.label}</option>)}</select></div>
      {scope === "PRIVATE" && <label className="grid gap-1 border-b border-border p-3 text-xs font-medium">Private conversation<select aria-label="Private conversation participant" className="h-9 rounded-md border border-input bg-background px-2 text-sm" value={privatePeer} onChange={event => setPrivatePeer(event.target.value)}><option value="">Select a participant</option>{(people.data || []).filter(person => person.id !== collaboration.data?.me?.participantId).map(person => <option key={person.id} value={person.id}>{person.displayName}</option>)}</select></label>}
      <div className="flex-1 space-y-3 overflow-y-auto bg-muted/40 p-3">
      {messages.isLoading && <p role="status" className="text-sm">Loading messages…</p>}{messages.isError && <RequestError error={messages.error} retry={() => void messages.refetch()} />}
      {(olderMessages.length === 0 && messages.data?.messages.length === 0) && <p className="text-sm text-muted-foreground">No messages yet.</p>}
      {(olderCursor || (olderMessages.length === 0 && messages.data?.nextCursor)) && <Button type="button" variant="outline" size="sm" className="self-center" disabled={loadOlder.isPending} onClick={() => void loadOlderMessages()}>{loadOlder.isPending ? "Loading older messages…" : "Load older messages"}</Button>}
      {[...(messages.data?.messages ?? []), ...olderMessages].reverse().map(message => <div key={message.id} className={`flex flex-col ${message.senderParticipantId === myParticipantId ? "items-end" : "items-start"}`}><p className="mb-1 text-xs text-muted-foreground">{message.senderDisplayName}{message.kind !== "MESSAGE" ? ` · ${message.kind.replaceAll("_", " ").toLowerCase()}` : ""}</p><p className="max-w-full break-words rounded-lg border border-border bg-card px-3 py-2 text-sm">{message.deleted ? "Message deleted" : message.body}</p>{!message.deleted && (message.senderParticipantId === myParticipantId || (scope !== "PRIVATE" && canModerateChat)) && <ActionDialog label="Delete message" destructive description="Hide this message from the conference chat." submit={async () => { await deleteMessage.mutateAsync(message.id); }} />}</div>)}
      {!chatOptions.length && <p className="text-sm text-muted-foreground">Chat is unavailable in this room.</p>}
      {scope === "PRIVATE" && !privatePeer && <p className="text-sm text-muted-foreground">Select an admitted participant to open a private conversation.</p>}
    </div><form className="space-y-2 border-t border-border p-3" onSubmit={event => { event.preventDefault(); void handleSend(); }}>{send.isError && <RequestError error={send.error} />}{loadOlder.isError && <RequestError error={loadOlder.error} />}{deleteMessage.isError && <RequestError error={deleteMessage.error} />}{!canSendChat && chatOptions.length > 0 && <p className="text-xs text-muted-foreground">{chatRestricted ? "Chat is disabled for your participant." : "Chat is currently read-only for your role or this chat scope."}</p>}<div className="flex gap-2"><Input aria-label="Chat message" placeholder={canSendChat ? "Type a message…" : "Chat is read-only"} value={draft} onChange={e => setDraft(e.target.value)} disabled={!canSendChat || (scope === "PRIVATE" && !privatePeer)} /><Button aria-label="Send message" size="icon" type="submit" disabled={!draft.trim() || send.isPending || !canSendChat || (scope === "PRIVATE" && !privatePeer)}><Send className="size-4" /></Button></div></form></section>}
    {activePanel === "breakouts" && <BreakoutRoomsPanel conferenceId={id} isHost={canManageBreakouts} onClose={() => setActivePanel("people")} />}
    {activePanel === "polls" && <PollsPanel conferenceId={id} isHost={canManageRoom} onClose={() => setActivePanel("chat")} />}
    {activePanel === "qa" && <QAPanel conferenceId={id} isHost={canManageRoom} anonymousAllowed={policy?.qnaAnonymousAllowed} onClose={() => setActivePanel("chat")} />}
  </div>;
}

function WaitingQueue({ id, people, live, canAdmit, canDeny }: { id: string; people: ParticipantResponse[]; live: boolean; canAdmit: boolean; canDeny: boolean }) {
  const admit = useAdmitParticipants(id), deny = useDenyParticipants(id);
  const [result, setResult] = useState<BulkAdmissionResponse>();
  const pending = admit.isPending || deny.isPending;
  const runAdmit = async (participantId?: string) => {
    try { setResult(await admit.mutateAsync(participantId ? { participantIds: [participantId] } : { all: true })); } catch { /* Mutation error is displayed below. */ }
  };
  return <div className="space-y-2">
    {!live && <p className="text-xs text-muted-foreground">Start the conference to admit waiting people.</p>}
    {admit.isError && <RequestError error={admit.error} />}{deny.isError && <RequestError error={deny.error} />}
    {result && <p role="status" className="text-xs text-muted-foreground">Changed {result.changed.length}; unchanged {result.unchanged.length}; skipped {result.skipped.length}.{result.skipped.map(item => ` ${item.participantId}: ${item.admissionState}`).join("")}</p>}
    {people.length === 0 ? <p className="text-sm text-muted-foreground">Nobody is waiting.</p> : <>{canAdmit && <Button size="sm" variant="outline" disabled={pending || !live} onClick={() => void runAdmit()}>Admit all ({people.length})</Button>}{canDeny && <WaitingDenyAction label={`Deny all (${people.length})`} disabled={pending} submit={async data => setResult(await deny.mutateAsync({ ...data, all: true }))} />}{people.map(person => <div key={person.id} className="space-y-2 rounded-md border border-border p-2"><p className="break-words text-sm">{person.displayName}</p><div className="flex gap-2">{canAdmit && <Button size="sm" disabled={pending || !live} onClick={() => void runAdmit(person.id)}>Admit</Button>}{canDeny && <WaitingDenyAction label="Deny" disabled={pending} submit={async data => setResult(await deny.mutateAsync({ ...data, participantIds: [person.id] }))} />}</div></div>)}</>}
  </div>;
}

function WaitingDenyAction({ label, disabled, submit }: {
  label: string;
  disabled: boolean;
  submit: (data: DenyRequest) => Promise<unknown>;
}) {
  const [reasonCode, setReasonCode] = useState("");
  const [message, setMessage] = useState("");
  const [internalNote, setInternalNote] = useState("");

  return (
    <ActionDialog
      label={label}
      title="Deny admission"
      description="The message is shown to the participant. The internal note is visible only to meeting administrators."
      destructive
      disabled={disabled}
      submit={() => submit({
        ...(reasonCode.trim() ? { reasonCode: reasonCode.trim() } : {}),
        ...(message.trim() ? { message: message.trim() } : {}),
        ...(internalNote.trim() ? { internalNote: internalNote.trim() } : {}),
      })}
    >
      <Field label="Reason code (optional)" value={reasonCode} maxLength={64} onChange={event => setReasonCode(event.target.value)} />
      <Field label="Message to participant (optional)" value={message} maxLength={500} onChange={event => setMessage(event.target.value)} />
      <Field label="Internal note (optional)" value={internalNote} onChange={event => setInternalNote(event.target.value)} />
    </ActionDialog>
  );
}

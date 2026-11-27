"use client";

import { useState } from "react";
import { X, Send, UserPlus, Check, Trash2, MessageCircle } from "lucide-react";
import { NiceSelect } from "@/components/ui/nice-select";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { cn } from "@/lib/utils";
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
import type { DenyRequest, MessageListQuery, MessageView, ParticipantResponse } from "@/types/v2";
import { ActionDialog, Field } from "@/components/meeting-spaces/ManagementUI";

export type SidebarPanel = "people" | "chat" | "breakouts" | "polls" | "qa" | "recordings";
interface SidebarProps {
 initialPanel?: SidebarPanel;
  conferenceId: string;
  participantId: string;
  onClose: () => void;
}

export default function Sidebar({ conferenceId, participantId, onClose, initialPanel = "people" }: SidebarProps) {
  const id = conferenceId;
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
  const TITLES: Record<SidebarPanel, string> = { people: "People", chat: "In-call messages", breakouts: "Breakout rooms", polls: "Polls", qa: "Q&A", recordings: "Recordings" };
  const [copied, setCopied] = useState(false);
  const meetingCode = space.data?.meetingCode;
  const copyInvite = () => {
    if (!meetingCode) return;
    void navigator.clipboard.writeText(`${window.location.origin}/${meetingCode}`);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };
  const waitingPeople = waiting.data?.participants ?? [];
  const thread = [...(messages.data?.messages ?? []), ...olderMessages].reverse();

  return <div className="flex h-full flex-col overflow-hidden rounded-2xl bg-card text-foreground">
    <header className="flex h-16 shrink-0 items-center justify-between px-5">
      <h2 className="text-lg font-semibold">{TITLES[activePanel]}</h2>
      <button type="button" aria-label="Close panel" onClick={onClose} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><X className="size-5" /></button>
    </header>

    {activePanel === "people" && <div className="no-scrollbar min-h-0 flex-1 space-y-5 overflow-y-auto px-5 pb-5">
      {meetingCode && <button type="button" onClick={copyInvite} className="inline-flex h-10 items-center gap-2 rounded-full bg-primary/15 px-4 text-sm font-semibold text-primary hover:bg-primary/20">
        {copied ? <Check className="size-4" /> : <UserPlus className="size-4" />}{copied ? "Invite link copied" : "Add people"}
      </button>}

      {canAdmit && waitingPeople.length > 0 && <WaitingQueue id={id} people={waitingPeople} live={conference.data?.status === "LIVE"} canAdmit={canAdmit} canDeny={meetingAllows(effectiveRole, "participant.deny", space.data?.rolePolicy)} />}
      {canAdmit && waiting.isError && <RequestError error={waiting.error} retry={() => void waiting.refetch()} />}

      <ParticipantsPanel conferenceId={id} spaceRole={space.data?.viewerRole} policy={space.data?.rolePolicy} />
    </div>}

    {activePanel === "recordings" && canSeeRecordings && <div className="no-scrollbar min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-5 pb-5"><ConferenceRecordingsPanel
      conferenceId={id}
      spaceRole={space.data?.viewerRole}
      rolePolicy={space.data?.rolePolicy}
      recordingPolicy={space.data?.recordingPolicy}
      live={conference.data?.status === "LIVE"}
    /></div>}

    {activePanel === "chat" && <section className="flex min-h-0 flex-1 flex-col">
      {(chatOptions.length > 1 || scope === "PRIVATE") && <div className="space-y-2 px-5 pb-3">
        {chatOptions.length > 1 && <NiceSelect value={scope} onChange={setChatScope} options={chatOptions.map(o => ({ value: o.value, label: o.value === "MAIN" ? "Everyone" : o.value === "BREAKOUT" ? "This breakout room" : "Private message" }))} />}
        {scope === "PRIVATE" && <NiceSelect value={privatePeer} placeholder="Choose who to message" onChange={setPrivatePeer} options={(people.data || []).filter(person => person.id !== collaboration.data?.me?.participantId).map(person => ({ value: person.id, label: person.displayName }))} />}
      </div>}

      <div className="no-scrollbar flex-1 space-y-4 overflow-y-auto px-5 py-2">
        {messages.isLoading && <div aria-busy="true" className="space-y-3">{[0, 1, 2].map(i => <div key={i} className="h-12 animate-pulse rounded-xl bg-muted" />)}</div>}
        {messages.isError && <RequestError error={messages.error} retry={() => void messages.refetch()} />}
        {(olderCursor || (olderMessages.length === 0 && messages.data?.nextCursor)) && <button type="button" className="mx-auto block text-xs font-semibold text-primary hover:underline disabled:opacity-60" disabled={loadOlder.isPending} onClick={() => void loadOlderMessages()}>{loadOlder.isPending ? "Loading…" : "Show earlier messages"}</button>}
        {!messages.isLoading && thread.length === 0 && chatOptions.length > 0 && !(scope === "PRIVATE" && !privatePeer) && (
          <div className="flex flex-col items-center py-10 text-center">
            <span className="flex size-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><MessageCircle className="size-5" /></span>
            <p className="mt-3 text-sm font-medium">No messages yet</p>
            <p className="text-sm text-muted-foreground">Messages here are only seen by people in this call.</p>
          </div>
        )}
        {thread.map(message => {
          const mine = message.senderParticipantId === myParticipantId;
          return <div key={message.id} className={cn("group flex flex-col", mine ? "items-end" : "items-start")}>
            <p className="mb-1 flex items-center gap-2 text-xs text-muted-foreground">
              <span className="font-semibold text-foreground">{mine ? "You" : message.senderDisplayName}</span>
              {message.kind !== "MESSAGE" ? <span className="rounded bg-muted px-1.5">{message.kind.replaceAll("_", " ").toLowerCase()}</span> : null}
              {message.createdAt ? <span>{new Date(message.createdAt).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })}</span> : null}
            </p>
            <div className="flex max-w-[90%] items-start gap-1.5">
              {!message.deleted && (mine || (scope !== "PRIVATE" && canModerateChat)) && <span className="order-first opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100"><DeleteMessage onDelete={() => deleteMessage.mutateAsync(message.id)} /></span>}
              <p className={cn("break-words rounded-2xl px-3.5 py-2 text-sm", message.deleted ? "italic text-muted-foreground" : mine ? "rounded-br-md bg-primary text-primary-foreground" : "rounded-bl-md bg-muted text-foreground")}>{message.deleted ? "Message deleted" : message.body}</p>
            </div>
          </div>;
        })}
        {!chatOptions.length && <p className="py-10 text-center text-sm text-muted-foreground">Chat is turned off for this call.</p>}
        {scope === "PRIVATE" && !privatePeer && <p className="py-10 text-center text-sm text-muted-foreground">Choose someone above to send them a private message.</p>}
      </div>

      <form className="space-y-2 p-4" onSubmit={event => { event.preventDefault(); void handleSend(); }}>
        {send.isError && <RequestError error={send.error} />}{loadOlder.isError && <RequestError error={loadOlder.error} />}{deleteMessage.isError && <RequestError error={deleteMessage.error} />}
        {!canSendChat && chatOptions.length > 0 && <p className="text-xs text-muted-foreground">{chatRestricted ? "A host turned off chat for you." : "Only hosts can send messages here."}</p>}
        <div className="flex items-center gap-2 rounded-full bg-muted px-2 py-1.5">
          <Input aria-label="Chat message" placeholder={canSendChat ? "Send a message" : "You can't send messages"} value={draft} onChange={e => setDraft(e.target.value)} disabled={!canSendChat || (scope === "PRIVATE" && !privatePeer)} className="h-9 flex-1 rounded-full border-0 bg-transparent px-3 text-sm shadow-none focus:ring-0 sm:h-9 sm:px-3 sm:text-sm" />
          <button aria-label="Send message" type="submit" disabled={!draft.trim() || send.isPending || !canSendChat || (scope === "PRIVATE" && !privatePeer)} className="flex size-9 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground transition-opacity disabled:opacity-30"><Send className="size-4" /></button>
        </div>
      </form>
    </section>}

    {activePanel === "breakouts" && <div className="no-scrollbar min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-5 pb-5"><BreakoutRoomsPanel conferenceId={id} isHost={canManageBreakouts} onClose={() => setActivePanel("people")} /></div>}
    {activePanel === "polls" && <div className="no-scrollbar min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-5 pb-5"><PollsPanel conferenceId={id} isHost={canManageRoom} onClose={() => setActivePanel("chat")} /></div>}
    {activePanel === "qa" && <div className="no-scrollbar min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-5 pb-5"><QAPanel conferenceId={id} isHost={canManageRoom} anonymousAllowed={policy?.qnaAnonymousAllowed} onClose={() => setActivePanel("chat")} /></div>}
  </div>;
}

function DeleteMessage({ onDelete }: { onDelete: () => Promise<unknown> }) {
  const [confirm, setConfirm] = useState(false);
  return confirm
    ? <button type="button" onClick={() => { void onDelete(); setConfirm(false); }} onBlur={() => setConfirm(false)} className="rounded-lg bg-destructive px-2 py-1 text-xs font-semibold text-white">Delete?</button>
    : <button type="button" aria-label="Delete message" onClick={() => setConfirm(true)} className="flex size-7 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-destructive"><Trash2 className="size-3.5" /></button>;
}

function WaitingQueue({ id, people, live, canAdmit, canDeny }: { id: string; people: ParticipantResponse[]; live: boolean; canAdmit: boolean; canDeny: boolean }) {
  const admit = useAdmitParticipants(id), deny = useDenyParticipants(id);
  const pending = admit.isPending || deny.isPending;
  const runAdmit = async (participantId?: string) => {
    try { await admit.mutateAsync(participantId ? { participantIds: [participantId] } : { all: true }); } catch { /* Mutation error is displayed below. */ }
  };
  return <section className="rounded-2xl border border-primary/30 bg-primary/10 p-4">
    <div className="flex items-center justify-between gap-2">
      <h3 className="text-sm font-semibold">Waiting to join · {people.length}</h3>
      {canAdmit && people.length > 1 && <button type="button" disabled={pending || !live} onClick={() => void runAdmit()} className="text-sm font-semibold text-primary hover:underline disabled:opacity-50">Admit all</button>}
    </div>
    {!live && <p className="mt-1 text-xs text-muted-foreground">Start the meeting to let people in.</p>}
    {admit.isError && <div className="mt-2"><RequestError error={admit.error} /></div>}{deny.isError && <div className="mt-2"><RequestError error={deny.error} /></div>}
    <ul className="mt-3 space-y-2">
      {people.map(person => <li key={person.id} className="flex items-center gap-3">
        <Avatar className="size-9"><AvatarFallback className="bg-primary/20 text-xs font-semibold text-primary">{person.displayName.slice(0, 2).toUpperCase()}</AvatarFallback></Avatar>
        <span className="min-w-0 flex-1 truncate text-sm font-medium">{person.displayName}</span>
        {canDeny && <WaitingDenyAction label="Deny" disabled={pending} submit={async data => deny.mutateAsync({ ...data, participantIds: [person.id] })} />}
        {canAdmit && <button type="button" disabled={pending || !live} onClick={() => void runAdmit(person.id)} className="h-9 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">Admit</button>}
      </li>)}
    </ul>
  </section>;
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
      title="Don't let them in?"
      description="You can add a short message they'll see. The internal note is only visible to meeting admins."
      destructive
      disabled={disabled}
      submit={() => submit({
        ...(reasonCode.trim() ? { reasonCode: reasonCode.trim() } : {}),
        ...(message.trim() ? { message: message.trim() } : {}),
        ...(internalNote.trim() ? { internalNote: internalNote.trim() } : {}),
      })}
    >
            <Field label="Message to them (optional)" value={message} maxLength={500} onChange={event => setMessage(event.target.value)} />
      <Field label="Internal note (optional)" value={internalNote} onChange={event => setInternalNote(event.target.value)} />
    </ActionDialog>
  );
}

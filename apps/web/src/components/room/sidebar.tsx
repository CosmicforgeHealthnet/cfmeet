"use client";

import { useState } from "react";
import { X, Send } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import ParticipantsPanel from "@/components/conferences/ParticipantsPanel";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useCurrentParticipant, useWaitingRoomV2, useAdmitParticipants, useDenyParticipants } from "@/hooks/v2/useParticipants";
import { useConference } from "@/hooks/v2/useConferences";
import { useSpace } from "@/hooks/v2/useSpaces";
import { useCollaborationState, useDeleteMessage, useLoadOlderMessages, useMessages, useSendMessage } from "@/hooks/v2";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import type { BulkAdmissionResponse, ParticipantResponse } from "@/types/v2";
import { ActionDialog } from "@/components/meeting-spaces/ManagementUI";

interface SidebarProps {
  conferenceId?: string;
  meetingId?: string;
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

export default function Sidebar({ conferenceId, meetingId, participantId, onClose }: SidebarProps) {
  const id = conferenceId || meetingId || "";
  const me = useCurrentParticipant(id);
  const conference = useConference(id);
  const space = useSpace(conference.data?.meetingSpaceId || "");
  const effectiveRole = effectiveConferenceRole(space.data?.viewerRole, me.data?.role, me.data?.admissionState);
  const canAdmit = meetingAllows(effectiveRole, "participant.admit", space.data?.rolePolicy);
  const waiting = useWaitingRoomV2(id, canAdmit);
  const messages = useMessages(id);
  const loadOlder = useLoadOlderMessages(id);
  const send = useSendMessage(id);
  const deleteMessage = useDeleteMessage(id);
  const collaboration = useCollaborationState(id);
  const canModerateChat = meetingAllows(effectiveRole, "chat.moderate", space.data?.rolePolicy);
  const canSendChat = !!collaboration.data?.me && (canModerateChat || (collaboration.data.policy.chatEnabled && collaboration.data.policy.chatSendMode !== "HOSTS_ONLY"));
  const [draft, setDraft] = useState("");
  const [olderMessages, setOlderMessages] = useState<import("@/types/v2").MessageView[]>([]);
  const [olderCursor, setOlderCursor] = useState<string>();
  const [olderHasMore, setOlderHasMore] = useState(false);
  const loadOlderMessages = async () => {
    const cursor = olderCursor || messages.data?.cursor;
    if (!cursor) return;
    try {
      const page = await loadOlder.mutateAsync(cursor);
      setOlderMessages(current => [...current, ...page.messages]);
      setOlderCursor(page.cursor);
      setOlderHasMore(page.hasMore);
    } catch { /* Mutation error is displayed below. */ }
  };
  const handleSend = async () => {
    const text = draft.trim();
    if (!text || send.isPending) return;
    try { await send.mutateAsync({ content: text }); setDraft(current => current.trim() === text ? "" : current); } catch { /* Mutation error is displayed below. */ }
  };
  return <div className="flex h-full flex-col gap-4 rounded-2xl bg-card p-4">
    <header className="flex items-center justify-between"><h2 className="font-semibold">Conference</h2><Button aria-label="Close conference sidebar" variant="ghost" size="icon" onClick={onClose}><X className="size-4" /></Button></header>
    <div className="max-h-[55%] shrink-0 space-y-4 overflow-y-auto"><ParticipantsPanel conferenceId={id} spaceRole={space.data?.viewerRole} policy={space.data?.rolePolicy} />
      {canAdmit && <section className="space-y-2"><h3 className="font-semibold text-sm">Waiting room</h3>{waiting.isLoading && <p role="status" className="text-sm">Loading waiting room…</p>}{waiting.isError && <RequestError error={waiting.error} retry={() => void waiting.refetch()} />}{waiting.data && <WaitingQueue id={id} people={waiting.data.participants} live={conference.data?.status === "LIVE"} />}</section>}
    </div>
    <section className="flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border border-border"><h3 className="border-b border-border p-3 text-sm font-medium">Chat</h3><div className="flex-1 space-y-3 overflow-y-auto bg-muted/40 p-3">
      {messages.isLoading && <p role="status" className="text-sm">Loading messages…</p>}{messages.isError && <RequestError error={messages.error} retry={() => void messages.refetch()} />}
      {messages.data?.messages.length === 0 && <p className="text-sm text-muted-foreground">No messages yet.</p>}
      {(olderHasMore || (olderMessages.length === 0 && messages.data?.hasMore)) && <Button type="button" variant="outline" size="sm" className="self-center" disabled={loadOlder.isPending} onClick={() => void loadOlderMessages()}>{loadOlder.isPending ? "Loading older messages…" : "Load older messages"}</Button>}
      {(messages.data?.messages.length ?? 0) === 0 && olderMessages.length === 0 && <p className="text-sm text-muted-foreground">No messages yet.</p>}
      {[...(messages.data?.messages ?? []), ...olderMessages].reverse().map(message => <div key={message.id} className={`flex flex-col ${message.senderId === participantId ? "items-end" : "items-start"}`}><p className="mb-1 text-xs text-muted-foreground">{message.senderDisplayName}</p><p className="max-w-full break-words rounded-lg border border-border bg-card px-3 py-2 text-sm">{message.deletedAt ? "Message deleted" : message.content}</p>{!message.deletedAt && (message.senderId === participantId || canModerateChat) && <ActionDialog label="Delete message" destructive description="Hide this message from the conference chat." submit={async () => { await deleteMessage.mutateAsync(message.id); }} />}</div>)}
    </div><form className="space-y-2 border-t border-border p-3" onSubmit={event => { event.preventDefault(); void handleSend(); }}>{send.isError && <RequestError error={send.error} />}{loadOlder.isError && <RequestError error={loadOlder.error} />}{deleteMessage.isError && <RequestError error={deleteMessage.error} />}{!canSendChat && <p className="text-xs text-muted-foreground">Chat is currently read-only for your role.</p>}<div className="flex gap-2"><Input aria-label="Chat message" placeholder={canSendChat ? "Type a message…" : "Chat is read-only"} value={draft} onChange={e => setDraft(e.target.value)} disabled={!canSendChat} /><Button aria-label="Send message" size="icon" type="submit" disabled={!draft.trim() || send.isPending || !canSendChat}><Send className="size-4" /></Button></div></form></section>
  </div>;
}

function WaitingQueue({ id, people, live }: { id: string; people: ParticipantResponse[]; live: boolean }) {
  const admit = useAdmitParticipants(id), deny = useDenyParticipants(id);
  const [result, setResult] = useState<BulkAdmissionResponse>();
  const pending = admit.isPending || deny.isPending;
  const run = async (action: "admit" | "deny", participantId?: string) => {
    try { setResult(await (action === "admit" ? admit : deny).mutateAsync(participantId ? { participantIds: [participantId] } : { all: true })); } catch { /* Mutation error is displayed below. */ }
  };
  return <div className="space-y-2">
    {!live && <p className="text-xs text-muted-foreground">Start the conference to admit waiting people.</p>}
    {admit.isError && <RequestError error={admit.error} />}{deny.isError && <RequestError error={deny.error} />}
    {result && <p role="status" className="text-xs text-muted-foreground">Changed {result.changed.length}; unchanged {result.unchanged.length}; skipped {result.skipped.length}.{result.skipped.map(item => ` ${item.participantId}: ${item.admissionState}`).join("")}</p>}
    {people.length === 0 ? <p className="text-sm text-muted-foreground">Nobody is waiting.</p> : <><Button size="sm" variant="outline" disabled={pending || !live} onClick={() => void run("admit")}>Admit all ({people.length})</Button>{people.map(person => <div key={person.id} className="space-y-2 rounded-md border border-border p-2"><p className="break-words text-sm">{person.displayName}</p><div className="flex gap-2"><Button size="sm" disabled={pending || !live} onClick={() => void run("admit", person.id)}>Admit</Button><Button size="sm" variant="outline" disabled={pending} onClick={() => void run("deny", person.id)}>Deny</Button></div></div>)}</>}
  </div>;
}

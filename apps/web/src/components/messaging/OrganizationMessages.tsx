"use client";

/**
 * Organization Messages (final closure, J-10): the conversations of the selected
 * organization workspace, in the organization's OWN Messaging scope. They are separate
 * from personal conversations, MANAGED (readable by CF Meet's servers, and labelled so),
 * and only active members of an active organization whose policy allows Messaging reach
 * them — the server decides every request; this screen only explains its answers.
 * Message text is rendered only as text (React escapes it).
 */
import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Building2, Send, ShieldAlert, UserPlus } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { useAccountProfile } from "@/hooks/useAccount";
import { MessagingApiError } from "@/lib/messaging/errors";
import {
  addableColleagues,
  colleagueLabel,
  organizationMessagingProblem,
  type OrganizationMember,
} from "@/lib/messaging/organization";
import { businessOrganizationsService } from "@/services/v2/business-organizations.service";
import { organizationMessaging, type OrganizationMessage } from "@/services/v2/organization-messaging.service";

function problemOf(error: unknown): string {
  if (error instanceof MessagingApiError) {
    return organizationMessagingProblem(error.code, error.status) ?? error.message;
  }
  const status = (error as { response?: { status?: number; data?: { error?: { code?: string } } } })?.response;
  return organizationMessagingProblem(status?.data?.error?.code, status?.status ?? 0) ?? "The request failed. Please try again.";
}

export default function OrganizationMessages({ organizationId, organizationName }: { organizationId: string; organizationName?: string | null }) {
  const api = useMemo(() => organizationMessaging(organizationId), [organizationId]);
  const queryClient = useQueryClient();
  const me = useAccountProfile().data?.userId ?? null;
  const [selected, setSelected] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [creating, setCreating] = useState(false);
  const [title, setTitle] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const keys = {
    conversations: ["org-messaging", organizationId, "conversations"] as const,
    messages: (id: string) => ["org-messaging", organizationId, "messages", id] as const,
  };

  const conversations = useQuery({ queryKey: keys.conversations, queryFn: () => api.conversations(), retry: false, refetchInterval: 15_000 });
  const messages = useQuery({
    queryKey: keys.messages(selected ?? "none"),
    queryFn: () => api.messages(selected as string),
    enabled: !!selected,
    retry: false,
    refetchInterval: 5_000,
  });
  const members = useQuery({
    queryKey: ["workspace", organizationId, "members"],
    queryFn: () => businessOrganizationsService.members(organizationId),
    enabled: creating,
    retry: false,
  });

  const send = useMutation({
    mutationFn: (text: string) => api.send(selected as string, text),
    onSuccess: () => {
      setDraft("");
      void queryClient.invalidateQueries({ queryKey: keys.messages(selected as string) });
    },
  });
  const create = useMutation({
    mutationFn: async () => {
      const conversation = await api.createGroup(title.trim() || "Team conversation");
      if (picked.length) {
        const { people } = await api.resolveMembers(picked);
        for (const p of people) await api.addMember(conversation.conversationId, p.principalId);
      }
      return conversation;
    },
    onSuccess: (conversation) => {
      setCreating(false);
      setTitle("");
      setPicked([]);
      setSelected(conversation.conversationId);
      void queryClient.invalidateQueries({ queryKey: keys.conversations });
    },
  });

  const blocking = conversations.error ? problemOf(conversations.error) : null;
  const colleagues = addableColleagues((members.data ?? []) as OrganizationMember[], me);
  const list = conversations.data?.conversations ?? [];
  const ordered: OrganizationMessage[] = [...(messages.data?.messages ?? [])].sort((a, b) => a.sequence - b.sequence);

  return (
    <div className="flex h-full min-h-[480px] flex-col gap-3 p-4" data-testid="organization-messages">
      <header className="flex items-center gap-2">
        <Building2 className="h-5 w-5" aria-hidden />
        <h1 className="text-lg font-semibold">{organizationName ? `${organizationName} messages` : "Organization messages"}</h1>
      </header>
      <p className="flex items-center gap-1 text-xs text-muted-foreground" data-testid="organization-messages-notice">
        <ShieldAlert className="h-3.5 w-3.5" aria-hidden />
        Organization conversations are readable by CF Meet&apos;s servers and kept under your organization&apos;s policy. They are separate from your personal messages.
      </p>
      {blocking ? (
        <p role="alert" className="rounded-md border p-3 text-sm" data-testid="organization-messages-problem">{blocking}</p>
      ) : (
        <div className="grid flex-1 grid-cols-1 gap-3 md:grid-cols-[260px_1fr]">
          <aside className="flex flex-col gap-2 rounded-md border p-2">
            <Button size="sm" variant="outline" onClick={() => setCreating((v) => !v)} data-testid="organization-new-conversation">
              <UserPlus className="mr-1 h-4 w-4" aria-hidden /> New group
            </Button>
            {creating && (
              <form
                className="flex flex-col gap-2 rounded-md bg-muted/40 p-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  create.mutate();
                }}
              >
                <Input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Conversation name" maxLength={120} aria-label="Conversation name" />
                {members.isLoading && <Spinner />}
                {members.error && <p role="alert" className="text-xs">{problemOf(members.error)}</p>}
                <ul className="max-h-48 overflow-auto text-sm">
                  {colleagues.map((c) => (
                    <li key={c.userId}>
                      <label className="flex items-center gap-2">
                        <input
                          type="checkbox"
                          checked={picked.includes(c.userId)}
                          onChange={(e) => setPicked((p) => (e.target.checked ? [...p, c.userId] : p.filter((x) => x !== c.userId)))}
                        />
                        {colleagueLabel(c)}
                      </label>
                    </li>
                  ))}
                </ul>
                {create.error && <p role="alert" className="text-xs">{problemOf(create.error)}</p>}
                <Button size="sm" type="submit" disabled={create.isPending}>Create</Button>
              </form>
            )}
            {conversations.isLoading && <Spinner />}
            <ul className="flex flex-col gap-1" data-testid="organization-conversation-list">
              {list.map((c) => (
                <li key={c.conversationId}>
                  <button
                    type="button"
                    className={cn("w-full rounded px-2 py-1 text-left text-sm hover:bg-muted", selected === c.conversationId && "bg-muted font-medium")}
                    onClick={() => setSelected(c.conversationId)}
                  >
                    {c.title || "Conversation"}
                  </button>
                </li>
              ))}
              {!conversations.isLoading && list.length === 0 && <li className="text-xs text-muted-foreground">No organization conversations yet.</li>}
            </ul>
          </aside>
          <section className="flex flex-col rounded-md border">
            {!selected ? (
              <p className="m-auto text-sm text-muted-foreground">Choose a conversation.</p>
            ) : (
              <>
                <ol className="flex-1 space-y-2 overflow-auto p-3" data-testid="organization-message-list">
                  {messages.error && <li role="alert" className="text-sm">{problemOf(messages.error)}</li>}
                  {ordered.map((m) => (
                    <li key={m.messageId} className="text-sm">
                      <span className="whitespace-pre-wrap break-words">{m.managed?.text ?? "(no text)"}</span>
                      <span className="ml-2 text-xs text-muted-foreground">{new Date(m.createdAt).toLocaleTimeString()}</span>
                    </li>
                  ))}
                </ol>
                <form
                  className="flex gap-2 border-t p-2"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (draft.trim()) send.mutate(draft.trim());
                  }}
                >
                  <Input value={draft} onChange={(e) => setDraft(e.target.value)} placeholder="Message" maxLength={4000} aria-label="Message" />
                  <Button type="submit" size="sm" disabled={send.isPending || !draft.trim()} aria-label="Send">
                    <Send className="h-4 w-4" aria-hidden />
                  </Button>
                </form>
                {send.error && <p role="alert" className="px-2 pb-2 text-xs">{problemOf(send.error)}</p>}
              </>
            )}
          </section>
        </div>
      )}
    </div>
  );
}

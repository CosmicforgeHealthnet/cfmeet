"use client";

/**
 * React Query bindings of the Communications Hub (personal communications: they belong to the
 * account, not to an organization workspace). `useHubSync` follows the Hub change feed (§4)
 * and refetches exactly the areas that changed on another device.
 */
import { useEffect, useRef } from "react";
import { useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { communicationsService as hub } from "@/services/v2/communications.service";
import { areasForDomains, syncStep, type HubArea } from "@/lib/communications/comms-core";
import type { Contact, ContactNotifications, Privacy } from "@/types/communications";

export const hubKeys = {
  all: ["communications"] as const,
  area: (area: HubArea | "capabilities") => ["communications", area] as const,
  capabilities: ["communications", "capabilities"] as const,
  profile: ["communications", "profile"] as const,
  qr: ["communications", "profile", "qr"] as const,
  invites: ["communications", "profile", "invites"] as const,
  privacy: ["communications", "privacy"] as const,
  contacts: ["communications", "contacts"] as const,
  incoming: ["communications", "requests", "incoming"] as const,
  outgoing: ["communications", "requests", "outgoing"] as const,
  blocks: ["communications", "blocks"] as const,
  activity: ["communications", "timeline", "activity"] as const,
  calls: (missed: boolean) => ["communications", "timeline", "calls", missed] as const,
  timeline: (conversationId: string) => ["communications", "timeline", "conversation", conversationId] as const,
  context: (conversationId: string) => ["communications", "inbox", "context", conversationId] as const,
  inbox: ["communications", "inbox", "list"] as const,
  meetings: (conversationId: string) => ["communications", "meetings", conversationId] as const,
};

const opts = { retry: false as const, staleTime: 15_000 };

export function useHubCapabilities() {
  return useQuery({ queryKey: hubKeys.capabilities, queryFn: hub.capabilities, ...opts, staleTime: 60_000 });
}
export function useHubProfile(enabled = true) {
  return useQuery({ queryKey: hubKeys.profile, queryFn: hub.me, enabled, ...opts });
}
export function useHubPrivacy(enabled = true) {
  return useQuery({ queryKey: hubKeys.privacy, queryFn: hub.privacy, enabled, ...opts });
}
export function useHubInvites(enabled = true) {
  return useQuery({ queryKey: hubKeys.invites, queryFn: () => hub.invites(), enabled, ...opts });
}
export function useHubContacts(enabled = true) {
  return useQuery({
    queryKey: hubKeys.contacts,
    queryFn: async () => {
      const out: Contact[] = [];
      let cursor: string | null = null;
      for (let i = 0; i < 20; i++) {
        const page = await hub.contacts(cursor);
        out.push(...page.data);
        cursor = page.nextCursor;
        if (!cursor) break;
      }
      return out;
    },
    enabled,
    ...opts,
  });
}
export function useHubRequests(enabled = true) {
  const incoming = useQuery({ queryKey: hubKeys.incoming, queryFn: () => hub.incomingRequests(), enabled, ...opts });
  const outgoing = useQuery({ queryKey: hubKeys.outgoing, queryFn: () => hub.outgoingRequests(), enabled, ...opts });
  return { incoming, outgoing };
}
export function useHubBlocks(enabled = true) {
  return useQuery({ queryKey: hubKeys.blocks, queryFn: () => hub.blocks(), enabled, ...opts });
}
export function useHubActivity(enabled = true) {
  return useQuery({ queryKey: hubKeys.activity, queryFn: () => hub.activity(), enabled, ...opts, refetchInterval: 60_000 });
}
export function useHubCallHistory(missed: boolean, enabled = true) {
  return useQuery({ queryKey: hubKeys.calls(missed), queryFn: () => hub.callHistory(missed || undefined), enabled, ...opts });
}
/** Timeline cards of one chat; `stale` cards are refreshed by polling (no Hub realtime channel). */
export function useConversationTimeline(conversationId: string | null, enabled = true) {
  return useQuery({
    queryKey: hubKeys.timeline(conversationId ?? ""),
    queryFn: () => hub.timeline(conversationId!),
    enabled: enabled && !!conversationId,
    ...opts,
    refetchInterval: (query) => (query.state.data?.stale ? 10_000 : 60_000),
  });
}
export function useConversationContext(conversationId: string | null, enabled = true) {
  return useQuery({ queryKey: hubKeys.context(conversationId ?? ""), queryFn: () => hub.conversationContext(conversationId!), enabled: enabled && !!conversationId, ...opts });
}
export function useHubInbox(enabled = true) {
  return useQuery({ queryKey: hubKeys.inbox, queryFn: () => hub.inbox(), enabled, ...opts, refetchInterval: 60_000 });
}

function invalidate(client: QueryClient, areas: Iterable<HubArea>) {
  for (const area of areas) void client.invalidateQueries({ queryKey: hubKeys.area(area) });
}

/** Mutations of the Contacts experience; each refetches the areas it changes. */
export function useHubActions() {
  const client = useQueryClient();
  const settle = (areas: HubArea[]) => () => invalidate(client, areas);
  return {
    requestContact: useMutation({ mutationFn: (cfMeetId: string) => hub.requestContact(cfMeetId), retry: false, onSettled: settle(["requests", "contacts"]) }),
    accept: useMutation({ mutationFn: (r: { requestId: string; version: number }) => hub.acceptRequest(r.requestId, r.version), retry: false, onSettled: settle(["requests", "contacts"]) }),
    decline: useMutation({ mutationFn: (r: { requestId: string; version: number }) => hub.declineRequest(r.requestId, r.version), retry: false, onSettled: settle(["requests"]) }),
    cancel: useMutation({ mutationFn: (r: { requestId: string; version: number }) => hub.cancelRequest(r.requestId, r.version), retry: false, onSettled: settle(["requests"]) }),
    block: useMutation({ mutationFn: (cfMeetId: string) => hub.block(cfMeetId), retry: false, onSettled: settle(["blocks", "contacts", "requests"]) }),
    unblock: useMutation({ mutationFn: (cfMeetId: string) => hub.unblock(cfMeetId), retry: false, onSettled: settle(["blocks", "contacts"]) }),
    removeContact: useMutation({ mutationFn: (contactId: string) => hub.removeContact(contactId), retry: false, onSettled: settle(["contacts"]) }),
    updateContact: useMutation({
      mutationFn: (p: { contactId: string; expectedVersion: number; alias?: string | null; favorite?: boolean; notifications?: ContactNotifications }) =>
        hub.updateContact(p.contactId, { expectedVersion: p.expectedVersion, alias: p.alias, favorite: p.favorite, notifications: p.notifications }),
      retry: false,
      onSettled: settle(["contacts"]),
    }),
    createInvite: useMutation({ mutationFn: (p: { expiresInSeconds: number; maxUses: number }) => hub.createInvite(p.expiresInSeconds, p.maxUses), retry: false, onSettled: settle(["profile"]) }),
    revokeInvite: useMutation({ mutationFn: (inviteId: string) => hub.revokeInvite(inviteId), retry: false, onSettled: settle(["profile"]) }),
    rotateId: useMutation({ mutationFn: (expectedVersion: number) => hub.rotatePublicId(expectedVersion), retry: false, onSettled: settle(["profile"]) }),
    updatePrivacy: useMutation({ mutationFn: (p: { version: number; privacy: Privacy }) => hub.updatePrivacy(p.version, p.privacy), retry: false, onSettled: settle(["privacy"]) }),
    report: useMutation({ mutationFn: (p: { cfMeetId: string; reason: "SPAM" | "HARASSMENT" | "IMPERSONATION" | "OTHER" }) => hub.reportProfile(p.cfMeetId, p.reason), retry: false }),
  };
}

/**
 * Follows the Hub change feed: `GET /bootstrap` → `sync.latestPosition`, then `GET /sync?after=`
 * every `intervalMs`; refetches the areas named by changed domains; `resyncRequired` reloads
 * every Hub query and continues from `latestPosition` (a gap is never acknowledged).
 */
export function useHubSync(enabled: boolean, intervalMs = 20_000) {
  const client = useQueryClient();
  const position = useRef<number | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const tick = async () => {
      try {
        if (position.current === null) {
          const boot = await hub.bootstrap();
          position.current = boot.sync.latestPosition;
          client.setQueryData(hubKeys.capabilities, boot.capabilities);
        } else {
          for (let i = 0; i < 10 && !stopped; i++) {
            const changes = await hub.sync(position.current);
            const step = syncStep(position.current, changes);
            if (step.kind === "resync") {
              position.current = step.position;
              void client.invalidateQueries({ queryKey: hubKeys.all });
              break;
            }
            invalidate(client, step.areas);
            position.current = step.position;
            if (!step.more) break;
          }
        }
      } catch {
        // Retried on the next tick; screens show their own errors.
      }
      if (!stopped) timer = setTimeout(() => void tick(), intervalMs);
    };
    void tick();
    return () => {
      stopped = true;
      if (timer) clearTimeout(timer);
    };
  }, [client, enabled, intervalMs]);
}

export { areasForDomains };

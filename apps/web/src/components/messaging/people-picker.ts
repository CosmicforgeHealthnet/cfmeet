"use client";

import { useQuery } from "@tanstack/react-query";
import { useWorkspace } from "@/context/WorkspaceContext";
import { useMessagingDirectory } from "@/hooks/v2/useMessaging";
import { useHubCapabilities, useHubContacts } from "@/hooks/v2/useCommunications";
import { organizationsService } from "@/services/v2/organizations.service";
import { colleaguesOfOrganization, communicationsAreas, connectedContacts, contactLabel, conversationPeopleSource } from "@/lib/communications/comms-core";

/**
 * The people a user can message from this workspace: their connected contacts (personal)
 * or their colleagues (organization). Shared by New chat, Add member and Forward.
 */
export function usePeoplePicker(enabled: boolean, search: string) {
  const workspace = useWorkspace();
  const source = conversationPeopleSource(workspace.kind);
  const capabilities = useHubCapabilities();
  const areas = communicationsAreas(capabilities.data);
  const contactsUsable = source === "contacts" && areas.contacts;
  const contacts = useHubContacts(enabled && contactsUsable);
  const colleaguesWanted = enabled && (source === "colleagues" || (!capabilities.isPending && !contactsUsable));
  const directory = useMessagingDirectory(search, colleaguesWanted);
  const orgId = workspace.kind === "ORGANIZATION" ? workspace.organizationId : null;
  const members = useQuery({
    queryKey: ["workspace", orgId ?? "personal", "members"],
    queryFn: () => organizationsService.listMembers(orgId!),
    enabled: colleaguesWanted && !!orgId,
    retry: false,
  });
  const colleagues = orgId ? colleaguesOfOrganization(directory.data?.people ?? [], members.data) : (directory.data?.people ?? []);
  const needle = search.trim().toLocaleLowerCase();
  const people = contactsUsable
    ? connectedContacts(contacts.data ?? [])
        .filter((c) => !needle || contactLabel(c).toLocaleLowerCase().includes(needle))
        .map((c) => ({ id: `cf:${c.cfMeetId}`, name: contactLabel(c), kind: "contact" as const, value: c.cfMeetId }))
    : colleagues.map((p) => ({ id: `u:${p.userId}`, name: p.displayName, kind: "colleague" as const, value: p.userId }));
  const pending = contactsUsable ? contacts.isPending : directory.isPending || (!!orgId && members.isPending);
  const error = contactsUsable ? contacts.error : (directory.error ?? members.error);
  const heading = contactsUsable
    ? "Your contacts"
    : orgId
      ? `Colleagues in ${workspace.workspace?.displayName ?? "this organization"}`
      : "Colleagues in your organizations";
  return { people, pending, error, heading, retry: () => void (contactsUsable ? contacts.refetch() : directory.refetch()) };
}

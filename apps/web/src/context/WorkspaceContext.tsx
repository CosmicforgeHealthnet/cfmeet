"use client";

/**
 * Workspace context provider (Blueprint Section 8 / Section 17).
 *
 *   const { workspaceId, organizationId, kind, capabilities, can, decision } = useWorkspace();
 *
 * - `workspaceId`: `"personal"` or an organization id; per browser tab (sessionStorage),
 *   seeded for new tabs from a localStorage preference. Never sent to a "switch" endpoint
 *   and never stored server-side.
 * - `organizationId`: the organization id, or null in Personal. Organization-scoped calls
 *   must pass it explicitly (path or body), exactly as the API documents.
 * - `capabilities`: the server's `GET /api/v2/me/workspaces/{id}/capabilities` answer
 *   (null while loading / unavailable). `can(name)` is default-deny.
 * - Per-context cached data: use React Query keys `workspaceQueryKey(workspaceId, ...)`.
 *   They are removed when the membership disappears (or the organization is suspended).
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import axios from "axios";
import { workspacesService } from "@/services/v2/workspaces.service";
import {
  DEFAULT_WORKSPACE_KEY,
  PERSONAL_WORKSPACE_ID,
  TAB_WORKSPACE_KEY,
  WORKSPACE_QUERY_ROOT,
  capabilityDecision,
  isWorkspaceId,
  removedWorkspaceIds,
  resolveWorkspaceId,
  selectableWorkspaces,
  type CapabilityDecision,
} from "@/lib/workspace/workspace-core";
import type { Workspace, WorkspaceAuthority, WorkspaceCapabilities } from "@/types/generated/meeting-service";

export const workspaceKeys = {
  list: () => ["me", "workspaces"] as const,
  capabilities: (workspaceId: string) => ["me", "workspaces", workspaceId, "capabilities"] as const,
};

/** Query key for data that belongs to one workspace context. */
export function workspaceQueryKey(workspaceId: string, ...rest: readonly unknown[]) {
  return [WORKSPACE_QUERY_ROOT, workspaceId, ...rest] as const;
}

export interface WorkspaceContextValue {
  workspaceId: string;
  kind: "PERSONAL" | "ORGANIZATION";
  organizationId: string | null;
  workspace: Workspace | null;
  /** Every workspace the server listed (including SUSPENDED organizations). */
  workspaces: Workspace[];
  /** Workspaces that can be selected (Personal + ACTIVE organizations). */
  selectable: Workspace[];
  authority: WorkspaceAuthority | null;
  capabilities: WorkspaceCapabilities | null;
  isLoading: boolean;
  /** The workspace list could not be read (Organizations unavailable: 503). Personal still works. */
  listError: unknown;
  capabilitiesError: unknown;
  can: (capability: string) => boolean;
  decision: (capability: string) => CapabilityDecision;
  selectWorkspace: (workspaceId: string) => void;
}

const PERSONAL_FALLBACK: WorkspaceContextValue = {
  workspaceId: PERSONAL_WORKSPACE_ID,
  kind: "PERSONAL",
  organizationId: null,
  workspace: null,
  workspaces: [],
  selectable: [],
  authority: null,
  capabilities: null,
  isLoading: false,
  listError: null,
  capabilitiesError: null,
  can: () => false,
  decision: () => ({ granted: false, reason: "UNKNOWN" }),
  selectWorkspace: () => {},
};

const WorkspaceContext = createContext<WorkspaceContextValue | null>(null);

function readStored(): string {
  if (typeof window === "undefined") return PERSONAL_WORKSPACE_ID;
  try {
    const tab = window.sessionStorage.getItem(TAB_WORKSPACE_KEY);
    if (isWorkspaceId(tab)) return tab;
    const preferred = window.localStorage.getItem(DEFAULT_WORKSPACE_KEY);
    if (isWorkspaceId(preferred)) return preferred;
  } catch {
    /* storage disabled: Personal */
  }
  return PERSONAL_WORKSPACE_ID;
}

function store(workspaceId: string) {
  try {
    window.sessionStorage.setItem(TAB_WORKSPACE_KEY, workspaceId);
    window.localStorage.setItem(DEFAULT_WORKSPACE_KEY, workspaceId);
  } catch {
    /* storage disabled: the choice lasts for this page only */
  }
}

/** Clears the workspace preference (sign-out). */
export function clearWorkspacePreference() {
  if (typeof window === "undefined") return;
  try {
    window.sessionStorage.removeItem(TAB_WORKSPACE_KEY);
    window.localStorage.removeItem(DEFAULT_WORKSPACE_KEY);
  } catch {
    /* nothing stored */
  }
}

export function WorkspaceProvider({ children }: { children: ReactNode }) {
  const queryClient = useQueryClient();
  const [requested, setRequested] = useState<string>(() => readStored());

  const list = useQuery({
    queryKey: workspaceKeys.list(),
    queryFn: () => workspacesService.list(),
    staleTime: 60_000,
    refetchOnWindowFocus: true,
    retry: (count, error) => count < 2 && !(axios.isAxiosError(error) && error.response?.status === 401),
  });

  const items = useMemo(() => list.data?.items ?? [], [list.data]);
  const workspaceId = resolveWorkspaceId(requested, list.data ? items : null);

  const capabilities = useQuery({
    queryKey: workspaceKeys.capabilities(workspaceId),
    queryFn: () => workspacesService.capabilities(workspaceId),
    staleTime: 30_000,
    refetchOnWindowFocus: true,
    retry: (count, error) =>
      count < 2 && !(axios.isAxiosError(error) && [401, 404].includes(error.response?.status ?? 0)),
  });

  // A membership that disappeared: drop its cached data and fall back to Personal.
  const previous = useRef<Workspace[] | null>(null);
  useEffect(() => {
    if (!list.data) return;
    for (const id of removedWorkspaceIds(previous.current, list.data.items)) {
      queryClient.removeQueries({ queryKey: [WORKSPACE_QUERY_ROOT, id] });
      queryClient.removeQueries({ queryKey: workspaceKeys.capabilities(id) });
    }
    previous.current = list.data.items;
  }, [list.data, queryClient]);

  // The selected organization answered 404 (membership gone): back to Personal, refresh the list.
  const capabilityStatus = axios.isAxiosError(capabilities.error) ? capabilities.error.response?.status : undefined;
  useEffect(() => {
    if (capabilityStatus === 404 && workspaceId !== PERSONAL_WORKSPACE_ID) {
      queryClient.removeQueries({ queryKey: [WORKSPACE_QUERY_ROOT, workspaceId] });
      setRequested(PERSONAL_WORKSPACE_ID);
      store(PERSONAL_WORKSPACE_ID);
      void queryClient.invalidateQueries({ queryKey: workspaceKeys.list() });
    }
  }, [capabilityStatus, workspaceId, queryClient]);

  const selectWorkspace = useCallback(
    (id: string) => {
      const next = isWorkspaceId(id) ? id : PERSONAL_WORKSPACE_ID;
      setRequested(next);
      store(next);
    },
    [],
  );

  const value = useMemo<WorkspaceContextValue>(() => {
    const workspace = items.find((w) => w.id === workspaceId) ?? null;
    const caps = capabilities.data && capabilities.data.workspaceId === workspaceId ? capabilities.data : null;
    const kind = workspaceId === PERSONAL_WORKSPACE_ID ? "PERSONAL" : "ORGANIZATION";
    return {
      workspaceId,
      kind,
      organizationId: kind === "ORGANIZATION" ? workspace?.organizationId ?? workspaceId : null,
      workspace,
      workspaces: items,
      selectable: selectableWorkspaces(items),
      authority: list.data?.authority ?? null,
      capabilities: caps,
      isLoading: list.isLoading || capabilities.isLoading,
      listError: list.error ?? null,
      capabilitiesError: capabilities.error ?? null,
      can: (name) => capabilityDecision(caps, name).granted,
      decision: (name) => capabilityDecision(caps, name),
      selectWorkspace,
    };
  }, [items, workspaceId, capabilities.data, capabilities.isLoading, capabilities.error, list.data, list.isLoading, list.error, selectWorkspace]);

  return <WorkspaceContext.Provider value={value}>{children}</WorkspaceContext.Provider>;
}

/**
 * The current tab's workspace context. Outside a WorkspaceProvider (public pages, rooms)
 * it answers Personal with no capabilities (default deny).
 */
export function useWorkspace(): WorkspaceContextValue {
  return useContext(WorkspaceContext) ?? PERSONAL_FALLBACK;
}

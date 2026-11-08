"use client";

/**
 * Developer Portal state: deployment configuration, the gateway client bound to the
 * signed-in CF Meet session, the user's developer workspaces and the selected one.
 *
 * Authentication is the existing human sign-in: the in-memory account access token
 * (src/lib/auth/session.ts) is sent as `Authorization: Bearer` to the API Gateway, whose
 * DEVELOPER_SESSION mode lets the Developer Platform verify it through the Meeting
 * Service session bridge. Developer Platform roles come from the workspace (`myRole`),
 * never from a first-party organization role.
 */
import { createContext, useCallback, useContext, useMemo, useSyncExternalStore } from "react";
import { useQuery } from "@tanstack/react-query";
import { accountSession } from "@/lib/auth/session";
import { createDeveloperClient, DeveloperApiError, type DeveloperClient } from "@/lib/developer-platform/client";
import { developerApiConfig, type DeveloperApiConfig } from "@/lib/developer-platform/config";
import { can as canDo, type ConsoleAction } from "@/lib/developer-platform/permissions";
import { pickWorkspace, SELECTED_WORKSPACE_KEY } from "@/lib/developer-platform/navigation";
import { retryDelayMs, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import type { Page, Workspace } from "@/lib/developer-platform/types";

type PortalContextValue = {
  config: DeveloperApiConfig;
  client: DeveloperClient;
  signedIn: boolean;
  workspaces: Workspace[];
  workspacesLoading: boolean;
  workspacesError: unknown;
  refetchWorkspaces: () => void;
  workspace: Workspace | null;
  selectWorkspace: (id: string) => void;
  can: (action: ConsoleAction) => boolean;
};

const PortalContext = createContext<PortalContextValue | null>(null);

const subscribeSession = (listener: () => void) => accountSession.subscribe(() => listener());
const readSignedIn = () => accountSession.hasSession();
const readSignedInServer = () => false;

// The selected workspace id lives in localStorage (an id, not a secret), read through an
// external store so the server render and hydration agree.
const workspaceListeners = new Set<() => void>();
let memoryWorkspace: string | null = null;
function readStoredWorkspace(): string | null {
  try {
    return window.localStorage.getItem(SELECTED_WORKSPACE_KEY) ?? memoryWorkspace;
  } catch {
    return memoryWorkspace;
  }
}
function storeWorkspace(id: string) {
  memoryWorkspace = id;
  try {
    window.localStorage.setItem(SELECTED_WORKSPACE_KEY, id);
  } catch {
    /* storage disabled: the in-memory selection lasts for this page */
  }
  for (const listener of workspaceListeners) listener();
}
const subscribeWorkspace = (listener: () => void) => {
  workspaceListeners.add(listener);
  return () => {
    workspaceListeners.delete(listener);
  };
};

export const WORKSPACES_QUERY_KEY = ["developer-portal", "workspaces"] as const;

/** All pages of `listOrganizations` (bounded: 10 pages of 100). */
async function listAllWorkspaces(client: DeveloperClient): Promise<Workspace[]> {
  const out: Workspace[] = [];
  let cursor: string | null = null;
  for (let i = 0; i < 10; i++) {
    const page: Page<Workspace> = await client.call<Page<Workspace>>({
      operation: "listOrganizations",
      query: { limit: 100, cursor },
    });
    out.push(...page.items);
    if (!page.nextCursor) break;
    cursor = page.nextCursor;
  }
  return out;
}

export function DeveloperPortalProvider({ children }: { children: React.ReactNode }) {
  const config = useMemo(() => developerApiConfig(), []);
  const signedIn = useSyncExternalStore(subscribeSession, readSignedIn, readSignedInServer);

  const client = useMemo(
    () =>
      createDeveloperClient({
        baseUrl: config.available ? config.baseUrl : null,
        getSession: async () => {
          try {
            return await accountSession.getAccessToken();
          } catch {
            throw new DeveloperApiError({
              status: 0,
              code: "SESSION_UNAVAILABLE",
              message: "The CF Meet sign-in service is temporarily unavailable.",
              kind: "session",
            });
          }
        },
        refreshSession: () => accountSession.refreshAccessToken(),
      }),
    [config],
  );

  const workspacesQuery = useQuery({
    queryKey: WORKSPACES_QUERY_KEY,
    queryFn: () => listAllWorkspaces(client),
    enabled: config.available && signedIn,
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
    staleTime: 30_000,
  });

  const selectedId = useSyncExternalStore(subscribeWorkspace, readStoredWorkspace, () => null);

  const workspaces = useMemo(() => workspacesQuery.data ?? [], [workspacesQuery.data]);
  const workspace = useMemo(() => pickWorkspace(workspaces, selectedId), [workspaces, selectedId]);

  const selectWorkspace = useCallback((id: string) => storeWorkspace(id), []);

  const can = useCallback((action: ConsoleAction) => canDo(workspace, action), [workspace]);

  const value: PortalContextValue = {
    config,
    client,
    signedIn,
    workspaces,
    workspacesLoading: workspacesQuery.isLoading,
    workspacesError: workspacesQuery.error,
    refetchWorkspaces: () => void workspacesQuery.refetch(),
    workspace,
    selectWorkspace,
    can,
  };

  return <PortalContext.Provider value={value}>{children}</PortalContext.Provider>;
}

export function useDeveloperPortal(): PortalContextValue {
  const value = useContext(PortalContext);
  if (!value) throw new Error("useDeveloperPortal must be used inside DeveloperPortalProvider");
  return value;
}

/** The selected workspace; pages that need one render only when it exists (see RequireWorkspace). */
export function useWorkspace(): Workspace {
  const { workspace } = useDeveloperPortal();
  if (!workspace) throw new Error("useWorkspace needs a selected developer workspace");
  return workspace;
}

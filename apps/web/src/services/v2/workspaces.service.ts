/**
 * Workspace context reads (Blueprint Section 8). There is deliberately no "switch
 * workspace" call: the selection stays in the browser tab (see src/context/WorkspaceContext.tsx).
 */
import apiV2 from "./api";
import type { Workspace, WorkspaceCapabilities, WorkspacePage } from "@/types/generated/meeting-service";

/** Upper bound on pages read for the switcher (200 items each). */
const MAX_PAGES = 5;

export const workspacesService = {
  /** Every workspace of the caller (Personal first, then organizations), following nextCursor. */
  list: async (): Promise<{ authority: WorkspacePage["authority"]; items: Workspace[] }> => {
    const items: Workspace[] = [];
    let cursor: string | null | undefined;
    let authority: WorkspacePage["authority"] = "LEGACY_DIRECTORY";
    for (let page = 0; page < MAX_PAGES; page += 1) {
      const response = await apiV2.get<WorkspacePage>("/me/workspaces", {
        params: { limit: 200, ...(cursor ? { cursor } : {}) },
      });
      authority = response.data.authority;
      items.push(...response.data.items);
      cursor = response.data.nextCursor;
      if (!cursor) break;
    }
    return { authority, items };
  },

  capabilities: async (workspaceId: string): Promise<WorkspaceCapabilities> => {
    const response = await apiV2.get<WorkspaceCapabilities>(
      `/me/workspaces/${encodeURIComponent(workspaceId)}/capabilities`,
    );
    return response.data;
  },
};

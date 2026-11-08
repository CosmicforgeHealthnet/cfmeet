"use client";

import { useMemo } from "react";
import { useWorkspace } from "@/context/WorkspaceContext";
import { dashboardNavigation, type DashboardNavItem } from "@/lib/dashboard-navigation";
import { visibleNavigation } from "@/lib/workspace/workspace-core";

/** Dashboard navigation filtered by the selected workspace's server capabilities. */
export function useDashboardNavigation(): DashboardNavItem[] {
  const { kind, capabilities } = useWorkspace();
  return useMemo(() => visibleNavigation(dashboardNavigation, { kind, capabilities }), [kind, capabilities]);
}

import { Home, Video, Phone, MessageSquare, Code2, Settings, Building2 } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { WorkspaceAwareNavItem } from "@/lib/workspace/workspace-core";

/**
 * Dashboard navigation. `feature` / `capability` / `organizationOnly` are evaluated against
 * the selected workspace's server capabilities by `useDashboardNavigation()`
 * (src/lib/use-dashboard-navigation.ts); owner services still enforce every request.
 */
export type DashboardNavItem = WorkspaceAwareNavItem & { label: string; icon: LucideIcon; href: string };

export const dashboardNavigation: DashboardNavItem[] = [
  { label: "Dashboard", icon: Home, href: "/dashboard" },
  { label: "Meetings", icon: Video, href: "/dashboard/meetings", feature: "meetings" },
  { label: "Organizations", icon: Building2, href: "/dashboard/organizations" },
  { label: "Calls", icon: Phone, href: "/calling", feature: "calling" },
  { label: "Messages", icon: MessageSquare, href: "/dashboard/messages", feature: "messaging" },
  { label: "Developers", icon: Code2, href: "/dashboard/developers" },
  { label: "Settings", icon: Settings, href: "/dashboard/settings" },
];

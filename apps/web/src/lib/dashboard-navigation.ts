import { Home, Video, CalendarDays, Phone, MessageSquare, Code2, Settings, Building2, Users, CircleDashed, UsersRound } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { WorkspaceAwareNavItem } from "@/lib/workspace/workspace-core";

/**
 * Dashboard navigation. `feature` / `capability` / `organizationOnly` are evaluated against
 * the selected workspace's server capabilities by `useDashboardNavigation()`
 * (src/lib/use-dashboard-navigation.ts); owner services still enforce every request.
 */
export type DashboardNavItem = WorkspaceAwareNavItem & { label: string; icon: LucideIcon; href: string };

export const dashboardNavigation: DashboardNavItem[] = [
  { label: "Home", icon: Home, href: "/dashboard" },
  { label: "Meetings", icon: Video, href: "/dashboard/meetings", feature: "meetings" },
  { label: "Calendar", icon: CalendarDays, href: "/dashboard/calendar", feature: "meetings" },
  { label: "Chats", icon: MessageSquare, href: "/dashboard/messages", feature: "messaging" },
  // Status (24 hours, your contacts) and channels.
  { label: "Updates", icon: CircleDashed, href: "/dashboard/updates" },
  // Communities (ADR 0073, Messaging): groups gathered under one community.
  { label: "Communities", icon: UsersRound, href: "/dashboard/communities", feature: "messaging" },
  { label: "Calls", icon: Phone, href: "/calling", feature: "calling" },
  // Personal contacts (Communications Hub); the screen gates itself on Hub capabilities.
  { label: "Contacts", icon: Users, href: "/dashboard/contacts" },
  { label: "Organizations", icon: Building2, href: "/dashboard/organizations" },
  { label: "Developers", icon: Code2, href: "/dashboard/developers" },
  { label: "Settings", icon: Settings, href: "/dashboard/settings" },
];

import { Home, Video, Calendar, Phone, MessageSquare, Code2, Settings } from "lucide-react";

export const dashboardNavigation = [
  { label: "Dashboard", icon: Home, href: "/dashboard" },
  { label: "Meetings", icon: Video, href: "/dashboard/meetings" },
  { label: "Conferences", icon: Calendar, href: "/dashboard/conferences" },
  { label: "Calls", icon: Phone, href: "/dashboard/calls" },
  { label: "Messages", icon: MessageSquare, href: "/dashboard/messages" },
  { label: "Developers", icon: Code2, href: "/dashboard/developers" },
  { label: "Settings", icon: Settings, href: "/dashboard/settings" },
];

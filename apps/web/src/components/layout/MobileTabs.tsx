"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { CalendarDays, Home, Menu, MessageSquare, Video } from "lucide-react";
import { cn } from "@/lib/utils";
import { isActivePath } from "./AppSidebar";
import { UnreadBadge, useUnreadChats } from "@/hooks/useUnreadChats";

/** Opens the full menu drawer (MobileSidebar) from the "More" tab. */
export const OPEN_MENU_EVENT = "cfmeet:open-menu";

const tabs = [
  { label: "Home", icon: Home, href: "/dashboard" },
  { label: "Meetings", icon: Video, href: "/dashboard/meetings" },
  { label: "Chats", icon: MessageSquare, href: "/dashboard/messages" },
  { label: "Schedule", icon: CalendarDays, href: "/dashboard/calendar" },
];

/** Phone bottom navigation; every other section (Calls, Contacts, Settings…) is under More. */
export default function MobileTabs() {
  const pathname = usePathname();
  const unreadChats = useUnreadChats();
  return (
    <nav
      aria-label="Main"
      className="fixed inset-x-0 bottom-0 z-40 border-t border-border bg-background/95 pb-[env(safe-area-inset-bottom)] backdrop-blur-md md:hidden"
    >
      <ul className="grid grid-cols-5">
        {tabs.map(({ label, icon: Icon, href }) => {
          const active = isActivePath(pathname, href);
          return (
            <li key={label}>
              <Link
                href={href}
                aria-current={active ? "page" : undefined}
                className={cn(
                  "flex h-16 flex-col items-center justify-center gap-1 text-[11px] font-medium",
                  active ? "font-semibold text-primary" : "text-muted-foreground",
                )}
              >
                <span className="relative">
                  <Icon className="size-5" strokeWidth={active ? 2.4 : 2} aria-hidden />
                  {href === "/dashboard/messages" ? <UnreadBadge count={unreadChats} className="absolute -right-3 -top-1.5" /> : null}
                </span>
                {label}
              </Link>
            </li>
          );
        })}
        <li>
          <button
            type="button"
            onClick={() => window.dispatchEvent(new Event(OPEN_MENU_EVENT))}
            className="flex h-16 w-full flex-col items-center justify-center gap-1 text-[11px] font-medium text-muted-foreground"
          >
            <Menu className="size-5" aria-hidden />
            More
          </button>
        </li>
      </ul>
    </nav>
  );
}

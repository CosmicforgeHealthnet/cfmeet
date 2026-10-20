"use client";

import { useState } from "react";
import Image from "next/image";
import { cn } from "@/lib/utils";
import { useRouter, usePathname } from "next/navigation";
import {
  Home,
  Video,
  Calendar,
  Code2,
  Settings,
  ChevronRight,
  ChevronLeft,
} from "lucide-react";
import ProfileMenu from "./ProfileMenu";
import { ThemeToggle } from "@/components/ui/theme-toggle";

const menu = [
  { label: "Dashboard", icon: Home, href: "/dashboard" },
  { label: "Meetings", icon: Video, href: "/dashboard/meetings" },
  { label: "Schedule", icon: Calendar, href: "/dashboard/schedule" },
  { label: "Developers", icon: Code2, href: "/dashboard/developers" },
  { label: "Settings", icon: Settings, href: "/dashboard/settings" },
];

export default function AppSidebar() {
  const router = useRouter();
  const pathname = usePathname();
  const [expanded, setExpanded] = useState(false);

  return (
    <aside
      className={cn(
        "relative h-[calc(100vh-2rem)] my-4 ml-4 bg-sidebar rounded-[40px] transition-all duration-300 shadow-sm",
        expanded ? "w-50" : "w-16",
      )}
    >
      {/* Logo */}
      <div className="h-16 flex items-center pl-4 overflow-hidden">
        {expanded ? (
          <>
            {/* Light mode full logo */}
            <Image
              src="/light-mode-cf-meet.png"
              alt="CF Meet Logo"
              width={120}
              height={32}
              className="h-8 w-auto object-contain object-left dark:hidden"
              priority
            />
            {/* Dark mode full logo */}
            <Image
              src="/dark-mode-cfmeet.png"
              alt="CF Meet Logo"
              width={120}
              height={32}
              className="h-8 w-auto object-contain object-left hidden dark:block"
              priority
            />
          </>
        ) : (
          /* Collapsed: show only the icon logo */
          <Image
            src="/CFLogo.png"
            alt="CF Meet Logo"
            width={40}
            height={40}
            className="w-[40px] min-w-[40px] object-contain object-left"
            priority
          />
        )}
      </div>

      {/* Toggle Button */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="absolute -right-3 top-5 z-10 w-6 h-6 rounded-full bg-card border border-border shadow-sm flex items-center justify-center text-primary hover:opacity-70 transition-opacity cursor-pointer"
        aria-label={expanded ? "Collapse sidebar" : "Expand sidebar"}
      >
        {expanded ? <ChevronLeft size={14} /> : <ChevronRight size={14} />}
      </button>

      {/* Nav */}
      <nav className="mt-6 space-y-2">
        {menu.map(({ label, icon: Icon, href }) => {
          const isActive =
            href === "/dashboard"
              ? pathname === "/dashboard"
              : pathname.startsWith(href);

          return (
            <button
              key={label}
              onClick={() => router.push(href)}
              className={cn(
                "relative w-full flex items-center gap-3 py-3 transition-colors",
                expanded ? "px-4" : "justify-center px-2",
                isActive ? "text-primary" : "text-muted-foreground",
              )}
            >
              <Icon className="w-5 h-5" />

              {expanded && <span className="text-sm font-medium">{label}</span>}

              {/* Active indicator */}
              {isActive && (
                <span className="absolute right-0 top-1/2 -translate-y-1/2 h-6 w-[3px] bg-primary rounded-full" />
              )}
            </button>
          );
        })}
      </nav>

      {/* Bottom Section */}
      <div
        className={cn(
          "absolute bottom-6 left-0 w-full space-y-4",
          expanded ? "px-4" : "px-0 flex flex-col items-center",
        )}
      >
        <ThemeToggle />
        <ProfileMenu expanded={expanded} />
      </div>
    </aside>
  );
}

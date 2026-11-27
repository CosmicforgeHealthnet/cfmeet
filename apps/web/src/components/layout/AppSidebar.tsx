"use client";

import { useEffect, useState } from "react";
import Image from "next/image";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { PanelLeftClose, PanelLeftOpen } from "lucide-react";
import { cn } from "@/lib/utils";
import { useDashboardNavigation } from "@/lib/use-dashboard-navigation";
import ProfileMenu from "./ProfileMenu";
import WorkspaceSwitcher from "./WorkspaceSwitcher";
import { UnreadBadge, useUnreadChats } from "@/hooks/useUnreadChats";

/** Is this nav item the current page? Home matches exactly; others match their section. */
export function isActivePath(pathname: string, href: string) {
  return href === "/dashboard" ? pathname === "/dashboard" : pathname.startsWith(href);
}

/**
 * Desktop navigation rail: logo and workspace on top, the app sections, profile at the
 * bottom. Full width (labels) on large screens, icons only on medium screens.
 */
export default function AppSidebar() {
  const menu = useDashboardNavigation();
  const pathname = usePathname();
  const unreadChats = useUnreadChats();
  // Collapse to an icon rail; remembered per browser.
  const [collapsed, setCollapsed] = useState(false);
  useEffect(() => {
    try {
      setCollapsed(localStorage.getItem("cfmeet.sidebar") === "collapsed");
    } catch {}
  }, []);
  const toggle = () =>
    setCollapsed((c) => {
      try {
        localStorage.setItem("cfmeet.sidebar", c ? "expanded" : "collapsed");
      } catch {}
      return !c;
    });
  // Labels show only on large screens with the rail expanded.
  const wide = collapsed ? "hidden" : "hidden lg:block";
  const narrow = collapsed ? "" : "lg:hidden";

  return (
    <aside
      className={cn(
        "relative flex h-screen shrink-0 flex-col border-r border-border bg-sidebar transition-[width] duration-200",
        collapsed ? "w-[76px]" : "w-[76px] lg:w-64",
      )}
    >
      <div className={cn("flex h-16 items-center gap-2 px-4", collapsed ? "justify-center" : "justify-center lg:justify-between")}>
        <Link href="/dashboard" aria-label="CF Meet home" className={cn("flex items-center", collapsed && "hidden")}>
          <Image src="/CFLogo.png" alt="CF Meet" width={36} height={36} className={cn("size-9 object-contain", narrow)} priority />
          <Image src="/light-mode-cf-meet.png" alt="CF Meet" width={120} height={40} className={cn("h-8 w-auto", collapsed ? "hidden" : "hidden lg:block lg:dark:hidden")} priority />
          <Image src="/dark-mode-cfmeet.png" alt="CF Meet" width={120} height={40} className={cn("hidden h-8 w-auto", !collapsed && "lg:dark:block")} priority />
        </Link>
        <button
          type="button"
          onClick={toggle}
          aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
          className="hidden size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground lg:flex"
        >
          {collapsed ? <PanelLeftOpen className="size-5" /> : <PanelLeftClose className="size-5" />}
        </button>
      </div>

      <div className={cn("px-4 pb-2", wide)}>
        <WorkspaceSwitcher expanded />
      </div>
      <div className={cn("px-3 pb-2", narrow)}>
        <WorkspaceSwitcher expanded={false} />
      </div>

      <nav aria-label="Main" className="no-scrollbar flex-1 overflow-y-auto px-3 py-2">
        <ul className="space-y-1">
          {menu.map(({ label, icon: Icon, href }) => {
            const active = isActivePath(pathname, href);
            return (
              <li key={label}>
                <Link
                  href={href}
                  title={label}
                  aria-current={active ? "page" : undefined}
                  className={cn(
                    "flex h-11 items-center gap-3 rounded-xl px-3 text-sm font-medium transition-colors",
                    collapsed ? "justify-center" : "justify-center lg:justify-start",
                    active
                      ? "bg-primary/10 font-semibold text-primary"
                      : "text-muted-foreground hover:bg-muted hover:text-foreground",
                  )}
                >
                  <span className="relative shrink-0">
                    <Icon className="size-5" strokeWidth={active ? 2.4 : 2} aria-hidden />
                    {href === "/dashboard/messages" && unreadChats ? <span className={cn("absolute -right-1 -top-1 size-2.5 rounded-full bg-primary ring-2 ring-background", !collapsed && "lg:hidden")} /> : null}
                  </span>
                  <span className={cn("flex-1", collapsed ? "hidden" : "hidden lg:inline")}>{label}</span>
                  {href === "/dashboard/messages" && !collapsed ? <UnreadBadge count={unreadChats} className="hidden lg:flex" /> : null}
                </Link>
              </li>
            );
          })}
        </ul>
      </nav>

      <div className="border-t border-border p-3">
        <div className={wide}>
          <ProfileMenu expanded />
        </div>
        <div className={cn("flex justify-center", narrow)}>
          <ProfileMenu expanded={false} />
        </div>
      </div>
    </aside>
  );
}

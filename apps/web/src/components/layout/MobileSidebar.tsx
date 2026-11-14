"use client";

import { useEffect, useState } from "react";
import Image from "next/image";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { X } from "lucide-react";
import { cn } from "@/lib/utils";
import { useDashboardNavigation } from "@/lib/use-dashboard-navigation";
import ProfileMenu from "./ProfileMenu";
import WorkspaceSwitcher from "./WorkspaceSwitcher";
import { isActivePath } from "./AppSidebar";
import { OPEN_MENU_EVENT } from "./MobileTabs";

/** Phone menu drawer (opened by the "More" tab): the same content and styling as the desktop sidebar. */
export default function MobileSidebar() {
  const menu = useDashboardNavigation();
  const pathname = usePathname();
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const onOpen = () => setOpen(true);
    window.addEventListener(OPEN_MENU_EVENT, onOpen);
    return () => window.removeEventListener(OPEN_MENU_EVENT, onOpen);
  }, []);

  // Close on navigation and on Escape.
  useEffect(() => setOpen(false), [pathname]);
  useEffect(() => {
    if (!open) return;
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("keydown", esc);
    return () => document.removeEventListener("keydown", esc);
  }, [open]);

  return (
    <div className="md:hidden">
      <div
        aria-hidden
        onClick={() => setOpen(false)}
        className={cn(
          "fixed inset-0 z-50 bg-black/40 transition-opacity duration-300",
          open ? "opacity-100" : "pointer-events-none opacity-0",
        )}
      />

      <aside
        aria-label="Menu"
        aria-hidden={!open}
        inert={!open}
        className={cn(
          "fixed inset-y-0 left-0 z-50 flex w-[280px] max-w-[85vw] flex-col border-r border-border bg-sidebar shadow-2xl transition-transform duration-300 ease-out",
          open ? "translate-x-0" : "-translate-x-full",
        )}
      >
        <div className="flex h-16 items-center justify-between px-4">
          <Image src="/light-mode-cf-meet.png" alt="CF Meet" width={120} height={40} className="h-8 w-auto dark:hidden" />
          <Image src="/dark-mode-cfmeet.png" alt="CF Meet" width={120} height={40} className="hidden h-8 w-auto dark:block" />
          <button
            type="button"
            onClick={() => setOpen(false)}
            aria-label="Close menu"
            className="flex size-9 items-center justify-center rounded-lg text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          >
            <X className="size-5" />
          </button>
        </div>

        <div className="px-4 pb-2">
          <WorkspaceSwitcher expanded />
        </div>

        <nav aria-label="Main" className="no-scrollbar flex-1 overflow-y-auto px-3 py-2">
          <ul className="space-y-1">
            {menu.map(({ label, icon: Icon, href }) => {
              const active = isActivePath(pathname, href);
              return (
                <li key={label}>
                  <Link
                    href={href}
                    aria-current={active ? "page" : undefined}
                    onClick={() => setOpen(false)}
                    className={cn(
                      "flex h-11 items-center gap-3 rounded-xl px-3 text-sm font-medium transition-colors",
                      active ? "bg-primary/10 font-semibold text-primary" : "text-muted-foreground hover:bg-muted hover:text-foreground",
                    )}
                  >
                    <Icon className="size-5 shrink-0" strokeWidth={active ? 2.4 : 2} aria-hidden />
                    {label}
                  </Link>
                </li>
              );
            })}
          </ul>
        </nav>

        <div className="border-t border-border p-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
          <ProfileMenu expanded mobile />
        </div>
      </aside>
    </div>
  );
}

"use client";

import { useState } from "react";
import Image from "next/image";
import { useRouter, usePathname } from "next/navigation";
import { cn } from "@/lib/utils";
import { Home, Video, Calendar, MessageSquare, Code2, Settings, Menu, X } from "lucide-react";
import ProfileMenu from "./ProfileMenu";
import { ThemeToggle } from "@/components/ui/theme-toggle";

const menu = [
  { label: "Dashboard", icon: Home, href: "/dashboard" },
  { label: "Meetings", icon: Video, href: "/dashboard/meetings" },
  { label: "Schedule", icon: Calendar, href: "/dashboard/schedule" },
  { label: "Messages", icon: MessageSquare, href: "/dashboard/messages" },
  { label: "Developers", icon: Code2, href: "/dashboard/developers" },
  { label: "Settings", icon: Settings, href: "/dashboard/settings" },
];

export default function MobileSidebar() {
  const router = useRouter();
  const pathname = usePathname();
  const [open, setOpen] = useState(false);

  return (
    <>
      {/* Menu trigger button — top-right, only on mobile */}
      {!open && (
        <button
          onClick={() => setOpen(true)}
          className="fixed top-4 right-4 z-40 w-10 h-10 rounded-full bg-sidebar shadow-sm flex items-center justify-center text-primary md:hidden"
          aria-label="Open menu"
        >
          <Menu size={20} />
        </button>
      )}

      {/* Backdrop */}
      <div
        className={cn(
          "fixed inset-0 z-50 bg-black/30 transition-opacity duration-300 md:hidden",
          open
            ? "opacity-100 pointer-events-auto"
            : "opacity-0 pointer-events-none",
        )}
        onClick={() => setOpen(false)}
      />

      {/* Drawer panel — slides from LEFT */}
      <aside
        className={cn(
          "fixed top-0 left-0 z-50 h-full w-[220px] transition-transform duration-300 ease-in-out md:hidden",
          open ? "translate-x-0" : "-translate-x-full",
        )}
      >
        {/* Inner container matching AppSidebar design */}
        <div className="h-[calc(100vh-2rem)] my-4 ml-4 bg-sidebar rounded-[40px] shadow-sm flex flex-col">
          {/* Top row: Logo + Close button */}
          <div className="h-16 flex items-center justify-between pl-5 pr-4">
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
            <button
              onClick={() => setOpen(false)}
              className="w-8 h-8 rounded-full flex items-center justify-center text-muted-foreground hover:text-primary transition-colors"
              aria-label="Close menu"
            >
              <X size={18} />
            </button>
          </div>

          {/* Nav */}
          <nav className="mt-6 space-y-1 flex-1">
            {menu.map(({ label, icon: Icon, href }) => {
              const isActive =
                href === "/dashboard"
                  ? pathname === "/dashboard"
                  : pathname.startsWith(href);
              return (
                <button
                  key={label}
                  onClick={() => {
                    router.push(href);
                    setOpen(false);
                  }}
                  className={cn(
                    "relative w-full flex items-center gap-3 px-5 py-3 transition-colors",
                    isActive ? "text-primary" : "text-muted-foreground",
                  )}
                >
                  <Icon className="w-5 h-5" />
                  <span className="text-sm font-medium">{label}</span>

                  {isActive && (
                    <span className="absolute right-0 top-1/2 -translate-y-1/2 h-6 w-[3px] bg-primary rounded-full" />
                  )}
                </button>
              );
            })}
          </nav>

          {/* Bottom Section */}
          <div className="pb-8 px-5 space-y-4">
            <ThemeToggle />
            <ProfileMenu expanded={true} mobile={true} />
          </div>
        </div>
      </aside>
    </>
  );
}

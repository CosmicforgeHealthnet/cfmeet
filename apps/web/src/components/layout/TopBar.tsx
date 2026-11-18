"use client";

import Image from "next/image";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { Search } from "lucide-react";
import { OPEN_COMMANDS_EVENT } from "./CommandPalette";
import NotificationBell from "./NotificationBell";

/** Signed-in header: the search / command bar, centred (Gmail / Teams pattern). */
export default function TopBar() {
  const openCommands = () => window.dispatchEvent(new Event(OPEN_COMMANDS_EVENT));
  // Phones (app-style): the header only on Home; every other screen has its own title row.
  const pathname = usePathname();

  return (
    <header className={`sticky top-0 z-30 h-16 shrink-0 items-center gap-3 border-b border-border bg-background/85 px-4 backdrop-blur-md sm:px-6 ${pathname === "/dashboard" ? "flex" : "hidden md:flex"}`}>
      <Link href="/dashboard" aria-label="CF Meet home" className="md:hidden">
        <Image src="/CFLogo.png" alt="CF Meet" width={32} height={32} className="size-8 object-contain" />
      </Link>
      {/* No page title here: the sidebar highlight and each page's own heading say where you are. */}
      <div className="flex min-w-0 flex-1 justify-end sm:justify-center">
        <button
          type="button"
          onClick={openCommands}
          className="flex h-10 w-10 items-center justify-center gap-2 rounded-xl border border-border bg-muted/50 px-3 text-sm text-muted-foreground transition-colors hover:bg-muted sm:w-full sm:max-w-xl sm:justify-start"
          aria-label="Search and commands"
        >
          <Search className="size-4" aria-hidden />
          <span className="hidden flex-1 text-left sm:inline">Search or jump to…</span>
          <kbd className="hidden rounded-md border border-border bg-background px-1.5 py-0.5 text-[11px] sm:inline">Ctrl K</kbd>
        </button>
      </div>
      <NotificationBell />
    </header>
  );
}

"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { ArrowLeft } from "lucide-react";
import { useWorkspace } from "@/context/WorkspaceContext";
import { ORGANIZATION_CALLING_CAPABILITY } from "@/components/calling/OrganizationCallingAdmin";
import { cn } from "@/lib/utils";

/** Call settings pages. History lives on the Calls page itself; webhooks live in the developer console. */
export const CALL_SETTINGS_LINKS: [string, string][] = [
  ["Devices", "/calling/devices"],
  ["Forwarding & voicemail", "/calling/settings"],
  ["Privacy", "/calling/privacy/contexts"],
  ["Usage", "/calling/usage"],
  ["Company phone system", "/calling/enterprise"],
  ["Help", "/calling/support"],
];

/** Shown on the call settings pages only (not on the Calls page). */
export default function CallingNavigation() {
  const pathname = usePathname();
  const workspace = useWorkspace();
  if (pathname === "/calling") return null;
  // Organization calling administration only where the server grants it (default deny).
  const organization = workspace.kind === "ORGANIZATION" && workspace.can(ORGANIZATION_CALLING_CAPABILITY);
  const items = organization ? [...CALL_SETTINGS_LINKS.slice(0, 4), ["Organization calling", "/calling/organization"] as [string, string], ...CALL_SETTINGS_LINKS.slice(4)] : CALL_SETTINGS_LINKS;
  return (
    <nav aria-label="Call settings" className="border-b border-border bg-background px-4 md:px-6">
      <div className="flex items-center gap-2 pt-4">
        <Link href="/calling" className="inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground"><ArrowLeft className="size-4" />Calls</Link>
      </div>
      <h1 className="mt-2 text-xl font-bold tracking-tight">Call settings</h1>
      <ul className="no-scrollbar -mb-px mt-3 flex gap-1 overflow-x-auto">
        {items.map(([label, href]) => {
          const active = pathname === href || pathname.startsWith(`${href}/`);
          return (
            <li key={href}>
              <Link href={href} aria-current={active ? "page" : undefined}
                className={cn("block h-11 whitespace-nowrap border-b-2 px-3 text-sm font-medium leading-[2.75rem] transition-colors", active ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}>
                {label}
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}

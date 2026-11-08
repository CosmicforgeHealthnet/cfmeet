"use client";

import Link from "next/link";
import { useWorkspace } from "@/context/WorkspaceContext";
import { ORGANIZATION_CALLING_CAPABILITY } from "@/components/calling/OrganizationCallingAdmin";

const links = [
  ["Calling", "/calling"],
  ["History", "/calling/history"],
  ["Devices", "/calling/devices"],
  ["Settings", "/calling/settings"],
  ["Privacy", "/calling/privacy/contexts"],
  ["Enterprise", "/calling/enterprise"],
  ["Webhooks", "/calling/developers/webhooks"],
  ["Usage", "/calling/usage"],
  ["Support", "/calling/support"],
];

export default function CallingNavigation() {
  const workspace = useWorkspace();
  // Organization calling administration only where the server grants it (default deny).
  const organization = workspace.kind === "ORGANIZATION" && workspace.can(ORGANIZATION_CALLING_CAPABILITY);
  const items = organization ? [...links.slice(0, 4), ["Organization calling", "/calling/organization"], ...links.slice(4)] : links;
  return (
    <nav aria-label="Calling navigation" className="border-b border-border bg-background px-4 py-2 md:px-6">
      <ul className="flex gap-2 overflow-x-auto">
        {items.map(([label, href]) => <li key={href}><Link href={href} className="block whitespace-nowrap rounded-lg px-3 py-2 text-sm text-muted-foreground hover:bg-muted hover:text-foreground">{label}</Link></li>)}
      </ul>
    </nav>
  );
}

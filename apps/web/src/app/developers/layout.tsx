import type { Metadata } from "next";
import PortalShell from "@/components/developer-portal/PortalShell";

export const metadata: Metadata = {
  title: "CF Meet Developers",
  description: "Developer console: workspaces, projects, service accounts, API keys, webhooks and usage.",
};

/** The Developer Portal: its own console layout, separate from the chat / meeting dashboard. */
export default function DevelopersLayout({ children }: { children: React.ReactNode }) {
  return <PortalShell>{children}</PortalShell>;
}

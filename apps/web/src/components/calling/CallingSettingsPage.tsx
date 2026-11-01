"use client";

/**
 * CallingSettingsPage - Container page for all calling settings.
 */

import { useState } from "react";
import { Settings, Ban, PhoneForwarded, BarChart3, Voicemail } from "lucide-react";
import BlockingPanel from "./BlockingPanel";
import ForwardingSettings from "./ForwardingSettings";
import UsageDashboard from "./UsageDashboard";
import VoicemailInbox from "./VoicemailInbox";

type TabId = "blocking" | "forwarding" | "usage" | "voicemail";

interface Tab {
  id: TabId;
  label: string;
  icon: typeof Settings;
}

const tabs: Tab[] = [
  { id: "blocking", label: "Blocking", icon: Ban },
  { id: "forwarding", label: "Forwarding", icon: PhoneForwarded },
  { id: "voicemail", label: "Voicemail", icon: Voicemail },
  { id: "usage", label: "Usage", icon: BarChart3 },
];

interface CallingSettingsPageProps {
  className?: string;
  defaultTab?: TabId;
}

export default function CallingSettingsPage({
  className,
  defaultTab = "blocking",
}: CallingSettingsPageProps) {
  const [activeTab, setActiveTab] = useState<TabId>(defaultTab);

  return (
    <div className={`mx-auto max-w-4xl space-y-6 p-4 md:p-6 ${className ?? ""}`}>
      {/* Header */}
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Call Settings</h1>
        <p className="text-sm text-muted-foreground">
          Manage your call blocking, forwarding, voicemail, and usage.
        </p>
      </header>

      {/* Tabs */}
      <div className="flex flex-wrap gap-2 border-b border-border pb-2">
        {tabs.map((tab) => {
          const Icon = tab.icon;
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-medium transition-colors ${
                activeTab === tab.id
                  ? "bg-primary text-primary-foreground"
                  : "text-muted-foreground hover:bg-muted hover:text-foreground"
              }`}
            >
              <Icon className="size-4" />
              {tab.label}
            </button>
          );
        })}
      </div>

      {/* Content */}
      <div className="rounded-2xl border border-border bg-card p-5">
        {activeTab === "blocking" && <BlockingPanel />}
        {activeTab === "forwarding" && <ForwardingSettings />}
        {activeTab === "voicemail" && <VoicemailInbox />}
        {activeTab === "usage" && <UsageDashboard />}
      </div>
    </div>
  );
}

"use client";

import ChatSettings from "@/components/account/ChatSettings";
import { useState } from "react";
import ApiKeysContent from "./ApiKeysContent";
import WebhooksContent from "./WebhooksContent";
import AccountProfileSettings from "@/components/account/AccountProfileSettings";
import AccountSecuritySettings from "@/components/account/AccountSecuritySettings";
import LinkedDevices from "@/components/account/LinkedDevices";
import { KeyRound, BellRing, ShieldCheck, UserRound, Webhook } from "lucide-react";
import { cn } from "@/lib/utils";

type Tab = "profile" | "security" | "chats" | "api-keys" | "webhooks";

const MAIN: { id: Tab; label: string; hint: string; icon: typeof UserRound }[] = [
  { id: "profile", label: "Profile", hint: "Name, username, time zone", icon: UserRound },
  { id: "security", label: "Security", hint: "Password and devices", icon: ShieldCheck },
  { id: "chats", label: "Chats & notifications", hint: "Alerts, storage, backup", icon: BellRing },
];
// Retired integrations: kept reachable so people can revoke what they still have.
const OLD: { id: Tab; label: string; icon: typeof UserRound }[] = [
  { id: "api-keys", label: "Old API keys", icon: KeyRound },
  { id: "webhooks", label: "Old webhooks", icon: Webhook },
];

/** Settings (Google Account pattern): a section list on the left, the section on the right. */
export default function SettingsContent() {
  const [activeTab, setActiveTab] = useState<Tab>("profile");
  const item = (active: boolean) => cn(
    "flex shrink-0 items-center gap-3 rounded-xl px-3 py-2.5 text-left text-sm transition-colors lg:w-full",
    active ? "bg-primary/10 font-semibold text-primary" : "text-foreground hover:bg-muted",
  );

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Settings</h1>
        <p className="mt-0.5 text-sm text-muted-foreground">Your account, sign-in and security.</p>
      </header>

      <div className="mt-6 grid items-start gap-6 lg:grid-cols-[15rem_minmax(0,1fr)] lg:gap-10">
        <nav aria-label="Settings sections" className="no-scrollbar -mx-4 flex gap-1 overflow-x-auto px-4 lg:sticky lg:top-6 lg:mx-0 lg:flex-col lg:px-0">
          {MAIN.map(({ id, label, hint, icon: Icon }) => (
            <button key={id} type="button" onClick={() => setActiveTab(id)} aria-current={activeTab === id ? "page" : undefined} className={item(activeTab === id)}>
              <Icon className="size-[18px] shrink-0" />
              <span className="min-w-0"><span className="block">{label}</span><span className="hidden text-xs font-normal text-muted-foreground lg:block">{hint}</span></span>
            </button>
          ))}
          <p className="hidden px-3 pb-1 pt-5 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground lg:block">Retired</p>
          {OLD.map(({ id, label, icon: Icon }) => (
            <button key={id} type="button" onClick={() => setActiveTab(id)} aria-current={activeTab === id ? "page" : undefined} className={cn(item(activeTab === id), activeTab !== id && "text-muted-foreground")}>
              <Icon className="size-[18px] shrink-0" />{label}
            </button>
          ))}
        </nav>

        <div className="min-w-0 max-w-3xl">
          {activeTab === "profile" && <AccountProfileSettings />}
          {activeTab === "security" && <div className="space-y-4"><AccountSecuritySettings /><LinkedDevices /></div>}
          {activeTab === "chats" && <ChatSettings />}
          {activeTab === "api-keys" && <ApiKeysContent />}
          {activeTab === "webhooks" && <WebhooksContent />}
        </div>
      </div>
    </div>
  );
}

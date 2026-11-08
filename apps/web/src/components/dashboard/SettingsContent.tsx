"use client";

import { useState } from "react";
import ApiKeysContent from "./ApiKeysContent";
import WebhooksContent from "./WebhooksContent";
import AccountProfileSettings from "@/components/account/AccountProfileSettings";
import AccountSecuritySettings from "@/components/account/AccountSecuritySettings";
import { User, Key, Webhook, Lock } from "lucide-react";
import { cn } from "@/lib/utils";

export default function SettingsContent() {
  const [activeTab, setActiveTab] = useState<"profile" | "security" | "api-keys" | "webhooks">("profile");

  return (
    <div className="max-w-4xl mx-auto p-6 space-y-8">
      <div>
        <h1 className="text-2xl font-bold tracking-tight">Settings</h1>
        <p className="text-muted-foreground text-muted-foreground">
          Manage your account settings and preferences.
        </p>
      </div>

      <div className="flex border-b border-border overflow-x-auto">
        <button
          onClick={() => setActiveTab("profile")}
          className={cn(
            "flex items-center gap-2 px-6 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "profile"
              ? "border-primary text-primary"
              : "border-transparent text-muted-foreground hover:text-muted-foreground",
          )}
        >
          <User className="w-4 h-4" />
          Profile
        </button>
        <button
          onClick={() => setActiveTab("security")}
          className={cn(
            "flex items-center gap-2 px-6 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "security"
              ? "border-primary text-primary"
              : "border-transparent text-muted-foreground hover:text-muted-foreground",
          )}
        >
          <Lock className="w-4 h-4" />
          Security
        </button>
        <button
          onClick={() => setActiveTab("api-keys")}
          className={cn(
            "flex items-center gap-2 px-6 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "api-keys"
              ? "border-primary text-primary"
              : "border-transparent text-muted-foreground hover:text-muted-foreground",
          )}
        >
          <Key className="w-4 h-4" />
          API Keys
        </button>
        <button
          onClick={() => setActiveTab("webhooks")}
          className={cn(
            "flex items-center gap-2 px-6 py-3 text-sm font-medium border-b-2 transition-colors whitespace-nowrap",
            activeTab === "webhooks"
              ? "border-primary text-primary"
              : "border-transparent text-muted-foreground hover:text-muted-foreground",
          )}
        >
          <Webhook className="w-4 h-4" />
          Webhooks
        </button>
      </div>

      {activeTab === "profile" && <AccountProfileSettings />}
      {activeTab === "security" && <AccountSecuritySettings />}
      {activeTab === "api-keys" && <ApiKeysContent />}
      {activeTab === "webhooks" && <WebhooksContent />}
    </div>
  );
}

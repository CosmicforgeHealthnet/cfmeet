"use client";

import { Suspense } from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { ArrowLeft, BellRing, ChevronRight, KeyRound, Laptop, ShieldCheck, UserRound, Webhook } from "lucide-react";
import ChatSettings from "@/components/account/ChatSettings";
import AccountProfileSettings from "@/components/account/AccountProfileSettings";
import AccountSecuritySettings from "@/components/account/AccountSecuritySettings";
import LinkedDevices from "@/components/account/LinkedDevices";
import { useAccountProfile } from "@/hooks/useAccount";
import { useHubProfile } from "@/hooks/v2/useCommunications";
import ApiKeysContent from "./ApiKeysContent";
import WebhooksContent from "./WebhooksContent";
import { cn } from "@/lib/utils";

type Tab = "profile" | "security" | "devices" | "chats" | "api-keys" | "webhooks";
/** `desktopOnly`: on a phone the profile card at the top is the way in (no second "Profile" row). */
type Item = { id: Tab; label: string; hint: string; icon: typeof UserRound; tint: string; desktopOnly?: boolean };

const GROUPS: { title: string; items: Item[] }[] = [
  {
    title: "Account",
    items: [
      { id: "profile", label: "Profile", hint: "Photo, name, email, your data", icon: UserRound, tint: "bg-[#006BFF]", desktopOnly: true },
      { id: "security", label: "Privacy & security", hint: "Two-step sign-in, where you're signed in", icon: ShieldCheck, tint: "bg-emerald-600" },
      { id: "devices", label: "Linked devices", hint: "Browsers and phones on your account", icon: Laptop, tint: "bg-violet-600" },
    ],
  },
  {
    title: "Chats",
    items: [
      { id: "chats", label: "Chats & notifications", hint: "Alerts, storage, backup", icon: BellRing, tint: "bg-rose-500" },
    ],
  },
  {
    // Retired integrations: kept reachable so people can revoke what they still have.
    title: "Retired",
    items: [
      { id: "api-keys", label: "Old API keys", hint: "Revoke keys you no longer use", icon: KeyRound, tint: "bg-slate-500" },
      { id: "webhooks", label: "Old webhooks", hint: "Remove old webhook endpoints", icon: Webhook, tint: "bg-slate-500" },
    ],
  },
];
const ALL = GROUPS.flatMap((g) => g.items);

export default function SettingsContent() {
  return <Suspense fallback={null}><Settings /></Suspense>;
}

/**
 * Settings (WhatsApp pattern). Phone: the list, then one section full screen with a back arrow.
 * Computer: the list on the left, the open section on the right. `?tab=` names the section, so
 * links (e.g. "?tab=security" from the new sign-in email) and the back button work.
 */
function Settings() {
  const params = useSearchParams();
  const router = useRouter();
  const pathname = usePathname();
  const requested = params.get("tab");
  const tab = ALL.find((t) => t.id === requested)?.id ?? null;
  const open = (id: Tab | null) => router.push(id ? `${pathname}?tab=${id}` : pathname, { scroll: false });
  const current = ALL.find((t) => t.id === (tab ?? "profile"))!;

  return (
    <div className="mx-auto w-full max-w-6xl lg:grid lg:grid-cols-[22rem_minmax(0,1fr)] lg:gap-8 lg:px-8 lg:py-8">
      {/* The list: the whole screen on a phone until a section is opened. */}
      <aside className={cn("px-4 pb-10 pt-5 lg:sticky lg:top-6 lg:block lg:self-start lg:p-0", tab && "hidden")}>
        <h1 className="mb-4 text-[28px] font-bold tracking-tight">Settings</h1>
        <div className="lg:hidden"><ProfileCard onOpen={() => open("profile")} /></div>
        {GROUPS.map((group) => (
          <section key={group.title} className="mt-6">
            <p className="mb-1.5 px-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground">{group.title}</p>
            <ul className="overflow-hidden rounded-2xl border border-border bg-card">
              {group.items.map(({ id, label, hint, icon: Icon, tint, desktopOnly }) => {
                const active = (tab ?? "profile") === id;
                return (
                  <li key={id} className={cn("border-t border-border first:border-t-0", desktopOnly && "hidden lg:block", desktopOnly && "[&+li]:border-t-0 lg:[&+li]:border-t")}>
                    <button type="button" onClick={() => open(id)} aria-current={active ? "page" : undefined}
                      className={cn("flex w-full items-center gap-3 px-3.5 py-3 text-left transition-colors hover:bg-muted/60", active && "lg:bg-primary/5")}>
                      <span className={cn("flex size-9 shrink-0 items-center justify-center rounded-xl text-white", tint)}><Icon className="size-[18px]" /></span>
                      <span className="min-w-0 flex-1">
                        <span className={cn("block text-[15px] font-medium", active && "lg:text-primary")}>{label}</span>
                        <span className="block truncate text-xs text-muted-foreground">{hint}</span>
                      </span>
                      <ChevronRight className="size-4 shrink-0 text-muted-foreground lg:hidden" />
                    </button>
                  </li>
                );
              })}
            </ul>
          </section>
        ))}
      </aside>

      {/* The open section: full screen on a phone (with a back arrow), the right column on a computer. */}
      <main className={cn("min-w-0", !tab && "hidden lg:block")}>
        <div className="sticky top-0 z-20 flex h-14 items-center gap-2 border-b border-border bg-background/95 px-2 backdrop-blur lg:static lg:h-auto lg:border-0 lg:bg-transparent lg:px-0 lg:pb-5">
          <button type="button" onClick={() => open(null)} aria-label="Back to settings" className="flex size-10 items-center justify-center rounded-full hover:bg-muted lg:hidden"><ArrowLeft className="size-5" /></button>
          <div className="min-w-0">
            <h2 className="truncate text-lg font-semibold lg:text-2xl lg:font-bold lg:tracking-tight">{current.label}</h2>
            <p className="hidden text-sm text-muted-foreground lg:block">{current.hint}</p>
          </div>
        </div>
        <div className="mx-auto max-w-3xl px-4 pb-24 pt-4 lg:mx-0 lg:px-0 lg:pt-0">
          {current.id === "profile" && <AccountProfileSettings />}
          {current.id === "security" && <AccountSecuritySettings />}
          {current.id === "devices" && <LinkedDevices />}
          {current.id === "chats" && <ChatSettings />}
          {current.id === "api-keys" && <ApiKeysContent />}
          {current.id === "webhooks" && <WebhooksContent />}
        </div>
      </main>
    </div>
  );
}

/** Your photo, name and CF Meet ID at the top of the list (WhatsApp's profile row). */
function ProfileCard({ onOpen }: { onOpen: () => void }) {
  const account = useAccountProfile();
  const hub = useHubProfile();
  const name = account.data?.effectiveDisplayName || account.data?.displayName || account.data?.username || "Your profile";
  const initials = name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
  const sub = hub.data?.cfMeetId ?? account.data?.email ?? "";
  return (
    <button type="button" onClick={onOpen}
      className="flex w-full items-center gap-4 rounded-2xl border border-border bg-card p-4 text-left transition-colors hover:bg-muted/60">
      {account.data?.avatarUrl ? (
        // eslint-disable-next-line @next/next/no-img-element
        <img src={account.data.avatarUrl} alt="" className="size-16 shrink-0 rounded-full object-cover" />
      ) : (
        <span className="flex size-16 shrink-0 items-center justify-center rounded-full bg-primary/10 text-xl font-semibold text-primary">{account.isPending ? "" : initials}</span>
      )}
      <span className="min-w-0 flex-1">
        {account.isPending ? <span className="block h-4 w-32 animate-pulse rounded bg-muted" /> : <span className="block truncate text-lg font-semibold">{name}</span>}
        <span className="mt-0.5 block truncate font-mono text-xs text-muted-foreground">{sub}</span>
      </span>
      <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
    </button>
  );
}

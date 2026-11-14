"use client";

import { useState } from "react";
import Link from "next/link";
import { ArrowRight, ArrowUpRight, BarChart3, BookOpen, Check, Clock, Copy, FolderKanban, KeyRound, ScrollText, Terminal, Users, Webhook } from "lucide-react";
import { developerApiConfig } from "@/lib/developer-platform/config";
import { cn } from "@/lib/utils";

/**
 * "Develop with CF Meet" entry point inside the dashboard. The developer console itself is
 * a separate web console at /developers (Blueprint v1.0 Section 17 / ORG-017).
 *
 * This page used to document the deprecated `/api/v1/api/meetings` API with legacy
 * `Api-Key` headers (wrong host and key format; PLT-004 / FE-013,
 * docs/api/legacy-api-keys-deprecation.md §6). It now points to the console and the
 * gateway documentation instead.
 */

// Mirrors sdks/typescript/examples/browser-session.ts (same as the landing page).
const code: [string, string][][] = [
  [["k", "import"], ["t", " { CfMeetClient } "], ["k", "from"], ["s", " '@cfmeet/platform-sdk'"], ["t", ";"]],
  [],
  [["k", "const"], ["t", " client = "], ["k", "new"], ["f", " CfMeetClient"], ["t", "({"]],
  [["t", "  baseUrl: "], ["s", "'https://api.cfmeet.com'"], ["t", ","]],
  [["t", "  auth: { type: "], ["s", "'userSession'"], ["t", ", token },"]],
  [["t", "});"]],
  [],
  [["k", "for await"], ["t", " ("], ["k", "const"], ["t", " org "], ["k", "of"], ["t", " client.developerPlatform"]],
  [["t", "  ."], ["f", "listOrganizations"], ["t", "."], ["f", "all"], ["t", "()) {"]],
  [["t", "  console."], ["f", "log"], ["t", "(org.displayName);"]],
  [["t", "}"]],
];
const plain = code.map((line) => line.map(([, t]) => t).join("")).join("\n");
const tone: Record<string, string> = { k: "text-sky-400", s: "text-emerald-300", f: "text-amber-200", t: "text-slate-300" };

const STEPS = [
  { n: "1", title: "Create a project", body: "Group your app's keys and webhooks.", href: "/developers/projects" },
  { n: "2", title: "Get an API key", body: "Scoped and expiring. Rotate any time.", href: "/developers/credentials" },
  { n: "3", title: "Listen for events", body: "Signed webhooks for meetings, chat and calls.", href: "/developers/webhooks" },
];

const TOOLS = [
  { icon: FolderKanban, title: "Projects", body: "Your apps and environments", href: "/developers/projects" },
  { icon: KeyRound, title: "API keys", body: "Keys and service accounts", href: "/developers/credentials" },
  { icon: Webhook, title: "Webhooks", body: "Deliveries, retries and replay", href: "/developers/webhooks" },
  { icon: BarChart3, title: "Usage", body: "Requests, minutes and limits", href: "/developers/usage" },
  { icon: Users, title: "Team", body: "Who can build and their roles", href: "/developers/members" },
  { icon: ScrollText, title: "Audit log", body: "Every change, who and when", href: "/developers/audit" },
];

export default function DevelopersContent() {
  const [copied, setCopied] = useState(false);
  // The console only works where the Developer Platform is switched on; never link into a dead end.
  const live = developerApiConfig().available;
  const Tile = ({ href, className, children }: { href: string; className: string; children: React.ReactNode }) =>
    live ? <Link href={href} className={className}>{children}</Link> : <div className={cn(className, "cursor-default hover:border-border hover:bg-card hover:shadow-none")}>{children}</div>;
  const copy = () => void navigator.clipboard?.writeText(plain).then(() => { setCopied(true); setTimeout(() => setCopied(false), 2000); });

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      {/* Hero */}
      <section className="overflow-hidden rounded-3xl bg-[#0B1220] text-white">
        <div className="grid items-center gap-8 p-6 sm:p-10 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
          <div>
            <span className="inline-flex items-center gap-1.5 rounded-full bg-white/10 px-3 py-1 text-xs font-semibold text-sky-300">{live ? <><Terminal className="size-3.5" />For developers</> : <><Clock className="size-3.5" />Coming soon</>}</span>
            <h1 className="mt-4 text-3xl font-bold tracking-tight sm:text-4xl">Build on CF Meet</h1>
            <p className="mt-3 max-w-md text-[15px] leading-relaxed text-slate-300">{live ? "Add meetings, chat and calling to your own product. Keys, webhooks and usage live in the developer console, with the sign-in you already have." : "Soon you'll be able to add meetings, chat and calling to your own product, with API keys, webhooks and usage in one console. It isn't switched on yet."}</p>
            {live ? (
              <div className="mt-6 flex flex-wrap gap-2">
                <Link href="/developers" className="inline-flex h-11 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Open developer console<ArrowRight className="size-4" /></Link>
                <Link href="/developers/docs" className="inline-flex h-11 items-center gap-2 rounded-full px-5 text-sm font-semibold text-white ring-1 ring-white/20 hover:bg-white/10"><BookOpen className="size-4" />API docs</Link>
              </div>
            ) : (
              <p className="mt-6 inline-flex items-center gap-2 rounded-full bg-white/10 px-4 py-2 text-sm font-medium text-slate-200"><Clock className="size-4 text-sky-300" />The developer console opens here when it&apos;s ready</p>
            )}
          </div>
          <div className="min-w-0 overflow-hidden rounded-2xl bg-black/40 ring-1 ring-white/10">
            <div className="flex items-center gap-2 border-b border-white/10 px-4 py-2.5">
              <span className="size-2.5 rounded-full bg-white/15" /><span className="size-2.5 rounded-full bg-white/15" /><span className="size-2.5 rounded-full bg-white/15" />
              <span className="ml-2 text-xs text-slate-400">example.ts</span>
              <button type="button" onClick={copy} className="ml-auto inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium text-slate-300 hover:bg-white/10">{copied ? <Check className="size-3.5 text-emerald-300" /> : <Copy className="size-3.5" />}{copied ? "Copied" : "Copy"}</button>
            </div>
            <pre className="no-scrollbar overflow-x-auto p-4 text-[13px] leading-6"><code>
              {code.map((line, i) => (
                <div key={i} className="flex">
                  <span className="mr-4 w-5 shrink-0 select-none text-right text-slate-600">{i + 1}</span>
                  <span className="whitespace-pre">{line.length ? line.map(([k, t], j) => <span key={j} className={tone[k]}>{t}</span>) : " "}</span>
                </div>
              ))}
            </code></pre>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-x-6 gap-y-2 border-t border-white/10 px-6 py-3.5 text-sm text-slate-300 sm:px-10">
          <span className="font-mono text-xs text-slate-400">npm i @cfmeet/platform-sdk</span>
          <span className="flex items-center gap-1.5"><Check className="size-4 text-emerald-400" />TypeScript SDK</span>
          <span className="flex items-center gap-1.5"><Check className="size-4 text-emerald-400" />Signed webhooks</span>
          <span className="flex items-center gap-1.5"><Check className="size-4 text-emerald-400" />Scoped keys</span>
        </div>
      </section>

      {/* Get started */}
      <section className="mt-10">
        <h2 className="text-lg font-semibold">{live ? "Get started in three steps" : "How it will work"}</h2>
        <ol className="mt-4 grid gap-3 md:grid-cols-3">
          {STEPS.map((s) => (
            <li key={s.n}>
              <Tile href={s.href} className="group flex h-full items-start gap-4 rounded-2xl border border-border bg-card p-5 transition-all hover:border-primary/40 hover:shadow-md">
                <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-bold text-primary">{s.n}</span>
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-1 font-semibold">{s.title}{live && <ArrowUpRight className="size-4 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />}</span>
                  <span className="mt-0.5 block text-sm text-muted-foreground">{s.body}</span>
                </span>
              </Tile>
            </li>
          ))}
        </ol>
      </section>

      {/* Console shortcuts */}
      <section className="mt-10">
        <h2 className="text-lg font-semibold">{live ? "In the console" : "What the console will include"}</h2>
        <ul className="mt-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {TOOLS.map(({ icon: Icon, title, body, href }) => (
            <li key={title}>
              <Tile href={href} className="group flex items-center gap-4 rounded-2xl border border-border bg-card p-4 transition-colors hover:bg-muted/50">
                <span className="flex size-11 shrink-0 items-center justify-center rounded-xl bg-muted text-foreground"><Icon className="size-5" /></span>
                <span className="min-w-0 flex-1"><span className="block font-semibold">{title}</span><span className="block text-sm text-muted-foreground">{body}</span></span>
                {live && <ArrowRight className="size-4 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />}
              </Tile>
            </li>
          ))}
        </ul>
      </section>

      <p className="mt-10 rounded-2xl bg-amber-500/10 px-5 py-4 text-sm text-amber-900 dark:text-amber-200">
        <span className="font-semibold">Using old API keys?</span> Keys and meeting webhooks under Settings are retired: new ones can&apos;t be made and old webhooks aren&apos;t sent. {live ? "Create keys in the console, switch your app over, then revoke the old keys." : "When the console opens, you'll create new keys there and switch your app over."}
      </p>
    </div>
  );
}

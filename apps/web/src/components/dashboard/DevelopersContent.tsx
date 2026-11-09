"use client";

import Link from "next/link";
import { ArrowRight, BookOpen, Code2, KeyRound, ShieldCheck, Webhook } from "lucide-react";
import { Button } from "@/components/ui/button";

/**
 * "Develop with CF Meet" entry point inside the dashboard. The developer console itself is
 * a separate web console at /developers (Blueprint v1.0 Section 17 / ORG-017).
 *
 * This page used to document the deprecated `/api/v1/api/meetings` API with legacy
 * `Api-Key` headers (wrong host and key format; PLT-004 / FE-013,
 * docs/api/legacy-api-keys-deprecation.md §6). It now points to the console and the
 * gateway documentation instead.
 */
const FEATURES = [
  { icon: KeyRound, title: "Service accounts & API keys", body: "Scoped, expiring keys for your backend, with rotation, suspension and revocation." },
  { icon: Webhook, title: "Signed webhooks", body: "Meeting, messaging and calling events with delivery history and replay." },
  { icon: ShieldCheck, title: "Roles & audit", body: "Developer roles per workspace and an audit log of every change." },
  { icon: BookOpen, title: "API reference & SDKs", body: "The public API reference and SDK packages served by the API Gateway." },
];

export default function DevelopersContent() {
  return (
    <div className="h-full font-inter">
      <div className="border-b border-border bg-card p-6 md:p-8">
        <h1 className="flex items-center gap-2 text-2xl font-bold text-foreground">
          <Code2 className="h-8 w-8 text-primary" />
          Develop with CF Meet
        </h1>
        <p className="mt-1 max-w-2xl text-muted-foreground">
          Build on CF Meet meetings, messaging and calling from your own backend. Developer workspaces, projects, API keys, webhooks and usage
          live in the CF Meet developer console, with your existing CF Meet sign-in.
        </p>
        <Button asChild className="mt-5">
          <Link href="/developers">
            Open the developer console <ArrowRight className="h-4 w-4" />
          </Link>
        </Button>
      </div>
      <div className="grid gap-4 p-6 md:grid-cols-2 md:p-8">
        {FEATURES.map(({ icon: Icon, title, body }) => (
          <div key={title} className="rounded-xl border border-border bg-card p-5">
            <Icon className="h-5 w-5 text-primary" />
            <p className="mt-2 font-semibold text-foreground">{title}</p>
            <p className="mt-1 text-sm text-muted-foreground">{body}</p>
          </div>
        ))}
        <div className="rounded-xl border border-amber-500/40 bg-amber-500/5 p-5 text-sm text-muted-foreground md:col-span-2">
          Legacy API keys and meeting webhooks under Settings are deprecated: no new legacy keys can be created and legacy webhooks are never
          delivered. Move integrations to the developer console, then revoke the legacy keys.
        </div>
      </div>
    </div>
  );
}

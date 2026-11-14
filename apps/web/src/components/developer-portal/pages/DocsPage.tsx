"use client";

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ArrowUpRight, BookOpen, Check, Copy, FileJson, Package, Terminal } from "lucide-react";
import { PUBLIC_DOCUMENTS } from "@/lib/developer-platform/client";
import { retryDelayMs, safeDocumentUrl, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import type { Discovery } from "@/lib/developer-platform/types";
import { useDeveloperPortal } from "../DeveloperPortalProvider";
import { Badge, Empty, ErrorNotice, Loading, PageHeader } from "../ui";

function CopyBlock({ code, label }: { code: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="overflow-hidden rounded-2xl bg-[#0B1220]">
      <div className="flex items-center justify-between border-b border-white/10 px-4 py-2">
        <span className="text-xs text-slate-400">{label ?? "Terminal"}</span>
        <button type="button" onClick={() => void navigator.clipboard?.writeText(code).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1500); })} className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium text-slate-300 hover:bg-white/10">
          {copied ? <Check className="size-3.5 text-emerald-300" /> : <Copy className="size-3.5" />}{copied ? "Copied" : "Copy"}
        </button>
      </div>
      <pre className="no-scrollbar overflow-x-auto p-4 font-mono text-[13px] leading-6 text-slate-200">{code}</pre>
    </div>
  );
}

export default function DocsPage() {
  const { client, config } = useDeveloperPortal();
  const base = config.available ? config.baseUrl : "";
  // Through the local dev forwarder the public host is still api.cfmeet.com.
  const publicBase = base.includes("localhost") ? "https://api.cfmeet.com" : base || "https://api.cfmeet.com";
  const discovery = useQuery({
    queryKey: ["developer-portal", "discovery"],
    queryFn: () => client.discovery<Discovery>(),
    staleTime: 10 * 60_000,
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });
  const d = discovery.data;
  // The gateway hasn't published its documents yet (404): say so, and don't link to pages that don't exist.
  const unpublished = (discovery.error as { status?: number } | null)?.status === 404;
  const reference = safeDocumentUrl(d?.documentation?.reference, base) ?? `${base}${PUBLIC_DOCUMENTS.docs}`;
  const aggregate = safeDocumentUrl(d?.documentation?.aggregate, base) ?? `${base}${PUBLIC_DOCUMENTS.openapi}`;
  const linkCard = "group flex items-center gap-4 rounded-2xl border border-border bg-card p-5 transition-all hover:border-primary/40 hover:shadow-md";
  return (
    <div className="space-y-8">
      <PageHeader title="Docs & SDKs" description="Everything you need to call the CF Meet API from your server." />

      {unpublished && (
        <p className="flex items-start gap-3 rounded-2xl bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-200">
          <BookOpen className="mt-0.5 size-4 shrink-0" />
          <span><span className="font-semibold">The full API docs aren&apos;t published yet.</span> The reference, OpenAPI file and SDK list will appear here once CF Meet puts them online. You can still use your API keys with the example below.</span>
        </p>
      )}
      <section className={unpublished ? "hidden" : "grid gap-3 sm:grid-cols-2"}>
        <a href={reference} target="_blank" rel="noopener noreferrer" className={linkCard}>
          <span className="flex size-12 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary"><BookOpen className="size-6" /></span>
          <span className="min-w-0 flex-1"><span className="block font-semibold">API reference</span><span className="block text-sm text-muted-foreground">Every endpoint, with examples you can try.</span></span>
          <ArrowUpRight className="size-5 shrink-0 text-muted-foreground transition-transform group-hover:-translate-y-0.5 group-hover:translate-x-0.5" />
        </a>
        <a href={aggregate} target="_blank" rel="noopener noreferrer" className={linkCard}>
          <span className="flex size-12 shrink-0 items-center justify-center rounded-xl bg-violet-500/10 text-violet-600"><FileJson className="size-6" /></span>
          <span className="min-w-0 flex-1"><span className="block font-semibold">OpenAPI file</span><span className="block text-sm text-muted-foreground">For code generators and API tools like Postman.</span></span>
          <ArrowUpRight className="size-5 shrink-0 text-muted-foreground transition-transform group-hover:-translate-y-0.5 group-hover:translate-x-0.5" />
        </a>
      </section>

      <section className="space-y-3">
        <h2 className="flex items-center gap-2 text-lg font-semibold"><Terminal className="size-5" />Your first request</h2>
        <p className="text-sm text-muted-foreground">Use an API key from the <span className="font-medium text-foreground">API keys</span> page. Keep it in an environment variable on your server.</p>
        <CopyBlock code={`curl ${publicBase}/v1/... \\\n  -H "Authorization: Bearer $CFMEET_API_KEY"`} />
      </section>

      <section className="space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="flex items-center gap-2 text-lg font-semibold"><Package className="size-5" />SDKs</h2>
          {d?.sdks?.status && <Badge label={d.sdks.status} tone="muted" />}
        </div>
        {discovery.isLoading ? (
          <Loading />
        ) : unpublished ? (
          <Empty>No SDKs published yet. Coming soon.</Empty>
        ) : discovery.error ? (
          <ErrorNotice error={discovery.error} onRetry={() => void discovery.refetch()} />
        ) : d?.sdks?.packages?.length ? (
          <ul className="grid gap-3 md:grid-cols-2">
            {d.sdks.packages.map((p, i) => {
              const quickstart = safeDocumentUrl(p.quickstart, base);
              return (
                <li key={`${p.package ?? i}`} className="space-y-3 rounded-2xl border border-border bg-card p-5">
                  <div className="flex items-center justify-between gap-2">
                    <p className="font-semibold">{p.language ?? "SDK"}</p>
                    {quickstart && <a href={quickstart} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 text-sm font-semibold text-primary hover:underline">Quick start<ArrowUpRight className="size-4" /></a>}
                  </div>
                  <p className="font-mono text-xs text-muted-foreground">{p.package ?? "—"}{p.version ? `@${p.version}` : ""}</p>
                  {p.install && <CopyBlock code={p.install} label="Install" />}
                </li>
              );
            })}
          </ul>
        ) : (
          <Empty>No SDK packages published yet.</Empty>
        )}
      </section>

      {d?.families?.length ? (
        <section className="space-y-3">
          <h2 className="text-lg font-semibold">What the API covers</h2>
          <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {d.families.map((f) => {
              const href = safeDocumentUrl(f.openapi, base);
              return (
                <li key={f.domain} className="rounded-2xl border border-border bg-card p-4">
                  <div className="flex items-start justify-between gap-2">
                    <p className="font-semibold">{f.name}{f.version ? <span className="ml-1 text-xs font-normal text-muted-foreground">v{f.version}</span> : null}</p>
                    <Badge label={f.enabled ? "Available" : "Not here yet"} tone={f.enabled ? "ok" : "muted"} />
                  </div>
                  <p className="mt-1 text-sm text-muted-foreground">{f.operations} endpoints</p>
                  {href && <a href={href} target="_blank" rel="noopener noreferrer" className="mt-2 inline-flex items-center gap-1 text-sm font-semibold text-primary hover:underline">Spec<ArrowUpRight className="size-4" /></a>}
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}
    </div>
  );
}

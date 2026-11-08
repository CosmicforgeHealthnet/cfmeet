"use client";

import { useQuery } from "@tanstack/react-query";
import { ExternalLink } from "lucide-react";
import { PUBLIC_DOCUMENTS } from "@/lib/developer-platform/client";
import { retryDelayMs, safeDocumentUrl, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import type { Discovery } from "@/lib/developer-platform/types";
import { useDeveloperPortal } from "../DeveloperPortalProvider";
import { Badge, Empty, ErrorNotice, Loading, Mono, PageHeader, Section, Table, Td } from "../ui";

function ExtLink({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <a href={href} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 font-medium text-primary underline">
      {children} <ExternalLink className="h-3.5 w-3.5" />
    </a>
  );
}

export default function DocsPage() {
  const { client, config } = useDeveloperPortal();
  const base = config.available ? config.baseUrl : "";
  const discovery = useQuery({
    queryKey: ["developer-portal", "discovery"],
    queryFn: () => client.discovery<Discovery>(),
    staleTime: 10 * 60_000,
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });
  const d = discovery.data;
  const reference = safeDocumentUrl(d?.documentation?.reference, base) ?? `${base}${PUBLIC_DOCUMENTS.docs}`;
  const aggregate = safeDocumentUrl(d?.documentation?.aggregate, base) ?? `${base}${PUBLIC_DOCUMENTS.openapi}`;
  return (
    <div className="space-y-6">
      <PageHeader title="Docs & SDKs" description="The public API reference and SDK packages, served by the CF Meet API Gateway." />
      <Section title="API reference">
        <ul className="space-y-2 text-sm">
          <li>
            <ExtLink href={reference}>API reference</ExtLink> — interactive documentation of every public operation.
          </li>
          <li>
            <ExtLink href={aggregate}>OpenAPI 3.1 document</ExtLink> — for code generators and API tools.
          </li>
        </ul>
        <div className="mt-4 rounded-lg bg-muted/50 p-3 text-sm text-muted-foreground">
          Authenticate server-side with an API key from <span className="font-medium text-foreground">Service accounts &amp; keys</span>:
          <pre className="mt-2 overflow-x-auto rounded bg-background p-2 font-mono text-xs">{`curl ${base || "https://api.cfmeet.com"}/v1/... \\
  -H "Authorization: Bearer $CFMEET_API_KEY"`}</pre>
          Keep the key in an environment variable or secret manager on your server.
        </div>
      </Section>
      <Section title="SDKs" description={d?.sdks?.status ? `Status: ${d.sdks.status}` : undefined}>
        {discovery.isLoading ? (
          <Loading />
        ) : discovery.error ? (
          <ErrorNotice error={discovery.error} onRetry={() => void discovery.refetch()} />
        ) : d?.sdks?.packages?.length ? (
          <Table head={["Language", "Package", "Install", "Quick start"]}>
            {d.sdks.packages.map((p, i) => {
              const quickstart = safeDocumentUrl(p.quickstart, base);
              return (
                <tr key={`${p.package ?? i}`}>
                  <Td>{p.language ?? "—"}</Td>
                  <Td>
                    <Mono>
                      {p.package ?? "—"}
                      {p.version ? `@${p.version}` : ""}
                    </Mono>
                  </Td>
                  <Td>{p.install ? <Mono>{p.install}</Mono> : "—"}</Td>
                  <Td>{quickstart ? <ExtLink href={quickstart}>Guide</ExtLink> : "—"}</Td>
                </tr>
              );
            })}
          </Table>
        ) : (
          <Empty>No SDK packages are published by this deployment yet.</Empty>
        )}
      </Section>
      {d?.families?.length ? (
        <Section title="API families">
          <Table head={["Family", "Operations", "Status", "OpenAPI"]}>
            {d.families.map((f) => {
              const href = safeDocumentUrl(f.openapi, base);
              return (
                <tr key={f.domain}>
                  <Td className="font-medium">
                    {f.name}
                    {f.version ? <span className="text-xs text-muted-foreground"> v{f.version}</span> : null}
                  </Td>
                  <Td>{f.operations}</Td>
                  <Td>
                    <Badge label={f.enabled ? "enabled" : "not enabled here"} tone={f.enabled ? "ok" : "muted"} />
                  </Td>
                  <Td>{href ? <ExtLink href={href}>Spec</ExtLink> : "—"}</Td>
                </tr>
              );
            })}
          </Table>
        </Section>
      ) : null}
    </div>
  );
}

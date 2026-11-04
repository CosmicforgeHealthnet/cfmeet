import Link from "next/link";
import { notFound } from "next/navigation";
import CallingDevicesPage from "@/components/calling/CallingDevicesPage";
import CallingEnterpriseHome from "@/components/calling/CallingEnterpriseHome";
import CallingResourcePage from "@/components/calling/CallingResourcePage";
import CallingSettingsPage from "@/components/calling/CallingSettingsPage";
import CallsContent from "@/components/calling/CallsContent";
import UsageDashboard from "@/components/calling/UsageDashboard";
import { Button } from "@/components/ui/button";
import { callingResourceRoutes } from "@/lib/calling/resource-routes";

function ResourceLanding({ title, description, links }: { title: string; description: string; links: Array<{ label: string; href: string }> }) {
  return <main className="mx-auto max-w-5xl space-y-4 p-4 md:p-6"><h1 className="text-2xl font-bold">{title}</h1><p className="text-sm text-muted-foreground">{description}</p><div className="flex flex-wrap gap-2">{links.map((link) => <Button asChild key={link.href} variant="outline"><Link href={link.href}>{link.label}</Link></Button>)}</div></main>;
}

function SupportPage() {
  return (
    <main className="mx-auto max-w-5xl space-y-5 p-4 md:p-6">
      <header><h1 className="text-2xl font-bold tracking-tight">Calling support</h1><p className="text-sm text-muted-foreground">Safe diagnostics and authorized audit access for call-flow issues.</p></header>
      <section className="rounded-2xl border border-border bg-card p-5 text-sm">
        <h2 className="font-semibold">Troubleshooting</h2>
        <ul className="mt-3 list-disc space-y-2 pl-5 text-muted-foreground">
          <li>Confirm a browser endpoint is registered and online before starting an application call.</li>
          <li>Use call history and the call detail view to check lifecycle state and failure category.</li>
          <li>Do not paste signaling credentials, access tokens, raw phone numbers, or protected media URLs into support requests.</li>
          <li>Backend request IDs and timestamps are safe to share with an authorized administrator.</li>
        </ul>
      </section>
      <div className="flex flex-wrap gap-2"><Button asChild variant="outline"><Link href="/calling/devices">Check devices</Link></Button><Button asChild variant="outline"><Link href="/calling/history">Open call history</Link></Button><Button asChild variant="outline"><Link href="/calling/enterprise/audit">Authorized audit records</Link></Button></div>
    </main>
  );
}

function EnterpriseLanding() {
  return <CallingEnterpriseHome />;
}

function resolveResource(segments: string[]) {
  const route = segments.join("/");
  const exact = callingResourceRoutes[route];
  if (exact) return { definition: exact };

  if (segments.length === 2 && segments[0] === "enterprise" && segments[1] === "recordings") {
    return undefined;
  }
  if (segments.length === 3 && segments[0] === "enterprise" && segments[1] === "recordings") {
    const definition = callingResourceRoutes["enterprise/recordings/lookup"];
    return { definition, detailId: segments[2] };
  }
  if (segments.length === 4 && segments[0] === "enterprise" && segments[1] === "queues" && segments[3] === "entries") {
    const queueId = encodeURIComponent(segments[2]);
    return {
      definition: {
        title: "Live queue entries",
        description: "Current entries for this call queue.",
        listPath: `/v1/enterprise/queues/${queueId}/entries`,
        docsHref: "/api-docs/calling/openapi.json",
      },
    };
  }
  if (segments.length === 3 && segments[0] === "enterprise" && segments[1] === "ivr" && segments[2] === "flows") {
    return { definition: callingResourceRoutes["enterprise/ivr"] };
  }
  if (segments.length >= 2 && segments[0] === "enterprise" && segments[1] === "voicemail") {
    const suffix = segments.slice(2);
    if (suffix.length === 1 && suffix[0] === "messages") return { definition: callingResourceRoutes["enterprise/voicemail/messages"] };
    if (suffix.length === 2 && suffix[0] === "messages") return { definition: callingResourceRoutes["enterprise/voicemail/messages"], detailId: suffix[1] };
    if (suffix.length === 1 && suffix[0] === "mailboxes") return { definition: callingResourceRoutes["enterprise/voicemail/mailboxes"] };
    if (suffix.length === 2 && suffix[0] === "mailboxes") return { definition: callingResourceRoutes["enterprise/voicemail/mailboxes"], detailId: suffix[1] };
  }
  if (segments.length >= 2 && segments[0] === "privacy") {
    const base = segments.slice(0, 2).join("/");
    const definition = callingResourceRoutes[base];
    if (definition && segments.length === 3) return { definition, detailId: segments[2] };
  }
  if (segments.length >= 3 && segments[0] === "developers") {
    const base = segments.slice(0, 2).join("/");
    const definition = callingResourceRoutes[base];
    if (definition && segments.length === 3) return { definition, detailId: segments[2] };
  }
  if (segments.length >= 2 && segments[0] === "enterprise") {
    const base = segments.slice(0, 2).join("/");
    const definition = callingResourceRoutes[base];
    if (definition && segments.length === 3) return { definition, detailId: segments[2] };
  }
  return undefined;
}

export default async function CallingSubPage({ params }: { params: Promise<{ segments: string[] }> }) {
  const { segments } = await params;
  const route = segments.join("/");
  if (route === "history") return <CallsContent />;
  if (route === "devices") return <CallingDevicesPage />;
  if (route === "settings") return <CallingSettingsPage />;
  if (route === "usage") return <main className="mx-auto max-w-6xl p-4 md:p-6"><UsageDashboard /></main>;
  if (route === "enterprise") return <EnterpriseLanding />;
  if (route === "privacy") return <ResourceLanding title="Privacy Relay" description="Manage authorized communication contexts, relay sessions, and protected contacts." links={[
    { label: "Communication contexts", href: "/calling/privacy/contexts" },
    { label: "Relay sessions", href: "/calling/privacy/sessions" },
    { label: "Protected contacts", href: "/calling/privacy/contacts" },
  ]} />;
  if (route === "support") return <SupportPage />;
  if (route === "developers") return <ResourceLanding title="Developer tools" description="Configure scoped Calling API webhook subscriptions and review delivery history." links={[
    { label: "Webhooks", href: "/calling/developers/webhooks" },
  ]} />;
  if (segments.length === 1) return <CallsContent initialCallId={segments[0]} />;

  const resource = resolveResource(segments);
  if (!resource) notFound();
  return <CallingResourcePage definition={resource.definition} detailId={resource.detailId} />;
}

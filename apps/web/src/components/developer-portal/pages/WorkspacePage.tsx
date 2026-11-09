"use client";

import { useState } from "react";
import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { PORTAL_BASE } from "@/lib/developer-platform/navigation";
import { formatTimestamp, retryDelayMs, shouldRetryQuery } from "@/lib/developer-platform/presentation";
import type { CredentialPolicy, SupportProfile, Workspace } from "@/lib/developer-platform/types";
import { useDeveloperPortal, WORKSPACES_QUERY_KEY } from "../DeveloperPortalProvider";
import { ConfirmButton, ErrorNotice, Field, inputClass, Loading, PageHeader, Section, StatusBadge, Table, Td, useConsoleMutation } from "../ui";

export default function WorkspacePage() {
  const { workspaces, workspace, selectWorkspace } = useDeveloperPortal();
  return (
    <div className="space-y-6">
      <PageHeader
        title="Developer workspaces"
        description="A developer workspace is your company's account on the CF Meet Developer Platform. It is separate from CF Meet business organizations used for meetings and chat."
        actions={
          <Button asChild size="sm" variant="outline">
            <Link href={`${PORTAL_BASE}/invitations/accept`}>Accept an invitation</Link>
          </Button>
        }
      />
      <Section title="Your workspaces">
        {workspaces.length === 0 ? (
          <p className="text-sm text-muted-foreground">You are not a member of any developer workspace yet.</p>
        ) : (
          <Table head={["Name", "Role", "State", "Created", ""]}>
            {workspaces.map((w) => (
              <tr key={w.id}>
                <Td className="font-medium">{w.displayName}</Td>
                <Td>{w.myRole ?? "—"}</Td>
                <Td>
                  <StatusBadge status={w.state} />
                </Td>
                <Td>{formatTimestamp(w.createdAt)}</Td>
                <Td>
                  {workspace?.id === w.id ? (
                    <span className="text-xs text-muted-foreground">Selected</span>
                  ) : (
                    <Button size="sm" variant="outline" onClick={() => selectWorkspace(w.id)}>
                      Switch
                    </Button>
                  )}
                </Td>
              </tr>
            ))}
          </Table>
        )}
      </Section>
      <CreateWorkspace />
      {workspace && <WorkspaceSettings key={workspace.id} workspace={workspace} />}
      {workspace && <CredentialPolicySection key={`policy-${workspace.id}`} workspace={workspace} />}
      {workspace && <SupportProfileSection key={`support-${workspace.id}`} workspace={workspace} />}
    </div>
  );
}

function CreateWorkspace() {
  const { client, selectWorkspace } = useDeveloperPortal();
  const [displayName, setDisplayName] = useState("");
  const [supportEmail, setSupportEmail] = useState("");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const create = useConsoleMutation(
    () =>
      client.call<Workspace>({
        operation: "createOrganization",
        body: { displayName: displayName.trim(), ...(supportEmail.trim() ? { supportEmail: supportEmail.trim() } : {}) },
        idempotencyKey,
      }),
    [WORKSPACES_QUERY_KEY],
    (created) => {
      selectWorkspace(created.id);
      setDisplayName("");
      setSupportEmail("");
      setIdempotencyKey(newIdempotencyKey());
    },
  );
  return (
    <Section title="Create a developer workspace" description="You become its OWNER. The display name can be changed later.">
      <form
        className="grid gap-3 sm:grid-cols-[1fr_1fr_auto] sm:items-end"
        onSubmit={(e) => {
          e.preventDefault();
          if (displayName.trim()) create.mutate(undefined);
        }}
      >
        <Field label="Display name">
          <input className={inputClass} value={displayName} maxLength={120} required onChange={(e) => setDisplayName(e.target.value)} />
        </Field>
        <Field label="Support email (optional)">
          <input className={inputClass} type="email" value={supportEmail} onChange={(e) => setSupportEmail(e.target.value)} />
        </Field>
        <Button type="submit" size="sm" loading={create.isPending} disabled={!displayName.trim()}>
          Create
        </Button>
      </form>
      {create.error ? (
        <div className="mt-3">
          <ErrorNotice error={create.error} />
        </div>
      ) : null}
    </Section>
  );
}

function WorkspaceSettings({ workspace }: { workspace: Workspace }) {
  const { client, can } = useDeveloperPortal();
  const [displayName, setDisplayName] = useState(workspace.displayName);
  const rename = useConsoleMutation(
    () =>
      client.call<Workspace>({
        operation: "updateOrganization",
        path: { organizationId: workspace.id },
        body: { expectedVersion: workspace.version, displayName: displayName.trim() },
      }),
    [WORKSPACES_QUERY_KEY],
  );
  const close = useConsoleMutation(
    () => client.call<Workspace>({ operation: "closeOrganization", path: { organizationId: workspace.id } }),
    [WORKSPACES_QUERY_KEY],
  );
  return (
    <Section title="Workspace settings" description={`Workspace ID ${workspace.id}`}>
      <div className="space-y-4">
        <form
          className="flex flex-col gap-3 sm:flex-row sm:items-end"
          onSubmit={(e) => {
            e.preventDefault();
            if (displayName.trim() && displayName.trim() !== workspace.displayName) rename.mutate(undefined);
          }}
        >
          <div className="flex-1">
            <Field label="Display name" hint="Renaming also updates the workspace's Platform Access tenant name.">
              <input className={inputClass} value={displayName} maxLength={120} disabled={!can("OrganizationUpdate")} onChange={(e) => setDisplayName(e.target.value)} />
            </Field>
          </div>
          {can("OrganizationUpdate") && (
            <Button type="submit" size="sm" loading={rename.isPending} disabled={!displayName.trim() || displayName.trim() === workspace.displayName}>
              Save
            </Button>
          )}
        </form>
        <ErrorNotice error={rename.error} />
        {can("OrganizationClose") && (
          <div className="flex flex-col gap-2 rounded-lg border border-red-500/30 p-4 sm:flex-row sm:items-center sm:justify-between">
            <div className="text-sm">
              <p className="font-medium">Close workspace</p>
              <p className="text-muted-foreground">Closing stops all projects, environments and API keys. The workspace stays readable.</p>
            </div>
            <ConfirmButton
              label="Close workspace"
              message="Close this developer workspace? This cannot be undone from the console."
              destructive
              pending={close.isPending}
              onConfirm={() => close.mutate(undefined)}
            />
          </div>
        )}
        <ErrorNotice error={close.error} />
      </div>
    </Section>
  );
}

function CredentialPolicySection({ workspace }: { workspace: Workspace }) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "credential-policy"];
  const policy = useQuery({
    queryKey: key,
    queryFn: () => client.call<CredentialPolicy>({ operation: "getCredentialPolicy", path: { organizationId: workspace.id } }),
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });
  const [lifetime, setLifetime] = useState("");
  const [overlap, setOverlap] = useState("");
  const save = useConsoleMutation(
    () =>
      client.call<CredentialPolicy>({
        operation: "putCredentialPolicy",
        path: { organizationId: workspace.id },
        body: { maxLifetimeDays: Number(lifetime), maxRotationOverlapMinutes: Number(overlap) },
      }),
    [key],
  );
  return (
    <Section title="API key policy" description="Upper bounds for every API key in this workspace (the platform's own limits still apply).">
      {policy.isLoading ? (
        <Loading />
      ) : policy.error ? (
        <ErrorNotice error={policy.error} onRetry={() => void policy.refetch()} />
      ) : policy.data ? (
        <div className="space-y-4">
          <dl className="grid gap-3 text-sm sm:grid-cols-2">
            <div>
              <dt className="text-muted-foreground">Maximum key lifetime</dt>
              <dd className="font-medium">{policy.data.configured && policy.data.maxLifetimeDays != null ? `${policy.data.maxLifetimeDays} days` : "Platform default"}</dd>
            </div>
            <div>
              <dt className="text-muted-foreground">Maximum rotation overlap</dt>
              <dd className="font-medium">
                {policy.data.configured && policy.data.maxRotationOverlapMinutes != null ? `${policy.data.maxRotationOverlapMinutes} minutes` : "Platform default"}
              </dd>
            </div>
          </dl>
          {can("OrganizationUpdate") && (
            <form
              className="grid gap-3 sm:grid-cols-[1fr_1fr_auto] sm:items-end"
              onSubmit={(e) => {
                e.preventDefault();
                save.mutate(undefined);
              }}
            >
              <Field label="Max lifetime (days)">
                <input className={inputClass} type="number" min={1} required value={lifetime} onChange={(e) => setLifetime(e.target.value)} />
              </Field>
              <Field label="Max rotation overlap (minutes)">
                <input className={inputClass} type="number" min={0} required value={overlap} onChange={(e) => setOverlap(e.target.value)} />
              </Field>
              <Button type="submit" size="sm" loading={save.isPending} disabled={!lifetime || overlap === ""}>
                Save policy
              </Button>
            </form>
          )}
          <ErrorNotice error={save.error} />
        </div>
      ) : null}
    </Section>
  );
}

function SupportProfileSection({ workspace }: { workspace: Workspace }) {
  const { client } = useDeveloperPortal();
  const profile = useQuery({
    queryKey: ["developer-portal", workspace.id, "support-profile"],
    queryFn: () => client.call<SupportProfile>({ operation: "getSupportProfile", path: { organizationId: workspace.id } }),
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });
  return (
    <Section title="Support" description="Routing metadata only: there is no ticketing system in the console.">
      {profile.isLoading ? (
        <Loading />
      ) : profile.error ? (
        <ErrorNotice error={profile.error} onRetry={() => void profile.refetch()} />
      ) : profile.data ? (
        <dl className="grid gap-3 text-sm sm:grid-cols-3">
          <div>
            <dt className="text-muted-foreground">Support tier</dt>
            <dd className="font-medium">{profile.data.supportTier}</dd>
          </div>
          <div>
            <dt className="text-muted-foreground">Production status</dt>
            <dd>
              <StatusBadge status={profile.data.productionStatus} />
            </dd>
          </div>
          <div>
            <dt className="text-muted-foreground">Support reference</dt>
            <dd className="font-medium">{profile.data.supportReference ?? "—"}</dd>
          </div>
        </dl>
      ) : null}
    </Section>
  );
}

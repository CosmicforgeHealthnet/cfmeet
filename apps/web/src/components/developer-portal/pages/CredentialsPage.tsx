"use client";

import { useState } from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { portalHref, safeId } from "@/lib/developer-platform/navigation";
import {
  credentialActions,
  credentialBadges,
  credentialSecret,
  expiryFromLocalInput,
  formatTimestamp,
  retryDelayMs,
  shouldRetryQuery,
  type OneTimeSecret,
} from "@/lib/developer-platform/presentation";
import type { Credential, IssuedCredential, ProjectScopes, ServiceAccount, Workspace } from "@/lib/developer-platform/types";
import { useDeveloperPortal, useWorkspace } from "../DeveloperPortalProvider";
import { EnvironmentPicker, useEnvironmentSelection } from "../EnvironmentPicker";
import { RequireWorkspace } from "../PortalShell";
import {
  Badge,
  ConfirmButton,
  Empty,
  ErrorNotice,
  Field,
  inputClass,
  LoadMore,
  Loading,
  Mono,
  NotPermitted,
  PageHeader,
  SecretOnce,
  Section,
  StatusBadge,
  Table,
  Td,
  useConsoleMutation,
  usePagedList,
} from "../ui";

export default function CredentialsPage() {
  return (
    <RequireWorkspace>
      <Credentials />
    </RequireWorkspace>
  );
}

function Credentials() {
  const workspace = useWorkspace();
  const { can } = useDeveloperPortal();
  const { projectId, environmentId } = useEnvironmentSelection();
  return (
    <div className="space-y-6">
      <PageHeader
        title="Service accounts & API keys"
        description="A service account is a machine identity in one environment; its API keys (cfmk_…) authenticate your backend to the CF Meet API. Keys belong on servers only — never in browsers, mobile apps or repositories."
      />
      {!can("ServiceAccountsRead") ? (
        <NotPermitted what="service accounts and API keys" />
      ) : (
        <>
          <EnvironmentPicker workspace={workspace} />
          {environmentId ? (
            <ServiceAccounts key={environmentId} workspace={workspace} projectId={projectId} environmentId={environmentId} />
          ) : (
            <Empty>Select a project and environment.</Empty>
          )}
        </>
      )}
    </div>
  );
}

function ServiceAccounts({ workspace, projectId, environmentId }: { workspace: Workspace; projectId: string | null; environmentId: string }) {
  const { client, can } = useDeveloperPortal();
  const router = useRouter();
  const pathname = usePathname();
  const params = useSearchParams();
  const selected = safeId(params.get("sa"));
  const key = ["developer-portal", workspace.id, "service-accounts", environmentId];
  const list = usePagedList<ServiceAccount>(key, { operation: "listServiceAccounts", path: { environmentId } });
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const create = useConsoleMutation(
    () =>
      client.call<ServiceAccount>({
        operation: "createServiceAccount",
        path: { environmentId },
        body: { name: name.trim(), ...(description.trim() ? { description: description.trim() } : {}) },
        idempotencyKey,
      }),
    [key],
    (sa) => {
      setName("");
      setDescription("");
      setIdempotencyKey(newIdempotencyKey());
      router.replace(portalHref(pathname, { project: projectId, env: environmentId, sa: sa.id }));
    },
  );
  const lifecycle = useConsoleMutation(
    (v: { sa: ServiceAccount; operation: "suspendServiceAccount" | "resumeServiceAccount" }) =>
      client.call<ServiceAccount>({ operation: v.operation, path: { environmentId, serviceAccountId: v.sa.id } }),
    [key],
  );
  const current = list.items.find((sa) => sa.id === selected) ?? null;

  return (
    <>
      <Section title="Service accounts">
        <div className="space-y-4">
          {list.isLoading ? (
            <Loading />
          ) : list.error ? (
            <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
          ) : list.items.length === 0 ? (
            <Empty>No service accounts in this environment.</Empty>
          ) : (
            <Table head={["Name", "Status", "Created", ""]}>
              {list.items.map((sa) => (
                <tr key={sa.id} className={sa.id === selected ? "bg-primary/5" : undefined}>
                  <Td>
                    <p className="font-medium">{sa.name}</p>
                    {sa.description && <p className="text-xs text-muted-foreground">{sa.description}</p>}
                  </Td>
                  <Td>
                    <StatusBadge status={sa.status} />
                  </Td>
                  <Td>{formatTimestamp(sa.createdAt)}</Td>
                  <Td className="space-x-1 whitespace-nowrap">
                    <Button size="sm" variant="outline" onClick={() => router.replace(portalHref(pathname, { project: projectId, env: environmentId, sa: sa.id }))}>
                      API keys
                    </Button>
                    {can("ServiceAccountsSuspend") && sa.status === "ACTIVE" && (
                      <ConfirmButton
                        label="Suspend"
                        message="Suspend this service account? All its keys stop authenticating until resumed."
                        pending={lifecycle.isPending}
                        onConfirm={() => lifecycle.mutate({ sa, operation: "suspendServiceAccount" })}
                      />
                    )}
                    {can("ServiceAccountsManage") && sa.status === "SUSPENDED" && (
                      <Button size="sm" variant="outline" loading={lifecycle.isPending} onClick={() => lifecycle.mutate({ sa, operation: "resumeServiceAccount" })}>
                        Resume
                      </Button>
                    )}
                  </Td>
                </tr>
              ))}
            </Table>
          )}
          <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
          <ErrorNotice error={lifecycle.error} />
          {can("ServiceAccountsManage") && (
            <form
              className="grid gap-3 sm:grid-cols-[1fr_2fr_auto] sm:items-end"
              onSubmit={(e) => {
                e.preventDefault();
                if (name.trim()) create.mutate(undefined);
              }}
            >
              <Field label="Name">
                <input className={inputClass} required maxLength={120} value={name} onChange={(e) => setName(e.target.value)} />
              </Field>
              <Field label="Description (optional)">
                <input className={inputClass} maxLength={500} value={description} onChange={(e) => setDescription(e.target.value)} />
              </Field>
              <Button type="submit" size="sm" loading={create.isPending} disabled={!name.trim()}>
                Add service account
              </Button>
            </form>
          )}
          <ErrorNotice error={create.error} />
        </div>
      </Section>
      {current && <CredentialList key={current.id} workspace={workspace} projectId={projectId} environmentId={environmentId} serviceAccount={current} />}
    </>
  );
}

function scopeNames(data: ProjectScopes | undefined): string[] {
  return (data?.scopes ?? [])
    .map((entry) => (typeof entry.scope === "string" ? entry.scope : typeof entry.name === "string" ? entry.name : null))
    .filter((s): s is string => Boolean(s));
}

function CredentialList({
  workspace,
  projectId,
  environmentId,
  serviceAccount,
}: {
  workspace: Workspace;
  projectId: string | null;
  environmentId: string;
  serviceAccount: ServiceAccount;
}) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "credentials", environmentId, serviceAccount.id];
  const list = usePagedList<Credential>(key, {
    operation: "listCredentials",
    path: { environmentId, serviceAccountId: serviceAccount.id },
  });
  const scopes = useQuery({
    queryKey: ["developer-portal", workspace.id, "project-scopes", projectId],
    queryFn: () => client.call<ProjectScopes>({ operation: "listProjectScopes", path: { projectId: projectId! } }),
    enabled: Boolean(projectId),
    retry: (count, error) => shouldRetryQuery(count, error),
    retryDelay: (attempt, error) => retryDelayMs(attempt, error),
  });
  const available = scopeNames(scopes.data);

  // The one-time secret lives only here, in component state, until "Done".
  const [secret, setSecret] = useState<{ title: string; value: OneTimeSecret } | null>(null);
  const [label, setLabel] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [expiry, setExpiry] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const [rotating, setRotating] = useState<{ credential: Credential; overlap: string; key: string } | null>(null);

  const create = useConsoleMutation(
    (expiresAt: string | null) =>
      client.call<IssuedCredential>({
        operation: "createCredential",
        path: { environmentId, serviceAccountId: serviceAccount.id },
        body: { ...(chosen.length ? { scopes: chosen } : {}), ...(label.trim() ? { label: label.trim() } : {}), ...(expiresAt ? { expiresAt } : {}) },
        idempotencyKey,
      }),
    [key],
    (issued) => {
      setSecret({ title: "New API key", value: credentialSecret(issued) });
      setLabel("");
      setChosen([]);
      setExpiry("");
      setIdempotencyKey(newIdempotencyKey());
    },
  );
  const rotate = useConsoleMutation(
    (v: { credential: Credential; overlapMinutes: number | null; key: string }) =>
      client.call<IssuedCredential>({
        operation: "rotateCredential",
        path: { environmentId, credentialId: v.credential.id },
        body: v.overlapMinutes != null ? { overlapMinutes: v.overlapMinutes } : {},
        idempotencyKey: v.key,
      }),
    [key],
    (issued) => {
      setRotating(null);
      setSecret({ title: "Rotated API key", value: credentialSecret(issued) });
    },
  );
  const action = useConsoleMutation(
    (v: { credential: Credential; operation: "revokeCredential" | "suspendCredential" | "resumeCredential" }) =>
      client.call<Credential>({
        operation: v.operation,
        path: { environmentId, credentialId: v.credential.id },
        ...(v.operation === "suspendCredential" ? { body: {} } : {}),
      }),
    [key],
  );

  return (
    <Section title={`API keys of ${serviceAccount.name}`} description="Only the masked key and public ID are ever shown after creation.">
      <div className="space-y-4">
        {secret && (
          <SecretOnce
            title={secret.title}
            secret={secret.value}
            onDone={() => setSecret(null)}
            redactedHelp="This request was already completed earlier, so the secret is not returned again. If you did not store it, rotate the key to get a new secret."
          />
        )}
        {list.isLoading ? (
          <Loading />
        ) : list.error ? (
          <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
        ) : list.items.length === 0 ? (
          <Empty>No API keys yet.</Empty>
        ) : (
          <Table head={["Key", "Status", "Scopes", "Expires", "Actions"]}>
            {list.items.map((c) => {
              const allowed = credentialActions(c);
              return (
                <tr key={c.id}>
                  <Td>
                    <p className="font-medium">{c.label ?? "Unlabelled key"}</p>
                    <Mono>{c.masked ?? c.publicId ?? c.id}</Mono>
                    {c.publicId && c.masked && (
                      <p className="text-xs text-muted-foreground">
                        Public ID <Mono>{c.publicId}</Mono>
                      </p>
                    )}
                  </Td>
                  <Td>
                    <div className="flex max-w-[16rem] flex-wrap gap-1">
                      {credentialBadges(c).map((b) => (
                        <Badge key={b.label} {...b} />
                      ))}
                    </div>
                    {c.supersessionValidUntil && <p className="mt-1 text-xs text-muted-foreground">Old key valid until {formatTimestamp(c.supersessionValidUntil)}</p>}
                  </Td>
                  <Td>{c.scopes?.length ? <Mono>{c.scopes.join(" ")}</Mono> : <span className="text-muted-foreground">—</span>}</Td>
                  <Td>{formatTimestamp(c.expiresAt)}</Td>
                  <Td className="space-y-1">
                    {can("CredentialsManage") && allowed.rotate && (
                      <Button size="sm" variant="outline" onClick={() => setRotating({ credential: c, overlap: "", key: newIdempotencyKey() })}>
                        Rotate
                      </Button>
                    )}
                    {can("CredentialsRevoke") && allowed.suspend && (
                      <ConfirmButton
                        label="Suspend"
                        message="Suspend this key? It stops authenticating until resumed."
                        pending={action.isPending}
                        onConfirm={() => action.mutate({ credential: c, operation: "suspendCredential" })}
                      />
                    )}
                    {can("CredentialsManage") && allowed.resume && (
                      <Button size="sm" variant="outline" loading={action.isPending} onClick={() => action.mutate({ credential: c, operation: "resumeCredential" })}>
                        Resume
                      </Button>
                    )}
                    {can("CredentialsRevoke") && allowed.revoke && (
                      <ConfirmButton
                        label="Revoke"
                        message="Revoke this key permanently? Requests using it fail immediately."
                        destructive
                        pending={action.isPending}
                        onConfirm={() => action.mutate({ credential: c, operation: "revokeCredential" })}
                      />
                    )}
                  </Td>
                </tr>
              );
            })}
          </Table>
        )}
        <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        <ErrorNotice error={action.error} />

        {rotating && (
          <form
            className="space-y-3 rounded-lg border border-border p-4"
            onSubmit={(e) => {
              e.preventDefault();
              const overlap = rotating.overlap.trim() === "" ? null : Number(rotating.overlap);
              rotate.mutate({ credential: rotating.credential, overlapMinutes: overlap, key: rotating.key });
            }}
          >
            <p className="text-sm font-medium">Rotate {rotating.credential.label ?? rotating.credential.masked ?? "key"}</p>
            <p className="text-sm text-muted-foreground">
              A new secret is issued. The old key keeps working for the overlap window (bounded by the workspace policy), then stops.
            </p>
            <Field label="Overlap (minutes, optional)">
              <input
                className={inputClass}
                type="number"
                min={0}
                value={rotating.overlap}
                onChange={(e) => setRotating({ ...rotating, overlap: e.target.value })}
              />
            </Field>
            <div className="flex gap-2">
              <Button type="submit" size="sm" loading={rotate.isPending}>
                Rotate key
              </Button>
              <Button type="button" size="sm" variant="ghost" onClick={() => setRotating(null)}>
                Cancel
              </Button>
            </div>
            <ErrorNotice error={rotate.error} />
          </form>
        )}

        {can("CredentialsManage") && serviceAccount.status === "ACTIVE" && (
          <form
            className="space-y-3 rounded-lg border border-border p-4"
            onSubmit={(e) => {
              e.preventDefault();
              const parsed = expiryFromLocalInput(expiry);
              if (!parsed.ok) {
                setFormError(parsed.reason);
                return;
              }
              setFormError(null);
              create.mutate(parsed.iso);
            }}
          >
            <p className="text-sm font-medium">Create an API key</p>
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="Label (optional)">
                <input className={inputClass} maxLength={120} value={label} onChange={(e) => setLabel(e.target.value)} />
              </Field>
              <Field label="Expires (optional)" hint="Empty: the workspace / platform maximum lifetime.">
                <input className={inputClass} type="datetime-local" value={expiry} onChange={(e) => setExpiry(e.target.value)} />
              </Field>
            </div>
            <fieldset className="space-y-1 text-sm">
              <legend className="font-medium">Scopes</legend>
              {available.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  {projectId ? "This project has no granted scopes yet; grant them on the project page." : "Select the project to choose scopes."}
                </p>
              ) : (
                <div className="flex flex-wrap gap-3">
                  {available.map((s) => (
                    <label key={s} className="flex items-center gap-1.5">
                      <input type="checkbox" checked={chosen.includes(s)} onChange={(e) => setChosen((prev) => (e.target.checked ? [...prev, s] : prev.filter((x) => x !== s)))} />
                      <Mono>{s}</Mono>
                    </label>
                  ))}
                </div>
              )}
            </fieldset>
            {formError && <p className="text-sm text-red-600">{formError}</p>}
            <Button type="submit" size="sm" loading={create.isPending}>
              Create API key
            </Button>
            <ErrorNotice error={create.error} />
          </form>
        )}
      </div>
    </Section>
  );
}

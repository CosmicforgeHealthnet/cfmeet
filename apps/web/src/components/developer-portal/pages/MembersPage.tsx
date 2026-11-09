"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { storageStore } from "@/store";
import { newIdempotencyKey } from "@/lib/developer-platform/client";
import { actionsFor, can as canDo, memberActions, ROLE_DESCRIPTIONS } from "@/lib/developer-platform/permissions";
import { formatTimestamp, invitationToken, type OneTimeSecret } from "@/lib/developer-platform/presentation";
import { ROLES, type Invitation, type InvitationWithToken, type Member, type Role, type Workspace } from "@/lib/developer-platform/types";
import { useDeveloperPortal, useWorkspace, WORKSPACES_QUERY_KEY } from "../DeveloperPortalProvider";
import { RequireWorkspace } from "../PortalShell";
import {
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

export default function MembersPage() {
  return (
    <RequireWorkspace>
      <Members />
    </RequireWorkspace>
  );
}

function Members() {
  const workspace = useWorkspace();
  const { can } = useDeveloperPortal();
  return (
    <div className="space-y-6">
      <PageHeader
        title="Members & roles"
        description="Developer roles apply only inside this developer workspace. They are independent of CF Meet organization roles and are checked on every request."
      />
      {can("MembersRead") ? <MemberList key={workspace.id} workspace={workspace} /> : <NotPermitted what="member lists" />}
      <InvitationsSection key={`inv-${workspace.id}`} workspace={workspace} />
      <RoleTable />
    </div>
  );
}

function MemberList({ workspace }: { workspace: Workspace }) {
  const { client } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "members"];
  const list = usePagedList<Member>(key, { operation: "listMembers", path: { organizationId: workspace.id } });
  const viewer = storageStore.getUser()?.id ?? null;
  const active = list.items.filter((m) => m.status === "ACTIVE");
  const owners = active.filter((m) => m.role === "OWNER").length;

  const changeRole = useConsoleMutation(
    (v: { member: Member; role: Role }) =>
      client.call<Member>({
        operation: "changeMemberRole",
        path: { organizationId: workspace.id, memberId: v.member.id },
        body: { role: v.role, expectedVersion: v.member.version },
      }),
    [key],
  );
  const remove = useConsoleMutation(
    (member: Member) => client.call<void>({ operation: "removeMember", path: { organizationId: workspace.id, memberId: member.id } }),
    [key, WORKSPACES_QUERY_KEY],
  );
  const transfer = useConsoleMutation(
    (member: Member) => client.call<unknown>({ operation: "transferOwnership", path: { organizationId: workspace.id }, body: { memberId: member.id } }),
    [key, WORKSPACES_QUERY_KEY],
  );

  return (
    <Section title="Members" description="Members are CF Meet accounts, shown by account ID.">
      {list.isLoading ? (
        <Loading />
      ) : list.error ? (
        <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
      ) : active.length === 0 ? (
        <Empty>No members.</Empty>
      ) : (
        <>
          <Table head={["Account", "Role", "Joined via", "Since", "Actions"]}>
            {active.map((m) => {
              const actions = memberActions(workspace, m, viewer, owners);
              return (
                <tr key={m.id}>
                  <Td>
                    <Mono>{m.userId}</Mono>
                    {viewer === m.userId && <span className="ml-2 text-xs text-primary">you</span>}
                  </Td>
                  <Td>
                    {actions.changeRole ? (
                      <select
                        aria-label="Role"
                        className="rounded-md border border-border bg-background px-2 py-1 text-sm"
                        value={m.role}
                        disabled={changeRole.isPending}
                        onChange={(e) => changeRole.mutate({ member: m, role: e.target.value as Role })}
                      >
                        {ROLES.map((r) => (
                          <option key={r} value={r}>
                            {r}
                          </option>
                        ))}
                      </select>
                    ) : (
                      m.role
                    )}
                  </Td>
                  <Td>{m.joinedVia.toLowerCase()}</Td>
                  <Td>{formatTimestamp(m.createdAt)}</Td>
                  <Td className="space-x-1 whitespace-nowrap">
                    {actions.transferTo && (
                      <ConfirmButton
                        label="Make owner"
                        message="Transfer workspace ownership to this member?"
                        pending={transfer.isPending && transfer.variables?.id === m.id}
                        onConfirm={() => transfer.mutate(m)}
                      />
                    )}
                    {actions.remove && (
                      <ConfirmButton
                        label="Remove"
                        message="Remove this member from the workspace?"
                        destructive
                        pending={remove.isPending && remove.variables?.id === m.id}
                        onConfirm={() => remove.mutate(m)}
                      />
                    )}
                    {actions.leave && (
                      <ConfirmButton
                        label="Leave workspace"
                        message="Leave this developer workspace? You will lose access."
                        destructive
                        pending={remove.isPending && remove.variables?.id === m.id}
                        onConfirm={() => remove.mutate(m)}
                      />
                    )}
                  </Td>
                </tr>
              );
            })}
          </Table>
          <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
        </>
      )}
      <div className="mt-3 space-y-2">
        <ErrorNotice error={changeRole.error} />
        <ErrorNotice error={remove.error} />
        <ErrorNotice error={transfer.error} />
      </div>
    </Section>
  );
}

function InvitationsSection({ workspace }: { workspace: Workspace }) {
  const { client, can } = useDeveloperPortal();
  const key = ["developer-portal", workspace.id, "invitations"];
  const list = usePagedList<Invitation>(key, { operation: "listInvitations", path: { organizationId: workspace.id } });
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Role>("DEVELOPER");
  const [idempotencyKey, setIdempotencyKey] = useState(newIdempotencyKey);
  const [token, setToken] = useState<OneTimeSecret | null>(null);

  const create = useConsoleMutation(
    () =>
      client.call<InvitationWithToken>({
        operation: "createInvitation",
        path: { organizationId: workspace.id },
        body: { email: email.trim(), role },
        idempotencyKey,
      }),
    [key],
    (result) => {
      setToken(invitationToken(result));
      setEmail("");
      setIdempotencyKey(newIdempotencyKey());
    },
  );
  const revoke = useConsoleMutation(
    (invitation: Invitation) =>
      client.call<Invitation>({ operation: "revokeInvitation", path: { organizationId: workspace.id, invitationId: invitation.id } }),
    [key],
  );

  return (
    <Section title="Invitations" description="Invite a person by email. They accept with their own CF Meet account, signed in with the invited, verified email.">
      <div className="space-y-4">
        {token && (
          <SecretOnce
            title="Invitation token"
            secret={token}
            onDone={() => setToken(null)}
            redactedHelp="This invitation was already created by an earlier attempt, so its token is not shown again. Revoke it and invite again if the token was lost."
          />
        )}
        {token?.kind === "secret" && (
          <p className="text-xs text-muted-foreground">
            CF Meet does not email this token yet. Send it to the invitee through a trusted channel; they paste it at Developers → Accept an
            invitation.
          </p>
        )}
        {can("MembersManage") && (
          <form
            className="grid gap-3 sm:grid-cols-[2fr_1fr_auto] sm:items-end"
            onSubmit={(e) => {
              e.preventDefault();
              if (email.trim()) create.mutate(undefined);
            }}
          >
            <Field label="Email">
              <input className={inputClass} type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
            </Field>
            <Field label="Role">
              <select className={inputClass} value={role} onChange={(e) => setRole(e.target.value as Role)}>
                {ROLES.map((r) => (
                  <option key={r} value={r}>
                    {r}
                  </option>
                ))}
              </select>
            </Field>
            <Button type="submit" size="sm" loading={create.isPending} disabled={!email.trim()}>
              Invite
            </Button>
          </form>
        )}
        <ErrorNotice error={create.error} />
        {list.isLoading ? (
          <Loading />
        ) : list.error ? (
          <ErrorNotice error={list.error} onRetry={() => void list.refetch()} />
        ) : list.items.length === 0 ? (
          <Empty>No invitations.</Empty>
        ) : (
          <>
            <Table head={["Email", "Role", "Status", "Expires", ""]}>
              {list.items.map((inv) => (
                <tr key={inv.id}>
                  <Td>{inv.email}</Td>
                  <Td>{inv.role}</Td>
                  <Td>
                    <StatusBadge status={inv.status} />
                  </Td>
                  <Td>{formatTimestamp(inv.expiresAt)}</Td>
                  <Td>
                    {inv.status === "PENDING" && can("MembersManage") && (
                      <ConfirmButton
                        label="Revoke"
                        message="Revoke this invitation?"
                        destructive
                        pending={revoke.isPending && revoke.variables?.id === inv.id}
                        onConfirm={() => revoke.mutate(inv)}
                      />
                    )}
                  </Td>
                </tr>
              ))}
            </Table>
            <LoadMore hasNextPage={list.hasNextPage} isFetchingNextPage={list.isFetchingNextPage} fetchNextPage={() => void list.fetchNextPage()} />
          </>
        )}
        <ErrorNotice error={revoke.error} />
      </div>
    </Section>
  );
}

const ACTIVE_STATE = "ACTIVE" as const;

function RoleTable() {
  return (
    <Section title="Developer roles" description="What each role can do in an active workspace.">
      <div className="grid gap-3 md:grid-cols-2">
        {ROLES.map((role) => (
          <div key={role} className="rounded-lg border border-border p-3 text-sm">
            <p className="font-medium">{role}</p>
            <p className="mt-1 text-muted-foreground">{ROLE_DESCRIPTIONS[role]}</p>
            <p className="mt-2 text-xs text-muted-foreground">
              {actionsFor(role).filter((a) => canDo({ myRole: role, state: ACTIVE_STATE }, a)).length} console actions
            </p>
          </div>
        ))}
      </div>
    </Section>
  );
}

"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useProfile } from "@/hooks";
import {
  useMyOrganizations,
  useOrganizationMembers,
  useUpsertOrganizationMember,
} from "@/hooks/v2/useOrganizations";
import type {
  OrganizationMemberStatus,
  OrganizationRole,
} from "@/types/v2";

const userIdPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export default function OrganizationsContent() {
  const organizations = useMyOrganizations();
  const profile = useProfile();
  const [selectedId, setSelectedId] = useState("");
  const [userId, setUserId] = useState("");
  const [role, setRole] = useState<OrganizationRole>("MEMBER");
  const [status, setStatus] = useState<OrganizationMemberStatus>("ACTIVE");
  const [feedback, setFeedback] = useState("");
  const list = organizations.data ?? [];
  const selected = list.find(item => item.organization.id === selectedId) ?? list[0];
  const canManage = selected?.role === "OWNER" || selected?.role === "ADMIN";
  const organizationId = selected?.organization.id ?? "";
  const members = useOrganizationMembers(organizationId, !!canManage);
  const upsert = useUpsertOrganizationMember(organizationId);
  const knownMember = members.data?.some(member => member.userId === userId.trim());
  const isSelf = profile.data?.id === userId.trim();

  const editMember = (member: NonNullable<typeof members.data>[number]) => {
    setUserId(member.userId);
    setRole(member.role);
    setStatus(member.status);
    setFeedback("");
  };

  const saveMembership = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    setFeedback("");
    const targetUserId = userId.trim();
    if (!userIdPattern.test(targetUserId)) {
      setFeedback("Enter a valid user UUID.");
      return;
    }
    if (isSelf) {
      setFeedback("You cannot change your own organization membership.");
      return;
    }
    if (selected?.role === "ADMIN" && role !== "MEMBER") {
      setFeedback("Organization admins can only manage MEMBER memberships.");
      return;
    }
    if (status === "REMOVED" && !knownMember) {
      setFeedback("A membership must exist before it can be removed.");
      return;
    }

    try {
      const membership = await upsert.mutateAsync({
        userId: targetUserId,
        data: { role, status },
      });
      setFeedback(`Membership saved: ${membership.role} · ${membership.status}.`);
    } catch {
      setFeedback("");
    }
  };

  return (
    <div className="mx-auto max-w-5xl space-y-6 p-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Organizations</h1>
        <p className="text-sm text-muted-foreground">
          Review your verified organization membership and manage members where your organization role permits.
        </p>
      </header>

      {organizations.isLoading && <p role="status">Loading your organizations…</p>}
      {organizations.isError && <RequestError error={organizations.error} retry={() => void organizations.refetch()} />}
      {!organizations.isLoading && !organizations.isError && list.length === 0 && (
        <p className="rounded-lg border border-border p-4 text-sm text-muted-foreground">
          You do not currently have an active, verified organization membership.
        </p>
      )}

      {list.length > 0 && selected && (
        <>
          <section className="space-y-3 rounded-xl border border-border bg-card p-4">
            <label className="grid gap-2 text-sm font-medium">
              Organization
              <select
                aria-label="Organization"
                className="h-10 rounded-md border border-input bg-background px-3"
                value={organizationId}
                onChange={event => {
                  setSelectedId(event.target.value);
                  setFeedback("");
                }}
              >
                {list.map(item => (
                  <option key={item.organization.id} value={item.organization.id}>
                    {item.organization.name} · {item.role}
                  </option>
                ))}
              </select>
            </label>
            <div className="text-sm">
              <p className="font-semibold">{selected.organization.name}</p>
              <p className="text-muted-foreground">
                {selected.organization.slug} · {selected.organization.status} · Your role: {selected.role}
              </p>
            </div>
          </section>

          {canManage ? (
            <section className="space-y-4 rounded-xl border border-border bg-card p-4">
              <div>
                <h2 className="font-semibold">Organization members</h2>
                <p className="text-sm text-muted-foreground">
                  The server enforces organization authority and validates that user IDs belong to active registered accounts.
                </p>
              </div>
              {members.isLoading && <p role="status" className="text-sm">Loading members…</p>}
              {members.isError && <RequestError error={members.error} retry={() => void members.refetch()} />}
              {members.data && (
                <div className="space-y-2">
                  {members.data.length === 0 && <p className="text-sm text-muted-foreground">No memberships found.</p>}
                  {members.data.map(member => (
                    <article key={member.id} className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-border p-3">
                      <div className="min-w-0">
                        <p className="break-all font-mono text-xs">{member.userId}</p>
                        <p className="text-sm text-muted-foreground">{member.role} · {member.status}</p>
                      </div>
                      <Button
                        type="button"
                        size="sm"
                        variant="outline"
                        disabled={selected.role === "ADMIN" && member.role !== "MEMBER"}
                        onClick={() => editMember(member)}
                      >
                        {selected.role === "ADMIN" && member.role !== "MEMBER" ? "Owner/admin only" : "Edit"}
                      </Button>
                    </article>
                  ))}
                </div>
              )}

              <form className="grid gap-3 border-t border-border pt-4 sm:grid-cols-2" onSubmit={event => void saveMembership(event)}>
                <label className="grid gap-2 text-sm font-medium sm:col-span-2">
                  User ID
                  <Input
                    value={userId}
                    onChange={event => setUserId(event.target.value)}
                    placeholder="UUID of an active registered user"
                    autoComplete="off"
                    required
                  />
                </label>
                <label className="grid gap-2 text-sm font-medium">
                  Organization role
                  <select
                    className="h-10 rounded-md border border-input bg-background px-3"
                    value={role}
                    onChange={event => setRole(event.target.value as OrganizationRole)}
                  >
                    {(selected.role === "OWNER" ? ["OWNER", "ADMIN", "MEMBER"] : ["MEMBER"]).map(value => (
                      <option key={value} value={value}>{value}</option>
                    ))}
                  </select>
                </label>
                <label className="grid gap-2 text-sm font-medium">
                  Membership status
                  <select
                    className="h-10 rounded-md border border-input bg-background px-3"
                    value={status}
                    onChange={event => setStatus(event.target.value as OrganizationMemberStatus)}
                  >
                    {(["ACTIVE", "SUSPENDED", "REMOVED"] as const).map(value => (
                      <option key={value} value={value}>{value}</option>
                    ))}
                  </select>
                </label>
                {isSelf && <p role="alert" className="text-sm text-destructive sm:col-span-2">You cannot change your own membership.</p>}
                {feedback && <p role="status" className="text-sm sm:col-span-2">{feedback}</p>}
                {upsert.isError && <div className="sm:col-span-2"><RequestError error={upsert.error} /></div>}
                <div className="flex flex-wrap gap-2 sm:col-span-2">
                  <Button type="submit" disabled={upsert.isPending || isSelf}>
                    {upsert.isPending ? "Saving…" : knownMember ? "Update membership" : "Add member"}
                  </Button>
                  <Button type="button" variant="outline" onClick={() => {
                    setUserId("");
                    setRole("MEMBER");
                    setStatus("ACTIVE");
                    setFeedback("");
                  }}>
                    Clear
                  </Button>
                </div>
              </form>
            </section>
          ) : (
            <p className="rounded-lg border border-border p-4 text-sm text-muted-foreground">
              Your {selected.role} role can view this organization in your membership list but cannot manage its members. Member access is limited to organization owners, admins, and platform administrators.
            </p>
          )}
        </>
      )}
    </div>
  );
}

"use client";

import { useState } from "react";
import Link from "next/link";
import { ArrowLeft } from "lucide-react";
import { cn } from "@/lib/utils";
import { storageStore } from "@/store";
import { useBusinessOrganization, useOrganizationProvisioning } from "@/hooks/v2/useBusinessOrganizations";
import {
  legalVerificationLabel,
  organizationSections,
  provisioningLabel,
  shortId,
  type OrganizationSections,
} from "@/lib/organizations/organization-core";
import type { BusinessOrganization } from "@/types/generated/meeting-service";
import { Detail, OrgError, Section, formatDateTime } from "./shared";
import OrganizationMembers from "./OrganizationMembers";
import OrganizationInvitations from "./OrganizationInvitations";
import OrganizationSettings from "./OrganizationSettings";
import OrganizationAudit from "./OrganizationAudit";

type Tab = "overview" | "members" | "invitations" | "settings" | "audit";

const TABS: Array<{ id: Tab; label: string; visible: (s: OrganizationSections) => boolean }> = [
  { id: "overview", label: "Overview", visible: (s) => s.overview },
  { id: "members", label: "Members", visible: (s) => s.overview },
  { id: "invitations", label: "Invitations", visible: (s) => s.invitations },
  { id: "settings", label: "Settings", visible: (s) => s.settings || s.lifecycle || s.ownership },
  { id: "audit", label: "Audit log", visible: (s) => s.audit },
];

/**
 * One organization (Section 8/17 "Manage organization"): every tab and control is shown only
 * when the caller's server-derived capabilities (`viewer.capabilities`, re-read on each visit)
 * allow it. The Members tab is always visible to members (it holds "Leave"); the list itself
 * needs `organization.members.read`.
 */
export default function OrganizationAdmin({ organizationId }: { organizationId: string }) {
  const org = useBusinessOrganization(organizationId);
  const [tab, setTab] = useState<Tab>("overview");

  if (org.isLoading) return <p role="status" className="p-6">Loading organization…</p>;
  if (org.isError || !org.data) {
    return (
      <div className="mx-auto max-w-5xl space-y-4 p-6">
        <BackLink />
        <OrgError error={org.error} retry={() => void org.refetch()} />
      </div>
    );
  }

  const data = org.data;
  const sections = organizationSections(data);
  const tabs = TABS.filter((t) => t.visible(sections));
  const active = tabs.some((t) => t.id === tab) ? tab : "overview";

  return (
    <div className="mx-auto max-w-5xl space-y-6 p-6">
      <BackLink />
      <header>
        <h1 className="text-2xl font-bold tracking-tight">{data.name}</h1>
        <p className="text-sm text-muted-foreground">
          {data.slug} · {data.lifecycleStatus}
          {data.viewer ? ` · Your role: ${data.viewer.role}` : ""}
        </p>
        {data.lifecycleStatus === "SUSPENDED" && (
          <p role="status" className="mt-2 rounded-md border border-amber-300 bg-amber-50 p-2 text-sm text-amber-900 dark:bg-amber-950 dark:text-amber-100">
            This organization is suspended: organization meetings, invitations and colleague lists are paused.
          </p>
        )}
      </header>

      <nav className="flex overflow-x-auto border-b border-border" aria-label="Organization sections">
        {tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            onClick={() => setTab(t.id)}
            aria-current={active === t.id ? "page" : undefined}
            className={cn(
              "whitespace-nowrap border-b-2 px-4 py-2 text-sm font-medium",
              active === t.id ? "border-primary text-primary" : "border-transparent text-muted-foreground hover:text-foreground",
            )}
          >
            {t.label}
          </button>
        ))}
      </nav>

      {active === "overview" && <Overview org={data} />}
      {active === "members" && <OrganizationMembers org={data} />}
      {active === "invitations" && <OrganizationInvitations org={data} />}
      {active === "settings" && <OrganizationSettings org={data} />}
      {active === "audit" && <OrganizationAudit organizationId={data.id} />}
    </div>
  );
}

function BackLink() {
  return (
    <Link href="/dashboard/organizations" className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
      <ArrowLeft className="h-4 w-4" /> Organizations
    </Link>
  );
}

/** ORG-003: creation metadata, profile, location and self-declared legal fields as served. */
function Overview({ org }: { org: BusinessOrganization }) {
  const provisioning = useOrganizationProvisioning(org.id);
  const profile = org.profile ?? {};
  const legal = profile.legalRegistration;
  const createdBy = org.createdBy
    ? org.createdBy === storageStore.getUser()?.id
      ? "You"
      : `Account ${shortId(org.createdBy)}`
    : "—";

  return (
    <div className="space-y-4">
      <Section title="About">
        <dl className="grid gap-4 sm:grid-cols-2">
          <Detail label="Description">{profile.description || "—"}</Detail>
          <Detail label="Industry">{profile.industry || "—"}</Detail>
          <Detail label="Organization type">{profile.organizationType || "—"}</Detail>
          <Detail label="Public contact email">{profile.publicContactEmail || "—"}</Detail>
          <Detail label="Members">{typeof org.memberCount === "number" ? org.memberCount : "—"}</Detail>
          <Detail label="Status">{org.lifecycleStatus}</Detail>
        </dl>
      </Section>

      <Section title="Location">
        <dl className="grid gap-4 sm:grid-cols-2">
          <Detail label="City">{profile.city || "—"}</Detail>
          <Detail label="Country">{profile.countryCode || "—"}</Detail>
          <Detail label="Operating address">{profile.operatingAddress || "—"}</Detail>
        </dl>
      </Section>

      <Section title="Legal registration" description={legalVerificationLabel(legal?.verification)}>
        {legal && (legal.registeredName || legal.registrationNumber || legal.incorporatedOn) ? (
          <dl className="grid gap-4 sm:grid-cols-2">
            <Detail label="Registered name">{legal.registeredName || "—"}</Detail>
            <Detail label="Registration number">{legal.registrationNumber || "—"}</Detail>
            <Detail label="Incorporated on">{legal.incorporatedOn || "—"}</Detail>
            <Detail label="Verification">{legalVerificationLabel(legal.verification)}</Detail>
          </dl>
        ) : (
          <p className="text-sm text-muted-foreground">No legal registration details were provided.</p>
        )}
      </Section>

      <Section title="Record">
        <dl className="grid gap-4 sm:grid-cols-2">
          <Detail label="Created">{formatDateTime(org.createdAt)}</Detail>
          <Detail label="Created by">{createdBy}</Detail>
          <Detail label="Created through">{org.creationSource}</Detail>
          <Detail label="Last updated">{formatDateTime(org.updatedAt)}</Detail>
          {org.archivedAt && <Detail label="Archived">{formatDateTime(org.archivedAt)}</Detail>}
        </dl>
      </Section>

      <Section title="Setup" description="Technical provisioning of this organization's services.">
        {provisioning.isLoading && <p role="status" className="text-sm">Checking setup…</p>}
        {provisioning.isError && <OrgError error={provisioning.error} retry={() => void provisioning.refetch()} />}
        {provisioning.data && (
          <dl className="grid gap-4 sm:grid-cols-2">
            <Detail label="State">{provisioningLabel(provisioning.data.provisioningState)}</Detail>
            {provisioning.data.step && <Detail label="Current step">{provisioning.data.step}</Detail>}
            {provisioning.data.lastErrorClass && <Detail label="Last failure">{provisioning.data.lastErrorClass}</Detail>}
            {provisioning.data.nextAttemptAt && <Detail label="Next attempt">{formatDateTime(provisioning.data.nextAttemptAt)}</Detail>}
            <Detail label="Updated">{formatDateTime(provisioning.data.updatedAt)}</Detail>
          </dl>
        )}
      </Section>
    </div>
  );
}

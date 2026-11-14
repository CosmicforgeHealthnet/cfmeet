"use client";

import { useState } from "react";
import Link from "next/link";
import { ChevronLeft } from "lucide-react";
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
import { Detail, OrgError, OrgMark, Section, formatDateTime } from "./shared";

const nice = (v?: string | null) => (v ? v.charAt(0) + v.slice(1).toLowerCase().replaceAll("_", " ") : "—");
import OrganizationMembers from "./OrganizationMembers";
import OrganizationInvitations from "./OrganizationInvitations";
import OrganizationSettings from "./OrganizationSettings";
import OrganizationAudit from "./OrganizationAudit";
import { ORGANIZATION_CALLING_CAPABILITY, OrganizationCallingPanel } from "@/components/calling/OrganizationCallingAdmin";

type Tab = "overview" | "members" | "invitations" | "settings" | "audit" | "calling";

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

  if (org.isLoading) return <div className="mx-auto max-w-7xl space-y-4 px-4 py-6 sm:px-6 lg:px-8 lg:py-8" aria-busy="true"><div className="h-20 animate-pulse rounded-2xl bg-muted" /><div className="h-64 animate-pulse rounded-2xl bg-muted" /></div>;
  if (org.isError || !org.data) {
    return (
      <div className="mx-auto max-w-7xl space-y-4 px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
        <BackLink />
        <OrgError error={org.error} retry={() => void org.refetch()} />
      </div>
    );
  }

  const data = org.data;
  const sections = organizationSections(data);
  // Enterprise calling administration (Section 12 relay): only with calling.enterprise.manage.
  const callingAdmin = data.viewer?.status === "ACTIVE" && Array.isArray(data.viewer.capabilities) && data.viewer.capabilities.includes(ORGANIZATION_CALLING_CAPABILITY);
  const tabs = [...TABS.filter((t) => t.visible(sections)), ...(callingAdmin ? [{ id: "calling" as Tab, label: "Calling", visible: () => true }] : [])];
  const active = tabs.some((t) => t.id === tab) ? tab : "overview";

  return (
    <div className="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8 lg:py-8">
      <BackLink />
      <header className="mt-3 flex items-center gap-4">
        <OrgMark name={data.name} size="size-14" className="text-lg" />
        <div className="min-w-0">
          <h1 className="truncate text-xl font-bold tracking-tight sm:text-3xl">{data.name}</h1>
          <p className="mt-1 flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground">
            {data.viewer ? <span className="rounded-full bg-primary/10 px-2.5 py-0.5 text-xs font-semibold text-primary">You&apos;re {nice(data.viewer.role).toLowerCase()}</span> : null}
            {data.lifecycleStatus !== "ACTIVE" ? <span className="rounded-full bg-amber-500/15 px-2.5 py-0.5 text-xs font-semibold text-amber-700 dark:text-amber-300">{nice(data.lifecycleStatus)}</span> : null}
            {typeof data.memberCount === "number" ? <span>{data.memberCount} {data.memberCount === 1 ? "member" : "members"}</span> : null}
          </p>
        </div>
      </header>
      {data.lifecycleStatus === "SUSPENDED" && (
        <p role="status" className="mt-4 rounded-xl bg-amber-500/10 px-4 py-3 text-sm text-amber-800 dark:text-amber-200">
          This organization is suspended. Its meetings, invitations and colleague lists are paused.
        </p>
      )}

      <nav className="no-scrollbar mt-6 flex overflow-x-auto border-b border-border" aria-label="Organization sections">
        {tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            onClick={() => setTab(t.id)}
            aria-current={active === t.id ? "page" : undefined}
            className={cn(
              "-mb-px h-11 whitespace-nowrap border-b-2 px-3 text-sm font-medium transition-colors sm:px-4",
              active === t.id ? "border-primary text-foreground" : "border-transparent text-muted-foreground hover:text-foreground",
            )}
          >
            {t.label}
          </button>
        ))}
      </nav>

      <div className="mt-6">
      {active === "overview" && <Overview org={data} />}
      {active === "members" && <OrganizationMembers org={data} />}
      {active === "invitations" && <OrganizationInvitations org={data} />}
      {active === "settings" && <OrganizationSettings org={data} />}
      {active === "audit" && <OrganizationAudit organizationId={data.id} />}
      {active === "calling" && <OrganizationCallingPanel organizationId={data.id} name={data.name} embedded />}
      </div>
    </div>
  );
}

function BackLink() {
  return (
    <Link href="/dashboard/organizations" className="inline-flex items-center gap-1 text-sm font-medium text-muted-foreground hover:text-foreground">
      <ChevronLeft className="size-4" /> Organizations
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

  const location = [profile.city, profile.countryCode].filter(Boolean).join(", ");
  const hasLegal = !!legal && !!(legal.registeredName || legal.registrationNumber || legal.incorporatedOn);
  const settingUp = !!provisioning.data && !["READY", "NOT_REQUIRED"].includes(provisioning.data.provisioningState);

  return (
    <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,1.6fr)_minmax(0,1fr)]">
      <div className="space-y-6">
        <Section title="About">
          {profile.description ? <p className="text-sm leading-relaxed text-foreground">{profile.description}</p> : <p className="text-sm text-muted-foreground">No description yet.</p>}
          <dl className="grid gap-5 sm:grid-cols-2">
            <Detail label="Industry">{profile.industry || "—"}</Detail>
            <Detail label="Type">{profile.organizationType || "—"}</Detail>
            <Detail label="Contact email">{profile.publicContactEmail || "—"}</Detail>
            <Detail label="Location">{location || "—"}</Detail>
            {profile.operatingAddress ? <div className="sm:col-span-2"><Detail label="Address">{profile.operatingAddress}</Detail></div> : null}
          </dl>
        </Section>
        {hasLegal ? (
          <Section title="Legal registration" description={legalVerificationLabel(legal?.verification)}>
            <dl className="grid gap-5 sm:grid-cols-2">
              <Detail label="Registered name">{legal?.registeredName || "—"}</Detail>
              <Detail label="Registration number">{legal?.registrationNumber || "—"}</Detail>
              <Detail label="Incorporated">{legal?.incorporatedOn || "—"}</Detail>
            </dl>
          </Section>
        ) : null}
      </div>
      <div className="space-y-6">
        {settingUp || provisioning.isError ? (
          <Section title="Setting up" description="We're preparing this organization's services.">
            {provisioning.isError && <OrgError error={provisioning.error} retry={() => void provisioning.refetch()} />}
            {provisioning.data && (
              <dl className="grid gap-4">
                <Detail label="Status">{provisioningLabel(provisioning.data.provisioningState)}</Detail>
                {provisioning.data.step && <Detail label="Step">{provisioning.data.step}</Detail>}
                {provisioning.data.lastErrorClass && <Detail label="Last problem">{provisioning.data.lastErrorClass}</Detail>}
                {provisioning.data.nextAttemptAt && <Detail label="Next try">{formatDateTime(provisioning.data.nextAttemptAt)}</Detail>}
              </dl>
            )}
          </Section>
        ) : null}
        <Section title="Details">
          <dl className="grid gap-4">
            <Detail label="Created">{formatDateTime(org.createdAt)}</Detail>
            <Detail label="Created by">{createdBy}</Detail>
            <Detail label="Last updated">{formatDateTime(org.updatedAt)}</Detail>
            {org.archivedAt && <Detail label="Archived">{formatDateTime(org.archivedAt)}</Detail>}
            {!hasLegal ? <Detail label="Legal registration">Not provided</Detail> : null}
          </dl>
        </Section>
      </div>
    </div>
  );
}

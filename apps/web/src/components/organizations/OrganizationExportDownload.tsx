"use client";

import { useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { Button } from "@/components/ui/button";
import { accountSession } from "@/lib/auth/session";
import {
  ORGANIZATION_EXPORT_PENDING_KEY,
  type DownloadOutcome,
  type OrganizationExportLink,
  asOrganizationExportLink,
  downloadOutcome,
  downloadOutcomeText,
  exportFileName,
  forgetLink,
  isFinalOutcome,
  organizationExportLinkFrom,
  stashLink,
  stashedLink,
} from "@/lib/account/owner-exports";
import { saveTextFile } from "@/lib/account/save-file";
import { ownerExportsService } from "@/services/owner-exports.service";

const PAGE = "/organizations/exports/download";
const store = () => {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
};

/** Reads the fragment once per page load and erases it from the address bar and history. */
function takeLink(): OrganizationExportLink | null {
  const fromHash = organizationExportLinkFrom(window.location.hash);
  if (window.location.hash) window.history.replaceState(null, "", window.location.pathname);
  if (fromHash) {
    stashLink(store(), ORGANIZATION_EXPORT_PENDING_KEY, fromHash, Date.now());
    return fromHash;
  }
  return stashedLink(store(), ORGANIZATION_EXPORT_PENDING_KEY, Date.now(), asOrganizationExportLink);
}

const noop = () => () => {};

/**
 * `/organizations/exports/download#org=<id>&export=<id>&token=<token>` (ADR 0068): the
 * organization export CF Meet support requested, downloaded once by an OWNER it was sent to.
 * The fragment never reaches a server; the token is posted only in the request body.
 */
export default function OrganizationExportDownload() {
  const mounted = useSyncExternalStore(noop, () => true, () => false);
  return (
    <main className="mx-auto flex min-h-screen max-w-lg items-center p-6">
      <div className="w-full space-y-4 rounded-2xl border border-border bg-card p-6 shadow-sm">
        <h1 className="text-xl font-bold">Download your organization&apos;s data</h1>
        {mounted ? <Download /> : <p role="status">Loading…</p>}
      </div>
    </main>
  );
}

function Download() {
  const [link] = useState(takeLink);
  const [signedIn] = useState(() => accountSession.hasSession());
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const [outcome, setOutcome] = useState<DownloadOutcome | null>(null);

  if (!link) {
    return (
      <p className="text-sm text-muted-foreground">
        This page needs the download link from your email. Open the link again; if it has expired, ask CF Meet support to request the export again.
      </p>
    );
  }

  if (!signedIn || outcome === "sign_in" || outcome === "reauthenticate") {
    return (
      <div className="space-y-3 text-sm">
        <p>Sign in with the CF Meet account of the organization owner this email was sent to, then come back here.</p>
        <Button asChild>
          <Link href={`/login?next=${encodeURIComponent(PAGE)}`}>Sign in</Link>
        </Button>
      </div>
    );
  }

  if (done) {
    return (
      <div className="space-y-3 text-sm">
        <p role="status">The organization&apos;s data was downloaded. This link no longer works.</p>
        <Link className="underline" href={`/dashboard/organizations/${encodeURIComponent(link.organizationId)}`}>
          Open the organization
        </Link>
      </div>
    );
  }

  const download = async () => {
    setBusy(true);
    setOutcome(null);
    try {
      const text = await ownerExportsService.downloadOrganizationExport(link.organizationId, link.exportId, link.token);
      saveTextFile(text, exportFileName("organization", link.exportId, new Date()));
      forgetLink(store(), ORGANIZATION_EXPORT_PENDING_KEY);
      setDone(true);
    } catch (error) {
      const o = downloadOutcome(error);
      if (isFinalOutcome(o)) forgetLink(store(), ORGANIZATION_EXPORT_PENDING_KEY);
      setOutcome(o);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-3 text-sm">
      <p>
        CF Meet support requested a copy of your organization&apos;s data for its owners. Only an owner it was sent to can download it, once with this link.
      </p>
      {outcome && (
        <p role="alert" className="rounded-md border border-destructive/30 p-2 text-destructive">
          {outcome === "invalid"
            ? "This download link is invalid, already used, or not for this account (only an active owner it was sent to can use it)."
            : downloadOutcomeText(outcome)}
        </p>
      )}
      {!(outcome && isFinalOutcome(outcome)) && (
        <Button disabled={busy} onClick={() => void download()}>
          {busy ? "Downloading…" : "Download the organization's data"}
        </Button>
      )}
    </div>
  );
}

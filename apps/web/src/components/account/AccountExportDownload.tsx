"use client";

import { useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Button } from "@/components/ui/button";
import { accountSession, clearBrowserSession } from "@/lib/auth/session";
import {
  ACCOUNT_EXPORT_PENDING_KEY,
  type AccountExportLink,
  accountExportLinkFrom,
  asAccountExportLink,
  type DownloadOutcome,
  downloadOutcome,
  downloadOutcomeText,
  exportFileName,
  forgetLink,
  isFinalOutcome,
  stashLink,
  stashedLink,
} from "@/lib/account/owner-exports";
import { saveTextFile } from "@/lib/account/save-file";
import { ownerExportsService } from "@/services/owner-exports.service";

const PAGE = "/settings/data-export/download";
const store = () => {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
};

/** Reads the link once per page load and removes the token from the address bar at once. */
function takeLink(): AccountExportLink | null {
  const fromUrl = accountExportLinkFrom(window.location.search);
  if (window.location.search) window.history.replaceState(null, "", window.location.pathname);
  if (fromUrl) {
    stashLink(store(), ACCOUNT_EXPORT_PENDING_KEY, fromUrl, Date.now());
    return fromUrl;
  }
  return stashedLink(store(), ACCOUNT_EXPORT_PENDING_KEY, Date.now(), asAccountExportLink);
}

const noop = () => () => {};

/**
 * `/settings/data-export/download?export=<id>&token=<token>`: the copy of your data CF Meet
 * support requested for you (ADR 0068). Needs a fresh sign-in; the file is saved once.
 */
export default function AccountExportDownload() {
  const mounted = useSyncExternalStore(noop, () => true, () => false);
  return (
    <main className="mx-auto flex min-h-screen max-w-lg items-center p-6">
      <div className="w-full space-y-4 rounded-2xl border border-border bg-card p-6 shadow-sm">
        <h1 className="text-xl font-bold">Download your data</h1>
        {mounted ? <Download /> : <p role="status">Loading…</p>}
      </div>
    </main>
  );
}

function Download() {
  const router = useRouter();
  const [link] = useState(takeLink);
  const [signedIn] = useState(() => accountSession.hasSession());
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const [outcome, setOutcome] = useState<DownloadOutcome | null>(null);

  const signInAgain = async () => {
    // A fresh sign-in: end this one, then come back here (the link waits in this tab).
    await clearBrowserSession();
    router.push(`/login?next=${encodeURIComponent(PAGE)}&reason=data_export`);
  };

  if (!link) {
    return (
      <p className="text-sm text-muted-foreground">
        This page needs the download link from your email. Open the link again; if it has expired, ask CF Meet support to request your data again.
      </p>
    );
  }

  if (!signedIn || outcome === "sign_in") {
    return (
      <div className="space-y-3 text-sm">
        <p>Sign in to the CF Meet account this email was sent for, then come back here to download your data.</p>
        <Button asChild>
          <Link href={`/login?next=${encodeURIComponent(PAGE)}&reason=data_export`}>Sign in</Link>
        </Button>
      </div>
    );
  }

  if (done) {
    return (
      <div className="space-y-3 text-sm">
        <p role="status">Your data was downloaded. The link no longer works and CF Meet deleted its copy.</p>
        <Link className="underline" href="/dashboard">
          Go to your dashboard
        </Link>
      </div>
    );
  }

  const download = async () => {
    setBusy(true);
    setOutcome(null);
    try {
      const text = await ownerExportsService.downloadAccountExport(link.exportId, link.token);
      saveTextFile(text, exportFileName("account", link.exportId, new Date()));
      forgetLink(store(), ACCOUNT_EXPORT_PENDING_KEY);
      setDone(true);
    } catch (error) {
      const o = downloadOutcome(error);
      if (isFinalOutcome(o)) forgetLink(store(), ACCOUNT_EXPORT_PENDING_KEY);
      setOutcome(o);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-3 text-sm">
      <p>
        CF Meet support requested a copy of your personal data for you. Only you can download it, once. CF Meet support never sees it.
      </p>
      {outcome && (
        <p role="alert" className="rounded-md border border-destructive/30 p-2 text-destructive">
          {downloadOutcomeText(outcome)}
        </p>
      )}
      {outcome === "reauthenticate" ? (
        <Button onClick={() => void signInAgain()}>Sign in again</Button>
      ) : (
        !(outcome && isFinalOutcome(outcome)) && (
          <Button disabled={busy} onClick={() => void download()}>
            {busy ? "Downloading…" : "Download my data"}
          </Button>
        )
      )}
    </div>
  );
}

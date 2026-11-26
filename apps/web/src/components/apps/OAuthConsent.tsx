"use client";

import { useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { Building2, Check, Plug, ShieldCheck, UserRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import apiV2 from "@/services/v2/api";
import { accountSession } from "@/lib/auth/session";
import { getErrorMessage } from "@/utils/error";

type ScopeView = { name: string; description: string | null; who: string | null };
type ConsentContext = {
  app: { clientId: string; name: string; distribution: string | null; status: string | null; developerName: string | null };
  scopes: ScopeView[];
  target: "USER" | "ORGANIZATION" | string;
  organizationId: string | null;
  redirectUri: string;
  state: string | null;
  organizationInstall: { allowed: boolean; reason: string | null } | null;
};

/** The OAuth request's own parameters, forwarded unchanged (nothing is invented here). */
const PARAMS = ["client_id", "redirect_uri", "scope", "state", "code_challenge", "code_challenge_method", "install", "organization_id", "response_type"] as const;

const INSTALL_REFUSED: Record<string, string> = {
  NOT_A_MEMBER: "You're not a member of that organization.",
  ROLE_REQUIRED: "Only the organization's admins can install apps for it.",
  NOT_ALLOWLISTED: "This organization doesn't allow this app.",
  ORGANIZATION_NOT_READY: "That organization isn't ready to install apps yet.",
};

const noop = () => () => {};

/**
 * CF Meet's OAuth consent page (`/oauth/authorize`): an app asks to use your account; you
 * see who made it and what it may do, then Allow or Cancel. The app's request is validated
 * by Platform Access (client, exact redirect URI, scopes, PKCE S256) before anything shows.
 */
export default function OAuthConsent() {
  const mounted = useSyncExternalStore(noop, () => true, () => false);
  return (
    <main className="mx-auto flex min-h-screen max-w-lg items-center p-6">
      <div className="w-full rounded-2xl border border-border bg-card p-6 shadow-sm">
        {mounted ? <Consent /> : <p role="status" className="text-sm">Loading…</p>}
      </div>
    </main>
  );
}

function Consent() {
  const [query] = useState(() => new URLSearchParams(window.location.search));
  const [signedIn] = useState(() => accountSession.hasSession());
  const request = Object.fromEntries(PARAMS.map((k) => [k, query.get(k)]).filter(([, v]) => v !== null)) as Record<string, string>;
  const context = useQuery({
    queryKey: ["oauth", "authorize", query.toString()],
    queryFn: async () => (await apiV2.get<ConsentContext>("/oauth/authorize/context", { params: request })).data,
    enabled: signedIn && !!request.client_id,
    retry: false,
  });
  const [deciding, setDeciding] = useState<"APPROVE" | "DENY" | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (!request.client_id) {
    return <p className="text-sm text-muted-foreground">This link is incomplete. Go back to the app and try connecting again.</p>;
  }
  if (!signedIn) {
    const next = `/oauth/authorize?${query.toString()}`;
    return (
      <div className="space-y-4 text-sm">
        <Heading />
        <p>Sign in to CF Meet to choose whether this app can use your account.</p>
        <Button asChild className="w-full"><Link href={`/login?next=${encodeURIComponent(next)}`}>Sign in</Link></Button>
      </div>
    );
  }
  if (context.isPending) return <p role="status" className="text-sm">Checking the app&apos;s request…</p>;
  if (context.isError || !context.data) {
    return (
      <div className="space-y-3 text-sm">
        <Heading />
        <p role="alert" className="rounded-xl bg-destructive/10 p-3 text-destructive">{getErrorMessage(context.error, "This app's request isn't valid.")}</p>
        <p className="text-muted-foreground">Go back to the app and try again. You haven&apos;t given it access.</p>
      </div>
    );
  }

  const c = context.data;
  const refused = c.organizationInstall && !c.organizationInstall.allowed;
  const decide = async (decision: "APPROVE" | "DENY") => {
    setDeciding(decision);
    setError(null);
    try {
      const r = (await apiV2.post<{ redirectTo: string }>("/oauth/authorize/approve", {
        client_id: request.client_id,
        redirect_uri: c.redirectUri,
        scope: request.scope ?? "",
        state: request.state ?? null,
        code_challenge: request.code_challenge ?? "",
        code_challenge_method: request.code_challenge_method ?? "",
        install: request.install ?? null,
        organization_id: request.organization_id ?? null,
        decision,
      })).data;
      // Only ever back to the redirect URI Platform Access validated for this app.
      if (!r.redirectTo.startsWith(c.redirectUri)) throw new Error("The app's return address doesn't match.");
      window.location.assign(r.redirectTo);
    } catch (e) {
      setError(getErrorMessage(e, decision === "APPROVE" ? "The app couldn't be connected. Try again." : "Couldn't return to the app."));
      setDeciding(null);
    }
  };

  return (
    <div className="space-y-5 text-sm" data-testid="oauth-consent">
      <div className="flex flex-col items-center text-center">
        <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Plug className="size-6" /></span>
        <h1 className="mt-3 text-xl font-bold">{c.app.name} wants to use your CF Meet account</h1>
        <p className="mt-1 text-muted-foreground">
          {c.app.developerName ? `By ${c.app.developerName}` : "Developer not listed"}
          {c.app.distribution === "PRIVATE" ? " · private app" : ""}
        </p>
      </div>
      <div className="flex items-center gap-2 rounded-xl bg-muted/60 px-3 py-2">
        {c.target === "ORGANIZATION" ? <Building2 className="size-4 text-muted-foreground" /> : <UserRound className="size-4 text-muted-foreground" />}
        <span>{c.target === "ORGANIZATION" ? "Installing for your organization" : "Connecting to your account"}</span>
      </div>
      <section>
        <p className="mb-2 font-semibold">It will be able to:</p>
        <ul className="space-y-2">
          {c.scopes.map((s) => (
            <li key={s.name} className="flex items-start gap-2">
              <Check className="mt-0.5 size-4 shrink-0 text-emerald-600" />
              <span><span className="block">{s.description ?? s.name}</span>{s.who === "bot" ? <span className="block text-xs text-muted-foreground">As the app itself, not as you</span> : null}</span>
            </li>
          ))}
        </ul>
      </section>
      <p className="flex items-start gap-2 text-xs text-muted-foreground">
        <ShieldCheck className="mt-0.5 size-4 shrink-0" />
        You can disconnect it any time in Settings → Connected apps. It will return to {new URL(c.redirectUri).host}.
      </p>
      {refused ? <p role="alert" className="rounded-xl bg-destructive/10 p-3 text-destructive">{INSTALL_REFUSED[c.organizationInstall!.reason ?? ""] ?? "This organization doesn't allow this install."}</p> : null}
      {error ? <p role="alert" className="rounded-xl bg-destructive/10 p-3 text-destructive">{error}</p> : null}
      <div className="flex gap-2">
        <Button variant="outline" className="flex-1" disabled={!!deciding} onClick={() => void decide("DENY")} data-testid="oauth-cancel">Cancel</Button>
        <Button className="flex-1" disabled={!!deciding || !!refused} onClick={() => void decide("APPROVE")} data-testid="oauth-allow">{deciding === "APPROVE" ? "Connecting…" : "Allow"}</Button>
      </div>
    </div>
  );
}

function Heading() {
  return <h1 className="text-xl font-bold">Connect an app to CF Meet</h1>;
}

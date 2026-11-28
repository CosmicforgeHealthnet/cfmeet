"use client";

import { useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { UsersRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import { communitiesService as api } from "@/services/v2/communities.service";
import { outcomeText, tokenFrom, PRIVACY_LABELS, MAX_REQUEST_MESSAGE } from "@/lib/communities";
import { useCommunitiesRefresh } from "@/hooks/v2/useCommunities";
import { ErrorNote, field } from "./bits";

/** A community invite link: a preview first; nothing happens until you tap Join. */
export default function JoinCommunity() {
  const router = useRouter();
  const token = tokenFrom(useSearchParams().get("token") ?? "");
  const refresh = useCommunitiesRefresh();
  const preview = useQuery({ queryKey: ["v2", "communities", "preview", token], queryFn: () => api.previewLink(token!), enabled: !!token, retry: false });
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const p = preview.data;
  const join = async () => {
    setBusy(true); setError(null);
    try {
      const o = await api.joinByLink(token!, message);
      await refresh();
      if (o.outcome === "REQUESTED") { setDone(outcomeText(o, "community")); setBusy(false); return; }
      router.replace(`/dashboard/communities/${o.community?.communityId ?? p!.communityId}`);
    } catch (e) {
      setError(messagingErrorMessage(e));
      setBusy(false);
    }
  };
  return (
    <div className="flex min-h-full items-center justify-center p-6">
      <div className="w-full max-w-sm space-y-4 rounded-3xl border border-border bg-card p-6 text-center shadow-sm">
        <span className="mx-auto flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><UsersRound className="size-7" /></span>
        {!token ? (
          <><h1 className="text-xl font-semibold">Join community</h1><p className="text-sm text-muted-foreground">This invite link isn&apos;t complete. Ask for a new one.</p></>
        ) : preview.isPending ? (
          <p role="status" className="text-sm text-muted-foreground">Checking the link…</p>
        ) : preview.error || !p ? (
          <><h1 className="text-xl font-semibold">Link not available</h1><p className="text-sm text-muted-foreground">It may have expired, been used up or revoked. Ask for a new one.</p></>
        ) : (
          <>
            <div>
              <h1 className="text-xl font-semibold">{p.name}</h1>
              <p className="text-sm text-muted-foreground">Community · {p.memberCount} {p.memberCount === 1 ? "member" : "members"} · {p.groupCount} {p.groupCount === 1 ? "group" : "groups"}</p>
            </div>
            {p.description ? <p className="whitespace-pre-line text-left text-sm">{p.description}</p> : null}
            {p.rules ? <details className="rounded-xl bg-muted px-3 py-2 text-left text-sm"><summary className="cursor-pointer font-semibold">Community rules</summary><p className="mt-1 whitespace-pre-line">{p.rules}</p></details> : null}
            {done ? <p role="status" className="rounded-xl bg-primary/10 px-3 py-2 text-sm text-primary">{done}</p>
              : p.member ? <Button className="h-11 w-full rounded-xl" onClick={() => router.replace(`/dashboard/communities/${p.communityId}`)}>Open community</Button>
                : p.pendingRequest ? <p className="text-sm text-muted-foreground">Your request is waiting for an admin.</p>
                  : <>
                    {p.privacy === "APPROVAL" ? (
                      <>
                        <p className="text-xs text-muted-foreground">{PRIVACY_LABELS.APPROVAL.detail}</p>
                        <textarea className={field} rows={2} maxLength={MAX_REQUEST_MESSAGE} value={message} onChange={(e) => setMessage(e.target.value)} placeholder="A note for the admins (optional)" aria-label="Note for the admins" />
                      </>
                    ) : null}
                    <ErrorNote error={error} />
                    <Button className="h-11 w-full rounded-xl" loading={busy} onClick={() => void join()}>{p.privacy === "APPROVAL" ? "Ask to join" : "Join community"}</Button>
                  </>}
          </>
        )}
        <button type="button" onClick={() => router.push("/dashboard/communities")} className="w-full text-sm font-semibold text-muted-foreground hover:text-foreground">Not now</button>
      </div>
    </div>
  );
}

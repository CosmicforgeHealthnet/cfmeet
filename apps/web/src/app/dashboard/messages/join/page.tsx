"use client";

import { Suspense, useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { Users } from "lucide-react";
import { Button } from "@/components/ui/button";
import { relay } from "@/services/v2/messaging.service";
import { messagingErrorMessage } from "@/lib/messaging/errors";

/** A group invite link (S12): join the group, then open it. Nothing happens until you tap Join. */
function JoinChat() {
  const router = useRouter();
  const token = useSearchParams().get("token") ?? "";
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const join = async () => {
    setBusy(true); setError(null);
    try {
      const r = await relay<{ conversationId?: string; conversation?: { conversationId?: string } }>("POST", "v1/messaging/invite-links/join", { token });
      const id = r.conversationId ?? r.conversation?.conversationId;
      router.replace(id ? `/dashboard/messages?conversation=${encodeURIComponent(id)}` : "/dashboard/messages");
    } catch (e) {
      setError(messagingErrorMessage(e));
      setBusy(false);
    }
  };
  return (
    <div className="flex min-h-full items-center justify-center p-6">
      <div className="w-full max-w-sm rounded-3xl border border-border bg-card p-6 text-center shadow-sm">
        <span className="mx-auto flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary"><Users className="size-7" /></span>
        <h1 className="mt-4 text-xl font-semibold">Join group chat</h1>
        <p className="mt-1 text-sm text-muted-foreground">{token ? "You were invited to a group on CF Meet." : "This invite link isn't complete. Ask for a new one."}</p>
        {error ? <p role="alert" className="mt-4 rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
        {token ? <Button className="mt-5 h-11 w-full rounded-xl" loading={busy} onClick={() => void join()}>Join group</Button> : null}
        <button type="button" onClick={() => router.push("/dashboard/messages")} className="mt-3 w-full text-sm font-semibold text-muted-foreground hover:text-foreground">Not now</button>
      </div>
    </div>
  );
}

export default function JoinChatPage() {
  return <Suspense><JoinChat /></Suspense>;
}

"use client";

/**
 * ForwardingSettings — the signed-in user's own call forwarding (CAL-006).
 *
 * Rules are Calling's `/v1/enterprise/forwarding-rules`, bound to the user's principal by the
 * adapter (ownership is verified before a change). Destinations are typed ids, never numbers:
 * a colleague (PRINCIPAL, from the verified-organization directory) or one of the user's own
 * voicemail boxes (VOICEMAIL). EXTENSION is accepted by the adapter but not offered here: there
 * is no end-user extension directory in the served contract (recorded gap).
 */

import { useMemo, useState } from "react";
import { PhoneForwarded, Trash2 } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { useCreateForwardingRule, useDeleteForwardingRule, useForwardingRules, useVoicemailMailboxes } from "@/hooks/calling";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { identityApi } from "@/lib/messaging/api";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { ForwardingCondition, ForwardingRule } from "@/types/calling";

const conditions: Record<ForwardingCondition, string> = {
  ALWAYS: "Always (before ringing)",
  BUSY: "When I decline on every device",
  NO_ANSWER: "When I don't answer",
  UNAVAILABLE: "When I have no device online",
};

export default function ForwardingSettings() {
  const rules = useForwardingRules();
  const mailboxes = useVoicemailMailboxes();
  const directory = useQuery({ queryKey: ["calling", "forwarding-directory"], queryFn: () => identityApi.directory(), retry: false, staleTime: 60_000 });
  const create = useCreateForwardingRule();
  const remove = useDeleteForwardingRule();
  const [condition, setCondition] = useState<ForwardingCondition>("NO_ANSWER");
  const [destination, setDestination] = useState("");
  const [error, setError] = useState<string | null>(null);

  const active = useMemo(() => (rules.data?.forwardingRules ?? []).filter((r) => r.status === "ACTIVE"), [rules.data]);
  const names = usePrincipalNames(active.map((r) => (r.destination?.type === "PRINCIPAL" ? r.destination.id : null)));
  const usedConditions = new Set(active.map((r) => r.condition));
  const describe = (rule: ForwardingRule) => {
    if (!rule.destination) return "No destination";
    if (rule.destination.type === "PRINCIPAL") return names.data?.[rule.destination.id] ?? "A colleague";
    if (rule.destination.type === "VOICEMAIL") return `Voicemail: ${mailboxes.data?.mailboxes.find((m) => m.mailboxId === rule.destination!.id)?.displayLabel ?? "mailbox"}`;
    return rule.destination.type.replaceAll("_", " ").toLowerCase();
  };

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    setError(null);
    const [type, id] = destination.split(":");
    try {
      let principalId = id;
      if (type === "USER") {
        const person = directory.data?.people.find((p) => p.userId === id);
        principalId = person?.principalId ?? (await identityApi.resolve([id])).people.find((p) => p.userId === id)?.principalId ?? "";
        if (!principalId) throw new Error("This colleague cannot receive calls yet.");
      }
      await create.mutateAsync({ condition, destination: { type: type === "VOICEMAIL" ? "VOICEMAIL" : "PRINCIPAL", id: principalId } });
      setDestination("");
    } catch (e) { setError(callingErrorMessage(e)); }
  };

  return (
    <div className="space-y-5">
      <header className="flex items-center gap-2"><PhoneForwarded className="size-5" /><h2 className="font-semibold">Call forwarding</h2></header>
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {rules.isPending ? <Spinner /> : rules.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(rules.error)}</p> : active.length === 0 ? (
        <p className="text-sm text-muted-foreground">Calls are not forwarded.</p>
      ) : (
        <ul className="divide-y divide-border rounded-xl border border-border">
          {active.map((rule) => (
            <li key={rule.forwardingRuleId} className="flex items-center gap-3 p-3 text-sm">
              <div className="flex-1"><p className="font-medium">{conditions[rule.condition]}</p><p className="text-muted-foreground">Forward to {describe(rule)}</p></div>
              <Button size="icon" variant="ghost" aria-label="Remove forwarding rule" disabled={remove.isPending}
                onClick={() => remove.mutate({ ruleId: rule.forwardingRuleId, expectedVersion: rule.version }, { onError: (e) => setError(callingErrorMessage(e)) })}><Trash2 className="size-4" /></Button>
            </li>
          ))}
        </ul>
      )}
      <form onSubmit={submit} className="grid gap-3 rounded-xl border border-border p-4 sm:grid-cols-[1fr_1fr_auto] sm:items-end">
        <div className="space-y-2">
          <Label htmlFor="forward-condition">When</Label>
          <select id="forward-condition" value={condition} onChange={(e) => setCondition(e.target.value as ForwardingCondition)} className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm">
            {(Object.keys(conditions) as ForwardingCondition[]).map((c) => <option key={c} value={c} disabled={usedConditions.has(c)}>{conditions[c]}</option>)}
          </select>
        </div>
        <div className="space-y-2">
          <Label htmlFor="forward-destination">Forward to</Label>
          <select id="forward-destination" required value={destination} onChange={(e) => setDestination(e.target.value)} className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm">
            <option value="">Choose…</option>
            {mailboxes.data?.mailboxes.length ? <optgroup label="My voicemail">{mailboxes.data.mailboxes.map((m) => <option key={m.mailboxId} value={`VOICEMAIL:${m.mailboxId}`}>{m.displayLabel}</option>)}</optgroup> : null}
            {directory.data?.people.length ? <optgroup label="Colleagues">{directory.data.people.map((p) => <option key={p.userId} value={`USER:${p.userId}`}>{p.displayName}</option>)}</optgroup> : null}
          </select>
        </div>
        <Button type="submit" disabled={create.isPending || !destination || usedConditions.has(condition)}>Add rule</Button>
      </form>
      <p className="text-xs text-muted-foreground">One rule per condition. Forwarding never goes to a telephone number. Rules to queues, ring groups or menus are managed by your organization&apos;s administrator.</p>
    </div>
  );
}

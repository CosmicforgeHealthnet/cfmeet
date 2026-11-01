"use client";

/**
 * ForwardingSettings - Manage call forwarding rules.
 */

import { useState } from "react";
import { PhoneForwarded, Plus, Trash2, Pencil, ArrowRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import {
  useForwardingRules,
  useCreateForwardingRule,
  useUpdateForwardingRule,
  useDeleteForwardingRule,
} from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import type {
  ForwardingCondition,
  ForwardingTargetType,
  ForwardingRule,
} from "@/types/calling";

interface ForwardingSettingsProps {
  className?: string;
}

const conditionLabels: Record<ForwardingCondition, string> = {
  ALWAYS: "Always",
  BUSY: "When Busy",
  NO_ANSWER: "No Answer",
  NOT_REACHABLE: "Not Reachable",
  OUTSIDE_HOURS: "Outside Hours",
};

const targetTypeLabels: Record<ForwardingTargetType, string> = {
  EXTENSION: "Extension",
  EXTERNAL_NUMBER: "External Number",
  VOICEMAIL: "Voicemail",
  PRINCIPAL: "Contact",
};

export default function ForwardingSettings({ className }: ForwardingSettingsProps) {
  const [showForm, setShowForm] = useState(false);
  const [editingRule, setEditingRule] = useState<ForwardingRule | null>(null);
  const [condition, setCondition] = useState<ForwardingCondition>("NO_ANSWER");
  const [targetType, setTargetType] = useState<ForwardingTargetType>("EXTENSION");
  const [targetValue, setTargetValue] = useState("");
  const [ringSeconds, setRingSeconds] = useState(20);
  const [error, setError] = useState<string | null>(null);

  const rules = useForwardingRules();
  const createRule = useCreateForwardingRule();
  const updateRule = useUpdateForwardingRule(editingRule?.forwardingRuleId ?? "");
  const deleteRule = useDeleteForwardingRule();

  const resetForm = () => {
    setCondition("NO_ANSWER");
    setTargetType("EXTENSION");
    setTargetValue("");
    setRingSeconds(20);
    setEditingRule(null);
    setShowForm(false);
  };

  const handleEdit = (rule: ForwardingRule) => {
    setEditingRule(rule);
    setCondition(rule.condition);
    if (rule.target.extension) {
      setTargetType("EXTENSION");
      setTargetValue(rule.target.extension);
    } else if (rule.target.externalNumber) {
      setTargetType("EXTERNAL_NUMBER");
      setTargetValue(rule.target.externalNumber);
    } else if (rule.target.voicemailBoxId) {
      setTargetType("VOICEMAIL");
      setTargetValue(rule.target.voicemailBoxId);
    }
    setRingSeconds(rule.ringSeconds ?? 20);
    setShowForm(true);
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      const target = {
        type: targetType,
        ...(targetType === "EXTENSION" && { extension: targetValue }),
        ...(targetType === "EXTERNAL_NUMBER" && { externalNumber: targetValue }),
        ...(targetType === "VOICEMAIL" && { voicemailBoxId: targetValue }),
      };

      if (editingRule) {
        await updateRule.mutateAsync({ target, ringSeconds, enabled: true });
      } else {
        await createRule.mutateAsync({ condition, target, ringSeconds });
      }
      resetForm();
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const handleDelete = async (ruleId: string) => {
    setError(null);
    try {
      await deleteRule.mutateAsync(ruleId);
    } catch (err) {
      setError(callingErrorMessage(err));
    }
  };

  const getTargetDisplay = (rule: ForwardingRule) => {
    if (rule.target.extension) return rule.target.extension;
    if (rule.target.externalNumber) return rule.target.externalNumber;
    if (rule.target.voicemailBoxId) return "Voicemail";
    return "Unknown";
  };

  return (
    <div className={className}>
      <div className="mb-4 flex items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <PhoneForwarded className="size-5 text-muted-foreground" />
          <h2 className="text-lg font-semibold">Call Forwarding</h2>
        </div>
        <Button
          size="sm"
          variant={showForm ? "outline" : "default"}
          onClick={() => {
            if (showForm) resetForm();
            else setShowForm(true);
          }}
        >
          {showForm ? "Cancel" : <><Plus className="mr-1 size-4" /> Add Rule</>}
        </Button>
      </div>

      {error && (
        <p className="mb-4 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">
          {error}
        </p>
      )}

      {showForm && (
        <form onSubmit={handleSubmit} className="mb-6 space-y-4 rounded-xl border border-border bg-muted/30 p-4">
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="forward-condition">When to Forward</Label>
              <select
                id="forward-condition"
                value={condition}
                onChange={(e) => setCondition(e.target.value as ForwardingCondition)}
                disabled={!!editingRule}
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm disabled:opacity-50"
              >
                {Object.entries(conditionLabels).map(([key, label]) => (
                  <option key={key} value={key}>{label}</option>
                ))}
              </select>
            </div>
            <div className="space-y-2">
              <Label htmlFor="forward-target-type">Forward To</Label>
              <select
                id="forward-target-type"
                value={targetType}
                onChange={(e) => {
                  setTargetType(e.target.value as ForwardingTargetType);
                  setTargetValue("");
                }}
                className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm"
              >
                {Object.entries(targetTypeLabels).map(([key, label]) => (
                  <option key={key} value={key}>{label}</option>
                ))}
              </select>
            </div>
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-2">
              <Label htmlFor="forward-target">
                {targetType === "EXTENSION" && "Extension Number"}
                {targetType === "EXTERNAL_NUMBER" && "Phone Number"}
                {targetType === "VOICEMAIL" && "Mailbox ID"}
                {targetType === "PRINCIPAL" && "Contact ID"}
              </Label>
              <Input
                id="forward-target"
                value={targetValue}
                onChange={(e) => setTargetValue(e.target.value)}
                placeholder={
                  targetType === "EXTENSION" ? "101" :
                  targetType === "EXTERNAL_NUMBER" ? "+1 555 123 4567" :
                  targetType === "VOICEMAIL" ? "Select mailbox" : "Enter ID"
                }
                required
              />
            </div>
            {condition !== "ALWAYS" && (
              <div className="space-y-2">
                <Label htmlFor="forward-ring">Ring for (seconds)</Label>
                <Input
                  id="forward-ring"
                  type="number"
                  min={5}
                  max={60}
                  value={ringSeconds}
                  onChange={(e) => setRingSeconds(Number(e.target.value))}
                />
              </div>
            )}
          </div>
          <div className="flex justify-end gap-2">
            <Button type="button" variant="outline" onClick={resetForm}>
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={createRule.isPending || updateRule.isPending}
            >
              {(createRule.isPending || updateRule.isPending) && <Spinner className="mr-2 size-4" />}
              {editingRule ? "Update Rule" : "Create Rule"}
            </Button>
          </div>
        </form>
      )}

      {rules.isPending ? (
        <div className="flex items-center justify-center py-8">
          <Spinner />
        </div>
      ) : rules.error ? (
        <p className="text-sm text-destructive">{callingErrorMessage(rules.error)}</p>
      ) : !rules.data?.data.length ? (
        <p className="py-8 text-center text-sm text-muted-foreground">
          No forwarding rules configured. Add a rule to redirect incoming calls.
        </p>
      ) : (
        <ul className="space-y-2">
          {rules.data.data.map((rule) => (
            <li
              key={rule.forwardingRuleId}
              className={`flex items-center justify-between gap-3 rounded-xl border border-border bg-card p-4 ${
                !rule.enabled ? "opacity-50" : ""
              }`}
            >
              <div className="flex items-center gap-3">
                <div className="flex size-10 items-center justify-center rounded-full bg-primary/10">
                  <PhoneForwarded className="size-5 text-primary" />
                </div>
                <div>
                  <div className="flex items-center gap-2">
                    <span className="font-medium">{conditionLabels[rule.condition]}</span>
                    <ArrowRight className="size-4 text-muted-foreground" />
                    <span className="font-medium">{getTargetDisplay(rule)}</span>
                  </div>
                  <p className="text-sm text-muted-foreground">
                    {rule.ringSeconds && `Ring ${rule.ringSeconds}s · `}
                    {rule.enabled ? "Active" : "Disabled"}
                  </p>
                </div>
              </div>
              <div className="flex gap-1">
                <Button
                  size="icon"
                  variant="ghost"
                  onClick={() => handleEdit(rule)}
                  aria-label="Edit"
                >
                  <Pencil className="size-4" />
                </Button>
                <Button
                  size="icon"
                  variant="ghost"
                  onClick={() => handleDelete(rule.forwardingRuleId)}
                  disabled={deleteRule.isPending}
                  aria-label="Delete"
                >
                  <Trash2 className="size-4 text-destructive" />
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

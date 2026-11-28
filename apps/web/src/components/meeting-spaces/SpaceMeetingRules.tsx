"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { profilesService } from "@/services/v2/profiles.service";
import { RULES } from "@/components/organizations/OrganizationModeration";
import type { MeetingSpaceResponse } from "@/types/v2";
import { RequestError } from "./ManagementUI";

const errorCode = (e: unknown) => (e as { response?: { data?: { error?: { code?: string } } } } | null)?.response?.data?.error?.code;

/**
 * Meeting space → Overview (owners and hosts): this space's meeting rules
 * (GET/PATCH /meeting-spaces/:id/profile-policy). The same switches as the organization's
 * Meeting rules; a space can be stricter than its organization, never looser — a rule the
 * organization has locked shows as such.
 */
export default function SpaceMeetingRules({ space }: { space: MeetingSpaceResponse }) {
  const client = useQueryClient();
  const key = ["v2", "spaces", space.id, "profile-policy"];
  const policy = useQuery({ queryKey: key, queryFn: () => profilesService.getPolicy(space.id), retry: false });
  const save = useMutation({
    mutationFn: (changes: Record<string, unknown>) => profilesService.updatePolicy(space.id, { changes, profilePolicyVersion: policy.data!.profilePolicyVersion }),
    onSuccess: (v) => client.setQueryData(key, v),
    onError: (e) => { if (errorCode(e) === "MEETING_VERSION_CONFLICT") void policy.refetch(); },
  });
  if (policy.isError && [403, 404].includes((policy.error as { response?: { status?: number } } | null)?.response?.status ?? 0)) return null;
  const effective = policy.data?.effective.policy as unknown as Record<string, Record<string, unknown>> | undefined;
  const clamped = new Set(policy.data?.effective.clampedFields ?? []);
  const deniedText = errorCode(save.error) === "PROFILE_POLICY_DENIED"
    ? "That isn't allowed for this space — its organization's rules or its \"Who can join\" setting don't permit it."
    : null;
  return (
    <section className="mt-6 rounded-2xl border border-border bg-card p-6" data-testid="space-meeting-rules">
      <h2 className="text-base font-semibold text-foreground">Meeting rules</h2>
      <p className="mt-1 text-sm text-muted-foreground">What&apos;s allowed in this space&apos;s meetings. Its organization can make these stricter.</p>
      {policy.isError ? <div className="mt-4"><RequestError error={policy.error} retry={() => void policy.refetch()} /></div> : null}
      {policy.data?.effective.misconfigured ? <p className="mt-4 rounded-xl bg-amber-500/10 px-4 py-3 text-sm text-amber-700 dark:text-amber-300">These rules couldn&apos;t be read in full, so the strictest settings apply until they&apos;re saved again.</p> : null}
      {save.isError ? <div className="mt-4">{deniedText ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">{deniedText}</p> : <RequestError error={save.error} />}</div> : null}
      <div className="mt-2 divide-y divide-border">
        {RULES.map((rule) => {
          const path = `${rule.section}.${rule.key}`;
          const on = (effective?.[rule.section]?.[rule.key] ?? rule.fallback) === true;
          const locked = clamped.has(path);
          return (
            <label key={path} className="flex items-center justify-between gap-4 py-3">
              <span>
                <span className="block text-sm font-medium">{rule.label}</span>
                <span className="block text-xs text-muted-foreground">{locked ? "Set by your organization." : rule.hint}</span>
              </span>
              <input type="checkbox" role="switch" className="size-5 shrink-0 accent-primary" checked={on}
                disabled={!policy.data || save.isPending || locked}
                onChange={() => save.mutate({ [rule.section]: { [rule.key]: !on } })} />
            </label>
          );
        })}
      </div>
    </section>
  );
}

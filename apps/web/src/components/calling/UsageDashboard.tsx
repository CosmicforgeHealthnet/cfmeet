"use client";

/**
 * UsageDashboard - Display calling usage metrics and limits.
 */

import { BarChart3, AlertTriangle, Clock, HardDrive } from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import { useCallingLimits, useCallingUsage } from "@/hooks/calling";
import { callingErrorMessage } from "@/lib/calling/errors";
import type { UsageMetric, UsageLimit } from "@/types/calling";

interface UsageDashboardProps {
  className?: string;
}

const metricLabels: Record<UsageMetric, string> = {
  OUTBOUND_MINUTES: "Outbound Minutes",
  INBOUND_MINUTES: "Inbound Minutes",
  RECORDINGS_MINUTES: "Recording Minutes",
  RECORDINGS_STORAGE_GB: "Recording Storage",
  CONCURRENT_CALLS: "Concurrent Calls",
  SMS_MESSAGES: "SMS Messages",
};

const metricIcons: Record<UsageMetric, typeof Clock> = {
  OUTBOUND_MINUTES: Clock,
  INBOUND_MINUTES: Clock,
  RECORDINGS_MINUTES: Clock,
  RECORDINGS_STORAGE_GB: HardDrive,
  CONCURRENT_CALLS: BarChart3,
  SMS_MESSAGES: BarChart3,
};

function UsageBar({ limit }: { limit: UsageLimit }) {
  const percentage = Math.min((limit.used / limit.limit) * 100, 100);
  const isWarning = percentage >= 80;
  const isCritical = percentage >= 95;
  const Icon = metricIcons[limit.metric];

  return (
    <div className="rounded-xl border border-border bg-card p-4">
      <div className="mb-3 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <Icon className="size-5 text-muted-foreground" />
          <span className="font-medium">{metricLabels[limit.metric]}</span>
        </div>
        {(isWarning || isCritical) && (
          <AlertTriangle
            className={`size-5 ${isCritical ? "text-destructive" : "text-yellow-500"}`}
          />
        )}
      </div>
      <div className="mb-2 flex items-end justify-between text-sm">
        <span className="text-2xl font-bold">
          {limit.used.toLocaleString()}
        </span>
        <span className="text-muted-foreground">
          of {limit.limit.toLocaleString()} {limit.period.toLowerCase()}
        </span>
      </div>
      <div className="h-2 overflow-hidden rounded-full bg-muted">
        <div
          className={`h-full rounded-full transition-all ${
            isCritical
              ? "bg-destructive"
              : isWarning
              ? "bg-yellow-500"
              : "bg-primary"
          }`}
          style={{ width: `${percentage}%` }}
        />
      </div>
      <div className="mt-2 flex justify-between text-xs text-muted-foreground">
        <span>{limit.remaining.toLocaleString()} remaining</span>
        {limit.resetsAt && (
          <span>
            Resets {new Date(limit.resetsAt).toLocaleDateString()}
          </span>
        )}
      </div>
    </div>
  );
}

export default function UsageDashboard({ className }: UsageDashboardProps) {
  const limits = useCallingLimits();
  const usage = useCallingUsage();

  return (
    <div className={className}>
      <div className="mb-4 flex items-center gap-2">
        <BarChart3 className="size-5 text-muted-foreground" />
        <h2 className="text-lg font-semibold">Usage & Limits</h2>
      </div>

      {limits.isPending ? (
        <div className="flex items-center justify-center py-8">
          <Spinner />
        </div>
      ) : limits.error ? (
        <p className="text-sm text-destructive">
          {callingErrorMessage(limits.error)}
        </p>
      ) : !limits.data?.limits.length ? (
        <p className="py-8 text-center text-sm text-muted-foreground">
          No usage limits configured for your account.
        </p>
      ) : (
        <>
          <div className="mb-4 grid gap-4 md:grid-cols-2">
            {limits.data.limits.map((limit) => (
              <UsageBar key={`${limit.metric}-${limit.period}`} limit={limit} />
            ))}
          </div>
          <p className="text-xs text-muted-foreground">
            Last updated: {new Date(limits.data.asOf).toLocaleString()}
          </p>
        </>
      )}

      {usage.data?.facts && usage.data.facts.length > 0 && (
        <div className="mt-6">
          <h3 className="mb-3 font-medium">Usage History</h3>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left">
                  <th className="pb-2 font-medium">Metric</th>
                  <th className="pb-2 font-medium">Period</th>
                  <th className="pb-2 text-right font-medium">Value</th>
                </tr>
              </thead>
              <tbody>
                {usage.data.facts.map((fact, index) => (
                  <tr key={index} className="border-b border-border">
                    <td className="py-2">{metricLabels[fact.metric]}</td>
                    <td className="py-2 text-muted-foreground">
                      {new Date(fact.periodStart).toLocaleDateString()} -{" "}
                      {new Date(fact.periodEnd).toLocaleDateString()}
                    </td>
                    <td className="py-2 text-right">
                      {fact.value.toLocaleString()} {fact.unit}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

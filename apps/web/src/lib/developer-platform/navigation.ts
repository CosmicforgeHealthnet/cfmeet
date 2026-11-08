/**
 * Developer Portal navigation and workspace selection (Blueprint v1.0 Section 17).
 *
 * The portal is its own console under `/developers/*`, separate from the chat / meeting
 * dashboard. Each section names the Developer Platform action that must be visible for it
 * to appear; the server still re-checks the role on every request.
 *
 * Pure module (node tests load it with --experimental-strip-types): type-only imports,
 * so the role predicate is passed in (`(action) => can(workspace, action)`).
 */
import type { ConsoleAction } from "./permissions";
import type { OrganizationState } from "./types";

export const PORTAL_BASE = "/developers";

export type PortalSection = {
  key: string;
  label: string;
  href: string;
  /** Needed to show the entry; null: always shown (no workspace needed). */
  requires: ConsoleAction | null;
  /** Shown without a selected workspace (docs, workspace creation). */
  global?: boolean;
};

export const PORTAL_SECTIONS: PortalSection[] = [
  { key: "overview", label: "Overview", href: `${PORTAL_BASE}`, requires: null, global: true },
  { key: "workspace", label: "Workspaces", href: `${PORTAL_BASE}/workspace`, requires: null, global: true },
  { key: "members", label: "Members & roles", href: `${PORTAL_BASE}/members`, requires: "MembersRead" },
  { key: "projects", label: "Projects & environments", href: `${PORTAL_BASE}/projects`, requires: "ProjectsRead" },
  { key: "credentials", label: "Service accounts & keys", href: `${PORTAL_BASE}/credentials`, requires: "ServiceAccountsRead" },
  { key: "webhooks", label: "Webhooks", href: `${PORTAL_BASE}/webhooks`, requires: "WebhooksRead" },
  { key: "usage", label: "Usage, limits & plans", href: `${PORTAL_BASE}/usage`, requires: "UsageRead" },
  { key: "audit", label: "Audit log", href: `${PORTAL_BASE}/audit`, requires: "ActivityRead" },
  { key: "docs", label: "Docs & SDKs", href: `${PORTAL_BASE}/docs`, requires: null, global: true },
];

/** Sections to list in the sidebar; `allowed` is `(action) => can(selectedWorkspace, action)`. */
export function visibleSections(allowed: (action: ConsoleAction) => boolean): PortalSection[] {
  return PORTAL_SECTIONS.filter((section) => section.requires === null || allowed(section.requires));
}

/** The active section for a pathname (longest matching href). */
export function activeSection(pathname: string | null | undefined): string | null {
  if (!pathname) return null;
  const clean = pathname.replace(/\/+$/, "") || "/";
  let best: PortalSection | null = null;
  for (const section of PORTAL_SECTIONS) {
    if (clean === section.href || clean.startsWith(`${section.href}/`)) {
      if (!best || section.href.length > best.href.length) best = section;
    }
  }
  return best?.key ?? null;
}

/** localStorage key of the selected developer workspace id (an id only: not a secret). */
export const SELECTED_WORKSPACE_KEY = "cfmeet.developer-portal.workspace";

const UUIDISH = /^[0-9A-Za-z-]{1,64}$/;

/**
 * The workspace to select: the stored one when the user is still a member, else the first
 * non-closed workspace, else the first one, else none.
 */
export function pickWorkspace<T extends { id: string; state: OrganizationState }>(
  workspaces: T[],
  storedId: string | null | undefined,
): T | null {
  if (storedId && UUIDISH.test(storedId)) {
    const stored = workspaces.find((w) => w.id === storedId);
    if (stored) return stored;
  }
  return workspaces.find((w) => w.state !== "CLOSED") ?? workspaces[0] ?? null;
}

/** Reads `?env=` / `?project=` style ids from the URL defensively (opaque ids only). */
export function safeId(value: string | null | undefined): string | null {
  if (!value) return null;
  return UUIDISH.test(value) ? value : null;
}

/** Builds a portal URL with query ids, dropping empty ones. */
export function portalHref(path: string, query: Record<string, string | null | undefined> = {}): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) if (value) params.set(key, value);
  const text = params.toString();
  return text ? `${path}?${text}` : path;
}

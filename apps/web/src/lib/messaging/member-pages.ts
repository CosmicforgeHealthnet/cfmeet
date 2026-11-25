/** Exhaust a keyset-paginated member directory without silently accepting a partial page. */
export async function allMemberPages<T extends { principalId: string }>(read: (cursor: string | null) => Promise<{ members: T[]; nextCursor?: string | null }>): Promise<T[]> {
  const members = new Map<string, T>();
  const seen = new Set<string>();
  let cursor: string | null = null;
  for (let pages = 0; pages < 10_000; pages++) {
    const page = await read(cursor);
    for (const member of page.members) members.set(member.principalId, member);
    if (!page.nextCursor) return [...members.values()];
    if (seen.has(page.nextCursor) || !page.members.length) throw new Error("MEMBER_DIRECTORY_INCOMPLETE");
    seen.add(page.nextCursor);
    cursor = page.nextCursor;
  }
  throw new Error("MEMBER_DIRECTORY_INCOMPLETE");
}

/**
 * Start with the old relay's queryless route. New servers explicitly advertise a
 * nextCursor boundary even on the first default-size page; old servers omit it and
 * return at most 100 rows. Exactly 100 legacy rows cannot prove completeness.
 */
export async function allDevicePages<T extends { deviceId: string }>(
  read: (query: string) => Promise<{ devices: T[]; nextCursor?: string | null }>,
  invalid: (code: string) => Error = code => new Error(code),
): Promise<T[]> {
  const devices = new Map<string, T>();
  const seen = new Set<string>();
  let cursor: string | null = null;
  for (;;) {
    let page: { devices: T[]; nextCursor?: string | null };
    try { page = await read(cursor === null ? "" : `?limit=500&cursor=${encodeURIComponent(cursor)}`); }
    catch (error) {
      if (cursor !== null && typeof error === "object" && error !== null && "code" in error && error.code === "BAD_PATH") throw invalid("DEVICE_DIRECTORY_PAGINATION_REQUIRED");
      throw error;
    }
    if (!page || !Array.isArray(page.devices)) throw invalid("DEVICE_DIRECTORY_PAGE_INVALID");
    for (const device of page.devices) {
      if (!device || typeof device.deviceId !== "string" || !device.deviceId) throw invalid("DEVICE_DIRECTORY_PAGE_INVALID");
      devices.set(device.deviceId, device);
    }
    if (!Object.prototype.hasOwnProperty.call(page, "nextCursor")) {
      if (cursor !== null || page.devices.length >= 100) throw invalid("DEVICE_DIRECTORY_PAGINATION_REQUIRED");
      return [...devices.values()];
    }
    if (page.nextCursor === null) return [...devices.values()];
    if (typeof page.nextCursor !== "string" || !page.nextCursor || !page.devices.length || seen.has(page.nextCursor)) throw invalid("DEVICE_DIRECTORY_PAGE_INVALID");
    seen.add(page.nextCursor);
    cursor = page.nextCursor;
  }
}

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

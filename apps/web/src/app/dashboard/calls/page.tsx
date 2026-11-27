import { redirect } from "next/navigation";

/** The Calls page lives at /calling (the sidebar link); keep old links working. */
export default async function CallsPage({ searchParams }: { searchParams: Promise<Record<string, string | string[] | undefined>> }) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(await searchParams)) if (typeof value === "string") params.set(key, value);
  const query = params.toString();
  redirect(query ? `/calling?${query}` : "/calling");
}

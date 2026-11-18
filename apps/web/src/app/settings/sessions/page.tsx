import { redirect } from "next/navigation";

/** Linked from the "New sign-in to your CF Meet account" email: where you're signed in. */
export default function SessionsPage() {
  redirect("/dashboard/settings?tab=security");
}

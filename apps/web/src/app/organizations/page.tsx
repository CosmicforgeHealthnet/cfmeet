import { redirect } from "next/navigation";

/** /organizations is the organizations list in the dashboard (it was read as a meeting code). */
export default function OrganizationsIndex() {
  redirect("/dashboard/organizations");
}

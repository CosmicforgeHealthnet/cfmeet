import { redirect } from "next/navigation";

// Scheduling lives in the calendar (agenda views) and the meeting space wizard.
export default function SchedulePage() {
  redirect("/dashboard/calendar");
}

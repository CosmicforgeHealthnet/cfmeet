import { redirect } from "next/navigation";

/** Email verification is part of the code sign-in now; old links land on sign-in. */
export default function VerifyPage() {
  redirect("/login");
}

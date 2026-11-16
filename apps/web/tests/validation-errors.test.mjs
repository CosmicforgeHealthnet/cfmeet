// F1: structured `400 VALIDATION_ERROR` details map onto form fields; older answers
// (message only, possibly `__all__: ...` / `password: ...` prefixed) fall back cleanly.
//   node --experimental-strip-types --test tests/validation-errors.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import {
  formErrorsOf,
  stripFieldPrefixes,
  validationIssuesOf,
  validationMessageOf,
} from "../src/lib/validation-errors.ts";

const answer = (error, status = 400) => ({ response: { status, data: { success: false, error } } });

const weakSignup = answer({
  code: "VALIDATION_ERROR",
  message: "confirm_password: Passwords do not match\npassword: Password must contain at least one uppercase letter",
  requestId: "r-1",
  details: [
    { field: "confirm_password", code: "passwords_mismatch", message: "Passwords do not match" },
    { field: "password", code: "password_strength", message: "Password must contain at least one uppercase letter" },
    { field: null, code: "whole", message: "Not allowed" },
  ],
});

test("issues are read from error.details of VALIDATION_ERROR answers only", () => {
  assert.equal(validationIssuesOf(weakSignup).length, 3);
  assert.deepEqual(validationIssuesOf(weakSignup)[1], {
    field: "password",
    code: "password_strength",
    message: "Password must contain at least one uppercase letter",
  });
  assert.deepEqual(validationIssuesOf(answer({ code: "CONFLICT", message: "x", details: [] }, 409)), []);
  assert.deepEqual(validationIssuesOf(new Error("network")), []);
  assert.deepEqual(validationIssuesOf(null), []);
});

test("issues land on the mapped form fields, the rest in the general slot", () => {
  const errors = formErrorsOf(weakSignup, { password: "password", confirm_password: "password", email: "email" });
  assert.deepEqual(errors, {
    password: "Passwords do not match", // first message wins for a shared input
    general: "Not allowed",
  });
  const reset = formErrorsOf(weakSignup, { password: "password", confirm_password: "confirmPassword" }, "banner");
  assert.equal(reset.password, "Password must contain at least one uppercase letter");
  assert.equal(reset.confirmPassword, "Passwords do not match");
  assert.equal(reset.banner, "Not allowed");
});

test("older answers without details fall back to the message without field prefixes", () => {
  const legacy = answer({ code: "VALIDATION_ERROR", message: "__all__: Password must contain a digit" });
  assert.deepEqual(formErrorsOf(legacy, { password: "password" }), { general: "Password must contain a digit" });
  assert.equal(validationMessageOf(legacy), "Password must contain a digit");
  assert.deepEqual(formErrorsOf(answer({ code: "NOT_FOUND", message: "gone" }, 404), {}), {});
  assert.equal(validationMessageOf(answer({ code: "NOT_FOUND", message: "gone" }, 404)), null);
});

test("single-line message joins the distinct issue messages", () => {
  assert.equal(
    validationMessageOf(weakSignup),
    "Passwords do not match Password must contain at least one uppercase letter Not allowed",
  );
  assert.equal(stripFieldPrefixes("new_password: Too short\nitems[0].email: Invalid"), "Too short Invalid");
  assert.equal(stripFieldPrefixes("Passwords do not match"), "Passwords do not match");
});

import assert from "node:assert/strict";
import { chromium } from "playwright";

const browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined) });
try {
  // Test UI behavior separately from the production CSP suite. Next's development
  // overlay evaluates debugging scripts which Trusted Types intentionally blocks.
  const page = await browser.newPage({ bypassCSP: true });
  page.setDefaultTimeout(15_000);
  await page.addInitScript(() => localStorage.setItem("cosmic_user_info", JSON.stringify({ id: "user", username: "Test User", email: "test@example.com" })));
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => { if (message.type() === "error") console.error(message.text()); });
  page.on("requestfailed", (request) => console.error("Failed request", request.url(), request.failure()?.errorText));
  const cors = { "Access-Control-Allow-Origin": "*", "Access-Control-Allow-Headers": "*", "Access-Control-Allow-Methods": "GET, POST, OPTIONS" };
  await page.route("**/users/me", (route) => route.fulfill({ headers: cors, json: { id: "user", username: "Test User", email: "test@example.com" } }));
  await page.route("**/api/calling/identity", (route) => route.fulfill({ json: { principalId: "principal" } }));
  await page.route("**/messaging/directory", (route) => route.fulfill({ headers: cors, json: { people: [] } }));
  await page.route("**/api/calling/v1/calls*", (route) => route.fulfill({ json: { principalId: "principal", data: [], nextCursor: null } }));
  await page.goto(process.env.CALLING_BROWSER_URL || "http://127.0.0.1:3100/dashboard/calls");
  try { await page.getByRole("heading", { name: "Calls", exact: true }).waitFor(); }
  catch (error) { console.error("Calling page:", page.url(), await page.locator("body").innerText(), errors); throw error; }
  await page.getByText("Your calls will appear here.").waitFor();
  assert.equal(await page.getByRole("button", { name: "Call", exact: true }).isDisabled(), true);
  await page.getByRole("button", { name: "Missed", exact: true }).click();
  await page.getByText("No missed calls.").waitFor();
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), true);
  await page.screenshot({ path: ".next/calling-mobile.png", fullPage: true });
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.screenshot({ path: ".next/calling-desktop.png", fullPage: true });
  assert.deepEqual(errors, []);
  console.log("Calling browser checks passed: empty history, missed filter, disabled creation, responsive layout, no runtime errors.");
} finally { await browser.close(); }

// Read-only live CF Meet browser diagnostic. Does NOT log in or submit forms.
// Run from apps/web: node tests/live-production-smoke.mjs
import { chromium, devices } from "playwright";

const cases = [
  { url: "https://www.cfmeet.com/", label: "www desktop", opts: { viewport: { width: 1440, height: 900 } } },
  { url: "https://www.cfmeet.com/", label: "www Android-like", opts: { ...devices["Pixel 7"] } },
  { url: "https://cfmeet.com/", label: "apex mobile", opts: { ...devices["Pixel 7"] } },
];
const browser = await chromium.launch({ headless: true });
const failures = [];
try {
  for (const scenario of cases) {
    const page = await browser.newPage({ ...scenario.opts, serviceWorkers: "block" });
    const pageErrors = [];
    const failedScripts = [];
    const cspViolations = [];
    const consoleErrors = [];
    await page.addInitScript(() => {
      window.__cspViolations = [];
      document.addEventListener("securitypolicyviolation", (e) => {
        window.__cspViolations.push({
          directive: e.effectiveDirective,
          blocked: e.blockedURI?.split("?")[0]?.slice(0, 160),
        });
      });
    });
    page.on("pageerror", e => pageErrors.push(String(e).slice(0, 1000)));
    page.on("console", msg => {
      if (msg.type() === "error") consoleErrors.push(msg.text().slice(0, 700));
    });
    page.on("requestfailed", req => {
      if (req.resourceType() === "script" || req.resourceType() === "document")
        failedScripts.push({ path: new URL(req.url()).pathname, error: req.failure()?.errorText });
    });
    page.on("response", res => {
      if (res.status() >= 400 && ["script", "document"].includes(res.request().resourceType()))
        failedScripts.push({ path: new URL(res.url()).pathname, status: res.status() });
    });
    let status = "navigation-failed";
    try {
      const res = await page.goto(scenario.url, { waitUntil: "domcontentloaded", timeout: 45000 });
      status = String(res?.status() ?? "no-response");
      await page.waitForTimeout(4000);
    } catch (e) {
      pageErrors.push("navigation: " + String(e).slice(0, 800));
    }
    let text = "";
    try {
      text = (await page.locator("body").innerText({ timeout: 6000 })).slice(0, 1200);
    } catch (e) {
      pageErrors.push("body: " + String(e).slice(0, 250));
    }
    try {
      cspViolations.push(...await page.evaluate(() => window.__cspViolations ?? []));
    } catch {}
    const crashed = /Application error:\s*a client-side exception/i.test(text);
    const homepage = /Connect Face-to-Face|Video Calls That Just Work|Fast\. Secure\. Real-Time/i.test(text);
    const result = {
      case: scenario.label,
      url: new URL(scenario.url).origin,
      finalOrigin: (() => { try {return new URL(page.url()).origin;}catch{return "invalid";} })(),
      status, crashed, homepage,
      pageErrors: pageErrors.slice(0, 8),
      failedScripts: failedScripts.slice(0, 10),
      cspViolations: cspViolations.slice(0, 10),
      consoleErrors: consoleErrors.slice(0, 6),
    };
    console.log("LIVE_BROWSER_DIAGNOSTIC " + JSON.stringify(result));
    if (crashed || !homepage || pageErrors.length || failedScripts.length || cspViolations.length) {
      failures.push(scenario.label);
    }
    await page.close();
  }
  for (const [url, label] of [
    ["https://api.cfmeet.com/api/v2/meeting-spaces", "correct V2 route"],
    ["https://api.cfmeet.com/api/v2/meetings-spaces", "misspelled V2 route"],
  ]) {
    try {
      const r = await fetch(url, { redirect: "manual", signal: AbortSignal.timeout(12000) });
      console.log("LIVE_API_DIAGNOSTIC " + JSON.stringify({ label, status: r.status }));
      if (label === "correct V2 route" && ![401, 403].includes(r.status)) failures.push("correct V2 route");
      if (label === "misspelled V2 route" && r.status !== 404) failures.push("misspelled V2 route");
    } catch (e) {
      failures.push(label);
      console.log("LIVE_API_DIAGNOSTIC " + JSON.stringify({ label, error: String(e).slice(0, 250) }));
    }
  }
} finally {
  await browser.close();
}
if (failures.length) {
  console.error("LIVE_SMOKE_FAILURES " + JSON.stringify(failures));
  process.exitCode = 1;
} else {
  console.log("LIVE_SMOKE_PASS: live web homepage and API contract accessible.");
}

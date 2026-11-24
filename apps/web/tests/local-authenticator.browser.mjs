import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

// Real Chromium WebAuthn/PRF ceremony. The virtual authenticator models the OS prompt;
// it is not evidence of physical Windows Hello/iOS/Android authenticator compatibility.
const web = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const require = createRequire(join(web, 'package.json'));
const { chromium } = require('playwright');
const ts = require('typescript');
const source = await readFile(join(web, 'src/lib/messaging/local-authenticator.ts'), 'utf8');
const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const server = createServer((req, res) => {
  if (req.url === '/local-authenticator.js') {
    res.setHeader('Content-Type', 'text/javascript');
    res.end(compiled);
  } else res.end('<html></html>');
});
await new Promise((done) => server.listen(0, '127.0.0.1', done));
const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  await page.goto(`http://localhost:${server.address().port}`);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('WebAuthn.enable', { enableUI: false });
  const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', { options: {
    protocol: 'ctap2', ctap2Version: 'ctap2_1', transport: 'internal',
    hasResidentKey: true, hasUserVerification: true, hasPrf: true,
    automaticPresenceSimulation: true, isUserVerified: true,
  } });
  const evidence = await page.evaluate(async () => {
    const api = await import('/local-authenticator.js');
    const id = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
    const binding = { account: id(1), accountDeviceId: id(2), messagingDeviceId: id(3), installationId: 'web-existing-installation', scope: { tenantId: id(4), applicationId: id(5), environmentId: id(6) } };
    const protectedLocal = await api.createLocalProtection(binding);
    const again = await api.unlockLocalProtection(protectedLocal.descriptor, binding);
    const same = protectedLocal.prfSecret.every((byte, i) => byte === again[i]);
    protectedLocal.prfSecret.fill(0);
    again.fill(0);
    const refused = [];
    for (const change of [{ account: id(7) }, { accountDeviceId: id(8) }, { installationId: 'another-profile' }, { messagingDeviceId: id(9) }, { scope: { ...binding.scope, tenantId: id(10) } }]) {
      try { await api.unlockLocalProtection(protectedLocal.descriptor, { ...binding, ...change }); }
      catch (e) { refused.push(e.code); }
    }
    window.localUnlockTest = { api, binding, descriptor: protectedLocal.descriptor };
    return { realPrfReproduced: same, bindingRefusals: refused };
  });
  assert.equal(evidence.realPrfReproduced, true);
  assert.deepEqual(evidence.bindingRefusals, Array(5).fill('LOCAL_UNLOCK_BINDING_MISMATCH'));
  await cdp.send('WebAuthn.setAutomaticPresenceSimulation', { authenticatorId, enabled: false });
  await page.evaluate(() => {
    const { api, binding, descriptor } = window.localUnlockTest;
    window.localUnlockPending = api.unlockLocalProtection(descriptor, binding).then(() => 'unexpected unlock', (e) => e.code);
  });
  await page.evaluate(() => window.localUnlockTest.api.cancelLocalAuthenticatorPrompts());
  assert.equal(await page.evaluate(() => window.localUnlockPending), 'LOCAL_UNLOCK_CANCELLED');
  await cdp.send('WebAuthn.removeVirtualAuthenticator', { authenticatorId });
  const noPrf = await cdp.send('WebAuthn.addVirtualAuthenticator', { options: {
    protocol: 'ctap2', ctap2Version: 'ctap2_1', transport: 'internal',
    hasResidentKey: true, hasUserVerification: true, hasPrf: false,
    automaticPresenceSimulation: true, isUserVerified: true,
  } });
  const unsupported = await page.evaluate(async () => {
    const { api, binding } = window.localUnlockTest;
    try { await api.createLocalProtection(binding); return 'unexpected protection'; }
    catch (e) { return e.code; }
  });
  assert.equal(unsupported, 'LOCAL_AUTHENTICATOR_PRF_UNAVAILABLE');
  await cdp.send('WebAuthn.removeVirtualAuthenticator', noPrf);
  process.stdout.write(`${JSON.stringify({ ...evidence, cancellation: 'CONFIRMED', unsupportedPrf: 'REFUSED', physicalAuthenticator: 'NOT_VERIFIED' })}\n`);
} finally {
  await browser.close();
  await new Promise((done) => server.close(done));
}

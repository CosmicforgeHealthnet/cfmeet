/**
 * Local history unlock only; this is not an Account sign-in credential. The PRF output
 * is used by the device worker to protect its at-rest key and is never sent to a service.
 * A fresh Account session and the persistent installation binding are checked separately.
 * See https://www.w3.org/TR/webauthn-3/#prf-extension.
 */
export type LocalProtectionScope = { tenantId: string; applicationId: string; environmentId: string };
export type LocalProtectionBinding = {
  account: string;
  installationId: string;
  accountDeviceId: string;
  messagingDeviceId: string;
  scope: LocalProtectionScope;
};
export type LocalProtectionDescriptor = LocalProtectionBinding & {
  v: 1;
  method: "WEBAUTHN_PRF";
  credentialId: string;
  prfSalt: string;
  origin: string;
};

export class LocalAuthenticatorError extends Error {
  constructor(public readonly code: string) {
    super(code);
    this.name = "LocalAuthenticatorError";
  }
}

type PrfOutput = { enabled?: boolean; results?: { first?: ArrayBuffer } };
type PrfExtensions = AuthenticationExtensionsClientInputs & { prf: { eval: { first: ArrayBuffer } } };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const pending = new Set<AbortController>();
const text = new TextEncoder();
function fail(code: string): never { throw new LocalAuthenticatorError(code); }
const encode = (bytes: Uint8Array): string => {
  let value = "";
  for (const byte of bytes) value += String.fromCharCode(byte);
  return btoa(value).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
};
const decode = (value: string, maxBytes: number): Uint8Array<ArrayBuffer> => {
  if (typeof value !== "string" || value.length > Math.ceil(maxBytes * 4 / 3) || !/^[A-Za-z0-9_-]+$/.test(value)) fail("LOCAL_UNLOCK_BINDING_INVALID");
  let decoded: string;
  try { decoded = atob(value.replaceAll("-", "+").replaceAll("_", "/") + "===".slice((value.length + 3) % 4)); }
  catch { return fail("LOCAL_UNLOCK_BINDING_INVALID"); }
  const bytes = Uint8Array.from(decoded, (c) => c.charCodeAt(0));
  if (!bytes.length || bytes.length > maxBytes || encode(bytes) !== value) fail("LOCAL_UNLOCK_BINDING_INVALID");
  return bytes;
};

function checkBinding(binding: LocalProtectionBinding): void {
  if (!binding || !UUID.test(binding.account) || !UUID.test(binding.accountDeviceId) || !UUID.test(binding.messagingDeviceId)
    || typeof binding.installationId !== "string" || binding.installationId.length < 1 || binding.installationId.length > 200
    || !binding.scope || ![binding.scope.tenantId, binding.scope.applicationId, binding.scope.environmentId].every((id) => UUID.test(id))) fail("LOCAL_UNLOCK_BINDING_INVALID");
}

/** Reject a copied descriptor, wrong account, changed installation, or moved origin before prompting. */
export function validateLocalProtection(descriptor: LocalProtectionDescriptor, expected: LocalProtectionBinding): void {
  checkBinding(descriptor);
  checkBinding(expected);
  if (descriptor.v !== 1 || descriptor.method !== "WEBAUTHN_PRF" || descriptor.origin !== globalThis.location?.origin
    || descriptor.account !== expected.account || descriptor.accountDeviceId !== expected.accountDeviceId
    || descriptor.installationId !== expected.installationId || descriptor.messagingDeviceId !== expected.messagingDeviceId
    || descriptor.scope.tenantId !== expected.scope.tenantId || descriptor.scope.applicationId !== expected.scope.applicationId
    || descriptor.scope.environmentId !== expected.scope.environmentId) fail("LOCAL_UNLOCK_BINDING_MISMATCH");
  decode(descriptor.credentialId, 1024);
  if (decode(descriptor.prfSalt, 32).length !== 32) fail("LOCAL_UNLOCK_BINDING_INVALID");
}

function available(): void {
  if (!globalThis.isSecureContext || typeof PublicKeyCredential === "undefined" || !navigator.credentials?.create || !navigator.credentials?.get) fail("LOCAL_AUTHENTICATOR_UNAVAILABLE");
  if (window.top !== window.self) fail("LOCAL_AUTHENTICATOR_CONTEXT_REFUSED");
}

/** A session end aborts pending system prompts as well as terminating device workers. */
export function cancelLocalAuthenticatorPrompts(): void {
  for (const controller of pending) controller.abort();
  pending.clear();
}

async function ceremony<T>(run: (signal: AbortSignal) => Promise<T>): Promise<T> {
  available();
  const controller = new AbortController();
  pending.add(controller);
  try { return await run(controller.signal); }
  catch (error) {
    if (error instanceof LocalAuthenticatorError) throw error;
    if (error instanceof DOMException && (error.name === "NotAllowedError" || error.name === "AbortError")) return fail("LOCAL_UNLOCK_CANCELLED");
    return fail("LOCAL_AUTHENTICATOR_UNAVAILABLE");
  } finally { pending.delete(controller); }
}

function checkClientData(credential: PublicKeyCredential, type: string, challenge: Uint8Array, origin: string): void {
  let data: { type?: string; challenge?: string; origin?: string; crossOrigin?: boolean; topOrigin?: string };
  try { data = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(credential.response.clientDataJSON)); }
  catch { return fail("LOCAL_AUTHENTICATOR_RESPONSE_INVALID"); }
  if (data.type !== type || data.challenge !== encode(challenge) || data.origin !== origin || data.crossOrigin === true
    || (data.topOrigin !== undefined && data.topOrigin !== origin)) fail("LOCAL_AUTHENTICATOR_RESPONSE_INVALID");
}

function output(credential: PublicKeyCredential): Uint8Array<ArrayBuffer> | null {
  const prf = (credential.getClientExtensionResults() as AuthenticationExtensionsClientOutputs & { prf?: PrfOutput }).prf;
  const first = prf?.results?.first;
  if (!first) return null;
  if (!(first instanceof ArrayBuffer) || first.byteLength !== 32) fail("LOCAL_AUTHENTICATOR_RESPONSE_INVALID");
  // Transfer this same buffer to the worker; do not retain a second PRF output copy.
  return new Uint8Array(first);
}

/** The caller transfers prfSecret.buffer to the device worker immediately, never persisting it. */
export async function unlockLocalProtection(descriptor: LocalProtectionDescriptor, expected: LocalProtectionBinding): Promise<Uint8Array<ArrayBuffer>> {
  validateLocalProtection(descriptor, expected);
  return ceremony(async (signal) => {
    const challenge = crypto.getRandomValues(new Uint8Array(32));
    const credential = await navigator.credentials.get({ signal, publicKey: {
      rpId: location.hostname,
      challenge,
      allowCredentials: [{ type: "public-key", id: decode(descriptor.credentialId, 1024) }],
      userVerification: "required",
      timeout: 60_000,
      extensions: { prf: { eval: { first: decode(descriptor.prfSalt, 32).buffer } } } as PrfExtensions,
    } });
    if (!(credential instanceof PublicKeyCredential) || encode(new Uint8Array(credential.rawId)) !== descriptor.credentialId) fail("LOCAL_AUTHENTICATOR_RESPONSE_INVALID");
    checkClientData(credential, "webauthn.get", challenge, descriptor.origin);
    const authenticatorData = new Uint8Array((credential.response as AuthenticatorAssertionResponse).authenticatorData);
    const rpHash = new Uint8Array(await crypto.subtle.digest("SHA-256", text.encode(location.hostname)));
    if (authenticatorData.length < 37 || (authenticatorData[32] & 5) !== 5 || !rpHash.every((byte, i) => authenticatorData[i] === byte)) fail("LOCAL_AUTHENTICATOR_RESPONSE_INVALID");
    return output(credential) ?? fail("LOCAL_AUTHENTICATOR_PRF_UNAVAILABLE");
  });
}

/** Creates a dedicated authenticator credential without changing any local history yet. */
export async function createLocalProtection(binding: LocalProtectionBinding): Promise<{ descriptor: LocalProtectionDescriptor; prfSecret: Uint8Array<ArrayBuffer> }> {
  checkBinding(binding);
  const descriptor: LocalProtectionDescriptor = { ...binding, scope: { ...binding.scope }, v: 1, method: "WEBAUTHN_PRF", credentialId: "", prfSalt: encode(crypto.getRandomValues(new Uint8Array(32))), origin: location.origin };
  await ceremony(async (signal) => {
    const challenge = crypto.getRandomValues(new Uint8Array(32));
    const credential = await navigator.credentials.create({ signal, publicKey: {
      rp: { id: location.hostname, name: "CF Meet local history" },
      user: { id: crypto.getRandomValues(new Uint8Array(32)), name: "CF Meet local history", displayName: "CF Meet local history" },
      challenge,
      pubKeyCredParams: [{ type: "public-key", alg: -7 }, { type: "public-key", alg: -257 }],
      authenticatorSelection: { authenticatorAttachment: "platform", residentKey: "preferred", userVerification: "required" },
      attestation: "none",
      timeout: 60_000,
      extensions: { prf: { eval: { first: decode(descriptor.prfSalt, 32).buffer } } } as PrfExtensions,
    } });
    if (!(credential instanceof PublicKeyCredential)) fail("LOCAL_AUTHENTICATOR_RESPONSE_INVALID");
    checkClientData(credential, "webauthn.create", challenge, descriptor.origin);
    const prf = (credential.getClientExtensionResults() as AuthenticationExtensionsClientOutputs & { prf?: PrfOutput }).prf;
    // Some authenticators cannot evaluate at creation. Always perform a UV assertion to
    // prove the exact credential can reproduce its output before migrating stored data.
    if (prf?.enabled !== true) fail("LOCAL_AUTHENTICATOR_PRF_UNAVAILABLE");
    const creationOutput = output(credential);
    creationOutput?.fill(0);
    descriptor.credentialId = encode(new Uint8Array(credential.rawId));
  });
  return { descriptor, prfSecret: await unlockLocalProtection(descriptor, binding) };
}

// Compile-time proof that the SDK's MessagingCrypto module types match the .d.ts that
// wasm-bindgen generates for packages/messaging-crypto/wasm (built by
// scripts/build-messaging-crypto-test-module.sh). Checked in CI: tsc -p wasm-shape.
import type { MessagingCryptoModule, MessagingCryptoWebGlue } from '../src/index.js';
import * as node from '../.wasm-test/nodejs/cf_messaging_crypto_wasm.js';
import * as web from '../.wasm-test/web/cf_messaging_crypto_wasm.js';

export const nodeModule: MessagingCryptoModule = node;
export const webGlue: MessagingCryptoWebGlue = web;

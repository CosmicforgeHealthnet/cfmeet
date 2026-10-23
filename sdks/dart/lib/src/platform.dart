/// Compile-time platform facts.
library;

/// True when compiled for a browser (dart2js / dart2wasm). Browser code must never hold a
/// platform credential (`cfmk_...`): it would be readable by anyone who loads the page.
const bool isWebPlatform = bool.fromEnvironment('dart.library.js_interop') ||
    bool.fromEnvironment('dart.library.html');

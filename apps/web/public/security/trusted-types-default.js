// The Trusted Types DEFAULT policy, loaded (with the request's CSP nonce) before any
// framework code. Next.js loads route chunks by assigning script.src without a policy of
// its own (Turbopack), which require-trusted-types-for 'script' blocks: client navigation
// would degrade into full page loads. This policy admits exactly one kind of untyped
// value: a same-origin script URL under /_next/static/ ending in .js. It defines no
// createHTML and no createScript, so every HTML or script string sink stays refused.
(function () {
  var tt = window.trustedTypes;
  if (!tt || tt.defaultPolicy) return;
  var origin = window.location.origin;
  tt.createPolicy("default", {
    createScriptURL: function (value) {
      var u;
      try {
        u = new URL(value, document.baseURI);
      } catch (e) {
        return null;
      }
      if (u.origin !== origin || u.hash || u.username || u.password) return null;
      if (u.pathname.indexOf("/_next/static/") !== 0 || !/\.js$/.test(u.pathname)) return null;
      // The value itself, unchanged: the bundler matches loaded chunks by their src.
      return value;
    },
  });
})();

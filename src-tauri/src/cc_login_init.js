(function() {
  // Poll for CCM-XSRF-TOKEN in document.cookie.  When found, write all
  // cookies into the URL hash fragment.  The Rust side polls webview.url(),
  // extracts cookies from the hash + WebView2 cookie store (which also
  // captures HttpOnly cookies), and returns the result.
  //
  // No guards, no redirect detection, no timeouts — same approach as the
  // working core-content-test project.  If the token is in document.cookie
  // (which it is for most Core Content deployments), we find it within 1s.
  // If the page redirects (login → main app), initialization_script re-runs
  // and we poll again on the new page.

  var signaled = false;

  function signalCompletion() {
    if (signaled) return;
    signaled = true;

    var pairs = {};
    var parts = (document.cookie || '').split(';');
    for (var i = 0; i < parts.length; i++) {
      var kv = parts[i].trim();
      var eq = kv.indexOf('=');
      if (eq > 0) {
        pairs[kv.substring(0, eq).trim()] = kv.substring(eq + 1).trim();
      }
    }
    var json = JSON.stringify(pairs);
    window.location.hash = '__cc_data__' + encodeURIComponent(json);
  }

  var attempts = 0;
  var maxAttempts = 60;

  var poll = setInterval(function() {
    attempts++;
    if ((document.cookie || '').indexOf('CCM-XSRF-TOKEN') !== -1 || attempts >= maxAttempts) {
      clearInterval(poll);
      signalCompletion();
    }
  }, 1000);
})();

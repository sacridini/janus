// Janus site: OS tabs, copy buttons, latest-release links, chart draw-in, guide contents.
(function () {
  "use strict";
  var REPO = "sacridini/janus";
  var LABEL = { win: "Windows", mac: "macOS", linux: "Linux" };

  function detectOS() {
    var p = ((navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "").toLowerCase();
    var ua = navigator.userAgent.toLowerCase();
    if (p.indexOf("win") >= 0 || ua.indexOf("windows") >= 0) return "win";
    if (p.indexOf("mac") >= 0 || ua.indexOf("mac os") >= 0) return "mac";
    if (p.indexOf("linux") >= 0 || ua.indexOf("linux") >= 0) return "linux";
    return null;
  }
  var os = detectOS();

  // ---- tabs (ARIA tablist with arrow keys)
  document.querySelectorAll("[data-tabs]").forEach(function (box) {
    var tabs = Array.prototype.slice.call(box.querySelectorAll('[role="tab"]'));
    function select(tab) {
      tabs.forEach(function (t) {
        var on = t === tab;
        t.setAttribute("aria-selected", on ? "true" : "false");
        t.tabIndex = on ? 0 : -1;
        document.getElementById(t.getAttribute("aria-controls")).hidden = !on;
      });
    }
    tabs.forEach(function (t, i) {
      t.addEventListener("click", function () { select(t); });
      t.addEventListener("keydown", function (e) {
        var j = e.key === "ArrowRight" ? i + 1 : e.key === "ArrowLeft" ? i - 1 : null;
        if (j === null) return;
        var n = tabs[(j + tabs.length) % tabs.length];
        select(n); n.focus(); e.preventDefault();
      });
    });
    var mine = os && box.querySelector('[data-os="' + os + '"]');
    if (mine) select(mine);
  });

  // ---- copy buttons
  document.querySelectorAll("[data-copy]").forEach(function (b) {
    b.addEventListener("click", function () {
      var text = b.getAttribute("data-copy");
      var done = function () { b.textContent = "Copied"; setTimeout(function () { b.textContent = "Copy"; }, 1600); };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(text).then(done, function () { selectCode(b); });
      } else { selectCode(b); }
    });
  });
  function selectCode(b) {
    var code = b.parentNode.querySelector("code");
    var r = document.createRange(); r.selectNodeContents(code);
    var s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
  }

  // ---- download buttons: label for this OS, then exact files from the latest release
  var auto = document.querySelector('[data-dl="auto"]');
  if (auto && os) auto.textContent = "Download for " + LABEL[os];
  if (os) {
    var card = document.querySelector('[data-os-card="' + os + '"]');
    if (card) card.classList.add("is-yours");
  }

  function pick(assets) {
    var out = {};
    assets.forEach(function (a) {
      var n = a.name.toLowerCase();
      if (/setup\.exe$/.test(n)) out.win = a;
      else if (/macos.*\.dmg$/.test(n)) out.mac = a;
      else if (/linux.*\.tar\.xz$/.test(n)) out.linux = a;
    });
    return out;
  }
  function mb(bytes) { return (bytes / 1048576).toFixed(0) + " MB"; }

  if (window.fetch) {
    fetch("https://api.github.com/repos/" + REPO + "/releases/latest", { headers: { Accept: "application/vnd.github+json" } })
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (rel) {
        if (!rel || !rel.assets) return;
        var files = pick(rel.assets);
        document.querySelectorAll("[data-version]").forEach(function (el) { el.textContent = rel.tag_name; });
        Object.keys(files).forEach(function (k) {
          var a = files[k];
          document.querySelectorAll('[data-dl="' + k + '"]').forEach(function (b) { b.href = a.browser_download_url; });
          var f = document.querySelector('[data-file="' + k + '"]');
          if (f) f.textContent = a.name + " · " + mb(a.size);
        });
        if (auto && os && files[os]) auto.href = files[os].browser_download_url;
      })
      .catch(function () { /* keep the links to the releases page */ });
  }

  // ---- draw the example series when the illustration comes into view
  var cube = document.querySelector(".cube");
  if (cube && "IntersectionObserver" in window) {
    var io = new IntersectionObserver(function (es) {
      es.forEach(function (e) { if (e.isIntersecting) { cube.classList.add("drawn"); io.disconnect(); } });
    }, { threshold: 0.4 });
    io.observe(cube);
  }

  // ---- guide pages: a figure opens larger over the page (click again: actual size; Esc or outside: close)
  var figs = document.querySelectorAll(".article .fig img");
  if (figs.length) {
    var box = document.createElement("div");
    box.className = "lightbox";
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    box.setAttribute("aria-label", "Enlarged image");
    box.hidden = true;
    box.innerHTML = '<button class="lb-close" type="button" aria-label="Close">×</button>' +
      '<div class="lb-scroll"><img alt=""></div><p class="lb-caption"></p>';
    document.body.appendChild(box);
    var big = box.querySelector("img"), cap = box.querySelector(".lb-caption");
    var closeBtn = box.querySelector(".lb-close"), opener = null;
    function close() {
      box.hidden = true;
      box.classList.remove("actual");
      document.documentElement.classList.remove("lb-open");
      if (opener) opener.focus();
    }
    function open(img, link) {
      opener = link || img;
      big.src = (link && link.getAttribute("href")) || img.currentSrc || img.src;
      big.alt = img.alt;
      var fc = img.closest("figure") && img.closest("figure").querySelector("figcaption");
      cap.textContent = fc ? fc.textContent : "";
      box.hidden = false;
      document.documentElement.classList.add("lb-open");
      closeBtn.focus();
    }
    figs.forEach(function (img) {
      var link = img.closest("a");
      var target = link || img;
      if (!link) { img.tabIndex = 0; img.setAttribute("role", "button"); }
      img.style.cursor = "zoom-in";
      target.addEventListener("click", function (e) { e.preventDefault(); open(img, link); });
      if (!link) img.addEventListener("keydown", function (e) {
        if (e.key === "Enter" || e.key === " ") { e.preventDefault(); open(img, null); }
      });
    });
    big.addEventListener("click", function (e) {
      e.stopPropagation();
      // Larger than the screen: toggle between fitting it and its actual size.
      if (big.naturalWidth > window.innerWidth * 0.94 || big.naturalHeight > window.innerHeight * 0.86) box.classList.toggle("actual");
    });
    box.addEventListener("click", function (e) { if (e.target === box || e.target.classList.contains("lb-scroll")) close(); });
    closeBtn.addEventListener("click", close);
    document.addEventListener("keydown", function (e) {
      if (box.hidden) return;
      if (e.key === "Escape") close();
      else if (e.key === "Tab") { e.preventDefault(); closeBtn.focus(); } // focus stays in the dialog
    });
  }

  // ---- guide pages: the contents entry of the section being read
  var toc = document.querySelectorAll(".toc a[href^='#']");
  if (toc.length && "IntersectionObserver" in window) {
    var byId = {};
    toc.forEach(function (a) { byId[a.getAttribute("href").slice(1)] = a; });
    var seen = new IntersectionObserver(function (es) {
      es.forEach(function (e) {
        if (!e.isIntersecting) return;
        toc.forEach(function (a) { a.removeAttribute("aria-current"); });
        byId[e.target.id].setAttribute("aria-current", "true");
      });
    }, { rootMargin: "-20% 0px -70% 0px" });
    Object.keys(byId).forEach(function (id) {
      var s = document.getElementById(id);
      if (s) seen.observe(s);
    });
  }
})();

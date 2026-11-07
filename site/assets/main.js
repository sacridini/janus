// Janus site: OS tabs, guide In Janus / In Python tabs, copy buttons, latest-release links, chart draw-in, guide lightbox, before/after swipes, annotated screenshots, looping clips, guide contents, menu button.
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
    // A tab list that starts hidden (the guides' In Janus / In Python): without this script both panels show. ?tab=NAME
    // opens the tab whose data-tab is NAME.
    var list = box.querySelector('[role="tablist"][hidden]');
    if (list) {
      list.hidden = false;
      var want = new URLSearchParams(location.search).get("tab");
      var first = tabs.filter(function (t) { return t.getAttribute("data-tab") === want; })[0];
      select(first || tabs.filter(function (t) { return t.getAttribute("aria-selected") === "true"; })[0] || tabs[0]);
    }
  });

  // ---- copy buttons (data-src: the whole script is fetched; if that fails, the text shown is copied)
  document.querySelectorAll("[data-copy]").forEach(function (b) {
    b.addEventListener("click", function () {
      var label = b.textContent;
      var done = function (msg) { b.textContent = msg || "Copied"; setTimeout(function () { b.textContent = label; }, msg ? 2600 : 1600); };
      var put = function (text, msg) {
        if (navigator.clipboard && navigator.clipboard.writeText) {
          navigator.clipboard.writeText(text).then(function () { done(msg); }, function () { selectCode(b); });
        } else { selectCode(b); }
      };
      var shown = function () { return codeOf(b).textContent; }; // data-copy="": the code beside it
      var src = b.getAttribute("data-src");
      if (src && window.fetch) {
        fetch(src).then(function (r) { if (!r.ok) throw new Error(r.status); return r.text(); })
          .then(function (t) { put(t); }, function () { put(shown(), "Copied what is shown"); });
        return;
      }
      put(b.getAttribute("data-copy") || shown());
    });
  });
  function codeOf(b) { return (b.closest(".pycode") || b.parentNode).querySelector("code"); }
  function selectCode(b) {
    var code = codeOf(b);
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
  // A swipe figure is left out (a click on it moves the divider); the markers of an annotated one are not images.
  var figs = document.querySelectorAll(".article .fig:not(.swipe) img");
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
      // The link's image (the full JPEG), or its WebP twin when that is what the page already loaded.
      var href = link && link.getAttribute("href");
      var shown = img.currentSrc || img.src;
      big.src = href ? (new URL(href, location.href).href.replace(/\.jpg$/i, ".webp") === shown ? shown : href) : shown;
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

  var query = new URLSearchParams(location.search);

  // The element to move with an image: its <picture> wrapper (the build writes one around the JPEGs that have a WebP twin).
  function unit(img) { return img.parentNode && img.parentNode.nodeName === "PICTURE" ? img.parentNode : img; }

  // ---- before / after swipe: <figure class="fig swipe"> with img.before and img.after of the same size
  // (data-label on each is its side's label). ?swipe=30 puts every divider at 30 %, for the screenshots.
  document.querySelectorAll(".fig.swipe").forEach(function (fig) {
    var before = fig.querySelector("img.before"), after = fig.querySelector("img.after");
    if (!before || !after) return;
    var beforeUnit = unit(before), afterUnit = unit(after); // the <picture> around an image, if it has one
    var start = parseFloat(query.get("swipe"));
    var pos = isNaN(start) ? 50 : Math.max(0, Math.min(100, start));
    var stage = document.createElement("div");
    stage.className = "sw-stage";
    fig.insertBefore(stage, beforeUnit);
    stage.appendChild(afterUnit);
    stage.appendChild(beforeUnit);
    var lb = before.getAttribute("data-label") || "Before", la = after.getAttribute("data-label") || "After";
    var left = document.createElement("span"), right = document.createElement("span");
    left.className = "sw-label l"; left.textContent = lb;
    right.className = "sw-label r"; right.textContent = la;
    left.setAttribute("aria-hidden", "true"); right.setAttribute("aria-hidden", "true");
    var handle = document.createElement("div");
    handle.className = "sw-handle";
    handle.tabIndex = 0;
    handle.setAttribute("role", "slider");
    handle.setAttribute("aria-label", "Divider: " + lb + " on the left, " + la + " on the right");
    handle.setAttribute("aria-valuemin", "0");
    handle.setAttribute("aria-valuemax", "100");
    handle.setAttribute("aria-orientation", "horizontal");
    stage.appendChild(left); stage.appendChild(right); stage.appendChild(handle);
    function set(p) {
      pos = Math.max(0, Math.min(100, p));
      stage.style.setProperty("--p", pos + "%");
      handle.setAttribute("aria-valuenow", String(Math.round(pos)));
      handle.setAttribute("aria-valuetext", Math.round(pos) + "% " + lb + ", " + Math.round(100 - pos) + "% " + la);
      left.classList.toggle("gone", pos < 16);
      right.classList.toggle("gone", pos > 84);
    }
    function fromEvent(e) {
      var r = stage.getBoundingClientRect();
      set((e.clientX - r.left) / r.width * 100);
    }
    var drag = false;
    stage.addEventListener("pointerdown", function (e) {
      if (e.pointerType === "mouse" && e.button !== 0) return;
      drag = true;
      try { stage.setPointerCapture(e.pointerId); } catch (x) { /* not capturable */ }
      // A finger that starts a vertical scroll must not move the divider: it follows on the first move.
      if (e.pointerType === "mouse" || e.target === handle) fromEvent(e);
      if (e.target === handle) handle.focus();
    });
    stage.addEventListener("pointermove", function (e) { if (drag) fromEvent(e); });
    ["pointerup", "pointercancel", "lostpointercapture"].forEach(function (n) {
      stage.addEventListener(n, function (e) { if (drag && n === "pointerup" && e.pointerType !== "mouse") fromEvent(e); drag = false; });
    });
    handle.addEventListener("keydown", function (e) {
      var d = e.key === "ArrowLeft" || e.key === "ArrowDown" ? -5 : e.key === "ArrowRight" || e.key === "ArrowUp" ? 5 : null;
      if (d !== null) set(pos + d);
      else if (e.key === "Home") set(0);
      else if (e.key === "End") set(100);
      else return;
      e.preventDefault();
    });
    set(pos);
    fig.classList.add("js-on");
  });

  // ---- annotated screenshot: <figure class="annotated"> with an <ol class="hotspots"> whose li have data-x and
  // data-y (% of the image). The list stays as the legend; ?hs=3 pins marker 3, for the screenshots.
  document.querySelectorAll("figure.annotated").forEach(function (fig) {
    var items = Array.prototype.slice.call(fig.querySelectorAll("ol.hotspots > li"));
    var img = fig.querySelector("img");
    if (!items.length || !img) return;
    var anchor = img.closest("a") && fig.contains(img.closest("a")) ? img.closest("a") : unit(img);
    var stage = document.createElement("div");
    stage.className = "an-stage";
    anchor.parentNode.insertBefore(stage, anchor);
    stage.appendChild(anchor);
    var marks = items.map(function (li, i) {
      var x = parseFloat(li.getAttribute("data-x")), y = parseFloat(li.getAttribute("data-y"));
      var text = li.textContent.replace(/\s+/g, " ").trim();
      var m = document.createElement("button");
      m.type = "button";
      m.className = "hs-mark" + (x < 20 ? " tip-l" : x > 80 ? " tip-r" : "") + (y < 18 ? " tip-b" : "");
      m.style.left = x + "%"; m.style.top = y + "%";
      m.setAttribute("aria-label", (i + 1) + ": " + text);
      m.appendChild(document.createTextNode(String(i + 1)));
      var tip = document.createElement("span");
      tip.className = "hs-tip"; tip.setAttribute("aria-hidden", "true"); tip.textContent = text;
      m.appendChild(tip);
      stage.appendChild(m);
      return m;
    });
    function light(i, on) { marks[i].classList.toggle("on", on); items[i].classList.toggle("on", on); }
    function pin(i) {
      var was = marks[i].classList.contains("pin");
      marks.forEach(function (m, j) { m.classList.remove("pin"); items[j].classList.remove("pin"); });
      if (!was) { marks[i].classList.add("pin"); items[i].classList.add("pin"); }
    }
    items.forEach(function (li, i) {
      var m = marks[i];
      [m, li].forEach(function (el) {
        el.addEventListener("mouseenter", function () { light(i, true); });
        el.addEventListener("mouseleave", function () { light(i, false); });
        el.addEventListener("click", function () { pin(i); });
      });
      m.addEventListener("focus", function () { light(i, true); });
      m.addEventListener("blur", function () { light(i, false); });
    });
    var want = parseInt(query.get("hs"), 10);
    if (want >= 1 && want <= marks.length) pin(want - 1);
    fig.classList.add("js-on");
  });

  // ---- "How these screenshots were made": ?open=howmade opens every one, for the screenshots
  if (query.get("open") === "howmade") {
    document.querySelectorAll("details.howmade").forEach(function (d) { d.open = true; });
  }

  // ---- looping clips: <video muted loop playsinline preload="none" controls poster="..."> in a .story-fig.
  // Without this script the poster shows with the browser's own controls. With it the controls go, the clip loads
  // when it is near the screen and plays only while at least half of it is visible (pausing when it is not, or the
  // tab is hidden); a button plays and pauses it (always visible while paused, on hover or focus while playing; a
  // click on the picture does the same). With "reduce motion" (or the data saver on) it never starts by itself:
  // the poster shows, with a play button.
  var clips = document.querySelectorAll(".story-fig video");
  if (clips.length) {
    var calm = (window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches) ||
      !!(navigator.connection && navigator.connection.saveData);
    var PLAY = '<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" focusable="false"><path d="M4 2.5v11l9-5.5z" fill="currentColor"/></svg>';
    var PAUSE = '<svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" focusable="false"><path d="M3.5 2.5h3v11h-3zM9.5 2.5h3v11h-3z" fill="currentColor"/></svg>';
    clips.forEach(function (v) {
      v.removeAttribute("controls");
      var wrap = document.createElement("div");
      wrap.className = "clip";
      v.parentNode.insertBefore(wrap, v);
      wrap.appendChild(v);
      var btn = document.createElement("button");
      btn.type = "button";
      btn.className = "clip-btn";
      wrap.appendChild(btn);
      var wants = !calm;   // does the reader want it playing? A press of the button decides from then on.
      var visible = false;
      function paint() {
        var playing = !v.paused;
        wrap.classList.toggle("is-playing", playing);
        btn.innerHTML = playing ? PAUSE : PLAY;
        btn.setAttribute("aria-label", playing ? "Pause the clip" : "Play the clip");
      }
      function play() {
        var p = v.play();
        if (p && p.catch) p.catch(function () { paint(); });
      }
      function sync() {
        if (wants && visible && !document.hidden) play();
        else if (!v.paused) v.pause();
      }
      btn.addEventListener("click", function () { wants = v.paused; if (wants) { visible = true; play(); } else v.pause(); });
      v.addEventListener("click", function () { btn.click(); });
      ["play", "pause"].forEach(function (n) { v.addEventListener(n, paint); });
      document.addEventListener("visibilitychange", sync);
      paint();
      if (!("IntersectionObserver" in window)) return;
      // near the screen: let it load (and load it, since the page said preload="none")
      var near = new IntersectionObserver(function (es) {
        if (!es[0].isIntersecting) return;
        near.disconnect();
        if (!calm) { v.preload = "auto"; v.load(); }
      }, { rootMargin: "500px 0px" });
      near.observe(v);
      var seen = new IntersectionObserver(function (es) {
        visible = es[0].isIntersecting;
        sync();
      }, { threshold: 0.5 });
      seen.observe(v);
    });
  }

  // ---- a link to something inside a closed <details> (a release of the news page) opens it
  function openTarget() {
    var id = location.hash && decodeURIComponent(location.hash.slice(1));
    var t = id && document.getElementById(id);
    var d = t && t.closest("details");
    if (d && !d.open) { d.open = true; t.scrollIntoView(); }
  }
  openTarget();
  window.addEventListener("hashchange", openTarget);

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

  // ---- menu button (narrow screens): the main nav as a dropdown panel; Esc, a link or a click outside closes it
  var navbar = document.querySelector(".nav");
  var toggle = navbar && navbar.querySelector(".nav-toggle");
  if (toggle) {
    var setMenu = function (open) {
      navbar.classList.toggle("is-open", open);
      toggle.setAttribute("aria-expanded", open ? "true" : "false");
    };
    toggle.addEventListener("click", function () { setMenu(toggle.getAttribute("aria-expanded") !== "true"); });
    document.addEventListener("keydown", function (e) {
      if (e.key === "Escape" && toggle.getAttribute("aria-expanded") === "true") { setMenu(false); toggle.focus(); }
    });
    document.addEventListener("click", function (e) {
      if (toggle.getAttribute("aria-expanded") === "true" && !navbar.contains(e.target)) setMenu(false);
    });
    navbar.querySelectorAll("nav a").forEach(function (a) { a.addEventListener("click", function () { setMenu(false); }); });
    window.addEventListener("resize", function () { if (window.innerWidth > 860) setMenu(false); });
  }
})();

// Janus site: the search dialog (Ctrl+K, Cmd+K or "/"), after the program's command palette. It reads
// data/search.json (made by tools/build_site.py) on the first open. Vanilla JS, no libraries.
(function () {
  "use strict";
  var script = document.currentScript;
  var root = script && script.src ? new URL("../", script.src) : new URL("./", location.href);
  var LIMIT = 12;
  var KINDS = [["m", "Methods"], ["g", "Guides"], ["s", "Sections"]];
  var TRY = ["landtrendr", "ccdc break", "validation area", "phenology", "install"];
  var PAGE = "https://sacridini.github.io/janus/";

  var entries = null, loading = null, failed = false;
  var dlg, input, list, msg, status, closeBtn, opener = null, shown = [], active = -1;

  // ---- text: lower case, no accents; the map gives each normalized character's index in the original
  function norm(str) {
    var s = "", map = [];
    for (var i = 0; i < str.length; i++) {
      var n = str[i].normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();
      for (var j = 0; j < n.length; j++) { s += n[j]; map.push(i); }
    }
    return { s: s, map: map };
  }
  function words(q) { return norm(q).s.split(/[^a-z0-9]+/).filter(Boolean); }
  function esc(s) { return String(s).replace(/[&<>"]/g, function (c) { return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]; }); }

  function prepare(list) {
    list.forEach(function (e) {
      // The kind is a word to search too: "ccdc guide", "ccdc method".
      e.nt = norm(e.t).s; e.nc = norm(e.c + (e.k === "g" ? " guide" : e.k === "m" ? " method" : "")).s; e.nx = norm(e.x).s;
    });
    return list;
  }

  // ---- matching: every word must be in the title, the context or the text; the title counts most
  function score(e, ws, whole) {
    var total = 0;
    for (var i = 0; i < ws.length; i++) {
      var w = ws[i], at = e.nt.indexOf(w), s = 0;
      if (at >= 0) s = (at === 0 || /[^a-z0-9]/.test(e.nt[at - 1])) ? 10 : 6;
      else if (e.nc.indexOf(w) >= 0) s = 3;
      else if (e.nx.indexOf(w) >= 0) s = 2;
      else return 0;
      total += s;
    }
    if (e.nt === whole) total += 30;
    else if (e.nt.indexOf(whole) === 0) total += 12;
    return total + (e.k === "m" ? 4 : e.k === "g" ? 2 : 0);
  }
  function search(q) {
    var ws = words(q);
    if (!ws.length) return [];
    var whole = ws.join(" "), hits = [];
    entries.forEach(function (e, i) {
      var s = score(e, ws, whole);
      if (s) hits.push({ e: e, s: s, i: i });
    });
    hits.sort(function (a, b) { return b.s - a.s || a.i - b.i; });
    hits = hits.slice(0, LIMIT);
    var out = [];
    KINDS.forEach(function (k) { hits.forEach(function (h) { if (h.e.k === k[0]) out.push(h.e); }); });
    return out;
  }

  // ---- highlighting the matched words in the original text
  function mark(text, ws) {
    var n = norm(text), ranges = [];
    ws.forEach(function (w) {
      for (var at = n.s.indexOf(w); at >= 0; at = n.s.indexOf(w, at + w.length)) {
        ranges.push([n.map[at], n.map[at + w.length - 1] + 1]);
      }
    });
    ranges.sort(function (a, b) { return a[0] - b[0]; });
    var out = "", pos = 0;
    ranges.forEach(function (r) {
      if (r[1] <= pos) return;
      var from = Math.max(r[0], pos);
      out += esc(text.slice(pos, from)) + "<mark>" + esc(text.slice(from, r[1])) + "</mark>";
      pos = r[1];
    });
    return out + esc(text.slice(pos));
  }

  // ---- the dialog
  function build() {
    dlg = document.createElement("div");
    dlg.className = "srch";
    dlg.hidden = true;
    dlg.innerHTML =
      '<div class="srch-box" role="dialog" aria-modal="true" aria-label="Search the site">' +
      '<div class="srch-bar"><svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true" focusable="false"><circle cx="6.5" cy="6.5" r="4.6" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="M10 10l4.2 4.2" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>' +
      '<input type="text" role="combobox" aria-expanded="true" aria-controls="srch-list" aria-autocomplete="list" aria-label="Search methods, guides and sections" placeholder="Search methods, guides and sections" autocomplete="off" autocapitalize="off" spellcheck="false" enterkeyhint="go">' +
      '<button type="button" class="srch-close" aria-label="Close search"><kbd>Esc</kbd></button></div>' +
      '<div class="srch-list" id="srch-list" role="listbox" aria-label="Results"></div>' +
      '<div class="srch-msg"></div>' +
      '<p class="srch-status" role="status" aria-live="polite"></p>' +
      '<p class="srch-keys" aria-hidden="true"><span><kbd>↑</kbd><kbd>↓</kbd> move</span><span><kbd>Enter</kbd> open</span><span><kbd>Esc</kbd> close</span></p>' +
      '</div>';
    document.body.appendChild(dlg);
    input = dlg.querySelector("input");
    list = dlg.querySelector(".srch-list");
    msg = dlg.querySelector(".srch-msg");
    status = dlg.querySelector(".srch-status");
    closeBtn = dlg.querySelector(".srch-close");

    input.addEventListener("input", render);
    closeBtn.addEventListener("click", close);
    dlg.addEventListener("mousedown", function (e) { if (e.target === dlg) { e.preventDefault(); close(); } });
    list.addEventListener("mousemove", function (e) {
      var o = e.target.closest("[role=option]");
      if (o && shown[+o.getAttribute("data-i")] !== undefined && +o.getAttribute("data-i") !== active) setActive(+o.getAttribute("data-i"), false);
    });
    list.addEventListener("click", function (e) {
      // A click on a result is a link click (Ctrl-click works); the dialog closes behind it.
      if (e.target.closest("[role=option]") && !e.ctrlKey && !e.metaKey && !e.shiftKey) close(true);
    });
    msg.addEventListener("click", function (e) {
      var t = e.target.closest("[data-try]");
      if (t) { input.value = t.getAttribute("data-try"); render(); input.focus(); }
    });
    dlg.addEventListener("keydown", function (e) {
      if (e.key === "Escape") { e.preventDefault(); close(); }
      else if (e.key === "ArrowDown") { e.preventDefault(); if (shown.length) setActive((active + 1) % shown.length, true); }
      else if (e.key === "ArrowUp") { e.preventDefault(); if (shown.length) setActive((active - 1 + shown.length) % shown.length, true); }
      else if (e.key === "Home" && shown.length && e.target !== input) { e.preventDefault(); setActive(0, true); }
      else if (e.key === "Enter") {
        if (e.target === closeBtn) return;
        if (active >= 0 && shown[active]) { e.preventDefault(); go(shown[active]); }
      } else if (e.key === "Tab") {
        // Focus stays in the dialog: the field and the close button.
        var order = [input, closeBtn], i = order.indexOf(document.activeElement);
        var next = order[(i + (e.shiftKey ? -1 : 1) + order.length) % order.length];
        e.preventDefault(); next.focus();
      }
    });
  }

  function href(e) { return new URL(e.u, root).href; }
  function go(e) { close(true); location.href = href(e); }

  function setActive(i, scroll) {
    var opts = list.querySelectorAll("[role=option]");
    if (opts[active]) { opts[active].removeAttribute("aria-selected"); opts[active].classList.remove("on"); }
    active = i;
    if (opts[i]) {
      opts[i].setAttribute("aria-selected", "true"); opts[i].classList.add("on");
      input.setAttribute("aria-activedescendant", opts[i].id);
      if (scroll) opts[i].scrollIntoView({ block: "nearest" });
    } else input.removeAttribute("aria-activedescendant");
  }

  function empty(text, withTry) {
    shown = []; active = -1;
    input.removeAttribute("aria-activedescendant");
    list.innerHTML = "";
    msg.innerHTML = '<p class="srch-empty">' + text + "</p>" + (withTry ?
      '<p class="srch-try">Try: ' + TRY.map(function (t) { return '<button type="button" data-try="' + esc(t) + '">' + esc(t) + "</button>"; }).join(" ") + "</p>" : "");
  }

  function render() {
    if (failed) {
      empty('Search could not load its index. ' + (location.protocol === "file:" ?
        'This page was opened from a file, and browsers do not let it read the index. Use <a href="' + PAGE + '">the published site</a> instead, or serve this folder (<code>python -m http.server</code>).' :
        'Check your connection and reload the page, or use <a href="' + PAGE + '">the published site</a>.'), false);
      status.textContent = "";
      return;
    }
    if (!entries) { empty("Loading…", false); status.textContent = ""; return; }
    var q = input.value, ws = words(q);
    if (!ws.length) { empty("Type a method, a guide or a word.", true); status.textContent = ""; return; }
    shown = search(q);
    if (!shown.length) { empty("Nothing matches “" + esc(q.trim()) + "”. Try fewer words.", true); status.textContent = "No results"; return; }
    var html = "", n = 0;
    KINDS.forEach(function (k) {
      var group = shown.filter(function (e) { return e.k === k[0]; });
      if (!group.length) return;
      html += '<div class="srch-group" role="presentation">' + k[1] + "</div>";
      group.forEach(function (e) {
        html += '<a role="option" class="srch-opt" id="srch-o' + n + '" data-i="' + n + '" href="' + esc(href(e)) + '" tabindex="-1">' +
          '<span class="srch-t">' + mark(e.t, ws) + "</span><span class=\"srch-c\">" + esc(e.c) + "</span>" +
          (e.x ? '<span class="srch-x">' + mark(e.x, ws) + "</span>" : "") + "</a>";
        n++;
      });
    });
    // search() returns the results in the order of KINDS, which is the order of the options.
    active = -1;
    msg.innerHTML = "";
    list.innerHTML = html;
    list.scrollTop = 0;
    setActive(0, false);
    status.textContent = shown.length + (shown.length === 1 ? " result" : " results");
  }

  function load() {
    if (entries || loading) return loading;
    loading = fetch(new URL("data/search.json", root).href)
      .then(function (r) { if (!r.ok) throw new Error("HTTP " + r.status); return r.json(); })
      .then(function (j) { entries = prepare(j); })
      .catch(function () { failed = true; });
    return loading;
  }

  function open(query) {
    if (!dlg) build();
    if (!dlg.hidden) { if (query != null) { input.value = query; render(); } input.focus(); return; }
    opener = document.activeElement;
    dlg.hidden = false;
    document.documentElement.classList.add("srch-open");
    if (query != null) input.value = query;
    render();
    input.focus();
    if (query == null) input.select();
    load().then(render);
  }
  function close(keepFocus) {
    if (!dlg || dlg.hidden) return;
    dlg.hidden = true;
    document.documentElement.classList.remove("srch-open");
    if (keepFocus !== true && opener && opener.focus) opener.focus();
  }

  // ---- the button and the keys
  var mac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || "");
  document.querySelectorAll("[data-search]").forEach(function (b) {
    var k = b.querySelector("kbd");
    if (k && mac) k.textContent = "⌘ K";
    b.addEventListener("click", function () { open(); });
  });
  document.addEventListener("keydown", function (e) {
    if (e.defaultPrevented) return;
    var t = e.target, typing = t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
    var k = (e.key || "").toLowerCase();
    if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && k === "k") {
      e.preventDefault();
      if (dlg && !dlg.hidden) close(); else open();
    } else if (e.key === "/" && !e.ctrlKey && !e.metaKey && !e.altKey && !typing && !(dlg && !dlg.hidden)) {
      e.preventDefault(); open();
    }
  });

  // A link with ?q=words opens the dialog with them (used for the screenshots of the site).
  var q = new URLSearchParams(location.search).get("q");
  if (q) open(q);
})();

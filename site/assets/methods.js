// Janus site: the methods chooser. The catalog is in the page (build_site.py); this only hides and
// shows its cards by the chips and the search, counts them and keeps the choice in the URL hash.
(function () {
  "use strict";
  var cards = Array.prototype.slice.call(document.querySelectorAll(".m-card"));
  var groups = Array.prototype.slice.call(document.querySelectorAll("[data-group]"));
  var chips = Array.prototype.slice.call(document.querySelectorAll(".chip[data-row]"));
  var box = document.getElementById("m-q");
  var count = document.getElementById("m-count");
  var empty = document.getElementById("m-empty");
  var clear = document.getElementById("m-clear");
  if (!cards.length || !box || !count) return;

  var state = { data: "", find: "", q: "" };

  function words(s) { return (s || "").split(" ").filter(Boolean); }

  function matches(card) {
    var cad = words(card.getAttribute("data-cadence"));
    if (state.data && cad.indexOf(state.data) < 0 && cad.indexOf("any") < 0) return false;
    if (state.find && words(card.getAttribute("data-questions")).indexOf(state.find) < 0) return false;
    var text = card.getAttribute("data-text") || "";
    return words(state.q.toLowerCase()).every(function (w) { return text.indexOf(w) >= 0; });
  }

  function writeHash() {
    var parts = [];
    if (state.data) parts.push("data=" + state.data);
    if (state.find) parts.push("find=" + state.find);
    if (state.q) parts.push("q=" + encodeURIComponent(state.q));
    var h = parts.length ? "#" + parts.join("&") : "";
    if (h !== location.hash) history.replaceState(null, "", location.pathname + location.search + h);
  }

  function apply() {
    var shown = 0;
    cards.forEach(function (c) {
      var on = matches(c);
      c.hidden = !on;
      if (on) shown++;
    });
    groups.forEach(function (g) { g.hidden = !g.querySelector(".m-card:not([hidden])"); });
    chips.forEach(function (b) {
      b.setAttribute("aria-pressed", state[b.getAttribute("data-row")] === b.getAttribute("data-value") ? "true" : "false");
    });
    count.textContent = shown + " of " + cards.length + " methods";
    empty.hidden = shown > 0;
    clear.hidden = !(state.data || state.find || state.q);
    writeHash();
  }

  function readHash() {
    var next = { data: "", find: "", q: "" };
    location.hash.replace(/^#/, "").split("&").forEach(function (kv) {
      var i = kv.indexOf("=");
      if (i < 0) return;
      var k = kv.slice(0, i), v = kv.slice(i + 1);
      try { v = decodeURIComponent(v); } catch (e) { return; }
      if (k === "q") next.q = v;
      else if ((k === "data" || k === "find") && chips.some(function (b) {
        return b.getAttribute("data-row") === k && b.getAttribute("data-value") === v;
      })) next[k] = v;
    });
    // A hash that is not a choice (a group's id, say) leaves the page as it is.
    if (next.data || next.find || next.q || /(^|[#&])(data|find|q)=/.test(location.hash)) {
      state = next;
      box.value = state.q;
    }
  }

  chips.forEach(function (b) {
    b.addEventListener("click", function () {
      var row = b.getAttribute("data-row"), v = b.getAttribute("data-value");
      state[row] = state[row] === v ? "" : v;
      apply();
    });
  });
  box.addEventListener("input", function () { state.q = box.value.trim(); apply(); });
  clear.addEventListener("click", function () {
    state = { data: "", find: "", q: "" };
    box.value = "";
    apply();
  });
  window.addEventListener("hashchange", function () { readHash(); apply(); });

  readHash();
  apply();
})();

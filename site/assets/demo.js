/* The LandTrendr demo of the Janus site: a map of 256 x 256 annual NDVI series, the series of the
 * pixel under the cursor, and the fits Zeit computed. Nothing is recomputed here: the vertices
 * come from lt.bin and grid.json, made by tools/make_site_demo.py. Vanilla JS, no libraries.
 * Fills every <div class="lt-demo" data-src="assets/demo/">. Two optional attributes: data-lazy loads
 * the data only when the widget is within 600 px of the viewport (the home page), and data-hash="off"
 * leaves the URL hash alone, neither read nor written (the home page has other uses for it). */
(function () {
  'use strict';

  var SZ = 256, NY = 40, COLS = 8, Y0 = 1985, MAXV = 7, BPP = 14;
  var STOPS = {
    viridis: ['#440154', '#482878', '#3E4A89', '#31688E', '#26828E', '#1F9E89', '#35B779', '#6DCD59', '#B4DE2C', '#FDE725'],
    plasma: ['#0D0887', '#6A00A8', '#B12A90', '#E16462', '#FCA636', '#F0F921'],
    magma: ['#000004', '#3B0F70', '#8C2981', '#DE4968', '#FE9F6D', '#FCFDBF']
  };
  var NONE_RGB = [21, 22, 26];
  var PIN_COL = ['#FF4B4B', '#3C8DFF'];
  var HOVER_SERIES = '#86D25B', HOVER_FIT = '#E64CE6';
  var SHORT = {
    clearing_pin: "Guide's clearing pin",
    old_clearing_regrown: 'Old clearing, regrown',
    recent_clearing: 'Recent clearing',
    mid_period_clearing: 'Clearing in 2011',
    degraded_patch: 'Partial loss',
    stable_forest: 'Stable forest'
  };
  var HELP = {
    ms: 'The most straight segments the fit may use. LandTrendr keeps fewer when a simpler model explains the series about as well.',
    sp: 'Dampens a one-year spike before the fit. 1.0 means no dampening; lower values dampen more.',
    rc: 'A recovery that regrows the whole drop in fewer than 1 / this many years is not allowed: 0.25 means 4 years, 1.0 allows any.'
  };

  function hexRgb(h) { return [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16)]; }
  function ramp(name, t) {
    var s = STOPS[name], f = Math.min(1, Math.max(0, t)) * (s.length - 1), i = Math.min(s.length - 2, Math.floor(f)), u = f - i;
    var a = hexRgb(s[i]), b = hexRgb(s[i + 1]);
    return [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u];
  }
  function pack(c) { return (255 << 24) | (Math.round(c[2]) << 16) | (Math.round(c[1]) << 8) | Math.round(c[0]); }
  function css(name) { return 'linear-gradient(90deg,' + STOPS[name].join(',') + ')'; }
  function f1(n) { return Number.isInteger(n) ? n.toFixed(1) : String(n); }
  function esc(s) { return String(s).replace(/[&<>"]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]; }); }
  function q2v(q) { return q ? (q - 1) / 254 * 1.2 - 0.2 : NaN; }
  function fmt(v, d) { return isNaN(v) ? 'no data' : v.toFixed(d == null ? 2 : d); }

  function getJSON(url) {
    return fetch(url).then(function (r) {
      if (!r.ok) throw new Error(url + ': HTTP ' + r.status);
      return r.json();
    });
  }
  function getBin(url) {
    return fetch(url).then(function (r) {
      if (!r.ok) throw new Error(url + ': HTTP ' + r.status);
      return r.arrayBuffer();
    }).then(function (b) { return new Uint8Array(b); });
  }
  function getGray(url) {
    return new Promise(function (resolve, reject) {
      var im = new Image();
      im.onload = function () {
        try {
          var w = im.naturalWidth, h = im.naturalHeight, c = document.createElement('canvas');
          c.width = w; c.height = h;
          var g = c.getContext('2d', { willReadFrequently: true });
          g.drawImage(im, 0, 0);
          var d = g.getImageData(0, 0, w, h).data, out = new Uint8Array(w * h);
          for (var i = 0; i < out.length; i++) out[i] = d[4 * i];
          resolve({ w: w, h: h, data: out });
        } catch (e) { reject(e); }
      };
      im.onerror = function () { reject(new Error('could not load ' + url)); };
      im.src = url;
    });
  }

  function init(root) {
    var hashOff = root.getAttribute('data-hash') === 'off';
    var base = root.getAttribute('data-src') || 'assets/demo/';
    if (base.slice(-1) !== '/') base += '/';
    root.innerHTML = '<p class="ltd-loading" role="status">Loading 40 years of pixels…</p>';
    Promise.all([
      getJSON(base + 'meta.json'), getJSON(base + 'grid.json'), getBin(base + 'lt.bin'),
      getGray(base + 'ndvi.png'), getGray(base + 'yod.png'), getGray(base + 'mag.png')
    ]).then(function (r) {
      start(root, hashOff, { meta: r[0], grid: r[1], lt: r[2], atlas: r[3], yod: r[4].data, mag: r[5].data });
    }).catch(function (e) {
      var local = location.protocol === 'file:';
      root.innerHTML = '<p class="ltd-error" role="alert"><strong>The demo could not load its data.</strong> ' +
        (local ? 'This page was opened from a file. Open it from a web server instead (for example, run <code>python -m http.server</code> in the site folder and visit <code>http://localhost:8000/</code>), because browsers do not let a page read its own data files from file://.'
          : 'Check your connection and reload the page.') +
        '</p><p class="ltd-error-detail">' + esc(e && e.message || e) + '</p>';
    });
  }

  function start(root, hashOff, D) {
    var meta = D.meta, grid = D.grid, lt = D.lt, atlas = D.atlas, yodA = D.yod, magA = D.mag;
    var years = meta.years, bounds = meta.bounds, psz = meta.pixel_size;
    var gp = meta.grid;
    var pixels = grid.pixels;
    var maxMag = 0;
    for (var i = 0; i < magA.length; i++) if (magA[i] > maxMag) maxMag = magA[i];
    maxMag /= 255;
    var MAG_LO = meta.event_filter.min_magnitude_ndvi;

    var S = {
      mode: 'ndvi', year: NY - 1, cur: null, pins: [], playing: false,
      s: 0, ms: 6, sp: 0.9, rc: 0.25
    };
    var cs = getComputedStyle(document.documentElement);
    var C = {
      date: (cs.getPropertyValue('--date') || '#F29A38').trim(), mute: (cs.getPropertyValue('--mute') || '#A0A7B2').trim(),
      rule: (cs.getPropertyValue('--rule-2') || '#33405A').trim(), text: (cs.getPropertyValue('--text') || '#E6E6E6').trim(),
      win: (cs.getPropertyValue('--win') || '#0F0F0F').trim(),
      mono: (cs.getPropertyValue('--f-mono') || 'monospace').trim()
    };

    /* data access */
    function qAt(yi, x, y) { return atlas.data[((yi >> 3) * SZ + y) * atlas.w + (yi % COLS) * SZ + x]; }
    function seriesAt(x, y) {
      var s = new Float32Array(NY);
      for (var k = 0; k < NY; k++) s[k] = q2v(qAt(k, x, y));
      return s;
    }
    function vertsAt(x, y) {
      var off = (y * SZ + x) * BPP, v = [];
      for (var k = 0; k < MAXV; k++) {
        var yr = lt[off + 2 * k], q = lt[off + 2 * k + 1];
        if (yr === 255 && q === 255) break;
        v.push([Y0 + yr, q2v(q)]);
      }
      return v;
    }
    function fitAt(v, yr) {
      for (var k = 0; k + 1 < v.length; k++) {
        if (yr >= v[k][0] && yr <= v[k + 1][0]) return v[k][1] + (v[k + 1][1] - v[k][1]) * (yr - v[k][0]) / (v[k + 1][0] - v[k][0]);
      }
      return NaN;
    }
    function info(x, y) {
      var i = y * SZ + x, v = vertsAt(x, y), yv = yodA[i], loss = null;
      if (yv !== 255) {
        loss = { from: Y0 + yv, to: null, mag: magA[i] / 255 };
        for (var k = 0; k + 1 < v.length; k++) if (v[k][0] === loss.from) { loss.to = v[k + 1][0]; break; }
      }
      return {
        x: x, y: y, lon: bounds.west + (x + 0.5) * psz.x, lat: bounds.north - (y + 0.5) * psz.y,
        ndvi: q2v(qAt(S.year, x, y)), fit: fitAt(v, years[S.year]), verts: v, loss: loss
      };
    }
    function summary(x, y) {
      var p = info(x, y);
      return 'Pixel ' + x + ', ' + y + '. NDVI ' + fmt(p.ndvi) + ' in ' + years[S.year] + '. ' +
        (p.loss ? 'Greatest loss from ' + p.loss.from + (p.loss.to ? ' to ' + p.loss.to : '') + ', magnitude ' + p.loss.mag.toFixed(2) + '.' : 'No loss event.') +
        ' ' + p.verts.length + ' LandTrendr vertices.';
    }

    /* DOM */
    root.innerHTML =
      '<div class="ltd">' +
      '<div class="ltd-main">' +
      '<section class="ltd-panel ltd-map-panel" aria-label="Map">' +
      '<h3 class="ltd-title">Map</h3>' +
      '<div class="ltd-body">' +
      '<div class="ltd-modes" role="group" aria-label="Map layer">' +
      '<button type="button" class="ltd-btn" data-mode="ndvi" aria-pressed="true">NDVI</button>' +
      '<button type="button" class="ltd-btn" data-mode="yod" aria-pressed="false">Year of loss</button>' +
      '<button type="button" class="ltd-btn" data-mode="mag" aria-pressed="false">Magnitude</button>' +
      '</div>' +
      '<div class="ltd-stage" tabindex="0" role="application" aria-roledescription="pixel map" ' +
      'aria-label="Map of 256 by 256 pixels. Arrow keys move the cursor pixel, Shift moves it by ten, Enter pins it.">' +
      '<canvas class="ltd-map" width="256" height="256"></canvas><canvas class="ltd-ovl" aria-hidden="true"></canvas></div>' +
      '<div class="ltd-legend"><span class="ltd-lg-lo"></span><span class="ltd-lg-bar"></span><span class="ltd-lg-hi"></span><span class="ltd-lg-none" hidden>no event</span></div>' +
      '<div class="ltd-play">' +
      '<button type="button" class="ltd-btn ltd-sq" data-act="prev" aria-label="Previous year">◀</button>' +
      '<button type="button" class="ltd-btn ltd-playbtn" data-act="play" aria-pressed="false">Play</button>' +
      '<button type="button" class="ltd-btn ltd-sq" data-act="next" aria-label="Next year">▶</button>' +
      '<input class="ltd-year" type="range" min="0" max="' + (NY - 1) + '" step="1" value="' + S.year + '" aria-label="Year">' +
      '<output class="ltd-yearlab"></output></div>' +
      '</div></section>' +
      '<div class="ltd-side">' +
      '<section class="ltd-panel" aria-label="Time series"><h3 class="ltd-title">Time series</h3>' +
      '<div class="ltd-body"><canvas class="ltd-chart ltd-chart-main" role="img" aria-label="NDVI of the pixel under the cursor by year, with the LandTrendr fit"></canvas>' +
      '<p class="ltd-cap">Dots: NDVI of each year. Line with squares: the LandTrendr fit and its vertices. The orange line is the year of the slider; click the chart to move it.</p></div></section>' +
      '<section class="ltd-panel" aria-label="Statistics"><h3 class="ltd-title">Statistics</h3>' +
      '<div class="ltd-body"><div class="ltd-stats"></div>' +
      '<div class="ltd-row"><button type="button" class="ltd-btn" data-act="pin">Pin cursor pixel</button>' +
      '<button type="button" class="ltd-btn" data-act="clear">Clear pins</button></div></div></section>' +
      '</div></div>' +
      '<section class="ltd-panel ltd-tune" aria-label="Tune LandTrendr"><h3 class="ltd-title">Tune LandTrendr</h3>' +
      '<div class="ltd-body ltd-tune-grid">' +
      '<div class="ltd-picks"><p class="ltd-h">Sample pixels <span>(numbered on the map)</span></p><div class="ltd-list" role="radiogroup" aria-label="Sample pixel"></div></div>' +
      '<div class="ltd-tune-main">' +
      '<p class="ltd-desc"></p>' +
      '<canvas class="ltd-chart ltd-chart-tune" role="img" aria-label="Series of the sample pixel with the fit for the chosen parameters"></canvas>' +
      '<div class="ltd-ctrls"></div>' +
      '<p class="ltd-fitinfo" aria-live="polite"></p>' +
      '</div></div></section>' +
      '<p class="ltd-note">Fits computed by Zeit’s LandTrendr (commit ' + esc(meta.zeit.commit) + ', version ' + esc(meta.zeit.version) +
      '), the code Janus runs; nothing is recomputed in your browser. Defaults: ' + meta.landtrendr_parameters.max_segments + ' segments, spike threshold ' +
      meta.landtrendr_parameters.spike_threshold + ', recovery threshold ' + meta.landtrendr_parameters.recovery_threshold + '; the loss events use a minimum magnitude of ' +
      MAG_LO + ' NDVI and a pre-event value of at least ' + meta.event_filter.pre_val_threshold_ndvi + '.</p>' +
      '<p class="ltd-sr" aria-live="polite"></p>' +
      '</div>';

    function $(sel) { return root.querySelector(sel); }
    var stage = $('.ltd-stage'), mapCv = $('.ltd-map'), ovl = $('.ltd-ovl'), chartCv = $('.ltd-chart-main'), tuneCv = $('.ltd-chart-tune');
    var statsEl = $('.ltd-stats'), live = $('.ltd-sr'), slider = $('.ltd-year'), yearLab = $('.ltd-yearlab'), playBtn = $('.ltd-playbtn');
    var lgLo = $('.ltd-lg-lo'), lgHi = $('.ltd-lg-hi'), lgBar = $('.ltd-lg-bar'), lgNone = $('.ltd-lg-none');
    var tuneInfo = $('.ltd-fitinfo'), tuneDesc = $('.ltd-desc');
    var mctx = mapCv.getContext('2d'), img = mctx.createImageData(SZ, SZ), u32 = new Uint32Array(img.data.buffer);

    /* colour tables */
    var NONE = pack(NONE_RGB);
    var lutNdvi = new Uint32Array(256), lutYod = new Uint32Array(NY);
    for (var q = 0; q < 256; q++) lutNdvi[q] = q ? pack(ramp('viridis', q2v(q))) : NONE;
    for (var k = 0; k < NY; k++) lutYod[k] = pack(ramp('plasma', k / (NY - 1)));
    var lutMag = new Uint32Array(256);
    for (var m = 0; m < 256; m++) lutMag[m] = m ? pack(ramp('magma', (m / 255 - MAG_LO) / (maxMag - MAG_LO))) : NONE;

    /* the hash */
    function readHash() {
      if (hashOff) return;
      var p = new URLSearchParams(location.hash.slice(1));
      function xy(s) {
        var a = (s || '').split(',').map(Number);
        return a.length === 2 && a[0] >= 0 && a[0] < SZ && a[1] >= 0 && a[1] < SZ && a[0] % 1 === 0 && a[1] % 1 === 0 ? { x: a[0], y: a[1] } : null;
      }
      var c = xy(p.get('px')); if (c) S.cur = c;
      p.getAll('pin').slice(0, 2).forEach(function (s) { var a = xy(s); if (a) S.pins.push({ x: a.x, y: a.y, slot: S.pins.length }); });
      var y = parseInt(p.get('year'), 10);
      if (y >= Y0 && y < Y0 + NY) S.year = y - Y0;
      if (['ndvi', 'yod', 'mag'].indexOf(p.get('mode')) >= 0) S.mode = p.get('mode');
      var s = parseInt(p.get('s'), 10); if (s >= 1 && s <= pixels.length) S.s = s - 1;
      var ms = parseInt(p.get('ms'), 10); if (gp.max_segments.indexOf(ms) >= 0) S.ms = ms;
      var sp = parseFloat(p.get('sp')); if (gp.spike_threshold.indexOf(sp) >= 0) S.sp = sp;
      var rc = parseFloat(p.get('rc')); if (gp.recovery_threshold.indexOf(rc) >= 0) S.rc = rc;
    }
    function writeHash() {
      if (hashOff) return;
      var p = new URLSearchParams();
      if (S.cur) p.set('px', S.cur.x + ',' + S.cur.y);
      S.pins.forEach(function (q) { p.append('pin', q.x + ',' + q.y); });
      p.set('year', years[S.year]); p.set('mode', S.mode);
      p.set('s', S.s + 1); p.set('ms', S.ms); p.set('sp', f1(S.sp)); p.set('rc', f1(S.rc));
      try { history.replaceState(null, '', '#' + p.toString().replace(/%2C/g, ',')); } catch (e) { /* ignore */ }
    }
    readHash();
    if (!S.cur) S.cur = { x: pixels[0].x, y: pixels[0].y };

    /* scheduling */
    var MAP = 1, OVL = 2, CHART = 4, STATS = 8, TUNE = 16, UI = 32;
    var dirty = 0, raf = 0;
    function inval(f) { dirty |= f; if (!raf) raf = requestAnimationFrame(frame); }
    function frame(t) {
      raf = 0;
      if (S.playing) {
        if (!frame.t) frame.t = t;
        if (t - frame.t > 220) { frame.t = t; S.year = (S.year + 1) % NY; dirty |= MAP | CHART | STATS | TUNE | UI | OVL; }
        raf = requestAnimationFrame(frame);
      } else frame.t = 0;
      var d = dirty; dirty = 0;
      if (d & MAP) drawMap();
      if (d & OVL) drawOverlay();
      if (d & CHART) drawMainChart();
      if (d & STATS) drawStats();
      if (d & TUNE) drawTune();
      if (d & UI) drawUI();
    }

    /* map */
    function drawMap() {
      var i;
      if (S.mode === 'ndvi') {
        var ox = (S.year % COLS) * SZ, oy = (S.year >> 3) * SZ, w = atlas.w, a = atlas.data;
        for (var y = 0; y < SZ; y++) for (var x = 0; x < SZ; x++) u32[y * SZ + x] = lutNdvi[a[(oy + y) * w + ox + x]];
      } else if (S.mode === 'yod') {
        for (i = 0; i < SZ * SZ; i++) u32[i] = yodA[i] < NY ? lutYod[yodA[i]] : NONE;
      } else {
        for (i = 0; i < SZ * SZ; i++) u32[i] = lutMag[magA[i]];
      }
      mctx.putImageData(img, 0, 0);
    }
    function sizeOverlay() {
      var dpr = window.devicePixelRatio || 1, w = Math.round(stage.clientWidth * dpr);
      if (ovl.width !== w) { ovl.width = w; ovl.height = w; }
    }
    function drawOverlay() {
      sizeOverlay();
      var g = ovl.getContext('2d'), k = ovl.width / SZ;
      g.setTransform(1, 0, 0, 1, 0, 0); g.clearRect(0, 0, ovl.width, ovl.height);
      var dpr = window.devicePixelRatio || 1;
      g.font = '500 ' + 10 * dpr + 'px ' + C.mono; g.textAlign = 'center'; g.textBaseline = 'middle';
      pixels.forEach(function (p, i) {
        var cx = (p.x + 0.5) * k, cy = (p.y + 0.5) * k, r = 7 * dpr, sel = i === S.s;
        g.beginPath(); g.arc(cx, cy, r, 0, 6.2832);
        g.fillStyle = sel ? 'rgba(66,150,250,.95)' : 'rgba(15,15,15,.78)'; g.fill();
        g.lineWidth = 1.2 * dpr; g.strokeStyle = sel ? '#CEE3FF' : 'rgba(230,230,230,.85)'; g.stroke();
        g.fillStyle = sel ? '#06101C' : '#E6E6E6'; g.fillText(String(i + 1), cx, cy + 0.5 * dpr);
      });
      function box(x, y, col, w) {
        var s = 9 * dpr;
        g.lineWidth = w * dpr; g.strokeStyle = col;
        g.strokeRect(x * k + k / 2 - s / 2, y * k + k / 2 - s / 2, s, s);
      }
      S.pins.forEach(function (p) {
        var cx = (p.x + 0.5) * k, cy = (p.y + 0.5) * k;
        g.lineWidth = 2 * dpr; g.strokeStyle = '#0F0F0F'; g.beginPath(); g.arc(cx, cy, 6 * dpr, 0, 6.2832); g.stroke();
        g.lineWidth = 2 * dpr; g.strokeStyle = PIN_COL[p.slot]; g.beginPath(); g.arc(cx, cy, 5 * dpr, 0, 6.2832); g.stroke();
        g.fillStyle = PIN_COL[p.slot]; g.beginPath(); g.arc(cx, cy, 1.6 * dpr, 0, 6.2832); g.fill();
      });
      if (S.cur) { box(S.cur.x, S.cur.y, '#000', 3); box(S.cur.x, S.cur.y, '#fff', 1.4); }
    }

    /* charts */
    function drawChart(cv, traces, yi) {
      var dpr = window.devicePixelRatio || 1, w = cv.clientWidth, h = cv.clientHeight;
      if (!w || !h) return;
      if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) { cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr); }
      var g = cv.getContext('2d');
      g.setTransform(dpr, 0, 0, dpr, 0, 0); g.clearRect(0, 0, w, h);
      var L = 38, R = 12, T = 28, B = 26, pw = w - L - R, ph = h - T - B, lo = 0, hi = 1, t, i;
      traces.forEach(function (tr) { for (var i = 0; i < NY; i++) if (tr.series[i] < lo) lo = Math.floor(tr.series[i] * 5) / 5; });
      function X(yr) { return L + (yr - Y0 + 0.5) / NY * pw; }
      function Y(v) { return T + (hi - v) / (hi - lo) * ph; }
      g.fillStyle = '#0B0E13'; g.fillRect(L, T, pw, ph);
      g.font = '11px ' + C.mono; g.fillStyle = C.mute; g.strokeStyle = 'rgba(80,100,140,.28)'; g.lineWidth = 1;
      g.textAlign = 'right'; g.textBaseline = 'middle';
      for (t = Math.ceil(lo * 5 - 1e-6) / 5; t <= hi + 1e-6; t += 0.2) {
        var yy = Math.round(Y(t)) + 0.5;
        g.beginPath(); g.moveTo(L, yy); g.lineTo(L + pw, yy); g.stroke(); g.fillText(t.toFixed(1), L - 6, yy);
      }
      g.textAlign = 'center'; g.textBaseline = 'top';
      var step = pw < 420 ? 10 : 5;
      for (var yr = Y0; yr < Y0 + NY; yr += step) {
        var xx = Math.round(X(yr)) + 0.5;
        g.beginPath(); g.moveTo(xx, T); g.lineTo(xx, T + ph); g.stroke(); g.fillText(String(yr), xx, T + ph + 7);
      }
      g.strokeStyle = C.rule; g.strokeRect(L + 0.5, T + 0.5, pw - 1, ph - 1);
      g.save(); g.beginPath(); g.rect(L, T, pw, ph); g.clip();
      traces.forEach(function (tr) {
        var s = tr.series, pen = false;
        g.strokeStyle = tr.sCol; g.lineWidth = 1; g.beginPath();
        for (i = 0; i < NY; i++) {
          if (isNaN(s[i])) { pen = false; continue; }
          if (pen) g.lineTo(X(years[i]), Y(s[i])); else g.moveTo(X(years[i]), Y(s[i]));
          pen = true;
        }
        g.globalAlpha = 0.55; g.stroke(); g.globalAlpha = 1;
        g.fillStyle = tr.sCol;
        for (i = 0; i < NY; i++) if (!isNaN(s[i])) { g.beginPath(); g.arc(X(years[i]), Y(s[i]), 2.4, 0, 6.2832); g.fill(); }
        var v = tr.fit;
        if (v && v.length) {
          g.strokeStyle = tr.fCol; g.lineWidth = 2.2; g.lineJoin = 'round'; g.beginPath();
          v.forEach(function (p, j) { if (j) g.lineTo(X(p[0]), Y(p[1])); else g.moveTo(X(p[0]), Y(p[1])); });
          g.stroke();
          g.fillStyle = tr.fCol; g.strokeStyle = '#0B0E13'; g.lineWidth = 1;
          v.forEach(function (p) { g.fillRect(X(p[0]) - 3.5, Y(p[1]) - 3.5, 7, 7); g.strokeRect(X(p[0]) - 3.5, Y(p[1]) - 3.5, 7, 7); });
        }
      });
      g.restore();
      var dx = Math.round(X(years[yi])) + 0.5;
      g.strokeStyle = C.date; g.lineWidth = 1.5; g.beginPath(); g.moveTo(dx, T); g.lineTo(dx, T + ph); g.stroke();
      var lab = String(years[yi]), lw = g.measureText(lab).width + 10, lx = Math.min(Math.max(dx - lw / 2, L), L + pw - lw);
      g.fillStyle = C.date; g.fillRect(lx, T + ph + 3, lw, 17);
      g.fillStyle = '#1A1206'; g.textAlign = 'center'; g.textBaseline = 'middle'; g.fillText(lab, lx + lw / 2, T + ph + 12);
      g.textAlign = 'left'; g.textBaseline = 'middle';
      var lx0 = L;
      traces.slice().reverse().forEach(function (tr) {
        if (!tr.name) return;
        g.fillStyle = tr.fCol; g.fillRect(lx0, 9, 8, 8);
        g.fillStyle = C.text; g.fillText(tr.name, lx0 + 13, 14);
        lx0 += 13 + g.measureText(tr.name).width + 16;
      });
    }
    function drawMainChart() {
      var traces = [];
      S.pins.forEach(function (p) {
        traces.push({ series: seriesAt(p.x, p.y), fit: vertsAt(p.x, p.y), sCol: PIN_COL[p.slot], fCol: PIN_COL[p.slot], name: 'Pin ' + (p.slot + 1) + '  ' + p.x + ', ' + p.y });
      });
      if (S.cur) traces.push({ series: seriesAt(S.cur.x, S.cur.y), fit: vertsAt(S.cur.x, S.cur.y), sCol: HOVER_SERIES, fCol: HOVER_FIT, name: 'Cursor  ' + S.cur.x + ', ' + S.cur.y });
      drawChart(chartCv, traces, S.year);
    }
    function tuneFit() { return pixels[S.s].fits[S.ms + '_' + f1(S.sp) + '_' + f1(S.rc)]; }
    function drawTune() {
      var p = pixels[S.s], f = tuneFit();
      drawChart(tuneCv, [{ series: Float32Array.from(p.series), fit: f ? f.vertices : [], sCol: HOVER_SERIES, fCol: HOVER_FIT, name: 'Sample ' + (S.s + 1) + '  ' + p.x + ', ' + p.y }], S.year);
      tuneDesc.textContent = p.label;
      tuneInfo.innerHTML = f ? '<span><b>RMSE</b> ' + f.rmse.toFixed(4) + ' NDVI</span><span><b>Segments</b> ' + (f.vertices.length - 1) +
        '</span><span><b>Vertices</b> ' + f.vertices.map(function (v) { return v[0]; }).join(', ') + '</span>' : 'No fit for these parameters.';
    }

    /* statistics */
    var lastStats = '';
    function drawStats() {
      var cols = [];
      if (S.cur) cols.push({ name: 'Cursor', col: HOVER_FIT, p: info(S.cur.x, S.cur.y) });
      S.pins.forEach(function (q) { cols.push({ name: 'Pin ' + (q.slot + 1), col: PIN_COL[q.slot], p: info(q.x, q.y) }); });
      var rows = [
        ['Pixel x, y', function (p) { return p.x + ', ' + p.y; }],
        ['Longitude', function (p) { return p.lon.toFixed(4); }],
        ['Latitude', function (p) { return p.lat.toFixed(4); }],
        ['NDVI ' + years[S.year], function (p) { return fmt(p.ndvi); }],
        ['Fit ' + years[S.year], function (p) { return fmt(p.fit); }],
        ['Greatest loss', function (p) { return p.loss ? p.loss.from + (p.loss.to ? '–' + p.loss.to : '') : 'none'; }],
        ['Magnitude', function (p) { return p.loss ? p.loss.mag.toFixed(2) + ' NDVI' : 'none'; }],
        ['Vertices', function (p) { return String(p.verts.length); }]
      ];
      var h = '<table><thead><tr><th scope="col">Metric</th>' + cols.map(function (c) {
        return '<th scope="col"><i style="background:' + c.col + '"></i>' + c.name + '</th>';
      }).join('') + '</tr></thead><tbody>' + rows.map(function (r) {
        return '<tr><th scope="row">' + r[0] + '</th>' + cols.map(function (c) { return '<td>' + esc(r[1](c.p)) + '</td>'; }).join('') + '</tr>';
      }).join('') + '</tbody></table>';
      if (h !== lastStats) { statsEl.innerHTML = h; lastStats = h; }
    }

    /* legend, year bar, tabs */
    function drawUI() {
      slider.value = S.year; yearLab.textContent = years[S.year];
      slider.setAttribute('aria-valuetext', String(years[S.year]));
      playBtn.textContent = S.playing ? 'Pause' : 'Play'; playBtn.setAttribute('aria-pressed', S.playing ? 'true' : 'false');
      root.querySelectorAll('[data-mode]').forEach(function (b) { b.setAttribute('aria-pressed', b.getAttribute('data-mode') === S.mode ? 'true' : 'false'); });
      var ramps = { ndvi: 'viridis', yod: 'plasma', mag: 'magma' };
      lgBar.style.background = css(ramps[S.mode]);
      lgNone.hidden = S.mode === 'ndvi';
      lgLo.textContent = S.mode === 'ndvi' ? 'NDVI 0.0' : S.mode === 'yod' ? String(Y0) : MAG_LO.toFixed(2) + ' NDVI';
      lgHi.textContent = S.mode === 'ndvi' ? '1.0' : S.mode === 'yod' ? String(Y0 + NY - 1) : maxMag.toFixed(2);
      stage.setAttribute('data-mode', S.mode);
    }

    /* controls of the tuning panel */
    var list = $('.ltd-list'), ctrls = $('.ltd-ctrls');
    pixels.forEach(function (p, i) {
      var b = document.createElement('button');
      b.type = 'button'; b.className = 'ltd-btn ltd-pick'; b.setAttribute('role', 'radio'); b.setAttribute('data-i', i);
      b.innerHTML = '<b>' + (i + 1) + '</b><span>' + esc(SHORT[p.role] || p.role) + '<small>' + p.x + ', ' + p.y + '</small></span>';
      list.appendChild(b);
    });
    function segGroup(key, label, vals, fmtv) {
      var d = document.createElement('div'); d.className = 'ltd-ctrl';
      d.innerHTML = '<p class="ltd-h" id="ltd-l-' + key + '">' + label + '</p><div class="ltd-seg" role="radiogroup" aria-labelledby="ltd-l-' + key + '"></div><p class="ltd-help">' + HELP[key] + '</p>';
      var seg = d.querySelector('.ltd-seg');
      vals.forEach(function (v) {
        var b = document.createElement('button');
        b.type = 'button'; b.className = 'ltd-btn'; b.setAttribute('role', 'radio'); b.setAttribute('data-k', key); b.setAttribute('data-v', v); b.textContent = fmtv(v);
        seg.appendChild(b);
      });
      ctrls.appendChild(d);
    }
    segGroup('ms', 'Max segments', gp.max_segments, String);
    segGroup('sp', 'Spike threshold', gp.spike_threshold, f1);
    segGroup('rc', 'Recovery threshold', gp.recovery_threshold, f1);
    function syncTuneUI() {
      list.querySelectorAll('button').forEach(function (b) { var on = +b.getAttribute('data-i') === S.s; b.setAttribute('aria-checked', on); b.classList.toggle('on', on); });
      ctrls.querySelectorAll('button').forEach(function (b) {
        var on = +b.getAttribute('data-v') === S[b.getAttribute('data-k')]; b.setAttribute('aria-checked', on); b.classList.toggle('on', on);
      });
    }

    /* interaction */
    function pixelOf(e) {
      var r = mapCv.getBoundingClientRect();
      return { x: Math.min(SZ - 1, Math.max(0, Math.floor((e.clientX - r.left) / r.width * SZ))), y: Math.min(SZ - 1, Math.max(0, Math.floor((e.clientY - r.top) / r.height * SZ))), k: r.width / SZ };
    }
    function setCursor(x, y, say) {
      if (S.cur && S.cur.x === x && S.cur.y === y) return;
      S.cur = { x: x, y: y }; inval(OVL | CHART | STATS);
      if (say) live.textContent = summary(x, y);
    }
    function togglePin(x, y, k) {
      var hit = -1, near = 8 / (k || 2);
      S.pins.forEach(function (p, i) { if (Math.hypot(p.x - x, p.y - y) <= near) hit = i; });
      if (hit >= 0) { S.pins.splice(hit, 1); live.textContent = 'Pin removed.'; }
      else {
        var slot;
        if (S.pins.length >= 2) slot = S.pins.shift().slot; else slot = S.pins.some(function (p) { return p.slot === 0; }) ? 1 : 0;
        S.pins.push({ x: x, y: y, slot: slot });
        live.textContent = 'Pin ' + (slot + 1) + ' set. ' + summary(x, y);
      }
      writeHash(); inval(OVL | CHART | STATS);
    }
    stage.addEventListener('pointermove', function (e) { var p = pixelOf(e); setCursor(p.x, p.y, false); });
    stage.addEventListener('pointerdown', function (e) { if (e.pointerType === 'touch') { var p = pixelOf(e); setCursor(p.x, p.y, false); } });
    stage.addEventListener('click', function (e) { var p = pixelOf(e); setCursor(p.x, p.y, false); togglePin(p.x, p.y, p.k); });
    stage.addEventListener('keydown', function (e) {
      var d = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
      if (d) {
        e.preventDefault();
        var n = e.shiftKey ? 10 : 1, c = S.cur || { x: 128, y: 128 };
        setCursor(Math.min(SZ - 1, Math.max(0, c.x + d[0] * n)), Math.min(SZ - 1, Math.max(0, c.y + d[1] * n)), true);
      } else if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault(); if (S.cur) togglePin(S.cur.x, S.cur.y, 1000);
      }
    });
    function setYear(i, commit) {
      S.year = (i + NY) % NY; inval(MAP | CHART | STATS | TUNE | UI);
      if (commit) writeHash();
    }
    slider.addEventListener('input', function () { setYear(+slider.value, false); });
    slider.addEventListener('change', function () { writeHash(); });
    [chartCv, tuneCv].forEach(function (cv) {
      var down = false;
      function go(e) { var r = cv.getBoundingClientRect(), f = (e.clientX - r.left - 38) / (r.width - 50) * NY - 0.5; setYear(Math.min(NY - 1, Math.max(0, Math.round(f))), false); }
      cv.addEventListener('pointerdown', function (e) { down = true; cv.setPointerCapture(e.pointerId); go(e); });
      cv.addEventListener('pointermove', function (e) { if (down) go(e); });
      cv.addEventListener('pointerup', function () { down = false; writeHash(); });
      cv.addEventListener('pointercancel', function () { down = false; });
    });
    root.addEventListener('click', function (e) {
      var b = e.target.closest('button'); if (!b || !root.contains(b)) return;
      var act = b.getAttribute('data-act'), mode = b.getAttribute('data-mode'), k = b.getAttribute('data-k');
      if (mode) { S.mode = mode; writeHash(); inval(MAP | UI); }
      else if (act === 'prev') setYear(S.year - 1, true);
      else if (act === 'next') setYear(S.year + 1, true);
      else if (act === 'play') { S.playing = !S.playing; inval(UI); if (!S.playing) writeHash(); }
      else if (act === 'pin') { if (S.cur) togglePin(S.cur.x, S.cur.y, 1000); }
      else if (act === 'clear') { S.pins = []; writeHash(); inval(OVL | CHART | STATS); }
      else if (k) { S[k] = k === 'ms' ? parseInt(b.getAttribute('data-v'), 10) : parseFloat(b.getAttribute('data-v')); syncTuneUI(); writeHash(); inval(TUNE); }
      else if (b.classList.contains('ltd-pick')) { S.s = +b.getAttribute('data-i'); syncTuneUI(); writeHash(); inval(OVL | TUNE); }
    });
    if (window.ResizeObserver) {
      new ResizeObserver(function () { inval(OVL | CHART | TUNE); }).observe(root);
    } else window.addEventListener('resize', function () { inval(OVL | CHART | TUNE); });
    if (document.fonts && document.fonts.ready) document.fonts.ready.then(function () { inval(CHART | TUNE | OVL); });

    syncTuneUI();
    inval(MAP | OVL | CHART | STATS | TUNE | UI);

    /* a small handle for checks: the vertices and the series the widget draws */
    root.ltDemo = { state: S, vertices: vertsAt, series: seriesAt, info: info };
  }

  var all = document.querySelectorAll('.lt-demo');
  window.LTDemo = { roots: all };
  for (var i = 0; i < all.length; i++) {
    if (all[i].hasAttribute('data-lazy') && 'IntersectionObserver' in window) lazy(all[i]);
    else init(all[i]);
  }
  function lazy(root) {
    var io = new IntersectionObserver(function (es) {
      if (!es.some(function (e) { return e.isIntersecting; })) return;
      io.disconnect();
      init(root);
    }, { rootMargin: '600px 0px' });
    io.observe(root);
  }
})();

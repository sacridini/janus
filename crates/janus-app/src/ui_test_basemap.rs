//! `--selftest-ui` stages 50-56 (src/app_selftest.cpp, selfTestBasemap), with A and B open and B active: a local
//! tile pyramid over B (each zoom 13 tile in a pattern of 32-pixel cells whose colour says where it is, one tile
//! left out) as a custom XYZ basemap (file://); its tiles read; the basemap alone where PROJ puts it (the coarser
//! level where the tile is missing), under the layers, the opacity of a layer over it and of the basemap; a map panel
//! and the swipe with the basemap alone; the attribution in the PNG and the view GeoTIFF; the basemap None again.

use gdal::Metadata;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_geo::basemap::{Basemap, BasemapSource};

use super::{MAP_SLOT, SIZE, UiTest, VIEW_SLOT};
use crate::app::State;
use crate::exports::ExportState;

const W: usize = 400;
const H: usize = 300;
const KZ: i32 = 13;
const E: f64 = 20037508.342789244;

fn tile_span() -> f64 {
    2.0 * E / f64::from(1 << KZ)
}

/// 32 pixels of zoom 13
fn cell() -> f64 {
    tile_span() / 8.0
}

/// The pattern's colour at Web Mercator (x, y), zoom z.
fn colour(x: f64, y: f64, z: i32) -> [u8; 3] {
    let a = ((x + E) / cell()).floor() as i64;
    let b = ((E - y) / cell()).floor() as i64;
    [(40 + 25 * a.rem_euclid(8)) as u8, (40 + 25 * b.rem_euclid(8)) as u8, (60 + 12 * z) as u8]
}

fn near(a: &[u8], b: &[u8], tol: i32) -> bool {
    (0..3).all(|k| (i32::from(a[k]) - i32::from(b[k])).abs() <= tol)
}

fn test_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("janus-selftest-basemap")
}

impl State {
    /// The test's view: B fitted to 400 x 300, zoomed 1.5x about the centre.
    fn basemap_view(&mut self) {
        self.map.canvas = [W as f32, H as f32];
        self.test_fit();
        self.map.scale *= 1.5;
        self.map.touched = true;
    }

    /// Canvas point (cx, cy) of the test's view -> Web Mercator, exactly (PROJ).
    fn merc(&self, ct: &CoordTransform, cx: f64, cy: f64) -> Option<(f64, f64)> {
        let g = self.grid()?;
        // as MapView::image_at, in f64
        let ox = self.map.center[0] + (cx - W as f64 / 2.0) / self.map.scale;
        let oy = self.map.center[1] + (cy - H as f64 / 2.0) / self.map.scale;
        let (x, y) = self.session.as_ref()?.info.pixel_to_geo(ox * g.fx, oy * g.fy)?;
        let (mut xs, mut ys) = ([x], [y]);
        ct.transform_coords(&mut xs, &mut ys, &mut []).ok()?;
        Some((xs[0], ys[0]))
    }

    /// The zoom 13 tile left out: under canvas point (100, 150) of the view.
    fn missing_tile(&mut self, ct: &CoordTransform) -> Option<(i64, i64)> {
        self.basemap_view();
        let (x, y) = self.merc(ct, 100.5, 150.5)?;
        Some((((x + E) / tile_span()).floor() as i64, ((E - y) / tile_span()).floor() as i64))
    }

    /// The test's view as the main map's target draws it (and every tile read on the GPU first).
    fn basemap_render(&mut self) -> Option<Vec<u8>> {
        if let (Some(b), Some(gpu)) = (self.basemap.b.as_mut(), self.gpu.as_ref()) {
            b.upload_ready(100000, |rgba, side| gpu.create_image(side, side, rgba));
        }
        self.basemap_view();
        self.test_render()
    }

    /// The test's view's rectangle in B's source pixels and its target pixels per source pixel (as render_map).
    fn basemap_view_rect(&self) -> Option<([f64; 4], f64)> {
        let g = self.grid()?;
        let m = &self.map;
        let (x0, y0) = (m.center[0] - W as f64 / 2.0 / m.scale, m.center[1] - H as f64 / 2.0 / m.scale);
        let (x1, y1) = (m.center[0] + W as f64 / 2.0 / m.scale, m.center[1] + H as f64 / 2.0 / m.scale);
        Some(([x0 * g.fx, y0 * g.fy, x1 * g.fx, y1 * g.fy], m.scale / g.fx))
    }

    pub(super) fn test_basemap(&mut self, st: &mut UiTest, t0: std::time::Instant, ctx: &egui::Context) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
            st.frames = 0;
        };
        let dir = test_dir();
        let info = self.session.as_ref()?.info.clone();
        let mut sa = SpatialRef::from_wkt(&info.crs_wkt).ok()?;
        let mut sm = SpatialRef::from_epsg(3857).ok()?;
        sa.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        sm.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let Ok(ct) = CoordTransform::new(&sa, &sm) else { return fail("no transformation from B's CRS to EPSG:3857") };
        let (a, b) = (0usize, 1usize);
        match st.stage {
            50 => {
                let opened = Basemap::<()>::datasets_opened();
                println!(
                    "    basemap None so far: Basemap {}, {opened} tile datasets opened",
                    if self.basemap.b.is_some() { "made" } else { "not made" }
                );
                if self.basemap.b.is_some() || opened > 0 {
                    return fail("with the basemap None nothing may be made or opened");
                }
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::create_dir_all(&dir);
                let mut url = dir.to_string_lossy().replace('\\', "/");
                if !url.starts_with('/') {
                    url.insert(0, '/');
                }
                let url = format!("file://{url}/{{z}}/{{x}}/{{y}}.png");
                {
                    // a layer without a CRS: not placed, with the reason
                    let mut c = (*info).clone();
                    c.crs_wkt.clear();
                    c.id = u64::MAX;
                    let src = BasemapSource {
                        id: "custom".into(),
                        name: "Custom XYZ".into(),
                        url: url.clone(),
                        max_zoom: KZ,
                        tile_size: 256,
                        attribution: String::new(),
                        terms: String::new(),
                        connections: 2,
                        cache_days: 0,
                    };
                    let mut bm = Basemap::<()>::new(&src, &dir, std::sync::Arc::new(|| {}));
                    match bm.set_map(&c, |_, _, _| ()) {
                        Err(why) if why.contains("no CRS") => println!("    a layer without a CRS: \"{why}\""),
                        r => return fail(&format!("a layer without a CRS: {r:?}")),
                    }
                }
                // the pyramid over B and what the map can show around it
                let (mx, my) = self.missing_tile(&ct)?;
                let m = 1.6 * info.width.max(info.height) as f64;
                let mut bx = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
                for py in [-m, info.height as f64 + m] {
                    for px in [-m, info.width as f64 + m] {
                        let (x, y) = info.pixel_to_geo(px, py)?;
                        let (mut xs, mut ys) = ([x], [y]);
                        ct.transform_coords(&mut xs, &mut ys, &mut []).ok()?;
                        bx = [bx[0].min(xs[0]), bx[1].min(ys[0]), bx[2].max(xs[0]), bx[3].max(ys[0])];
                    }
                }
                let t = std::time::Instant::now();
                let mut tiles = 0;
                let mut px = vec![0u8; 256 * 256 * 3];
                for z in 0..=KZ {
                    let s = 2.0 * E / f64::from(1 << z);
                    let res = s / 256.0;
                    let (x0, x1) = (((bx[0] + E) / s).floor() as i64, ((bx[2] + E) / s).floor() as i64);
                    let (y0, y1) = (((E - bx[3]) / s).floor() as i64, ((E - bx[1]) / s).floor() as i64);
                    for ty in y0..=y1 {
                        for tx in x0..=x1 {
                            if z == KZ && tx == mx && ty == my {
                                continue;
                            }
                            for j in 0..256 {
                                for i in 0..256 {
                                    let c = colour(
                                        -E + ((tx * 256 + i) as f64 + 0.5) * res,
                                        E - ((ty * 256 + j) as f64 + 0.5) * res,
                                        z,
                                    );
                                    px[(j as usize * 256 + i as usize) * 3..][..3].copy_from_slice(&c);
                                }
                            }
                            let f = dir.join(z.to_string()).join(tx.to_string()).join(format!("{ty}.png"));
                            let _ = std::fs::create_dir_all(f.parent()?);
                            if image::RgbImage::from_raw(256, 256, px.clone())
                                .map(|i| i.save(&f))
                                .is_none_or(|r| r.is_err())
                            {
                                return fail(&format!("could not write the tile {}", f.display()));
                            }
                            tiles += 1;
                        }
                    }
                }
                println!(
                    "    {tiles} tiles (zoom 0-{KZ}) written in {:.0} ms; zoom {KZ} tile {mx}/{my} left out",
                    t.elapsed().as_secs_f64() * 1000.0
                );
                let ui = &mut self.basemap.ui;
                ui.source = "custom".into();
                ui.url = url;
                ui.max_zoom = KZ;
                ui.tile_size = 256;
                ui.attribution = "Janus self-test tiles".into();
                ui.opacity = 1.0;
                ui.on = true; // as when ticked
                next(st, "local tile pyramid picked as a custom XYZ basemap (file://)");
            }
            51 => {
                if !self.basemap.b.as_ref().is_some_and(|b| b.has_map()) {
                    if !self.basemap.why.is_empty() {
                        return fail(&format!("the basemap is not drawn: {}", self.basemap.why));
                    }
                    return None;
                }
                self.basemap_view();
                let (v, scale) = self.basemap_view_rect()?;
                let bm = self.basemap.b.as_mut()?;
                let z = bm.update(&v, scale);
                if st.frames == 0
                    && let Some(r) = bm.map()
                {
                    println!(
                        "    warp grid to Web Mercator: {} x {} nodes, error {:.3} px, made in {:.1} ms",
                        r.grid_w, r.grid_h, r.grid_error, r.build_ms
                    );
                }
                st.frames += 1;
                if bm.inflight() > 0 {
                    return None;
                }
                println!(
                    "    zoom {z:?}: {} tiles read in {:.0} ms (first after {:.0} ms), {:.1} ms each",
                    bm.loads(),
                    st.since?.elapsed().as_secs_f64() * 1000.0,
                    bm.first_tile_ms().unwrap_or(0.0),
                    bm.avg_load_ms()
                );
                if z != Some(KZ) {
                    return fail("the view should ask for zoom 13");
                }
                if bm.offline() || !bm.error().is_empty() {
                    return fail(&format!("reading the tiles failed: {}", bm.error()));
                }
                next(st, "tiles of the view read (file://)");
            }
            52 => {
                let (mx, my) = self.missing_tile(&ct)?;
                self.layers[a].visible = false;
                self.layers[b].visible = false;
                let base = self.basemap_render()?;
                self.basemap.ui.on = false;
                let empty = self.basemap_render()?;
                // every 5th pixel, away from cell and tile edges (bilinear sampling mixes neighbours there): the
                // colour of PROJ's cell
                let (mut checked, mut bad, mut fallback, mut worst) = (0, 0, 0, 0);
                for py in (2..H - 2).step_by(5) {
                    for px in (2..W - 2).step_by(5) {
                        let Some((x, y)) = self.merc(&ct, px as f64 + 0.5, py as f64 + 0.5) else { continue };
                        let (c0, r0) = (((x + E) / cell()).floor() as i64, ((E - y) / cell()).floor() as i64);
                        // where the zoom 13 tile is missing, the coarse preview (zoom 10, 8x larger pixels) shows
                        // through
                        let gap = ((x + E) / tile_span()).floor() as i64 == mx
                            && ((E - y) / tile_span()).floor() as i64 == my;
                        let edge = if gap { 6.0 } else { 0.8 }; // screen pixels from a cell's edge (> half a texel)
                        let interior =
                            [(-edge, 0.0), (edge, 0.0), (0.0, -edge), (0.0, edge)].iter().all(|&(dx, dy)| {
                                self.merc(&ct, px as f64 + 0.5 + dx, py as f64 + 0.5 + dy).is_some_and(|(x2, y2)| {
                                    ((x2 + E) / cell()).floor() as i64 == c0 && ((E - y2) / cell()).floor() as i64 == r0
                                })
                            });
                        if !interior {
                            continue; // cells are inside tiles: also away from tile edges
                        }
                        let want = colour(x, y, if gap { KZ - 3 } else { KZ });
                        let got = &base[(py * W + px) * 4..][..3];
                        let d = (0..3).map(|k| (i32::from(got[k]) - i32::from(want[k])).abs()).max().unwrap_or(0);
                        worst = worst.max(d);
                        checked += 1;
                        fallback += usize::from(gap);
                        if d > 2 {
                            bad += 1;
                            if bad <= 3 {
                                println!("    pixel ({px}, {py}): rgb{got:?}, PROJ's cell rgb{want:?}");
                            }
                        }
                    }
                }
                println!(
                    "    basemap alone: {checked} map pixels on PROJ's cell colour ({fallback} from zoom 10 where the zoom 13 tile is\n    missing), {bad} differ, largest difference {worst}"
                );
                if bad > 0 || checked < 1500 || fallback < 50 {
                    return fail("the basemap should land where PROJ puts it");
                }
                // under the layers: they cover it where they have data
                self.layers[a].visible = true;
                self.layers[b].visible = true;
                let none = self.basemap_render()?;
                self.basemap.ui.on = true;
                let both = self.basemap_render()?;
                let (mut covered, mut wrong) = (0, 0);
                for i in 0..W * H {
                    let layer = !near(&none[i * 4..], &empty[i * 4..], 0);
                    covered += usize::from(layer);
                    wrong += usize::from(!near(&both[i * 4..], if layer { &none[i * 4..] } else { &base[i * 4..] }, 0));
                }
                println!("    under the layers: {covered} of {} pixels show a layer, {wrong} differ", W * H);
                if wrong > 0 || covered < W * H / 4 {
                    return fail("the layers should be drawn over the basemap");
                }
                // B at half opacity (A hidden): half B, half the basemap
                self.layers[a].visible = false;
                self.basemap.ui.on = false;
                let b_only = self.basemap_render()?;
                self.basemap.ui.on = true;
                self.layers[b].opacity = 0.5;
                let half = self.basemap_render()?;
                self.layers[b].opacity = 1.0;
                let (mut in_b, mut wrong) = (0, 0);
                for i in 0..W * H {
                    if near(&b_only[i * 4..], &empty[i * 4..], 0) {
                        continue;
                    }
                    in_b += 1;
                    let want: Vec<u8> = (0..3)
                        .map(|k| (0.5 * f64::from(b_only[i * 4 + k]) + 0.5 * f64::from(base[i * 4 + k])).round() as u8)
                        .collect();
                    wrong += usize::from(!near(&half[i * 4..], &want, 2));
                }
                // the basemap at half opacity, alone: half way to the background
                self.layers[b].visible = false;
                self.basemap.ui.opacity = 0.5;
                let faded = self.basemap_render()?;
                self.basemap.ui.opacity = 1.0;
                let mut wrong_fade = 0;
                for i in 0..W * H {
                    let want: Vec<u8> = (0..3)
                        .map(|k| (0.5 * f64::from(base[i * 4 + k]) + 0.5 * f64::from(empty[i * 4 + k])).round() as u8)
                        .collect();
                    wrong_fade += usize::from(!near(&faded[i * 4..], &want, 2));
                }
                self.layers[a].visible = true;
                self.layers[b].visible = true;
                println!(
                    "    B at opacity 0.5 over it: {wrong} of {in_b} pixels off; basemap at opacity 0.5: {wrong_fade} off"
                );
                if wrong > 0 || in_b < W * H / 4 {
                    return fail("a layer's opacity should show the basemap through");
                }
                if wrong_fade > 0 {
                    return fail("the basemap's opacity should fade it towards the background");
                }
                // costs: the per-frame request of a view whose tiles are all there, and a render
                let (v, scale) = self.basemap_view_rect()?;
                let t = std::time::Instant::now();
                for _ in 0..200 {
                    self.basemap.b.as_mut()?.update(&v, scale);
                }
                let update_us = t.elapsed().as_secs_f64() / 200.0 * 1e6;
                let render_ms = |s: &mut State, on: bool| {
                    s.basemap.ui.on = on;
                    let _ = s.basemap_render();
                    let t = std::time::Instant::now();
                    for _ in 0..20 {
                        let _ = s.test_render();
                    }
                    s.basemap.ui.on = true;
                    t.elapsed().as_secs_f64() / 20.0 * 1000.0
                };
                let off = render_ms(self, false);
                let on = render_ms(self, true);
                println!(
                    "    per frame: request of the view {update_us:.1} us; render + readback {off:.3} ms without, {on:.3} ms with it"
                );
                next(st, "basemap on PROJ's place, under the layers, opacity of both");
            }
            53 => {
                self.layers[a].visible = false;
                self.layers[b].visible = false;
                let base = self.basemap_render()?;
                self.layers[a].visible = true;
                self.layers[b].visible = true;
                let cube = self.session.as_ref()?.info.id;
                let panel = |s: &mut State, only: bool| -> Option<Vec<u8>> {
                    let mut v = crate::views::ViewSetup::new(VIEW_SLOT, cube);
                    v.basemap_only = only;
                    s.render_view(&mut v, SIZE, 1.0, VIEW_SLOT as i32);
                    let img = s.gpu.as_ref()?.read_target(VIEW_SLOT as i32)?.2;
                    s.gpu.as_mut()?.release_target(VIEW_SLOT as i32);
                    Some(img)
                };
                if panel(self, true)? != base {
                    return fail("a map panel with the basemap only should draw the basemap alone");
                }
                let of_b = panel(self, false)?;
                self.layers[a].visible = false;
                let b_only = self.basemap_render()?;
                self.layers[a].visible = true;
                if of_b != b_only {
                    return fail("a map panel of B should draw B over the basemap, as the main map");
                }
                let had = self.swipe.is_some();
                if !had {
                    self.toggle_swipe();
                }
                let mut v = self.swipe.as_ref()?.view.clone();
                v.basemap_only = true;
                self.render_view(&mut v, SIZE, 1.0, crate::views::SWIPE_SLOT);
                let img = self.gpu.as_ref()?.read_target(crate::views::SWIPE_SLOT)?.2;
                if !had {
                    self.toggle_swipe();
                }
                if img != base {
                    return fail("the swipe's comparison should be able to show the basemap alone");
                }
                next(st, "map panel and swipe: the basemap alone, a layer over it");
            }
            54 => {
                // the exports at the screen's pixels per point, the map 400 x 300 points
                let ppp = ctx.pixels_per_point();
                self.basemap_render()?;
                let (pw, ph) = ((W as f32 * ppp).round() as u32, (H as f32 * ppp).round() as u32);
                self.render_map(MAP_SLOT, [pw, ph], [W as f32, H as f32], ppp);
                let both = self.gpu.as_ref()?.read_target(MAP_SLOT)?.2;
                let out = dir.join("export");
                let _ = std::fs::create_dir_all(&out);
                let o = crate::export_map::PngOptions { label: false, legend: false, ..Default::default() };
                let png = out.join("map.png").to_string_lossy().into_owned();
                let tif = out.join("view.tif").to_string_lossy().into_owned();
                let done = |j: Option<std::sync::Arc<crate::exports::ExportJob>>| {
                    j.is_some_and(|j| {
                        j.wait();
                        j.state() == ExportState::Done
                    })
                };
                let jp = self.export_png(png.clone(), o, ctx);
                let jv = self.export_view(tif.clone(), 1, ctx);
                if !done(jp) || !done(jv) {
                    return fail("the exports failed");
                }
                self.exports.clear();
                self.show_exports = false;
                let Ok(ds) = gdal::Dataset::open(&png) else { return fail("the PNG cannot be opened") };
                let (w, h) = ds.raster_size();
                if (w, h) != (pw as usize, ph as usize) {
                    return fail("the PNG should be the map's size");
                }
                let bands: Vec<Vec<u8>> = (1..=3)
                    .map(|k| {
                        ds.rasterband(k)
                            .and_then(|r| r.read_as::<u8>((0, 0), (w, h), (w, h), None))
                            .map(|b| b.into_shape_and_vec().1)
                            .unwrap_or_default()
                    })
                    .collect();
                drop(ds);
                let (mut away, mut credit) = (0, 0);
                let (cx, cy) = (w as f32 - 200.0 * ppp, h as f32 - 30.0 * ppp);
                for y in 0..h {
                    for x in 0..w {
                        let i = y * w + x;
                        let same = (0..3).all(|k| bands[k][i] == both[i * 4 + k]);
                        if (x as f32) < cx || (y as f32) < cy {
                            away += usize::from(!same);
                        } else {
                            credit += usize::from(!same);
                        }
                    }
                }
                let tag = gdal::Dataset::open(&tif)
                    .ok()
                    .and_then(|d| d.metadata_item("TIFFTAG_COPYRIGHT", ""))
                    .unwrap_or_default();
                println!(
                    "    PNG: {credit} pixels of the attribution box bottom right, {away} differ elsewhere; view GeoTIFF:\n    copyright \"{tag}\""
                );
                if away > 0 || credit < 100 {
                    return fail("the PNG should be the map with the attribution bottom right");
                }
                if !tag.contains("Janus self-test tiles") {
                    return fail("the view GeoTIFF should carry the attribution");
                }
                next(st, "attribution in the PNG and the view GeoTIFF");
            }
            55 => {
                self.basemap.ui.source = "none".into(); // the next frame retires the Basemap
                next(st, "basemap None again");
            }
            56 => {
                if st.frames < 2 {
                    st.frames += 1;
                    return None;
                }
                let img = self.basemap_render()?;
                self.basemap.ui.on = false;
                let none = self.basemap_render()?;
                self.basemap.ui.on = true;
                if self.basemap.b.is_some() || self.basemap.shown() {
                    return fail("with the basemap None the Basemap should be gone");
                }
                if img != none {
                    return fail("with the basemap None nothing should be drawn under the layers");
                }
                self.basemap.ui.on = false;
                // its tiles' readers closed on another thread: the files go once it is done
                std::thread::sleep(std::time::Duration::from_millis(200));
                let _ = std::fs::remove_dir_all(&dir);
                self.map.touched = false;
                self.map.fit();
                next(st, "basemap None: nothing made, nothing drawn");
                st.stage = 57; // the full-resolution cache, then 6
            }
            _ => {}
        }
        None
    }
}

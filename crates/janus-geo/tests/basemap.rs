//! The basemap from a tile pyramid written to a temporary folder and read through GDAL's WMS driver as a file://
//! XYZ source (no network), as the C++'s self-test (stages 50-52, app_selftest.cpp): Web Mercator cells of 32
//! pixels of the finest level, red and green by cell, blue by zoom level, one tile of the finest level left out.
//! The tiles of a 400 × 300 view on a UTM layer are read, then drawn on the CPU as the display shader draws them
//! (each tile's quad, the warp grid, the tile's uv) and every 5th map pixel, away from cell edges, checked against
//! PROJ: the layer's pixel -> its CRS -> EPSG:3857 -> the cell's colour (the coarse preview where the tile is
//! missing).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gdal::DriverManager;
use gdal::raster::Buffer;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_cube::CubeInfo;
use janus_geo::basemap::{Basemap, BasemapSource};

const E: f64 = 20037508.342789244;
const KZ: i32 = 13;
const W: f64 = 400.0;
const H: f64 = 300.0;

fn colour(x: f64, y: f64, z: i32, cell: f64) -> [u8; 3] {
    let a = ((x + E) / cell).floor() as i64;
    let b = ((E - y) / cell).floor() as i64;
    [(40 + 25 * a.rem_euclid(8)) as u8, (40 + 25 * b.rem_euclid(8)) as u8, (60 + 12 * z) as u8]
}

#[test]
fn local_pyramid_lands_where_proj_puts_it() {
    gdal::DriverManager::register_all();
    let tile_span = 2.0 * E / f64::from(1 << KZ);
    let cell = tile_span / 8.0; // 32 pixels of zoom 13
    let dir = std::env::temp_dir().join("janus-geo-test-basemap");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // a 400 × 300 UTM 22S layer of 30 m pixels near São Paulo
    let info = CubeInfo {
        id: 7,
        width: 400,
        height: 300,
        geo_transform: Some([330000.0, 30.0, 0.0, 7395000.0, 0.0, -30.0]),
        crs_wkt: SpatialRef::from_epsg(32722).unwrap().to_wkt().unwrap(),
        crs_authority: "EPSG:32722".into(),
        ..Default::default()
    };
    let g = info.geo_transform.unwrap();
    let mut sa = SpatialRef::from_epsg(32722).unwrap();
    let mut sm = SpatialRef::from_epsg(3857).unwrap();
    sa.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    sm.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = CoordTransform::new(&sa, &sm).unwrap();
    let merc_of = |px: f64, py: f64| -> (f64, f64) {
        let (mut x, mut y, mut z) = ([g[0] + px * g[1]], [g[3] + py * g[5]], [0.0]);
        ct.transform_coords(&mut x, &mut y, &mut z).unwrap();
        (x[0], y[0])
    };

    // the view: the layer fitted to 400 × 300, zoomed 1.5× about the centre
    let scale = (W / info.width as f64).min(H / info.height as f64) * 1.5;
    let (cx, cy) = (info.width as f64 / 2.0, info.height as f64 / 2.0);
    let view = [cx - W / 2.0 / scale, cy - H / 2.0 / scale, cx + W / 2.0 / scale, cy + H / 2.0 / scale];
    // a view point to a layer pixel, and to Web Mercator exactly
    let map_at = |sx: f64, sy: f64| (view[0] + sx / scale, view[1] + sy / scale);
    let merc = |sx: f64, sy: f64| {
        let (mx, my) = map_at(sx, sy);
        merc_of(mx, my)
    };

    // the zoom 13 tile left out: under view point (100, 150)
    let (x, y) = merc(100.5, 150.5);
    let (missing_x, missing_y) = (((x + E) / tile_span).floor() as i32, ((E - y) / tile_span).floor() as i32);

    // the pyramid over the layer and what the map can show around it
    let m = 1.6 * info.width.max(info.height) as f64;
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for py in [-m, info.height as f64 + m] {
        for px in [-m, info.width as f64 + m] {
            let (x, y) = merc_of(px, py);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
    }
    let mem = DriverManager::get_driver_by_name("MEM").unwrap();
    let png = DriverManager::get_driver_by_name("PNG").unwrap();
    let mut tiles = 0;
    for z in 0..=KZ {
        let s = 2.0 * E / f64::from(1 << z);
        let res = s / 256.0;
        let (x0, x1) = (((b[0] + E) / s).floor() as i32, ((b[2] + E) / s).floor() as i32);
        let (y0, y1) = (((E - b[3]) / s).floor() as i32, ((E - b[1]) / s).floor() as i32);
        for ty in y0..=y1 {
            for tx in x0..=x1 {
                if z == KZ && tx == missing_x && ty == missing_y {
                    continue;
                }
                let ds = mem.create_with_band_type::<u8, _>("", 256, 256, 3).unwrap();
                for band in 0..3 {
                    let mut px = vec![0u8; 256 * 256];
                    for j in 0..256 {
                        for i in 0..256 {
                            let c = colour(
                                -E + (f64::from(tx) * 256.0 + i as f64 + 0.5) * res,
                                E - (f64::from(ty) * 256.0 + j as f64 + 0.5) * res,
                                z,
                                cell,
                            );
                            px[j * 256 + i] = c[band];
                        }
                    }
                    let mut buf = Buffer::new((256, 256), px);
                    ds.rasterband(band + 1).unwrap().write((0, 0), (256, 256), &mut buf).unwrap();
                }
                let f = dir.join(z.to_string()).join(tx.to_string()).join(format!("{ty}.png"));
                std::fs::create_dir_all(f.parent().unwrap()).unwrap();
                ds.create_copy(&png, &f, &Default::default()).unwrap();
                tiles += 1;
            }
        }
    }
    println!("{tiles} tiles (zoom 0-{KZ}); zoom {KZ} tile {missing_x}/{missing_y} left out");

    // nothing opened so far; a layer without a CRS is not placed, with the reason
    assert_eq!(Basemap::<Vec<u8>>::datasets_opened(), 0);
    let url =
        format!("file:///{}/{{z}}/{{x}}/{{y}}.png", dir.to_string_lossy().replace('\\', "/").trim_start_matches('/'));
    let src = BasemapSource {
        id: "custom".into(),
        name: "test".into(),
        url,
        max_zoom: KZ,
        tile_size: 256,
        attribution: "Janus test tiles".into(),
        terms: String::new(),
        connections: 4,
        cache_days: 1,
    };
    let mut no_crs = info.clone();
    no_crs.crs_wkt.clear();
    let mut bm: Basemap<Vec<u8>> = Basemap::new(&src, &dir.join("cache"), Arc::new(|| {}));
    let why = bm.set_map(&no_crs, |_, _, _| Vec::new()).unwrap_err();
    assert!(why.contains("no CRS"), "{why}");

    // the tiles of the view
    bm.set_map(&info, |_, _, _| Vec::new()).unwrap();
    let r = bm.map().unwrap();
    println!("warp grid to Web Mercator: {} x {} nodes, error {:.3} px", r.grid_w, r.grid_h, r.grid_error);
    let start = Instant::now();
    let mut z = None;
    loop {
        bm.tick();
        z = bm.update(&view, scale).or(z);
        bm.upload_ready(64, |rgba, _| rgba.to_vec());
        if bm.loads() > 0 && bm.inflight() == 0 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "tiles not read: {}", bm.error());
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("zoom {z:?}: {} tiles read, {:.1} ms each", bm.loads(), bm.avg_load_ms());
    assert_eq!(z, Some(KZ));
    assert!(!bm.offline() && bm.error().is_empty(), "{}", bm.error());

    // the tiles drawn as the shader draws them: the last tile covering a point wins (coarse to fine)
    let mut drawn: Vec<(Vec<u8>, janus_geo::Placement)> = Vec::new();
    bm.for_each_visible(&view, scale, |tex, _, p| drawn.push((tex.clone(), *p)));
    let r = bm.map().unwrap();
    let d = r.domain;
    let draw = |mx: f64, my: f64| -> Option<[u8; 3]> {
        let mut out = None;
        for (tex, p) in &drawn {
            let q = p.quad;
            if mx < q[0] || mx >= q[2] || my < q[1] || my >= q[3] {
                continue;
            }
            let vuv = ((mx - q[0]) / (q[2] - q[0]), (my - q[1]) / (q[3] - q[1]));
            let w = (
                f64::from(p.warp_quad[0]) + vuv.0 * f64::from(p.warp_quad[2]),
                f64::from(p.warp_quad[1]) + vuv.1 * f64::from(p.warp_quad[3]),
            );
            let Some((lu, lv)) = r.grid_uv(d[0] + w.0 * (d[2] - d[0]), d[1] + w.1 * (d[3] - d[1])) else { continue };
            let u = (lu - f64::from(p.warp_src[0])) / f64::from(p.warp_src[2]);
            let v = (lv - f64::from(p.warp_src[1])) / f64::from(p.warp_src[3]);
            if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                continue;
            }
            let (i, j) = ((u * 256.0) as usize, (v * 256.0) as usize);
            let k = (j * 256 + i) * 4;
            // blended by its alpha: a tile the source does not have is transparent (the coarser one shows)
            if tex[k + 3] > 0 {
                out = Some([tex[k], tex[k + 1], tex[k + 2]]);
            }
        }
        out
    };

    let (mut checked, mut bad, mut fallback, mut worst) = (0, 0, 0, 0);
    for py in (2..H as i32 - 2).step_by(5) {
        for px in (2..W as i32 - 2).step_by(5) {
            let (sx, sy) = (f64::from(px) + 0.5, f64::from(py) + 0.5);
            let (x, y) = merc(sx, sy);
            let (c0, r0) = (((x + E) / cell).floor() as i64, ((E - y) / cell).floor() as i64);
            // where the zoom 13 tile is missing, the coarse preview (zoom 10, 8× larger pixels) shows through
            let gap =
                ((x + E) / tile_span).floor() as i32 == missing_x && ((E - y) / tile_span).floor() as i32 == missing_y;
            let edge = if gap { 6.0 } else { 0.8 }; // view pixels from a cell's edge (> half a texel)
            let interior = [(-edge, 0.0), (edge, 0.0), (0.0, -edge), (0.0, edge)].iter().all(|&(dx, dy)| {
                let (x2, y2) = merc(sx + dx, sy + dy);
                ((x2 + E) / cell).floor() as i64 == c0 && ((E - y2) / cell).floor() as i64 == r0
            });
            if !interior {
                continue;
            }
            let want = colour(x, y, if gap { KZ - 3 } else { KZ }, cell);
            let (mx, my) = map_at(sx, sy);
            let got = draw(mx, my).unwrap_or([0, 0, 0]);
            let diff = (0..3).map(|c| (i32::from(got[c]) - i32::from(want[c])).abs()).max().unwrap();
            worst = worst.max(diff);
            checked += 1;
            fallback += usize::from(gap);
            if diff > 2 {
                bad += 1;
                if bad <= 3 {
                    println!("pixel ({px}, {py}): {got:?}, PROJ's cell {want:?}");
                }
            }
        }
    }
    println!("{checked} map pixels on PROJ's cell colour ({fallback} from zoom 10), {bad} differ, largest {worst}");
    assert!(bad == 0 && checked >= 1500 && fallback >= 50, "{checked} {fallback} {bad}");
    assert!(Basemap::<Vec<u8>>::datasets_opened() > 0);
    drop(bm);
    let _ = std::fs::remove_dir_all(&dir);
}

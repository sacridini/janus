//! Composites of a series (janus_zeit::composite) on local files: the best available pixel block by block equal to
//! zeit_core::bap on the whole image (the clouds of the neighbouring blocks seen through the margin), only the
//! winners' bands read; the greenest, the medoid and the median against their definitions.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gdal::DriverManager;
use gdal::Metadata;
use gdal::raster::Buffer;
use janus_cube::{BandSelection, QaRule, open_cube};
use janus_zeit::composite::{CompOptions, Method, composite};
use zeit_core::bap;

const W: usize = 70;
const H: usize = 53;
const T: usize = 7;
const DAYS: [&str; T] = ["20230602", "20230618", "20230705", "20230721", "20230810", "20230826", "20230915"];

/// A pseudo-random generator (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Each date: red, NIR and an SCL band (4 vegetation, 9 cloud, 3 shadow, 0 no data, 2 dark), 10 m in UTM.
struct Series {
    files: Vec<String>,
    red: Vec<Vec<f32>>,
    nir: Vec<Vec<f32>>,
    scl: Vec<Vec<f32>>,
}

fn series(dir: &Path) -> Series {
    std::fs::create_dir_all(dir).unwrap();
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
    let (mut files, mut red, mut nir, mut scl) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (t, day) in DAYS.iter().enumerate() {
        let mut q = vec![4f32; W * H];
        for _ in 0..4 {
            let (cy, cx, r) = (rng.below(H as u64) as i64, rng.below(W as u64) as i64, 2 + rng.below(6) as i64);
            let code = if rng.below(3) == 0 { 3.0 } else { 9.0 };
            for y in 0..H as i64 {
                for x in 0..W as i64 {
                    if (y - cy).pow(2) + (x - cx).pow(2) <= r * r {
                        q[y as usize * W + x as usize] = code;
                    }
                }
            }
        }
        if t == 1 {
            // outside the scene: a corner without data
            for y in 0..15 {
                for x in 0..20 {
                    q[y * W + x] = 0.0;
                }
            }
        }
        if t == 3 {
            q[5 * W..6 * W].fill(2.0);
        }
        let r: Vec<f32> = (0..W * H).map(|_| 0.02 + rng.below(1000) as f32 * 1e-4).collect();
        let mut n: Vec<f32> = (0..W * H).map(|_| 0.2 + rng.below(3000) as f32 * 1e-4).collect();
        if t == 4 {
            // clear by its quality but without a NIR value: a scene's edge
            for y in 30..40 {
                for x in 40..W {
                    n[y * W + x] = f32::NAN;
                }
            }
        }
        let f = dir.join(format!("s_{day}.tif"));
        let mut ds = drv.create_with_band_type::<f32, _>(&f, W, H, 3).unwrap();
        ds.set_geo_transform(&[500000.0, 10.0, 0.0, 8_900_000.0, 0.0, -10.0]).unwrap();
        ds.set_projection(&gdal::spatial_ref::SpatialRef::from_epsg(32720).unwrap().to_wkt().unwrap()).unwrap();
        for (b, (v, name)) in [(&r, "Red"), (&n, "NIR"), (&q, "SCL")].into_iter().enumerate() {
            let mut rb = ds.rasterband(b + 1).unwrap();
            rb.set_no_data_value(Some(f64::NAN)).unwrap();
            rb.set_description(name).unwrap();
            let mut buf = Buffer::new((W, H), v.clone());
            rb.write((0, 0), (W, H), &mut buf).unwrap();
        }
        files.push(f.to_string_lossy().into_owned());
        red.push(r);
        nir.push(n);
        scl.push(q);
    }
    Series { files, red, nir, scl }
}

fn read_all(f: &str) -> Vec<Vec<f32>> {
    let ds = gdal::Dataset::open(f).unwrap();
    (1..=ds.raster_count())
        .map(|b| ds.rasterband(b).unwrap().read_as::<f32>((0, 0), (W, H), (W, H), None).unwrap().data().to_vec())
        .collect()
}

fn same(a: f32, b: f32) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

fn options(method: Method) -> CompOptions {
    CompOptions {
        method,
        bands: vec![1, 2],
        names: vec!["Red".into(), "NIR".into()],
        qa_band: 3,
        qa_rule: QaRule::Scl,
        red: 1,
        nir: 2,
        // 10 m pixels: 1 to 6 pixels, a margin of 6
        bap: bap::Options {
            target_doy: 200.0,
            doy_sigma: 25.0,
            cloud_min: 10.0,
            cloud_max: 60.0,
            ..Default::default()
        },
        sensor: vec![1.0, 1.0, 0.5, 1.0, 1.0, 1.0, 1.0],
        cover: (0..T).map(|t| 1.0 - 0.1 * t as f64).collect(),
        info: true,
        block: 16,
        threads: 3,
        stem: "c".into(),
        ..Default::default()
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("janus-composite-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn best_available_pixel_block_by_block_as_on_the_whole_image() {
    let dir = tmp("bap");
    let s = series(&dir.join("in"));
    let info = Arc::new(open_cube(&s.files, &BandSelection::default()).unwrap());
    assert_eq!(info.t(), T);
    let opts = options(Method::Bap);
    let out = composite(&info, &opts, &dir.join("out"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(out.files.len(), 1);
    assert_eq!(out.failed, 0);
    let got = read_all(&out.files[0]);
    let inf = read_all(&out.info_files[0]);

    // the reference: the whole image at once, a winner without a value left out and the pixel chosen again
    let mut states = vec![bap::NO_DATA; T * W * H];
    for t in 0..T {
        for i in 0..W * H {
            let q = s.scl[t][i] as u32;
            states[t * W * H + i] = if QaRule::Scl.usable(q) {
                bap::CLEAR
            } else if QaRule::Scl.is_cloud(q) {
                bap::CLOUD
            } else {
                bap::NO_DATA
            };
        }
    }
    let doy = [153.0, 169.0, 186.0, 202.0, 222.0, 238.0, 258.0];
    let dates: Vec<bap::Date> =
        (0..T).map(|t| bap::Date { doy: doy[t], sensor: opts.sensor[t], cover: opts.cover[t] }).collect();
    let px = bap::Options { cloud_min: 1.0, cloud_max: 6.0, ..opts.bap.clone() };
    let mut date = vec![-1i32; W * H];
    let mut todo = vec![true; W * H];
    for _ in 0..3 {
        let b = bap::best(&states, H, W, &dates, &px, 1).unwrap();
        let mut again = vec![false; W * H];
        for i in 0..W * H {
            if !todo[i] {
                continue;
            }
            date[i] = b.date[i];
            if date[i] >= 0 && s.nir[date[i] as usize][i].is_nan() {
                states[date[i] as usize * W * H + i] = bap::NO_DATA;
                again[i] = true;
            }
        }
        todo = again;
    }
    let mut changed = 0;
    for i in 0..W * H {
        let t = date[i];
        let (r, n) = if t >= 0 { (s.red[t as usize][i], s.nir[t as usize][i]) } else { (f32::NAN, f32::NAN) };
        assert!(
            same(got[0][i], r) && same(got[1][i], n),
            "pixel {i}: {:?} vs {:?} (date {t})",
            (got[0][i], got[1][i]),
            (r, n)
        );
        if t >= 0 && !n.is_nan() {
            assert_eq!(inf[1][i], doy[t as usize] as f32, "pixel {i}");
            assert_eq!(inf[0][i], 2023.0);
        }
        changed += usize::from(t == 4 && (30..40).contains(&(i / W)) && i % W >= 40);
    }
    // the NIR's hole never chosen
    assert_eq!(changed, 0);
    // the quality of every date over each block and its margin (6 pixels), then the bands of the winners only, each
    // date around its winners: fewer pixels than every date's bands, and every pixel's winner read
    let mut qa = 0;
    for by in (0..H).step_by(16) {
        for bx in (0..W).step_by(16) {
            let (x0, y0, x1, y1) = (bx.saturating_sub(6), by.saturating_sub(6), (bx + 22).min(W), (by + 22).min(H));
            qa += (x1 - x0) * (y1 - y0);
        }
    }
    assert_eq!(out.full_reads, T * 3 * W * H);
    let bands = out.reads - T * qa;
    let chosen = date.iter().filter(|&&t| t >= 0).count();
    assert!(bands < T * 2 * W * H / 2 && bands >= 2 * chosen, "{bands} band pixels read, {chosen} chosen");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[allow(clippy::needless_range_loop)] // a pixel of every date and band at once
fn greenest_medoid_median() {
    let dir = tmp("others");
    let s = series(&dir.join("in"));
    let info = Arc::new(open_cube(&s.files, &BandSelection::default()).unwrap());
    let clear = |t: usize, i: usize| QaRule::Scl.usable(s.scl[t][i] as u32);
    for method in [Method::Greenest, Method::Medoid, Method::Median] {
        let out =
            composite(&info, &options(method), &dir.join(method.id()), &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let got = read_all(&out.files[0]);
        for i in 0..W * H {
            let obs: Vec<usize> =
                (0..T).filter(|&t| clear(t, i) && !s.red[t][i].is_nan() && !s.nir[t][i].is_nan()).collect();
            let want: (f32, f32) = match method {
                Method::Greenest => {
                    let mut best: Option<(f32, usize)> = None;
                    for &t in &obs {
                        let v = (s.nir[t][i] - s.red[t][i]) / (s.nir[t][i] + s.red[t][i]);
                        if best.is_none_or(|b| v > b.0) {
                            best = Some((v, t));
                        }
                    }
                    best.map_or((f32::NAN, f32::NAN), |(_, t)| (s.red[t][i], s.nir[t][i]))
                }
                Method::Median => {
                    let med = |v: &mut Vec<f32>| -> f32 {
                        if v.is_empty() {
                            return f32::NAN;
                        }
                        v.sort_by(f32::total_cmp);
                        let k = v.len() / 2;
                        if v.len() % 2 == 1 { v[k] } else { ((f64::from(v[k - 1]) + f64::from(v[k])) / 2.0) as f32 }
                    };
                    let mut r: Vec<f32> = (0..T).filter(|&t| clear(t, i)).map(|t| s.red[t][i]).collect();
                    let mut n: Vec<f32> =
                        (0..T).filter(|&t| clear(t, i) && !s.nir[t][i].is_nan()).map(|t| s.nir[t][i]).collect();
                    (med(&mut r), med(&mut n))
                }
                _ => {
                    if obs.is_empty() {
                        (f32::NAN, f32::NAN)
                    } else {
                        // the medoid is one of the clear observations
                        let k = obs.iter().find(|&&t| s.red[t][i] == got[0][i] && s.nir[t][i] == got[1][i]);
                        assert!(k.is_some(), "pixel {i}: the medoid is no observation");
                        (got[0][i], got[1][i])
                    }
                }
            };
            assert!(
                same(got[0][i], want.0) && same(got[1][i], want.1),
                "{method:?} pixel {i}: {:?} vs {want:?}",
                (got[0][i], got[1][i])
            );
        }
        if method == Method::Greenest {
            // red, NIR and the quality of every date, their winners' kept: what every date's bands are here
            assert_eq!(out.reads, out.full_reads);
        } else {
            assert_eq!(out.reads, out.full_reads);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_composite_a_year() {
    let dir = tmp("years");
    let s = series(&dir.join("in"));
    // the last two dates a year later
    let mut files = s.files.clone();
    for f in files.iter_mut().skip(5) {
        let g = f.replace("s_2023", "s_2024");
        std::fs::rename(&*f, &g).unwrap();
        *f = g;
    }
    let info = Arc::new(open_cube(&files, &BandSelection::default()).unwrap());
    let opts = CompOptions { step: Some(janus_zeit::regularize::Step::Year), ..options(Method::Bap) };
    let out = composite(&info, &opts, &dir.join("out"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(out.files.len(), 2);
    assert!(out.files[0].ends_with("c_20230101.tif") && out.files[1].ends_with("c_20240101.tif"), "{:?}", out.files);
    let y2024 = read_all(&out.info_files[1]);
    assert!(y2024[0].iter().filter(|v| !v.is_nan()).all(|&y| y == 2024.0));
    let _ = std::fs::remove_dir_all(&dir);
}

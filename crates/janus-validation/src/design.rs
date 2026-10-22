//! The sample's size, its allocation to the strata and the errors it should give; the stratified random sample
//! (src/validation.cpp: sampleSize, allocate, expectedErrors, drawSample).

use std::collections::{HashMap, HashSet};

use crate::estimate::z_of;
use crate::reader::MapReader;
use crate::strata::{Strata, Stratum};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Alloc {
    #[default]
    Proportional,
    Equal,
    Neyman,
    Manual,
}

impl Alloc {
    pub fn name(self) -> &'static str {
        match self {
            Alloc::Proportional => "proportional",
            Alloc::Equal => "equal",
            Alloc::Neyman => "neyman",
            Alloc::Manual => "manual",
        }
    }

    pub fn from_name(s: &str) -> Alloc {
        match s {
            "equal" => Alloc::Equal,
            "neyman" => Alloc::Neyman,
            "manual" => Alloc::Manual,
            _ => Alloc::Proportional,
        }
    }
}

/// The sample size for a standard error of the overall accuracy (eq. 13).
pub fn sample_size(s: &[Stratum], target_se: f64) -> i32 {
    let (mut ws, mut ws2, mut n) = (0.0, 0.0, 0.0);
    for t in s {
        if t.pixels <= 0 {
            continue;
        }
        let sd = (t.expected_ua * (1.0 - t.expected_ua)).sqrt();
        ws += t.weight * sd;
        ws2 += t.weight * sd * sd;
        n += t.pixels as f64;
    }
    if target_se <= 0.0 || n <= 0.0 {
        return 0;
    }
    (ws * ws / (target_se * target_se + ws2 / n)).ceil() as i32
}

/// Largest remainders (as zeit's _round: ties to the first stratum).
fn round_to(v: &[f64], total: i64) -> Vec<i64> {
    let mut f: Vec<i64> = v.iter().map(|x| x.floor() as i64).collect();
    let mut extra = total - f.iter().sum::<i64>();
    if extra > 0 {
        let mut order: Vec<usize> = (0..v.len()).collect();
        // a stable sort, the largest remainders first
        order.sort_by(|&a, &b| (v[b] - f[b] as f64).total_cmp(&(v[a] - f[a] as f64)));
        for &k in &order {
            if extra <= 0 {
                break;
            }
            f[k] += 1;
            extra -= 1;
        }
    }
    f
}

/// n points by area share (proportional), equally or by area x expected standard deviation (Neyman), largest
/// remainders, at least `min_per` per stratum (taken from the strata above it); never more than a stratum's
/// pixels. Manual: the strata's own n.
pub fn allocate(s: &[Stratum], n: i32, a: Alloc, min_per: i32) -> Vec<i32> {
    let ns = s.len();
    let mut out = vec![0; ns];
    if a == Alloc::Manual {
        for i in 0..ns {
            out[i] = i64::from(s[i].n).clamp(0, s[i].pixels.max(0)) as i32;
        }
        return out;
    }
    let mut share: Vec<f64> = s
        .iter()
        .map(|t| {
            let sd = (t.expected_ua * (1.0 - t.expected_ua)).sqrt();
            if t.pixels <= 0 {
                0.0
            } else {
                match a {
                    Alloc::Equal => 1.0,
                    Alloc::Neyman => t.weight * sd,
                    _ => t.weight,
                }
            }
        })
        .collect();
    let sum: f64 = share.iter().sum();
    if sum <= 0.0 || n <= 0 {
        return out;
    }
    for v in &mut share {
        *v /= sum;
    }
    let scaled = |total: i64, sh: &[f64]| round_to(&sh.iter().map(|x| total as f64 * x).collect::<Vec<_>>(), total);
    let mut counts = scaled(i64::from(n), &share);
    if min_per > 0 {
        let min_per = i64::from(min_per);
        let mut low: Vec<bool> = counts.iter().map(|&c| c < min_per).collect();
        for _ in 0..ns {
            if !low.iter().any(|&l| l) {
                break;
            }
            let n_low = low.iter().filter(|&&l| l).count() as i64;
            let rest = i64::from(n) - min_per * n_low;
            if rest <= 0 {
                let all = n_low == ns as i64;
                for i in 0..ns {
                    if low[i] || all {
                        counts[i] = min_per;
                    }
                }
                break;
            }
            let mut free: Vec<f64> = (0..ns).map(|i| if low[i] { 0.0 } else { share[i] }).collect();
            let fs: f64 = free.iter().sum();
            if fs > 0.0 {
                for v in &mut free {
                    *v /= fs;
                }
                counts = scaled(rest, &free);
            } else {
                counts.fill(0);
            }
            for i in 0..ns {
                if low[i] {
                    counts[i] = min_per;
                }
                low[i] = low[i] || counts[i] < min_per;
            }
        }
    }
    for i in 0..ns {
        out[i] = counts[i].min(s[i].pixels) as i32;
    }
    out
}

/// The errors a design should give if the strata's accuracies are as expected: the overall accuracy's and each
/// user's accuracy's standard error, and the confidence interval of the area of change (the errors of a stratum
/// without change assumed to be change, as a binary map: Olofsson et al. 2014, sec. 5.1).
#[derive(Clone, Debug)]
pub struct Expected {
    pub overall_se: f64,
    pub user_se: Vec<f64>,
    pub change: bool,
    pub change_area: f64,
    pub change_ci: f64,
}

pub fn expected_errors(s: &[Stratum], total_area: f64, confidence: f64) -> Expected {
    let mut e = Expected {
        overall_se: f64::NAN,
        user_se: Vec::new(),
        change: false,
        change_area: f64::NAN,
        change_ci: f64::NAN,
    };
    let z = z_of(confidence);
    let (mut v, mut vc, mut pc) = (0.0, 0.0, 0.0);
    let mut ok = true;
    for t in s {
        let u = t.expected_ua;
        e.user_se.push(if t.n > 1 { (u * (1.0 - u) / f64::from(t.n - 1)).sqrt() } else { f64::NAN });
        e.change |= t.change;
        if t.weight <= 0.0 {
            continue;
        }
        if t.n <= 1 {
            ok = false;
            continue;
        }
        let d = f64::from(t.n - 1);
        v += t.weight * t.weight * u * (1.0 - u) / d;
        let pch = if t.change { u } else { 1.0 - u }; // a stratum's errors taken as change (or as no change)
        pc += t.weight * pch;
        vc += t.weight * t.weight * pch * (1.0 - pch) / d;
    }
    if ok {
        e.overall_se = v.sqrt();
        if e.change {
            e.change_area = pc * total_area;
            e.change_ci = z * vc.sqrt() * total_area;
        }
    }
    e
}

/// SplitMix64 and an unbiased bounded draw: the same numbers on every platform (and as the C++).
pub(crate) struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// [0, bound)
    pub fn below(&mut self, bound: u64) -> u64 {
        let threshold = 0u64.wrapping_sub(bound) % bound;
        loop {
            let r = self.next();
            if r >= threshold {
                return r % bound;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// 1.. (point i has id i + 1)
    pub id: i32,
    /// code
    pub stratum: i32,
    /// pixel of the strata raster
    pub row: i32,
    pub col: i32,
    /// its centre, in the grid's CRS
    pub x: f64,
    pub y: f64,
}

/// n[i] pixels of stratum i (strata order) drawn at random without replacement, none of `existing` (a sample made
/// larger keeps its points); ids follow theirs. The same strata, seed and existing points give the same points on
/// every platform, and the C++'s. Needs s.row_counts.
pub fn draw_sample(
    s: &Strata,
    strata_path: &str,
    n: &[i32],
    seed: u64,
    existing: &[Point],
) -> Result<Vec<Point>, String> {
    if s.row_counts.len() != s.strata.len() {
        return Err("the strata were not counted".into());
    }
    let (h, w) = (s.grid.h, s.grid.w);
    // the rows of the strata raster read so far
    struct Rows<'a> {
        rd: MapReader,
        rows: HashMap<usize, Vec<f32>>,
        path: &'a str,
    }
    impl Rows<'_> {
        fn get(&mut self, y: usize) -> Result<&[f32], String> {
            if !self.rows.contains_key(&y) {
                let mut v = vec![0f32; self.rd.w];
                self.rd.rows(y, 1, &mut v).map_err(|e| format!("{}: read failed: {e}", self.path))?;
                self.rows.insert(y, v);
            }
            Ok(&self.rows[&y])
        }
    }
    let mut rows = Rows { rd: MapReader::open(strata_path, 1, None, 1)?, rows: HashMap::new(), path: strata_path };
    struct Pick {
        stratum: usize,
        row: usize,
        within: i64,
    }
    let mut picks: Vec<Pick> = Vec::new();
    for i in 0..s.strata.len().min(n.len()) {
        if n[i] <= 0 {
            continue;
        }
        let code = s.strata[i].code;
        let per = &s.row_counts[i];
        let mut ends = vec![0i64; h];
        let mut acc = 0i64;
        for y in 0..h {
            acc += i64::from(per[y]);
            ends[y] = acc;
        }
        // the ranks (among the stratum's pixels, in row order) of the points it already has
        let mut taken: Vec<i64> = Vec::new();
        for p in existing {
            if p.stratum != code || p.row < 0 || p.row as usize >= h {
                continue;
            }
            let r = rows.get(p.row as usize)?;
            let k = r.iter().take((p.col.max(0) as usize).min(w)).filter(|&&v| v == code as f32).count() as i64;
            taken.push(if p.row > 0 { ends[p.row as usize - 1] } else { 0 } + k);
        }
        taken.sort_unstable();
        taken.dedup();
        let free = acc - taken.len() as i64;
        let k = i64::from(n[i]).min(free.max(0));
        if k <= 0 {
            continue;
        }
        let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ ((code as u64) << 32) ^ existing.len() as u64);
        // Floyd's algorithm: k distinct of [0, free)
        let mut chosen: HashSet<i64> = HashSet::with_capacity(k as usize * 2);
        for j in free - k..free {
            let t = rng.below((j + 1) as u64) as i64;
            if !chosen.insert(t) {
                chosen.insert(j);
            }
        }
        let mut idx: Vec<i64> = chosen.into_iter().collect();
        idx.sort_unstable();
        let mut e = 0;
        for c in idx {
            let mut rank = c + e as i64; // the c-th free rank: skip the taken ones below it
            while e < taken.len() && taken[e] <= rank {
                e += 1;
                rank += 1;
            }
            let y = ends.partition_point(|&v| v <= rank);
            picks.push(Pick { stratum: i, row: y, within: rank - (ends[y] - i64::from(per[y])) });
        }
    }
    let last = existing.iter().map(|p| p.id).max().unwrap_or(0).max(0);
    picks.sort_by_key(|p| (p.stratum, p.row, p.within));
    let mut out = Vec::with_capacity(picks.len());
    for (k, id) in picks.iter().zip(last + 1..) {
        let r = rows.get(k.row)?;
        let code = s.strata[k.stratum].code as f32;
        let col = r
            .iter()
            .enumerate()
            .filter(|&(_, &v)| v == code)
            .nth(k.within as usize)
            .map(|(x, _)| x)
            .ok_or_else(|| format!("{strata_path}: the strata raster changed since it was counted"))?;
        let (x, y) = s.grid.centre(col as i64, k.row as i64);
        out.push(Point { id, stratum: s.strata[k.stratum].code, row: k.row as i32, col: col as i32, x, y });
    }
    Ok(out)
}

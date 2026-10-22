//! Stehman's (2014) estimators of a stratified random sample, the confusion matrix, accuracies and
//! error-adjusted areas, the agreement of dates and of interpreters (src/validation.cpp: zOf, Estimator, assess,
//! dateAgreement, cohen).

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Est {
    pub est: f64,
    pub se: f64,
    pub lo: f64,
    pub hi: f64,
}

impl Default for Est {
    fn default() -> Self {
        Est { est: f64::NAN, se: f64::NAN, lo: f64::NAN, hi: f64::NAN }
    }
}

/// Two-sided normal quantile (1.96 at 0.95): Acklam's inverse normal, then a Newton step on erfc.
#[allow(clippy::excessive_precision)] // Acklam's coefficients as published
pub fn z_of(confidence: f64) -> f64 {
    let p = 0.5 + confidence / 2.0;
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [7.784695709041462e-03, 3.224671290700398e-01, 2.445134137142996e+00, 3.754408661907416e+00];
    if !(p > 0.0 && p < 1.0) {
        return f64::NAN;
    }
    let x = if p < 0.02425 {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - 0.02425 {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    let e = 0.5 * libm::erfc(-x / 2f64.sqrt()) - p;
    let u = e * (2.0 * std::f64::consts::PI).sqrt() * (x * x / 2.0).exp();
    x - u / (1.0 + x * u / 2.0)
}

/// Stehman's (2014) estimators of a stratified random sample: means of point values and ratios of them, weighted
/// by the area of each stratum. Points without a stratum are left out; strata without points are left out and
/// the weights of the others scaled up (`covered`: the share of the area they hold).
#[derive(Clone, Debug)]
pub struct Estimator {
    pub z: f64,
    pub covered: f64,
    /// strata (with area) without a point
    pub missing: Vec<usize>,
    members: Vec<Vec<usize>>,
    weight: Vec<f64>,
}

impl Estimator {
    pub fn new(stratum_area: &[f64], stratum_of_point: &[Option<usize>], confidence: f64) -> Estimator {
        let total: f64 = stratum_area.iter().map(|a| a.max(0.0)).sum();
        let mut m: Vec<Vec<usize>> = vec![Vec::new(); stratum_area.len()];
        for (i, s) in stratum_of_point.iter().enumerate() {
            if let Some(s) = *s
                && s < m.len()
            {
                m[s].push(i);
            }
        }
        let mut e = Estimator {
            z: z_of(confidence),
            covered: 0.0,
            missing: Vec::new(),
            members: Vec::new(),
            weight: Vec::new(),
        };
        for (h, mh) in m.into_iter().enumerate() {
            let w = if total > 0.0 { stratum_area[h] / total } else { 0.0 };
            if w <= 0.0 {
                continue;
            }
            if mh.is_empty() {
                e.missing.push(h);
                continue;
            }
            e.members.push(mh);
            e.weight.push(w);
            e.covered += w;
        }
        if e.covered > 0.0 && e.covered < 0.999999 {
            for w in &mut e.weight {
                *w /= e.covered;
            }
        }
        e
    }

    fn interval(&self, est: f64, se: f64) -> Est {
        Est { est, se, lo: est - self.z * se, hi: est + self.z * se }
    }

    pub fn mean(&self, y: &[f64]) -> Est {
        if self.members.is_empty() {
            return Est::default();
        }
        let (mut est, mut var) = (0.0, 0.0);
        for (g, m) in self.members.iter().enumerate() {
            let s: f64 = m.iter().map(|&i| y[i]).sum();
            let mu = s / m.len() as f64;
            let ss: f64 = m.iter().map(|&i| (y[i] - mu) * (y[i] - mu)).sum();
            est += self.weight[g] * mu;
            if m.len() > 1 {
                var += self.weight[g] * self.weight[g] * (ss / (m.len() - 1) as f64) / m.len() as f64;
            }
        }
        self.interval(est, var.sqrt())
    }

    pub fn ratio(&self, y: &[f64], x: &[f64]) -> Est {
        if self.members.is_empty() {
            return Est::default();
        }
        let ng = self.members.len();
        let (mut my, mut mx) = (vec![0.0; ng], vec![0.0; ng]);
        let (mut yb, mut xb) = (0.0, 0.0);
        for (g, m) in self.members.iter().enumerate() {
            let (mut sy, mut sx) = (0.0, 0.0);
            for &i in m {
                sy += y[i];
                sx += x[i];
            }
            my[g] = sy / m.len() as f64;
            mx[g] = sx / m.len() as f64;
            yb += self.weight[g] * my[g];
            xb += self.weight[g] * mx[g];
        }
        if xb == 0.0 {
            return Est::default();
        }
        let r = yb / xb;
        let mut var = 0.0;
        for (g, m) in self.members.iter().enumerate() {
            if m.len() < 2 {
                continue;
            }
            let (mut vy, mut vx, mut cxy) = (0.0, 0.0, 0.0);
            for &i in m {
                vy += (y[i] - my[g]) * (y[i] - my[g]);
                vx += (x[i] - mx[g]) * (x[i] - mx[g]);
                cxy += (y[i] - my[g]) * (x[i] - mx[g]);
            }
            let d = (m.len() - 1) as f64;
            var += self.weight[g] * self.weight[g] * (vy / d + r * r * vx / d - 2.0 * r * cxy / d) / m.len() as f64;
        }
        self.interval(r, (var / (xb * xb)).max(0.0).sqrt())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClassArea {
    pub mapped: f64,
    pub est: f64,
    pub se: f64,
    pub ci: f64,
    pub lo: f64,
    pub hi: f64,
}

impl Default for ClassArea {
    fn default() -> Self {
        ClassArea { mapped: 0.0, est: f64::NAN, se: f64::NAN, ci: f64::NAN, lo: f64::NAN, hi: f64::NAN }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Assessment {
    pub classes: Vec<String>,
    /// [map][reference]: proportions of area
    pub confusion: Vec<Vec<f64>>,
    /// [map][reference]: points
    pub counts: Vec<Vec<i32>>,
    /// [map][reference]: the points
    pub cells: Vec<Vec<Vec<usize>>>,
    pub overall: Est,
    pub users: Vec<Est>,
    pub producers: Vec<Est>,
    pub area: Vec<ClassArea>,
    pub total_area: f64,
    /// points used
    pub n: i32,
    pub covered: f64,
    /// strata without a point
    pub missing: Vec<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct AssessInput {
    /// per stratum
    pub stratum_area: Vec<f64>,
    /// per point: stratum index (None: none)
    pub stratum: Vec<Option<usize>>,
    /// per point: class index (None: none, not labelled, left out)
    pub map: Vec<Option<usize>>,
    pub reference: Vec<Option<usize>>,
    pub classes: Vec<String>,
    /// per class: the area the map gives it
    pub mapped: Vec<f64>,
    pub confidence: f64,
}

pub fn assess(inp: &AssessInput) -> Assessment {
    let nc = inp.classes.len();
    let np = inp.stratum.len();
    let mut a = Assessment { classes: inp.classes.clone(), covered: 1.0, ..Default::default() };
    let mut st: Vec<Option<usize>> = vec![None; np];
    // (map, reference) of each point used
    let mut mr: Vec<(usize, usize)> = vec![(usize::MAX, usize::MAX); np];
    for i in 0..np {
        if let (Some(s), Some(m), Some(r)) = (inp.stratum[i], inp.map[i], inp.reference[i])
            && m < nc
            && r < nc
        {
            st[i] = Some(s);
            mr[i] = (m, r);
            a.n += 1;
        }
    }
    let e = Estimator::new(&inp.stratum_area, &st, inp.confidence);
    a.covered = e.covered;
    a.missing = e.missing.clone();
    a.total_area = inp.stratum_area.iter().map(|v| v.max(0.0)).sum();
    a.confusion = vec![vec![0.0; nc]; nc];
    a.counts = vec![vec![0; nc]; nc];
    a.cells = vec![vec![Vec::new(); nc]; nc];
    for i in 0..np {
        if st[i].is_some() {
            let (m, r) = mr[i];
            a.counts[m][r] += 1;
            a.cells[m][r].push(i);
        }
    }
    if a.n == 0 {
        a.users = vec![Est::default(); nc];
        a.producers = vec![Est::default(); nc];
        a.area = vec![ClassArea::default(); nc];
        for c in 0..nc.min(inp.mapped.len()) {
            a.area[c].mapped = inp.mapped[c];
        }
        return a;
    }
    let (mut y, mut x) = (vec![0.0; np], vec![0.0; np]);
    let fill = |v: &mut Vec<f64>, f: &dyn Fn(usize, usize) -> bool| {
        for i in 0..np {
            v[i] = if st[i].is_some() && f(mr[i].0, mr[i].1) { 1.0 } else { 0.0 };
        }
    };
    for i in 0..nc {
        for j in 0..nc {
            if a.counts[i][j] == 0 {
                continue;
            }
            fill(&mut y, &|m, r| m == i && r == j);
            a.confusion[i][j] = e.mean(&y).est;
        }
    }
    fill(&mut y, &|m, r| m == r);
    a.overall = e.mean(&y);
    for c in 0..nc {
        fill(&mut y, &|m, r| m == c && r == c);
        fill(&mut x, &|m, _| m == c);
        a.users.push(e.ratio(&y, &x));
        fill(&mut x, &|_, r| r == c);
        a.producers.push(e.ratio(&y, &x));
        let p = e.mean(&x);
        let mut ar = ClassArea { mapped: inp.mapped.get(c).copied().unwrap_or(0.0), ..Default::default() };
        ar.est = p.est * a.total_area;
        ar.se = p.se * a.total_area;
        ar.ci = e.z * ar.se;
        ar.lo = ar.est - ar.ci;
        ar.hi = ar.est + ar.ci;
        a.area.push(ar);
    }
    a
}

/// Of the points both the map and the reference call change, with both years: the share whose years are at most
/// `tolerance` apart and the mean difference.
#[derive(Clone, Debug, Default)]
pub struct DateAgreement {
    pub tolerance: f64,
    pub within: Est,
    pub mean_diff: Est,
    pub n: i32,
    /// map year - reference year -> points
    pub diffs: BTreeMap<i32, i32>,
}

pub fn date_agreement(e: &Estimator, map_year: &[i32], ref_year: &[i32], tolerance: f64) -> DateAgreement {
    let mut d = DateAgreement { tolerance, ..Default::default() };
    let n = map_year.len();
    let (mut both, mut within, mut diff) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    for i in 0..n {
        if map_year[i] <= 0 || ref_year[i] <= 0 {
            continue;
        }
        both[i] = 1.0;
        let dd = map_year[i] - ref_year[i];
        diff[i] = f64::from(dd.abs());
        within[i] = if f64::from(dd.abs()) <= tolerance { 1.0 } else { 0.0 };
        d.n += 1;
        *d.diffs.entry(dd).or_insert(0) += 1;
    }
    if d.n > 0 {
        d.within = e.ratio(&within, &both);
        d.mean_diff = e.ratio(&diff, &both);
    }
    d
}

/// Two interpreters' labels of the same points (pairs where both gave one).
#[derive(Clone, Copy, Debug)]
pub struct Agreement {
    pub n: i32,
    pub observed: f64,
    pub kappa: f64,
}

pub fn cohen(a: &[String], b: &[String]) -> Agreement {
    let mut g = Agreement { n: 0, observed: f64::NAN, kappa: f64::NAN };
    let (mut ca, mut cb): (BTreeMap<&str, i32>, BTreeMap<&str, i32>) = (BTreeMap::new(), BTreeMap::new());
    let mut same = 0;
    for (x, y) in a.iter().zip(b) {
        if x.is_empty() || y.is_empty() {
            continue;
        }
        g.n += 1;
        *ca.entry(x).or_insert(0) += 1;
        *cb.entry(y).or_insert(0) += 1;
        same += i32::from(x == y);
    }
    if g.n == 0 {
        return g;
    }
    let n = f64::from(g.n);
    g.observed = f64::from(same) / n;
    let mut pe = 0.0;
    for (k, &na) in &ca {
        if let Some(&nb) = cb.get(k) {
            pe += f64::from(na) / n * f64::from(nb) / n;
        }
    }
    g.kappa = if pe < 1.0 { (g.observed - pe) / (1.0 - pe) } else { f64::NAN };
    g
}

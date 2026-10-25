//! A compiled expression as numpy (Copy Python): each operation as the calculator computes it, no data as NaN
//! (comparisons and logic of NaN are NaN, where/coalesce/isnan decide on it), parts used more than once computed
//! once.

use std::fmt::Write as _;

use super::canonical::operands;
use super::program::{BinFn, Node, NodeId, Program, TernFn, UnFn, qa_rule};

/// The helpers the translation calls (numpy as np).
pub const NUMPY_PRELUDE: &str = r#"
def _f(r):
    """Booleans as 1.0 / 0.0."""
    return np.asarray(r, dtype="float64")


def _cmp(a, b, r):
    """A comparison: no data where either side has none."""
    return np.where(np.isnan(a) | np.isnan(b), np.nan, _f(r))


def _truth(v):
    return np.where(np.isnan(v), np.nan, _f(v != 0))


def _and(a, b):
    x, y = _truth(a), _truth(b)
    return np.where((x == 0) | (y == 0), 0.0, np.where(np.isnan(x) | np.isnan(y), np.nan, 1.0))


def _or(a, b):
    x, y = _truth(a), _truth(b)
    return np.where((x == 1) | (y == 1), 1.0, np.where(np.isnan(x) | np.isnan(y), np.nan, 0.0))


def _not(a):
    return np.where(np.isnan(a), np.nan, _f(a == 0))


def _where(c, a, b):
    return np.where(np.isnan(c), np.nan, np.where(c != 0, a, b))


def _clamp(x, lo, hi):
    bad = np.isnan(x) | np.isnan(lo) | np.isnan(hi)
    return np.where(bad, np.nan, np.minimum(np.maximum(x, lo), hi))


def _nd(a, b):
    s = a + b
    return np.where(s != 0, (a - b) / np.where(s != 0, s, 1.0), np.nan)


def _ints(*vs):
    """Whole numbers of 32 bits (else None where not)."""
    ok = np.ones(np.broadcast(*vs).shape, dtype=bool)
    for v in vs:
        ok &= np.isfinite(v) & (v >= 0) & (v < 2.0 ** 32)
    return ok, [np.where(ok, v, 0).astype(np.uint64) for v in vs]


def _bit(a, n):
    ok, (x, k) = _ints(a, n)
    ok &= k < 32
    return np.where(ok, _f((x >> np.minimum(k, 31)) & 1), np.nan)


def _bits(a, s, n):
    ok, (x, k, m) = _ints(a, s, n)
    ok &= (k < 32) & (m <= 32)
    mask = (np.uint64(1) << np.minimum(m, 32)) - np.uint64(1)
    return np.where(ok, _f((x >> np.minimum(k, 31)) & mask), np.nan)


def _qa(x, q, rule):
    """x where the quality code q is usable by the rule, else no data."""
    ok = np.isfinite(q) & (q >= 0)
    c = np.where(ok, q, 0).astype(np.int64)
    if rule == "fmask":          # 0 clear land, 1 water
        ok &= (c == 0) | (c == 1)
    elif rule == "landsat_c2":   # QA_PIXEL bits 0 fill, 1 dilated cloud, 3 cloud, 4 shadow, 5 snow
        ok &= (c & 0x3B) == 0
    elif rule == "scl":          # Sentinel-2 SCL: 4 vegetation, 5 bare soil, 6 water
        ok &= (c >= 4) & (c <= 6)
    else:                        # a 0/1 mask
        ok &= c != 0
    return np.where(ok, x, np.nan)


def _out(r):
    """Infinities (a division by zero) as no data, as Janus writes them."""
    r = np.asarray(r, dtype="float64")
    return np.where(np.isfinite(r), r, np.nan)
"#;

fn number(v: f32) -> String {
    if v.is_nan() {
        "np.nan".into()
    } else if v.is_infinite() {
        if v > 0.0 { "np.inf".into() } else { "(-np.inf)".into() }
    } else if v < 0.0 {
        format!("({:?})", v as f64)
    } else {
        format!("{:?}", v as f64)
    }
}

fn un(f: UnFn, a: &str) -> String {
    let np = |n: &str| format!("np.{n}({a})");
    match f {
        UnFn::Neg => format!("(-{a})"),
        UnFn::Not => format!("_not({a})"),
        UnFn::Abs => np("abs"),
        UnFn::Sqrt => np("sqrt"),
        UnFn::Exp => np("exp"),
        UnFn::Ln => np("log"),
        UnFn::Log10 => np("log10"),
        UnFn::Log2 => np("log2"),
        UnFn::Sin => np("sin"),
        UnFn::Cos => np("cos"),
        UnFn::Tan => np("tan"),
        UnFn::Asin => np("arcsin"),
        UnFn::Acos => np("arccos"),
        UnFn::Atan => np("arctan"),
        UnFn::Sinh => np("sinh"),
        UnFn::Cosh => np("cosh"),
        UnFn::Tanh => np("tanh"),
        UnFn::Floor => np("floor"),
        UnFn::Ceil => np("ceil"),
        UnFn::Round => np("round"),
        UnFn::Trunc => np("trunc"),
        UnFn::Sign => np("sign"),
        UnFn::IsNan => format!("_f(np.isnan({a}))"),
        UnFn::IsValid => format!("_f(np.isfinite({a}))"),
    }
}

fn bin(f: BinFn, a: &str, b: &str) -> String {
    let op = |s: &str| format!("({a} {s} {b})");
    let cmp = |s: &str| format!("_cmp({a}, {b}, {a} {s} {b})");
    match f {
        BinFn::Add => op("+"),
        BinFn::Sub => op("-"),
        BinFn::Mul => op("*"),
        BinFn::Div => op("/"),
        BinFn::Mod => format!("np.mod({a}, {b})"),
        BinFn::Pow => format!("np.power({a}, {b})"),
        BinFn::Min => format!("np.minimum({a}, {b})"),
        BinFn::Max => format!("np.maximum({a}, {b})"),
        BinFn::Atan2 => format!("np.arctan2({a}, {b})"),
        BinFn::Hypot => format!("np.hypot({a}, {b})"),
        BinFn::Lt => cmp("<"),
        BinFn::Le => cmp("<="),
        BinFn::Gt => cmp(">"),
        BinFn::Ge => cmp(">="),
        BinFn::Eq => cmp("=="),
        BinFn::Ne => cmp("!="),
        BinFn::And => format!("_and({a}, {b})"),
        BinFn::Or => format!("_or({a}, {b})"),
        BinFn::Coalesce => format!("np.where(np.isfinite({a}), {a}, {b})"),
        BinFn::Nd => format!("_nd({a}, {b})"),
        BinFn::Bit => format!("_bit({a}, {b})"),
    }
}

/// The program as numpy: the statements of its shared parts (indented by `indent`) and the result's expression,
/// input k written as `input(k)`, a date's variable by its name. The caller wraps the result in `_out(...)`.
pub fn numpy(p: &Program, input: &dyn Fn(u16) -> String, indent: &str) -> (String, String) {
    let nodes = &p.nodes;
    let mut uses = vec![0u32; nodes.len()];
    let mut reached = vec![false; nodes.len()];
    let mut stack = vec![p.root];
    while let Some(id) = stack.pop() {
        if reached[id as usize] {
            continue;
        }
        reached[id as usize] = true;
        for o in operands(&nodes[id as usize]) {
            uses[o as usize] += 1;
            stack.push(o);
        }
    }
    // a part used more than once is a variable; so is one nested deep (Python's parser has a limit)
    let mut depth = vec![0u32; nodes.len()];
    let mut var = vec![false; nodes.len()];
    for id in 0..nodes.len() {
        if !reached[id] {
            continue;
        }
        let ops = operands(&nodes[id]);
        let d = ops.iter().map(|&o| if var[o as usize] { 0 } else { depth[o as usize] }).max().map_or(0, |d| d + 1);
        depth[id] = d;
        let op = matches!(nodes[id], Node::Un(..) | Node::Bin(..) | Node::Tern(..));
        var[id] = op && id as NodeId != p.root && (uses[id] > 1 || d >= 24);
    }
    let mut out = String::new();
    for id in 0..nodes.len() as NodeId {
        if var[id as usize] {
            let _ = writeln!(out, "{indent}_{id} = {}", text(nodes, id, &var, input, true));
        }
    }
    (out, text(nodes, p.root, &var, input, true))
}

fn text(nodes: &[Node], id: NodeId, var: &[bool], input: &dyn Fn(u16) -> String, top: bool) -> String {
    if !top && var[id as usize] {
        return format!("_{id}");
    }
    let t = |o: NodeId| text(nodes, o, var, input, false);
    match nodes[id as usize] {
        Node::Const(v) => number(v),
        Node::Input(k) => input(k),
        Node::Var(v) => v.name().to_string(),
        Node::Un(f, a) => un(f, &t(a)),
        Node::Bin(f, a, b) => bin(f, &t(a), &t(b)),
        Node::Tern(TernFn::Where, c, a, b) => format!("_where({}, {}, {})", t(c), t(a), t(b)),
        Node::Tern(TernFn::Clamp, x, lo, hi) => format!("_clamp({}, {}, {})", t(x), t(lo), t(hi)),
        Node::Tern(TernFn::Bits, x, s, n) => format!("_bits({}, {}, {})", t(x), t(s), t(n)),
        Node::Tern(TernFn::Qa, x, q, r) => {
            let rule = match nodes[r as usize] {
                Node::Const(c) => qa_rule(c).id(),
                _ => "nonzero",
            };
            format!("_qa({}, {}, \"{rule}\")", t(x), t(q))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::names::BandNames;
    use crate::calc::{DateVars, LayerExpr};

    const TEXTS: &[&str] = &[
        "(N - R) / (N + R)",
        "2.5 * (N - R) / (N + 6 * R - 7.5 * B + 1)",
        "x = N - R; y = x * x; where(y > 0.01 and not isnan(S1), -y, coalesce(S2, 0.5))",
        "qa(N, QA_PIXEL, \"landsat_c2\") ^ 2 % 3",
        "clamp(nd(N, S1), -0.5, 0.5) + bits(QA_PIXEL, 8, 2) + bit(QA_PIXEL, 3) + month / 12 + t",
        "N > 0.3 = 1\nN == nan = -1e-10",
        "sqrt(abs(R - G)) + log10(N) + exp(-B) + atan2(R, N) + hypot(R, G) + sign(R - G) + round(N * 7)",
        "max(N, R, G) - min(N, R) + mean(R, G) + (R or B) + (R != G) + floor(N) + ceil(R) + trunc(-R)",
        "R / (G - G)",
    ];

    /// The texts as numpy and the values Janus computes, as a script that checks them (needs numpy).
    fn script() -> String {
        let names = BandNames::new(
            ["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR 2", "QA_PIXEL"].iter().map(|s| s.to_string()).collect(),
            &[],
        );
        let pixels: [[f32; 7]; 4] = [
            [0.04, 0.07, 0.05, 0.35, 0.18, 0.09, 21824.0],
            [0.1, 0.1, 0.4, 0.2, f32::NAN, 0.3, 1.0],
            [f32::NAN, -0.2, 0.3, 0.3, 0.0, -0.4, 22280.0],
            [0.5, 0.25, -0.75, 1.5, 2.0, 0.0, 8.0],
        ];
        let vars = DateVars::new(Some(1594684800.0), 5);
        let mut s = String::from("import numpy as np\nnp.seterr(all=\"ignore\")\n");
        s += NUMPY_PRELUDE;
        s += "year, month, day, doy, decyear, t = 2020.0, 7.0, 14.0, 196.0, 2020.53, 5.0\nbad = 0\n";
        for (k, text) in TEXTS.iter().enumerate() {
            let e = LayerExpr::compile(text, &names).unwrap();
            let col = |b: usize| {
                let v: Vec<String> = pixels.iter().map(|p| number(p[b])).collect();
                format!("np.array([{}])", v.join(", "))
            };
            let (stmts, r) = numpy(&e.program, &|i| format!("b{}", e.bands[i as usize]), "    ");
            let _ = writeln!(s, "def f{k}():");
            for b in 1..=7 {
                let _ = writeln!(s, "    b{b} = {}", col(b - 1));
            }
            s += &stmts;
            let _ = writeln!(s, "    return _out({r})");
            let want: Vec<String> = pixels
                .iter()
                .map(|p| {
                    let ins: Vec<f32> = e.bands.iter().map(|&b| p[b as usize - 1]).collect();
                    number(e.program.eval_one(&ins, &vars))
                })
                .collect();
            let _ = writeln!(
                s,
                "got, want = f{k}(), np.array([{}])\nif not np.allclose(got, want, rtol=1e-5, atol=1e-6, equal_nan=True):\n    \
                 bad += 1\n    print({text:?}, got, want)",
                want.join(", ")
            );
        }
        s += "print(\"mismatches:\", bad)\nraise SystemExit(bad)\n";
        s
    }

    #[test]
    fn translates() {
        let s = script();
        assert!(s.contains("_nd(") && s.contains("_qa(") && s.contains("\"landsat_c2\""));
    }

    /// Janus's values against numpy's (needs python with numpy on the PATH).
    #[test]
    #[ignore]
    fn against_numpy() {
        let path = std::env::temp_dir().join("janus_calc_numpy.py");
        std::fs::write(&path, script()).unwrap();
        let out = std::process::Command::new("python").arg(&path).output().expect("python");
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{text}");
        assert!(text.contains("mismatches: 0"), "{text}");
    }
}

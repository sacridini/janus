//! A compiled graph written back as text over inputs `$1`, `$2`...: what a VRT's derived band carries for the
//! pixel function to compile again (the same graph). Parts used more than once become variables `_k`.

use std::fmt::Write as _;

use super::program::{BinFn, Node, NodeId, Program, TernFn, UnFn};

fn un_name(f: UnFn) -> &'static str {
    match f {
        UnFn::Neg => "-",
        UnFn::Not => "not",
        UnFn::Abs => "abs",
        UnFn::Sqrt => "sqrt",
        UnFn::Exp => "exp",
        UnFn::Ln => "ln",
        UnFn::Log10 => "log10",
        UnFn::Log2 => "log2",
        UnFn::Sin => "sin",
        UnFn::Cos => "cos",
        UnFn::Tan => "tan",
        UnFn::Asin => "asin",
        UnFn::Acos => "acos",
        UnFn::Atan => "atan",
        UnFn::Sinh => "sinh",
        UnFn::Cosh => "cosh",
        UnFn::Tanh => "tanh",
        UnFn::Floor => "floor",
        UnFn::Ceil => "ceil",
        UnFn::Round => "round",
        UnFn::Trunc => "trunc",
        UnFn::Sign => "sign",
        UnFn::IsNan => "isnan",
        UnFn::IsValid => "isvalid",
    }
}

/// An operator's symbol, or the function's name.
fn bin_name(f: BinFn) -> (&'static str, bool) {
    match f {
        BinFn::Add => ("+", true),
        BinFn::Sub => ("-", true),
        BinFn::Mul => ("*", true),
        BinFn::Div => ("/", true),
        BinFn::Mod => ("%", true),
        BinFn::Pow => ("^", true),
        BinFn::Lt => ("<", true),
        BinFn::Le => ("<=", true),
        BinFn::Gt => (">", true),
        BinFn::Ge => (">=", true),
        BinFn::Eq => ("==", true),
        BinFn::Ne => ("!=", true),
        BinFn::And => ("and", true),
        BinFn::Or => ("or", true),
        BinFn::Min => ("min", false),
        BinFn::Max => ("max", false),
        BinFn::Atan2 => ("atan2", false),
        BinFn::Hypot => ("hypot", false),
        BinFn::Coalesce => ("coalesce", false),
        BinFn::Nd => ("nd", false),
        BinFn::Bit => ("bit", false),
    }
}

fn tern_name(f: TernFn) -> &'static str {
    match f {
        TernFn::Where => "where",
        TernFn::Clamp => "clamp",
        TernFn::Bits => "bits",
        TernFn::Qa => "qa",
    }
}

/// A float that reads back as the same f32.
pub fn number(v: f32) -> String {
    if v.is_nan() {
        "nan".into()
    } else if v.is_infinite() {
        if v > 0.0 { "inf".into() } else { "(-inf)".into() }
    } else if v < 0.0 || (v == 0.0 && v.is_sign_negative()) {
        format!("(-{:?})", -v)
    } else {
        format!("{v:?}")
    }
}

/// The text of a program's graph.
pub fn canonical(p: &Program) -> String {
    canonical_mapped(p, &|k| k as usize + 1)
}

/// As `canonical`, input k written as `$map(k)`.
pub fn canonical_mapped(p: &Program, map: &dyn Fn(u16) -> usize) -> String {
    let nodes = &p.nodes;
    // the nodes reached from the result and how often each is an operand
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
    let shared = |id: NodeId| {
        uses[id as usize] > 1 && matches!(nodes[id as usize], Node::Un(..) | Node::Bin(..) | Node::Tern(..))
    };
    let mut out = String::new();
    // shared parts first, operands before (node ids grow with their operands)
    for id in 0..nodes.len() as NodeId {
        if reached[id as usize] && shared(id) && id != p.root {
            let _ = write!(out, "_{id} = {}; ", text(nodes, id, &shared, map, true));
        }
    }
    out + &text(nodes, p.root, &shared, map, true)
}

pub(crate) fn operands(n: &Node) -> Vec<NodeId> {
    match *n {
        Node::Un(_, a) => vec![a],
        Node::Bin(_, a, b) => vec![a, b],
        Node::Tern(_, a, b, c) => vec![a, b, c],
        _ => vec![],
    }
}

fn text(nodes: &[Node], id: NodeId, shared: &dyn Fn(NodeId) -> bool, map: &dyn Fn(u16) -> usize, top: bool) -> String {
    if !top && shared(id) {
        return format!("_{id}");
    }
    let t = |o: NodeId| text(nodes, o, shared, map, false);
    match nodes[id as usize] {
        Node::Const(v) => number(v),
        Node::Input(k) => format!("${}", map(k)),
        Node::Var(v) => v.name().to_string(),
        Node::Un(UnFn::Neg, a) => format!("(-{})", t(a)),
        Node::Un(UnFn::Not, a) => format!("(not {})", t(a)),
        Node::Un(f, a) => format!("{}({})", un_name(f), t(a)),
        Node::Bin(f, a, b) => match bin_name(f) {
            (s, true) => format!("({} {s} {})", t(a), t(b)),
            (s, false) => format!("{s}({}, {})", t(a), t(b)),
        },
        Node::Tern(f, a, b, c) => format!("{}({}, {}, {})", tern_name(f), t(a), t(b), t(c)),
    }
}

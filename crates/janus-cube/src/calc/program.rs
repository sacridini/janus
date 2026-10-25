//! An expression compiled: a graph of operations (common parts shared, constants folded) laid out as a short
//! list of instructions over registers of a few thousand pixels, evaluated block by block so the registers stay
//! in the processor's cache and each instruction is one tight loop the compiler vectorizes.
//!
//! No data is NaN. An operation on it gives NaN, except those that say otherwise: `isnan`, `isvalid`,
//! `coalesce`, and the logic of `and` / `or`, which is Kleene's (`0 and NaN` is 0, `1 or NaN` is 1). Infinities
//! become NaN at the end, as the reader does with its inputs.

use std::collections::HashMap;

use crate::QaRule;

/// Pixels per block: 4096 floats are 16 KB a register.
pub const BLOCK: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnFn {
    Neg,
    Not,
    Abs,
    Sqrt,
    Exp,
    Ln,
    Log10,
    Log2,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Floor,
    Ceil,
    Round,
    Trunc,
    Sign,
    IsNan,
    IsValid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinFn {
    Add,
    Sub,
    Mul,
    Div,
    /// floored, as Python's and numpy's %
    Mod,
    Pow,
    Min,
    Max,
    Atan2,
    Hypot,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    /// the first if valid, else the second
    Coalesce,
    /// (a - b) / (a + b), NaN where the sum is 0 (the reader's normalized difference)
    Nd,
    /// bit b of the integer a
    Bit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TernFn {
    /// c ? a : b
    Where,
    /// x clamped to [lo, hi]
    Clamp,
    /// count bits of x from start
    Bits,
    /// x where the quality code q is usable by the rule (QaRule::code), else NaN
    Qa,
}

/// The date of the layer being evaluated: constant over its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DateVar {
    Year,
    Month,
    Day,
    Doy,
    DecYear,
    /// the layer's index from 0
    T,
}

impl DateVar {
    pub const ALL: [DateVar; 6] =
        [DateVar::Year, DateVar::Month, DateVar::Day, DateVar::Doy, DateVar::DecYear, DateVar::T];

    pub fn name(self) -> &'static str {
        match self {
            DateVar::Year => "year",
            DateVar::Month => "month",
            DateVar::Day => "day",
            DateVar::Doy => "doy",
            DateVar::DecYear => "decyear",
            DateVar::T => "t",
        }
    }

    pub fn of(name: &str) -> Option<DateVar> {
        DateVar::ALL.into_iter().find(|v| v.name() == name)
    }
}

/// The values of the date variables for one layer (NaN without dates, except t).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DateVars(pub [f32; 6]);

impl Default for DateVars {
    fn default() -> Self {
        DateVars([f32::NAN, f32::NAN, f32::NAN, f32::NAN, f32::NAN, 0.0])
    }
}

impl DateVars {
    /// Of Unix seconds (None: not a date) and an index.
    pub fn new(time: Option<f64>, t: usize) -> Self {
        let mut v = DateVars::default();
        v.0[5] = t as f32;
        if let Some(time) = time {
            let days = (time / 86400.0).floor() as i64;
            let (y, m, d) = crate::dates::civil_from_days(days);
            let doy = days - crate::dates::days_from_civil(y, 1, 1) + 1;
            v.0[..5].copy_from_slice(&[
                y as f32,
                m as f32,
                d as f32,
                doy as f32,
                crate::dates::decimal_year(time) as f32,
            ]);
        }
        v
    }

    pub fn get(&self, v: DateVar) -> f32 {
        self.0[v as usize]
    }
}

pub type NodeId = u32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Node {
    Const(f32),
    Input(u16),
    Var(DateVar),
    Un(UnFn, NodeId),
    Bin(BinFn, NodeId, NodeId),
    Tern(TernFn, NodeId, NodeId, NodeId),
}

/// A node as a key of the memo: floats by their bits (NaN equals NaN).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Const(u32),
    Input(u16),
    Var(DateVar),
    Un(UnFn, NodeId),
    Bin(BinFn, NodeId, NodeId),
    Tern(TernFn, NodeId, NodeId, NodeId),
}

fn key(n: &Node) -> Key {
    match *n {
        Node::Const(v) => Key::Const(if v.is_nan() { f32::NAN.to_bits() } else { v.to_bits() }),
        Node::Input(k) => Key::Input(k),
        Node::Var(v) => Key::Var(v),
        Node::Un(f, a) => Key::Un(f, a),
        Node::Bin(f, a, b) => Key::Bin(f, a, b),
        Node::Tern(f, a, b, c) => Key::Tern(f, a, b, c),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The scalar operations (folding constants, and the body of each instruction's loop)
// ---------------------------------------------------------------------------------------------------------------

#[inline(always)]
fn truth(v: f32) -> f32 {
    if v.is_nan() { f32::NAN } else { f32::from(v != 0.0) }
}

#[inline(always)]
fn cmp(a: f32, b: f32, r: bool) -> f32 {
    if a.is_nan() || b.is_nan() { f32::NAN } else { f32::from(r) }
}

#[inline(always)]
fn as_bits(v: f32) -> Option<u32> {
    (v.is_finite() && (0.0..4294967296.0).contains(&v)).then_some(v as u32)
}

#[inline(always)]
pub fn un(f: UnFn, a: f32) -> f32 {
    match f {
        UnFn::Neg => -a,
        UnFn::Not => {
            if a.is_nan() {
                f32::NAN
            } else {
                f32::from(a == 0.0)
            }
        }
        UnFn::Abs => a.abs(),
        UnFn::Sqrt => a.sqrt(),
        UnFn::Exp => a.exp(),
        UnFn::Ln => a.ln(),
        UnFn::Log10 => a.log10(),
        UnFn::Log2 => a.log2(),
        UnFn::Sin => a.sin(),
        UnFn::Cos => a.cos(),
        UnFn::Tan => a.tan(),
        UnFn::Asin => a.asin(),
        UnFn::Acos => a.acos(),
        UnFn::Atan => a.atan(),
        UnFn::Sinh => a.sinh(),
        UnFn::Cosh => a.cosh(),
        UnFn::Tanh => a.tanh(),
        UnFn::Floor => a.floor(),
        UnFn::Ceil => a.ceil(),
        UnFn::Round => a.round_ties_even(),
        UnFn::Trunc => a.trunc(),
        UnFn::Sign => {
            if a.is_nan() || a == 0.0 {
                a
            } else {
                a.signum()
            }
        }
        UnFn::IsNan => f32::from(a.is_nan()),
        UnFn::IsValid => f32::from(a.is_finite()),
    }
}

#[inline(always)]
pub fn bin(f: BinFn, a: f32, b: f32) -> f32 {
    match f {
        BinFn::Add => a + b,
        BinFn::Sub => a - b,
        BinFn::Mul => a * b,
        BinFn::Div => a / b,
        BinFn::Mod => a - b * (a / b).floor(),
        BinFn::Pow => a.powf(b),
        // NaN propagates (f32::min would drop it)
        BinFn::Min => {
            if a.is_nan() || b.is_nan() {
                f32::NAN
            } else {
                a.min(b)
            }
        }
        BinFn::Max => {
            if a.is_nan() || b.is_nan() {
                f32::NAN
            } else {
                a.max(b)
            }
        }
        BinFn::Atan2 => a.atan2(b),
        BinFn::Hypot => a.hypot(b),
        BinFn::Lt => cmp(a, b, a < b),
        BinFn::Le => cmp(a, b, a <= b),
        BinFn::Gt => cmp(a, b, a > b),
        BinFn::Ge => cmp(a, b, a >= b),
        BinFn::Eq => cmp(a, b, a == b),
        BinFn::Ne => cmp(a, b, a != b),
        BinFn::And => {
            let (x, y) = (truth(a), truth(b));
            if x == 0.0 || y == 0.0 {
                0.0
            } else if x.is_nan() || y.is_nan() {
                f32::NAN
            } else {
                1.0
            }
        }
        BinFn::Or => {
            let (x, y) = (truth(a), truth(b));
            if x == 1.0 || y == 1.0 {
                1.0
            } else if x.is_nan() || y.is_nan() {
                f32::NAN
            } else {
                0.0
            }
        }
        BinFn::Coalesce => {
            if a.is_finite() {
                a
            } else {
                b
            }
        }
        BinFn::Nd => {
            let s = a + b;
            if s != 0.0 { (a - b) / s } else { f32::NAN }
        }
        BinFn::Bit => match (as_bits(a), as_bits(b)) {
            (Some(x), Some(n)) if n < 32 => ((x >> n) & 1) as f32,
            _ => f32::NAN,
        },
    }
}

#[inline(always)]
pub fn tern(f: TernFn, a: f32, b: f32, c: f32) -> f32 {
    match f {
        TernFn::Where => {
            if a.is_nan() {
                f32::NAN
            } else if a != 0.0 {
                b
            } else {
                c
            }
        }
        TernFn::Clamp => {
            if a.is_nan() || b.is_nan() || c.is_nan() {
                f32::NAN
            } else {
                a.max(b).min(c)
            }
        }
        TernFn::Bits => match (as_bits(a), as_bits(b), as_bits(c)) {
            (Some(x), Some(s), Some(n)) if s < 32 && n <= 32 => {
                let mask = if n == 32 { u32::MAX } else { (1u32 << n) - 1 };
                ((x >> s) & mask) as f32
            }
            _ => f32::NAN,
        },
        TernFn::Qa => {
            if b.is_nan() || b < 0.0 || !qa_rule(c).usable(b as u32) {
                f32::NAN
            } else {
                a
            }
        }
    }
}

/// A quality rule from its code (QaRule::code).
pub fn qa_rule(code: f32) -> QaRule {
    match code as i32 {
        1 => QaRule::Fmask,
        2 => QaRule::LandsatC2,
        3 => QaRule::NonZero,
        4 => QaRule::Scl,
        _ => QaRule::None,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The graph
// ---------------------------------------------------------------------------------------------------------------

/// Builds the graph of an expression: equal nodes made once, operations on constants computed now. `K` names the
/// inputs (a band; a layer's band); each different one gets a slot.
pub struct Builder<K> {
    pub nodes: Vec<Node>,
    memo: HashMap<Key, NodeId>,
    pub inputs: Vec<K>,
}

impl<K: PartialEq + Clone> Default for Builder<K> {
    fn default() -> Self {
        Builder { nodes: Vec::new(), memo: HashMap::new(), inputs: Vec::new() }
    }
}

impl<K: PartialEq + Clone> Builder<K> {
    fn add(&mut self, n: Node) -> NodeId {
        let k = key(&n);
        if let Some(&id) = self.memo.get(&k) {
            return id;
        }
        let id = self.nodes.len() as NodeId;
        self.nodes.push(n);
        self.memo.insert(k, id);
        id
    }

    fn konst_of(&self, id: NodeId) -> Option<f32> {
        match self.nodes[id as usize] {
            Node::Const(v) => Some(v),
            _ => None,
        }
    }

    pub fn konst(&mut self, v: f32) -> NodeId {
        self.add(Node::Const(v))
    }

    pub fn input(&mut self, k: K) -> NodeId {
        let slot = match self.inputs.iter().position(|i| *i == k) {
            Some(s) => s,
            None => {
                self.inputs.push(k);
                self.inputs.len() - 1
            }
        };
        self.add(Node::Input(slot as u16))
    }

    pub fn var(&mut self, v: DateVar) -> NodeId {
        self.add(Node::Var(v))
    }

    pub fn un(&mut self, f: UnFn, a: NodeId) -> NodeId {
        if let Some(x) = self.konst_of(a) {
            return self.konst(un(f, x));
        }
        self.add(Node::Un(f, a))
    }

    pub fn bin(&mut self, f: BinFn, a: NodeId, b: NodeId) -> NodeId {
        match (self.konst_of(a), self.konst_of(b)) {
            (Some(x), Some(y)) => return self.konst(bin(f, x, y)),
            // x * 1, x + 0, x / 1, x ^ 1
            (_, Some(y))
                if (y == 1.0 && matches!(f, BinFn::Mul | BinFn::Div | BinFn::Pow))
                    || (y == 0.0 && matches!(f, BinFn::Add | BinFn::Sub)) =>
            {
                return a;
            }
            (Some(x), _) if (x == 1.0 && f == BinFn::Mul) || (x == 0.0 && f == BinFn::Add) => return b,
            _ => {}
        }
        self.add(Node::Bin(f, a, b))
    }

    pub fn tern(&mut self, f: TernFn, a: NodeId, b: NodeId, c: NodeId) -> NodeId {
        match (self.konst_of(a), self.konst_of(b), self.konst_of(c)) {
            (Some(x), Some(y), Some(z)) => return self.konst(tern(f, x, y, z)),
            (Some(x), _, _) if f == TernFn::Where && !x.is_nan() => return if x != 0.0 { b } else { c },
            _ => {}
        }
        self.add(Node::Tern(f, a, b, c))
    }

    /// The program computing node `out`, its inputs those of the builder.
    pub fn finish(&self, out: NodeId) -> Program {
        Program::lay_out(&self.nodes, out, self.inputs.len())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The program
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Opnd {
    Reg(u16),
    Input(u16),
    Const(f32),
    Var(DateVar),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Un(UnFn),
    Bin(BinFn),
    Tern(TernFn),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instr {
    pub op: Op,
    pub dst: u16,
    pub a: Opnd,
    pub b: Opnd,
    pub c: Opnd,
}

/// The instructions of an expression and what they need.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub code: Vec<Instr>,
    pub regs: usize,
    pub inputs: usize,
    pub out: Opnd,
    /// the graph, for the canonical text (calc::canonical)
    pub(crate) nodes: Vec<Node>,
    pub(crate) root: NodeId,
}

thread_local! {
    static SCRATCH: std::cell::RefCell<Vec<f32>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl Program {
    /// The nodes `out` needs, in an order where each comes after its operands, on as few registers as it can:
    /// a register is free again after its value's last use.
    fn lay_out(nodes: &[Node], out: NodeId, inputs: usize) -> Program {
        // the nodes reached from out, depth first (operands before)
        let mut order: Vec<NodeId> = Vec::new();
        let mut seen = vec![false; nodes.len()];
        let mut stack = vec![(out, false)];
        while let Some((id, done)) = stack.pop() {
            if done {
                order.push(id);
                continue;
            }
            if seen[id as usize] {
                continue;
            }
            seen[id as usize] = true;
            stack.push((id, true));
            let ops: &[NodeId] = match &nodes[id as usize] {
                Node::Un(_, a) => &[*a],
                Node::Bin(_, a, b) => &[*a, *b],
                Node::Tern(_, a, b, c) => &[*a, *b, *c],
                _ => &[],
            };
            for &o in ops.iter().rev() {
                if !seen[o as usize] {
                    stack.push((o, false));
                }
            }
        }
        let operands = |n: &Node| -> Vec<NodeId> {
            match *n {
                Node::Un(_, a) => vec![a],
                Node::Bin(_, a, b) => vec![a, b],
                Node::Tern(_, a, b, c) => vec![a, b, c],
                _ => vec![],
            }
        };
        let mut last_use: HashMap<NodeId, usize> = HashMap::new();
        for (i, &id) in order.iter().enumerate() {
            for o in operands(&nodes[id as usize]) {
                last_use.insert(o, i);
            }
        }
        let mut reg_of: HashMap<NodeId, u16> = HashMap::new();
        let mut free: Vec<u16> = Vec::new();
        let mut regs = 0usize;
        let mut code = Vec::new();
        let opnd = |id: NodeId, reg_of: &HashMap<NodeId, u16>| -> Opnd {
            match nodes[id as usize] {
                Node::Const(v) => Opnd::Const(v),
                Node::Input(k) => Opnd::Input(k),
                Node::Var(v) => Opnd::Var(v),
                _ => Opnd::Reg(reg_of[&id]),
            }
        };
        for (i, &id) in order.iter().enumerate() {
            let n = nodes[id as usize];
            let (op, ops) = match n {
                Node::Un(f, a) => (Op::Un(f), [a, a, a]),
                Node::Bin(f, a, b) => (Op::Bin(f), [a, b, b]),
                Node::Tern(f, a, b, c) => (Op::Tern(f), [a, b, c]),
                _ => continue,
            };
            let (a, b, c) = (opnd(ops[0], &reg_of), opnd(ops[1], &reg_of), opnd(ops[2], &reg_of));
            // the result never takes an operand's register (no slice is both read and written)
            let dst = free.pop().unwrap_or_else(|| {
                regs += 1;
                (regs - 1) as u16
            });
            // operands whose last use is here free their registers for the next instructions
            for o in operands(&n) {
                if last_use.get(&o) == Some(&i)
                    && let Some(r) = reg_of.get(&o)
                    && !free.contains(r)
                {
                    free.push(*r);
                }
            }
            reg_of.insert(id, dst);
            code.push(Instr { op, dst, a, b, c });
        }
        let out_opnd = opnd(out, &reg_of);
        Program { code, regs, inputs, out: out_opnd, nodes: nodes.to_vec(), root: out }
    }

    /// Whether the result depends on the date variables.
    pub fn uses_date(&self) -> bool {
        self.nodes.iter().any(|n| matches!(n, Node::Var(_)))
    }

    /// The result over `out.len()` pixels from the inputs (each at least as long), the date's variables given.
    pub fn eval(&self, inputs: &[&[f32]], vars: &DateVars, out: &mut [f32]) {
        let n = out.len();
        debug_assert!(inputs.len() >= self.inputs && inputs.iter().all(|i| i.len() >= n));
        SCRATCH.with_borrow_mut(|scratch| {
            if scratch.len() < self.regs * BLOCK {
                scratch.resize(self.regs * BLOCK, 0.0);
            }
            let mut start = 0;
            while start < n {
                let len = BLOCK.min(n - start);
                for ins in &self.code {
                    run(ins, inputs, vars, scratch, start, len);
                }
                let dst = &mut out[start..start + len];
                match self.out {
                    Opnd::Reg(r) => dst.copy_from_slice(&scratch[r as usize * BLOCK..r as usize * BLOCK + len]),
                    Opnd::Input(k) => dst.copy_from_slice(&inputs[k as usize][start..start + len]),
                    Opnd::Const(v) => dst.fill(v),
                    Opnd::Var(v) => dst.fill(vars.get(v)),
                }
                for v in dst.iter_mut() {
                    if !v.is_finite() {
                        *v = f32::NAN;
                    }
                }
                start += len;
            }
            // a large program's registers are not kept by the thread
            if scratch.len() > 64 * BLOCK {
                *scratch = Vec::new();
            }
        });
    }

    /// The value at one pixel (the inputs' values given).
    pub fn eval_one(&self, inputs: &[f32], vars: &DateVars) -> f32 {
        let slices: Vec<&[f32]> = inputs.iter().map(std::slice::from_ref).collect();
        let mut v = [0.0f32];
        self.eval(&slices, vars, &mut v);
        v[0]
    }
}

/// A block's operand: a slice of a register or an input, or a value for every pixel.
enum Arg<'a> {
    S(&'a [f32]),
    V(f32),
}

/// One instruction over a block.
fn run(ins: &Instr, inputs: &[&[f32]], vars: &DateVars, regs: &mut [f32], start: usize, len: usize) {
    let base = regs.as_mut_ptr();
    let arg = |o: Opnd| -> Arg {
        match o {
            Opnd::Reg(r) => {
                debug_assert!(r != ins.dst && (r as usize + 1) * BLOCK <= regs.len());
                // SAFETY: an operand's register is never the destination's (Program::lay_out), so this slice and
                // `dst` below do not overlap; both lie within `regs`.
                Arg::S(unsafe { std::slice::from_raw_parts(base.add(r as usize * BLOCK), len) })
            }
            Opnd::Input(k) => Arg::S(&inputs[k as usize][start..start + len]),
            Opnd::Const(v) => Arg::V(v),
            Opnd::Var(v) => Arg::V(vars.get(v)),
        }
    };
    let (a, b, c) = (arg(ins.a), arg(ins.b), arg(ins.c));
    // SAFETY: the destination register lies within `regs` and no operand slice overlaps it (above).
    let dst = unsafe { std::slice::from_raw_parts_mut(base.add(ins.dst as usize * BLOCK), len) };
    match ins.op {
        Op::Un(f) => unary_loop(f, dst, a),
        Op::Bin(f) => binary_loop(f, dst, a, b),
        Op::Tern(f) => ternary_loop(f, dst, a, b, c),
    }
}

#[inline(always)]
fn map1(dst: &mut [f32], a: &Arg, f: impl Fn(f32) -> f32) {
    match a {
        Arg::S(x) => {
            for i in 0..dst.len() {
                dst[i] = f(x[i]);
            }
        }
        Arg::V(v) => dst.fill(f(*v)),
    }
}

#[inline(always)]
fn map2(dst: &mut [f32], a: &Arg, b: &Arg, f: impl Fn(f32, f32) -> f32) {
    match (a, b) {
        (Arg::S(x), Arg::S(y)) => {
            let (x, y) = (&x[..dst.len()], &y[..dst.len()]);
            for i in 0..dst.len() {
                dst[i] = f(x[i], y[i]);
            }
        }
        (Arg::S(x), Arg::V(v)) => {
            let x = &x[..dst.len()];
            for i in 0..dst.len() {
                dst[i] = f(x[i], *v);
            }
        }
        (Arg::V(v), Arg::S(y)) => {
            let y = &y[..dst.len()];
            for i in 0..dst.len() {
                dst[i] = f(*v, y[i]);
            }
        }
        (Arg::V(u), Arg::V(v)) => dst.fill(f(*u, *v)),
    }
}

#[inline(always)]
fn map3(dst: &mut [f32], a: &Arg, b: &Arg, c: &Arg, f: impl Fn(f32, f32, f32) -> f32) {
    let at = |x: &Arg, i: usize| match x {
        Arg::S(s) => s[i],
        Arg::V(v) => *v,
    };
    for (i, d) in dst.iter_mut().enumerate() {
        *d = f(at(a, i), at(b, i), at(c, i));
    }
}

/// Each function its own loop (`f` a constant inside it, so each is compiled and vectorized on its own).
macro_rules! per_fn {
    ($f:expr, $ty:ident, [$($v:ident),*], $body:ident) => {
        match $f {
            $( $ty::$v => $body!($ty::$v), )*
        }
    };
}

fn unary_loop(f: UnFn, dst: &mut [f32], a: Arg) {
    macro_rules! go {
        ($k:expr) => {
            map1(dst, &a, |x| un($k, x))
        };
    }
    per_fn!(
        f,
        UnFn,
        [
            Neg, Not, Abs, Sqrt, Exp, Ln, Log10, Log2, Sin, Cos, Tan, Asin, Acos, Atan, Sinh, Cosh, Tanh, Floor, Ceil,
            Round, Trunc, Sign, IsNan, IsValid
        ],
        go
    )
}

fn binary_loop(f: BinFn, dst: &mut [f32], a: Arg, b: Arg) {
    macro_rules! go {
        ($k:expr) => {
            map2(dst, &a, &b, |x, y| bin($k, x, y))
        };
    }
    per_fn!(
        f,
        BinFn,
        [Add, Sub, Mul, Div, Mod, Pow, Min, Max, Atan2, Hypot, Lt, Le, Gt, Ge, Eq, Ne, And, Or, Coalesce, Nd, Bit],
        go
    )
}

fn ternary_loop(f: TernFn, dst: &mut [f32], a: Arg, b: Arg, c: Arg) {
    macro_rules! go {
        ($k:expr) => {
            map3(dst, &a, &b, &c, |x, y, z| tern($k, x, y, z))
        };
    }
    per_fn!(f, TernFn, [Where, Clamp, Bits, Qa], go)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_parts_and_registers() {
        let mut b: Builder<i32> = Builder::default();
        let (n, r) = (b.input(8), b.input(4));
        let num = b.bin(BinFn::Sub, n, r);
        let den = b.bin(BinFn::Add, n, r);
        let again = b.bin(BinFn::Add, n, r);
        assert_eq!(den, again);
        let q = b.bin(BinFn::Div, num, den);
        let p = b.finish(q);
        assert_eq!((p.code.len(), p.inputs), (3, 2));
        assert_eq!(p.regs, 3);
        let (nv, rv): (Vec<f32>, Vec<f32>) = ((0..10000).map(|i| i as f32).collect(), vec![1.0; 10000]);
        let mut out = vec![0.0; 10000];
        p.eval(&[&nv, &rv], &DateVars::default(), &mut out);
        for i in [0usize, 1, 4095, 4096, 9999] {
            let want = (i as f32 - 1.0) / (i as f32 + 1.0);
            assert_eq!(out[i], want);
        }
    }

    #[test]
    fn folding() {
        let mut b: Builder<i32> = Builder::default();
        let x = b.input(1);
        let two = b.konst(2.0);
        let three = b.konst(3.0);
        let six = b.bin(BinFn::Mul, two, three);
        assert!(matches!(b.nodes[six as usize], Node::Const(6.0)));
        let one = b.konst(1.0);
        assert_eq!(b.bin(BinFn::Mul, x, one), x);
        let c = b.bin(BinFn::Gt, two, one);
        assert_eq!(b.tern(TernFn::Where, c, x, three), x);
    }

    #[test]
    fn nodata_and_logic() {
        let nan = f32::NAN;
        assert!(bin(BinFn::Add, 1.0, nan).is_nan());
        assert!(bin(BinFn::Lt, nan, 1.0).is_nan());
        assert_eq!(bin(BinFn::And, 0.0, nan), 0.0);
        assert!(bin(BinFn::And, 1.0, nan).is_nan());
        assert_eq!(bin(BinFn::Or, 1.0, nan), 1.0);
        assert!(bin(BinFn::Min, 1.0, nan).is_nan());
        assert_eq!(bin(BinFn::Coalesce, nan, 2.0), 2.0);
        assert_eq!(bin(BinFn::Mod, -7.0, 3.0), 2.0);
        assert!(bin(BinFn::Nd, 1.0, -1.0).is_nan());
        assert_eq!(bin(BinFn::Bit, 8.0, 3.0), 1.0);
        assert_eq!(tern(TernFn::Bits, 0b1011_0000 as f32, 4.0, 3.0), 3.0);
        assert!(tern(TernFn::Where, nan, 1.0, 2.0).is_nan());
        assert_eq!(un(UnFn::Round, 2.5), 2.0);
        assert_eq!(un(UnFn::IsNan, nan), 1.0);
        // SCL: 4 vegetation kept, 9 cloud masked
        assert_eq!(tern(TernFn::Qa, 0.5, 4.0, QaRule::Scl.code() as f32), 0.5);
        assert!(tern(TernFn::Qa, 0.5, 9.0, QaRule::Scl.code() as f32).is_nan());
    }

    #[test]
    fn operands_in_place() {
        // registers freed after their last use and taken again
        let mut b: Builder<i32> = Builder::default();
        let a = b.input(1);
        let s = b.bin(BinFn::Add, a, a);
        let m = b.bin(BinFn::Mul, s, s);
        let r = b.bin(BinFn::Sub, m, a);
        let p = b.finish(r);
        assert_eq!(p.regs, 2);
        let x: Vec<f32> = (0..5000).map(|i| i as f32 * 0.5).collect();
        let mut out = vec![0.0; 5000];
        p.eval(&[&x], &DateVars::default(), &mut out);
        for i in [0usize, 7, 4999] {
            let v = x[i];
            assert_eq!(out[i], (v + v) * (v + v) - v);
        }
        // a date variable
        let mut b: Builder<i32> = Builder::default();
        let y = b.var(DateVar::Month);
        let p = b.finish(y);
        assert!(p.uses_date());
        let vars = DateVars::new(Some(crate::dates::days_from_civil(2020, 7, 14) as f64 * 86400.0), 3);
        assert_eq!(p.eval_one(&[], &vars), 7.0);
        assert_eq!(vars.get(DateVar::Doy), 196.0);
    }
}

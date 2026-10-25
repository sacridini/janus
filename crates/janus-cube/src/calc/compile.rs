//! From the syntax tree to the graph: names resolved by a `Scope` (a layer's bands; the open layers; the inputs
//! of a canonical text), statements and rules, functions checked against their arity, whether each value is a
//! series or one image, reductions over time compiled as bodies of their own.

use std::collections::HashMap;
use std::ops::Range;

use super::funcs::{self, Kind};
use super::parse::{Ast, BinOp, CalcError, Expr, ExprKind, Stmt, UnOp};
use super::program::{BinFn, Builder, DateVar, NodeId, Program, TernFn, UnFn};

/// A compiled value: its node, and the layer whose dates it follows (None: the same at every date).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Value {
    pub node: NodeId,
    pub series: Option<usize>,
}

/// What follows a layer's name: `.name` or `[k]`.
#[derive(Clone, Debug)]
pub enum Member<'a> {
    Name(&'a str, Range<usize>),
    Index(i64, Range<usize>),
}

/// A reduction over the dates of a series.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RedFn {
    Min,
    Max,
    Mean,
    Median,
    Sum,
    Std,
    Count,
    Percentile(f32),
}

impl RedFn {
    pub fn name(self) -> String {
        match self {
            RedFn::Min => "min".into(),
            RedFn::Max => "max".into(),
            RedFn::Mean => "mean".into(),
            RedFn::Median => "median".into(),
            RedFn::Sum => "sum".into(),
            RedFn::Std => "std".into(),
            RedFn::Count => "count".into(),
            RedFn::Percentile(p) => format!("p{p}"),
        }
    }

    pub fn parse(s: &str) -> Option<RedFn> {
        Some(match s {
            "min" => RedFn::Min,
            "max" => RedFn::Max,
            "mean" => RedFn::Mean,
            "median" => RedFn::Median,
            "sum" => RedFn::Sum,
            "std" => RedFn::Std,
            "count" => RedFn::Count,
            _ => RedFn::Percentile(s.strip_prefix('p')?.parse().ok()?),
        })
    }
}

/// A reduction's argument compiled on its own: computed at every date of `series`, then reduced.
pub struct Body<K> {
    pub program: Program,
    pub inputs: Vec<K>,
    pub series: usize,
}

/// Where the names of an expression come from.
pub trait Scope {
    type K: PartialEq + Clone;

    /// A name that is not a variable of the text: Some if it means something here (a band, a layer).
    fn name(&mut self, b: &mut Builder<Self::K>, name: &str, span: &Range<usize>) -> Option<Result<Value, CalcError>>;

    /// A layer, by its quoted name or its short name, and what follows it.
    fn layer(
        &mut self,
        b: &mut Builder<Self::K>,
        layer: &str,
        span: &Range<usize>,
        member: Option<Member>,
    ) -> Result<Value, CalcError> {
        let _ = (b, member);
        Err(CalcError::new(span.clone(), format!("layers (\"{layer}\") are named in Tools > Raster calculator only")))
    }

    /// Whether a name is a layer's short name (`img.band1`).
    fn is_layer(&self, name: &str) -> bool {
        let _ = name;
        false
    }

    /// `$k` (canonical texts only).
    fn input(&mut self, b: &mut Builder<Self::K>, k: u32, span: &Range<usize>) -> Result<Value, CalcError> {
        let _ = (b, k);
        Err(CalcError::new(span.clone(), "'$' is not understood"))
    }

    /// A reduction over time of a series; a scope without reductions refuses it.
    fn reduce(
        &mut self,
        b: &mut Builder<Self::K>,
        f: RedFn,
        body: Body<Self::K>,
        span: &Range<usize>,
    ) -> Result<Value, CalcError> {
        let _ = (b, f, body);
        Err(CalcError::new(span.clone(), "reductions over time are computed in Tools > Raster calculator"))
    }

    /// Names to suggest when one is not known.
    fn suggestions(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Local {
    name: String,
    expr: Expr,
    /// the variables defined before it (those it sees)
    visible: usize,
}

struct Compiler<'s, S: Scope> {
    scope: &'s mut S,
    locals: Vec<Local>,
    /// each builder's compiled variables (the reductions' bodies have builders of their own)
    memo: Vec<HashMap<usize, Value>>,
}

fn err(span: &Range<usize>, m: impl Into<String>) -> CalcError {
    CalcError::new(span.clone(), m)
}

fn is_nan_literal(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Name(n) if n.eq_ignore_ascii_case("nan"))
}

/// The value a rule starts from without a result yet: its condition's first operand (`img` of `img > 0`).
fn leftmost(e: &Expr) -> &Expr {
    match &e.kind {
        ExprKind::Binary(op, a, _)
            if !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Pow) =>
        {
            leftmost(a)
        }
        ExprKind::Unary(UnOp::Not, a) => leftmost(a),
        _ => e,
    }
}

/// The edit distance of two short names (suggestions).
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let c = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + c);
        }
        prev = cur;
    }
    prev[b.len()]
}

impl<S: Scope> Compiler<'_, S> {
    fn combine(a: Option<usize>, b: Option<usize>) -> Option<usize> {
        a.or(b)
    }

    fn un(&self, b: &mut Builder<S::K>, f: UnFn, a: Value) -> Value {
        Value { node: b.un(f, a.node), series: a.series }
    }

    fn bin(&self, b: &mut Builder<S::K>, f: BinFn, x: Value, y: Value) -> Value {
        Value { node: b.bin(f, x.node, y.node), series: Self::combine(x.series, y.series) }
    }

    fn tern(&self, b: &mut Builder<S::K>, f: TernFn, x: Value, y: Value, z: Value) -> Value {
        Value {
            node: b.tern(f, x.node, y.node, z.node),
            series: Self::combine(Self::combine(x.series, y.series), z.series),
        }
    }

    fn konst(&self, b: &mut Builder<S::K>, v: f32) -> Value {
        Value { node: b.konst(v), series: None }
    }

    fn local(&mut self, b: &mut Builder<S::K>, i: usize) -> Result<Value, CalcError> {
        if let Some(v) = self.memo.last().and_then(|m| m.get(&i)) {
            return Ok(*v);
        }
        let (expr, visible) = (self.locals[i].expr.clone(), self.locals[i].visible);
        let v = self.expr(b, &expr, visible)?;
        self.memo.last_mut().unwrap().insert(i, v);
        Ok(v)
    }

    fn find_local(&self, name: &str, env: usize) -> Option<usize> {
        (0..env).rev().find(|&i| self.locals[i].name == name)
    }

    fn unknown(&self, name: &str, span: &Range<usize>, env: usize) -> CalcError {
        let mut known: Vec<String> = self.scope.suggestions();
        known.extend(self.locals[..env].iter().map(|l| l.name.clone()));
        known.extend(funcs::DATE_VARS.iter().map(|v| v.0.to_string()));
        known.extend(funcs::CONSTANTS.iter().map(|c| c.0.to_string()));
        let best = known
            .iter()
            .filter(|k| k.as_str() != name)
            .map(|k| (distance(name, k), k))
            .filter(|(d, k)| *d <= 2.max(k.len() / 3) && *d < name.len().max(2))
            .min_by_key(|(d, _)| *d);
        match best {
            Some((_, k)) => err(span, format!("'{name}' is not a band, layer or variable here: did you mean '{k}'?")),
            None => err(span, format!("'{name}' is not a band, layer or variable here")),
        }
    }

    fn name(&mut self, b: &mut Builder<S::K>, name: &str, span: &Range<usize>, env: usize) -> Result<Value, CalcError> {
        if let Some(i) = self.find_local(name, env) {
            return self.local(b, i);
        }
        if name == funcs::RESULT {
            return Err(err(span, "there is no result yet: start with a rule (cond = value) or result = ..."));
        }
        if let Some(v) = self.scope.name(b, name, span) {
            return v;
        }
        if let Some(v) = DateVar::of(name) {
            return Ok(Value { node: b.var(v), series: None });
        }
        if let Some(c) = funcs::constant(name) {
            return Ok(self.konst(b, c));
        }
        if funcs::func(name).is_some() {
            return Err(err(span, format!("{name} is a function: {name}(...)")));
        }
        Err(self.unknown(name, span, env))
    }

    /// A layer named by its base (quoted, or its short name) and what follows it.
    fn layer_of(
        &mut self,
        b: &mut Builder<S::K>,
        base: &Expr,
        member: Member,
        span: &Range<usize>,
        env: usize,
    ) -> Result<Value, CalcError> {
        match &base.kind {
            ExprKind::Layer(l) => self.scope.layer(b, l, span, Some(member)),
            // a layer, or a name of none (the scope says which are open)
            ExprKind::Name(n) if self.find_local(n, env).is_none() => self.scope.layer(b, n, &base.span, Some(member)),
            _ => Err(err(&base.span, "only a layer has bands to pick (\"layer\".band or layer.band)")),
        }
    }

    fn expr(&mut self, b: &mut Builder<S::K>, e: &Expr, env: usize) -> Result<Value, CalcError> {
        let span = &e.span;
        match &e.kind {
            ExprKind::Num(v) => Ok(self.konst(b, *v as f32)),
            ExprKind::Name(n) => self.name(b, n, span, env),
            ExprKind::Layer(l) => self.scope.layer(b, l, span, None),
            ExprKind::Input(k) => self.scope.input(b, *k, span),
            ExprKind::Member { base, member, member_span } => {
                self.layer_of(b, base, Member::Name(member, member_span.clone()), span, env)
            }
            ExprKind::Index { base, index } => {
                let k = match index.kind {
                    ExprKind::Num(v) if v.fract() == 0.0 => v as i64,
                    _ => return Err(err(&index.span, "a band's number (1, 2...) goes between the brackets")),
                };
                self.layer_of(b, base, Member::Index(k, index.span.clone()), span, env)
            }
            ExprKind::Unary(op, a) => {
                let a = self.expr(b, a, env)?;
                Ok(self.un(b, if *op == UnOp::Neg { UnFn::Neg } else { UnFn::Not }, a))
            }
            ExprKind::Binary(op, x, y) => {
                // `x == nan`: whether x is no data (NaN never equals itself)
                if matches!(op, BinOp::Eq | BinOp::Ne) && (is_nan_literal(x) || is_nan_literal(y)) {
                    let other = if is_nan_literal(x) { y } else { x };
                    let v = self.expr(b, other, env)?;
                    return Ok(self.un(b, if *op == BinOp::Eq { UnFn::IsNan } else { UnFn::IsValid }, v));
                }
                let (x, y) = (self.expr(b, x, env)?, self.expr(b, y, env)?);
                let f = match op {
                    BinOp::Add => BinFn::Add,
                    BinOp::Sub => BinFn::Sub,
                    BinOp::Mul => BinFn::Mul,
                    BinOp::Div => BinFn::Div,
                    BinOp::Mod => BinFn::Mod,
                    BinOp::Pow => BinFn::Pow,
                    BinOp::Lt => BinFn::Lt,
                    BinOp::Le => BinFn::Le,
                    BinOp::Gt => BinFn::Gt,
                    BinOp::Ge => BinFn::Ge,
                    BinOp::Eq => BinFn::Eq,
                    BinOp::Ne => BinFn::Ne,
                    BinOp::And => BinFn::And,
                    BinOp::Or => BinFn::Or,
                };
                Ok(self.bin(b, f, x, y))
            }
            ExprKind::Call { name, name_span, args } => self.call(b, name, name_span, args, span, env),
        }
    }

    fn call(
        &mut self,
        b: &mut Builder<S::K>,
        name: &str,
        name_span: &Range<usize>,
        args: &[Expr],
        span: &Range<usize>,
        env: usize,
    ) -> Result<Value, CalcError> {
        let Some(f) = funcs::func(name) else {
            if self.find_local(name, env).is_some() || DateVar::of(name).is_some() {
                return Err(err(name_span, format!("{name} is a value, not a function")));
            }
            let mut e = self.unknown(name, name_span, env);
            e.message = e.message.replace("is not a band, layer or variable here", "is not a function");
            return Err(e);
        };
        let n = args.len();
        if n < f.arity.0 || n > f.arity.1 {
            let want = if f.arity.0 == f.arity.1 {
                format!("{} argument{}", f.arity.0, if f.arity.0 == 1 { "" } else { "s" })
            } else {
                format!("at least {}", f.arity.0)
            };
            return Err(err(span, format!("{name}({}) takes {want}, given {n}", f.args)));
        }
        let reduction = match (f.kind, name) {
            (Kind::Many, "min") if n == 1 => Some(RedFn::Min),
            (Kind::Many, "max") if n == 1 => Some(RedFn::Max),
            (Kind::Many, "mean") if n == 1 => Some(RedFn::Mean),
            (Kind::Many, "sum") if n == 1 => Some(RedFn::Sum),
            (Kind::Reduce, "median") => Some(RedFn::Median),
            (Kind::Reduce, "std") => Some(RedFn::Std),
            (Kind::Reduce, "count") => Some(RedFn::Count),
            (Kind::Reduce, "percentile") => {
                let p = self.expr(b, &args[1], env)?;
                match b.nodes[p.node as usize] {
                    super::program::Node::Const(v) if (0.0..=100.0).contains(&v) => Some(RedFn::Percentile(v)),
                    _ => return Err(err(&args[1].span, "the percentile is a number from 0 to 100")),
                }
            }
            _ => None,
        };
        if let Some(r) = reduction {
            return self.reduction(b, r, &args[0], span, env);
        }
        let mut vals = Vec::with_capacity(n);
        for (i, a) in args.iter().enumerate() {
            // qa's rule by name
            if f.kind == Kind::Qa && i == 2 {
                let code = match &a.kind {
                    ExprKind::Layer(r) => match r.to_lowercase().as_str() {
                        "fmask" => 1.0,
                        "landsat_c2" | "landsat" | "qa_pixel" => 2.0,
                        "nonzero" | "mask" => 3.0,
                        "scl" => 4.0,
                        _ => return Err(err(&a.span, "the rule is \"fmask\", \"landsat_c2\", \"scl\" or \"nonzero\"")),
                    },
                    ExprKind::Num(v) if (1.0..=4.0).contains(v) => *v as f32,
                    _ => return Err(err(&a.span, "the rule is \"fmask\", \"landsat_c2\", \"scl\" or \"nonzero\"")),
                };
                vals.push(self.konst(b, code));
                continue;
            }
            vals.push(self.expr(b, a, env)?);
        }
        Ok(match f.kind {
            Kind::Un(u) => self.un(b, u, vals[0]),
            Kind::Bin(g) => self.bin(b, g, vals[0], vals[1]),
            Kind::Tern(t) => self.tern(b, t, vals[0], vals[1], vals[2]),
            Kind::Qa => self.tern(b, TernFn::Qa, vals[0], vals[1], vals[2]),
            Kind::In => {
                let x = vals[0];
                let mut acc: Option<Value> = None;
                for v in &vals[1..] {
                    let eq = self.bin(b, BinFn::Eq, x, *v);
                    acc = Some(match acc {
                        None => eq,
                        Some(a) => self.bin(b, BinFn::Or, a, eq),
                    });
                }
                acc.unwrap()
            }
            Kind::Many => {
                let g = match name {
                    "min" => BinFn::Min,
                    "max" => BinFn::Max,
                    "coalesce" => BinFn::Coalesce,
                    _ => BinFn::Add,
                };
                let mut acc = vals[0];
                for v in &vals[1..] {
                    acc = self.bin(b, g, acc, *v);
                }
                if name == "mean" {
                    let k = self.konst(b, n as f32);
                    acc = self.bin(b, BinFn::Div, acc, k);
                }
                acc
            }
            Kind::Reduce => unreachable!(),
        })
    }

    /// A reduction over time: its argument compiled as a body at every date of its series; of a value that is
    /// the same at every date, the value itself (its count: 1 where it has one).
    fn reduction(
        &mut self,
        b: &mut Builder<S::K>,
        f: RedFn,
        arg: &Expr,
        span: &Range<usize>,
        env: usize,
    ) -> Result<Value, CalcError> {
        let mut sub: Builder<S::K> = Builder::default();
        self.memo.push(HashMap::new());
        let body = self.expr(&mut sub, arg, env);
        self.memo.pop();
        let body = body?;
        let Some(series) = body.series else {
            let v = self.expr(b, arg, env)?;
            return Ok(match f {
                RedFn::Count => self.un(b, UnFn::IsValid, v),
                RedFn::Std => {
                    let ok = self.un(b, UnFn::IsValid, v);
                    let (z, nan) = (self.konst(b, 0.0), self.konst(b, f32::NAN));
                    self.tern(b, TernFn::Where, ok, z, nan)
                }
                _ => v,
            });
        };
        let program = sub.finish(body.node);
        self.scope.reduce(b, f, Body { program, inputs: sub.inputs.clone(), series }, span)
    }

    fn stmt(&mut self, b: &mut Builder<S::K>, s: &Stmt, last: bool) -> Result<Option<Value>, CalcError> {
        let env = self.locals.len();
        if let Some(cond) = &s.cond {
            // cond = value: where(cond, value, result); an unknown condition (no data) keeps the result
            let base = match self.find_local(funcs::RESULT, env) {
                Some(_) => Expr { kind: ExprKind::Name(funcs::RESULT.into()), span: cond.span.clone() },
                None => leftmost(cond).clone(),
            };
            let at = cond.span.clone();
            let call = |name: &str, args: Vec<Expr>| Expr {
                kind: ExprKind::Call { name: name.into(), name_span: at.clone(), args },
                span: at.clone(),
            };
            let zero = Expr { kind: ExprKind::Num(0.0), span: at.clone() };
            let known = call("coalesce", vec![cond.clone(), zero]);
            let rule = call("where", vec![known, s.expr.clone(), base]);
            self.locals.push(Local { name: funcs::RESULT.into(), expr: rule, visible: env });
            return if last { self.local(b, env).map(Some) } else { Ok(None) };
        }
        if let Some((name, span)) = &s.target {
            if funcs::reserved(name) {
                return Err(err(span, format!("'{name}' is a function's name: give the value another")));
            }
            if DateVar::of(name).is_some() || funcs::CONSTANTS.iter().any(|c| c.0 == name) {
                return Err(err(span, format!("'{name}' is a variable of Janus: give the value another name")));
            }
            self.locals.push(Local { name: name.clone(), expr: s.expr.clone(), visible: env });
            // checked now, so an error points at its line even when the value is not used
            let v = self.local(b, env)?;
            return Ok(last.then_some(v));
        }
        let v = self.expr(b, &s.expr, env)?;
        Ok(last.then_some(v))
    }
}

/// A compiled text: the graph's builder, its result, the name given to it.
pub struct Compiled<K> {
    pub builder: Builder<K>,
    pub value: Value,
    pub name: Option<String>,
}

impl<K: PartialEq + Clone> Compiled<K> {
    pub fn program(&self) -> Program {
        self.builder.finish(self.value.node)
    }
}

/// Compiles a parsed text in a scope.
pub fn compile<S: Scope>(ast: &Ast, scope: &mut S) -> Result<Compiled<S::K>, CalcError> {
    compile_with(ast, scope, Builder::default())
}

/// As `compile`, the builder given (its inputs already placed: a canonical text's $1, $2...).
pub fn compile_with<S: Scope>(ast: &Ast, scope: &mut S, mut b: Builder<S::K>) -> Result<Compiled<S::K>, CalcError> {
    let mut c = Compiler { scope, locals: Vec::new(), memo: vec![HashMap::new()] };
    let mut value = None;
    let n = ast.stmts.len();
    for (i, s) in ast.stmts.iter().enumerate() {
        if let Some(v) = c.stmt(&mut b, s, i + 1 == n)? {
            value = Some(v);
        }
    }
    let value = value.expect("the last statement gives the value");
    let name = match ast.stmts.last() {
        Some(s) if s.cond.is_some() => Some(funcs::RESULT.to_string()),
        Some(s) => s.target.as_ref().map(|t| t.0.clone()),
        None => None,
    };
    Ok(Compiled { builder: b, value, name })
}

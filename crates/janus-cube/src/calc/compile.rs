//! From the syntax tree to the graph: names resolved by a `Scope` (a layer's bands; the open layers; the inputs
//! of a canonical text), statements and rules, functions checked against their arity, whether each value is a
//! series or one image, reductions over time compiled as bodies of their own.

use std::collections::HashMap;
use std::ops::Range;

use super::funcs::{self, Kind};
use super::parse::{Ast, BinOp, CalcError, Expr, ExprKind, Stmt, UnOp};
use super::program::{BinFn, Builder, DateVar, NodeId, Program, TernFn, UnFn};

/// The series of the periods of a reduction `by year` (`by month`): not a layer's dates but new ones, one per
/// period (Value::series).
pub const BY_YEAR: usize = usize::MAX - 1;
pub const BY_MONTH: usize = usize::MAX - 2;

/// Whether a value's series is that of periods (`by year`, `by month`).
pub fn is_periods(series: usize) -> bool {
    series >= BY_MONTH
}

/// A period a series is grouped by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Year,
    Month,
}

impl Period {
    pub fn series(self) -> usize {
        match self {
            Period::Year => BY_YEAR,
            Period::Month => BY_MONTH,
        }
    }

    pub fn of_series(s: usize) -> Option<Period> {
        match s {
            BY_YEAR => Some(Period::Year),
            BY_MONTH => Some(Period::Month),
            _ => None,
        }
    }

    /// The period of a date (Unix seconds): its year, or its year * 12 + month - 1.
    pub fn key(self, time: f64) -> i64 {
        let (y, m, _) = crate::dates::civil_from_days((time / 86400.0).floor() as i64);
        match self {
            Period::Year => i64::from(y),
            Period::Month => i64::from(y) * 12 + i64::from(m) - 1,
        }
    }

    /// A period's first day (Unix seconds) and its label.
    pub fn start(self, key: i64) -> (f64, String) {
        let (y, m) = match self {
            Period::Year => (key as i32, 1),
            Period::Month => (key.div_euclid(12) as i32, key.rem_euclid(12) as u32 + 1),
        };
        let t = crate::dates::days_from_civil(y, m, 1) as f64 * 86400.0;
        (t, if self == Period::Year { format!("{y}") } else { format!("{y}-{m:02}") })
    }
}

/// A point of a calendar slice: a day, a month or a year; or a month of every year.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum When {
    Day(i32, u32, u32),
    Month(i32, u32),
    Year(i32),
    MonthOfYear(u32),
}

/// The dates of a calendar slice, from the start of one point to the end of the other (`ndvi[2019-06 .. 2019-09]`:
/// June 1 to September 30; `ndvi[11 .. 2]`: November to February of every year).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    pub from: When,
    pub to: When,
}

impl Slice {
    pub fn contains(&self, time: f64) -> bool {
        let days = (time / 86400.0).floor() as i64;
        let (_, m, _) = crate::dates::civil_from_days(days);
        let first = |w: When| match w {
            When::Day(y, m, d) => crate::dates::days_from_civil(y, m, d),
            When::Month(y, m) => crate::dates::days_from_civil(y, m, 1),
            When::Year(y) => crate::dates::days_from_civil(y, 1, 1),
            When::MonthOfYear(_) => i64::MIN,
        };
        let last = |w: When| match w {
            When::Day(y, m, d) => crate::dates::days_from_civil(y, m, d),
            When::Month(y, m) => {
                let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
                crate::dates::days_from_civil(ny, nm, 1) - 1
            }
            When::Year(y) => crate::dates::days_from_civil(y, 12, 31),
            When::MonthOfYear(_) => i64::MAX,
        };
        match (self.from, self.to) {
            (When::MonthOfYear(a), When::MonthOfYear(b)) if a <= b => (a..=b).contains(&m),
            (When::MonthOfYear(a), When::MonthOfYear(b)) => m >= a || m <= b,
            (a, b) => days >= first(a) && days <= last(b),
        }
    }
}

/// How a climatology groups its base: by month of the year, or by the days of the year within this many.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClimBy {
    Month,
    Doy(f64),
}

/// The dates a reduction is over.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Over {
    /// every date (of its slice): an image
    All,
    /// each year or month: a series of the periods
    By(Period),
    /// the dates of the last this many days up to each date (pandas' rolling("90D")): a series
    Window(f64),
    /// the date this many days before each date (or the same day `years` years before), the nearest within a
    /// tolerance: a series
    LagTime { days: f64, years: i32 },
    /// the dates before each one, of which the k-th with a value is taken (`ndvi[-1]`): a series
    LagObs(usize),
    /// the dates up to each one (persist's last observations): a series
    Upto(usize),
    /// the base years' dates of each date's month (or days of the year): a series
    Clim { from: i32, to: i32, by: ClimBy },
}

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
    /// the date (decimal year, or the place of a series without dates) of the largest, the smallest value
    ArgMax,
    ArgMin,
    /// the date of the first, the last date the value is true (not 0)
    First,
    Last,
    /// the longest run of dates the value is true, in days: from its first date to the next date it is not (or the
    /// last date)
    Longest,
    /// 1 where the value was true at the last k dates with a value, 0 where not
    Persist(u32),
    /// the k-th value from the end (a lag of k observations)
    Back(u32),
    /// one date: a year, a month of a year or a day (0: not given), resolved by the plan
    At {
        year: u16,
        month: u8,
        day: u8,
    },
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
            RedFn::ArgMax => "argmax".into(),
            RedFn::ArgMin => "argmin".into(),
            RedFn::First => "first".into(),
            RedFn::Last => "last".into(),
            RedFn::Longest => "longest".into(),
            RedFn::Persist(k) => format!("persist{k}"),
            RedFn::Back(k) => format!("back{k}"),
            RedFn::At { year, month, day } => match (month, day) {
                (0, _) => format!("at{year}"),
                (m, 0) => format!("at{year}{m:02}"),
                (m, d) => format!("at{year}{m:02}{d:02}"),
            },
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
            "argmax" => RedFn::ArgMax,
            "argmin" => RedFn::ArgMin,
            "first" => RedFn::First,
            "last" => RedFn::Last,
            "longest" => RedFn::Longest,
            _ if s.starts_with("persist") => RedFn::Persist(s["persist".len()..].parse().ok()?),
            _ if s.starts_with("back") => RedFn::Back(s["back".len()..].parse().ok()?),
            _ => RedFn::Percentile(s.strip_prefix('p')?.parse().ok()?),
        })
    }
}

/// A reduction's argument compiled on its own: computed at every date of `series` (of its slice), then reduced
/// over the dates `over` says.
#[derive(Clone)]
pub struct Body<K> {
    pub program: Program,
    pub inputs: Vec<K>,
    pub series: usize,
    pub over: Over,
    pub slice: Option<Slice>,
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
    /// each reduction's calendar slice, as its body is compiled
    slices: Vec<Option<Slice>>,
}

fn err(span: &Range<usize>, m: impl Into<String>) -> CalcError {
    CalcError::new(span.clone(), m)
}

fn is_nan_literal(e: &Expr) -> bool {
    matches!(&e.kind, ExprKind::Name(n) if n.eq_ignore_ascii_case("nan"))
}

/// Days of a duration (`90d`, `2w`, `1y`).
fn duration_days(n: f64, unit: char) -> f64 {
    match unit {
        'w' => n * 7.0,
        'y' => n * 365.25,
        _ => n,
    }
}

/// A point of a calendar slice: 2019, 2019-06, 2019-06-15, or a month of every year (6).
fn when(e: &Expr) -> Result<When, CalcError> {
    let int = |e: &Expr| match e.kind {
        ExprKind::Num(v) if v.fract() == 0.0 && v >= 0.0 => Some(v as i64),
        _ => None,
    };
    let bad = || err(&e.span, "a date is 2019, 2019-06 or 2019-06-15 (a month of every year: 6)");
    let year = |v: Option<i64>| v.filter(|y| (1000..=9999).contains(y)).map(|y| y as i32);
    let month = |v: Option<i64>| v.filter(|m| (1..=12).contains(m)).map(|m| m as u32);
    match &e.kind {
        ExprKind::Num(_) => match int(e) {
            Some(m @ 1..=12) => Ok(When::MonthOfYear(m as u32)),
            v => year(v).map(When::Year).ok_or_else(bad),
        },
        ExprKind::Binary(BinOp::Sub, a, d) => match &a.kind {
            ExprKind::Binary(BinOp::Sub, y, m) => {
                let day = int(d).filter(|d| (1..=31).contains(d)).map(|d| d as u32);
                match (year(int(y)), month(int(m)), day) {
                    (Some(y), Some(m), Some(d)) => Ok(When::Day(y, m, d)),
                    _ => Err(bad()),
                }
            }
            _ => match (year(int(a)), month(int(d))) {
                (Some(y), Some(m)) => Ok(When::Month(y, m)),
                _ => Err(bad()),
            },
        },
        _ => Err(bad()),
    }
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
                // a calendar slice: the dates the reduction around it is over
                if let ExprKind::Range(from, to) = &index.kind {
                    let slice = Slice { from: when(from)?, to: when(to)? };
                    if matches!(slice.from, When::MonthOfYear(_)) != matches!(slice.to, When::MonthOfYear(_)) {
                        return Err(err(
                            &index.span,
                            "both dates of every year (6 .. 9) or both dates (2019-06 .. 2019-09)",
                        ));
                    }
                    if self.slices.len() < 2 {
                        return Err(err(
                            span,
                            "a slice of dates goes in a reduction: median(ndvi[2019-06 .. 2019-09])",
                        ));
                    }
                    let v = self.expr(b, base, env)?;
                    if v.series.is_none() {
                        return Err(err(&base.span, "only a series has dates to slice"));
                    }
                    let cur = self.slices.last_mut().unwrap();
                    if cur.is_some_and(|c| c != slice) {
                        return Err(err(&index.span, "one slice of dates per reduction"));
                    }
                    *cur = Some(slice);
                    return Ok(v);
                }
                // the value k observations before: ndvi[-1]
                if let ExprKind::Unary(UnOp::Neg, k) = &index.kind
                    && let ExprKind::Num(k) = k.kind
                    && k.fract() == 0.0
                    && k >= 1.0
                {
                    return self.reduction(b, RedFn::Back(k as u32), Over::LagObs(k as usize), base, span, env);
                }
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
                Self::shapes(&[x, y], span)?;
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
            ExprKind::By { base, period, period_span } => {
                let p = match period.as_str() {
                    "year" => Period::Year,
                    "month" => Period::Month,
                    _ => return Err(err(period_span, "by year or by month")),
                };
                let ExprKind::Call { name, name_span, args } = &base.kind else {
                    return Err(err(span, "by follows a reduction over time: max(ndvi) by year"));
                };
                self.call_over(b, name, name_span, args, span, env, Over::By(p))
            }
            ExprKind::Duration { .. } => {
                Err(err(span, "a duration goes in a window or a lag: mean(ndvi, 90d), lag(ndvi, 1y)"))
            }
            ExprKind::Range(..) => Err(err(
                span,
                "a range goes in brackets after a series (ndvi[2019-06 .. 2019-09]) or in anomaly's base=2015..2019",
            )),
            ExprKind::Keyword { name, .. } => Err(err(span, format!("{name}= is an option of anomaly(...) only"))),
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
        self.call_over(b, name, name_span, args, span, env, Over::All)
    }

    /// A call; `over`: by year or month (`max(ndvi) by year`), every date otherwise.
    #[allow(clippy::too_many_arguments)]
    fn call_over(
        &mut self,
        b: &mut Builder<S::K>,
        name: &str,
        name_span: &Range<usize>,
        args: &[Expr],
        span: &Range<usize>,
        env: usize,
        over: Over,
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
            } else if f.arity.1 == usize::MAX {
                format!("at least {}", f.arity.0)
            } else {
                format!("{} to {}", f.arity.0, f.arity.1)
            };
            return Err(err(span, format!("{name}({}) takes {want}, given {n}", f.args)));
        }
        if name == "anomaly" {
            if over != Over::All {
                return Err(err(span, "anomaly gives a series of the same dates: no by"));
            }
            return self.anomaly(b, args, span, env);
        }
        if let Some(k) = args.iter().find(|a| matches!(a.kind, ExprKind::Keyword { .. })) {
            return Err(err(&k.span, format!("{name}() takes no option by name")));
        }
        let int = |e: &Expr| match e.kind {
            ExprKind::Num(v) if v.fract() == 0.0 && v >= 1.0 => Some(v as usize),
            _ => None,
        };
        // a lag, persist: their own dates
        let own = match name {
            "lag" => Some(match &args[1].kind {
                ExprKind::Duration { n, unit: 'y' } if n.fract() == 0.0 && *n >= 1.0 => {
                    (RedFn::Back(1), Over::LagTime { days: *n * 365.25, years: *n as i32 })
                }
                ExprKind::Duration { n, unit } if *n > 0.0 => {
                    (RedFn::Back(1), Over::LagTime { days: duration_days(*n, *unit), years: 0 })
                }
                _ => {
                    let k = int(&args[1]).ok_or_else(|| {
                        err(&args[1].span, "a lag is a number of observations (1, 2...) or a duration (30d, 1y)")
                    })?;
                    (RedFn::Back(k as u32), Over::LagObs(k))
                }
            }),
            "persist" => {
                let k = int(&args[1])
                    .ok_or_else(|| err(&args[1].span, "persist(cond, k): k observations in a row (1, 2...)"))?;
                Some((RedFn::Persist(k as u32), Over::Upto(k)))
            }
            _ => None,
        };
        if let Some((r, o)) = own {
            if over != Over::All {
                return Err(err(span, format!("{name} gives a series of the same dates: no by")));
            }
            return self.reduction(b, r, o, &args[0], span, env);
        }
        // a window of days: the last argument
        let window = match args.last().map(|a| &a.kind) {
            Some(ExprKind::Duration { n: d, unit }) if n >= 2 => Some(duration_days(*d, *unit)),
            _ => None,
        };
        let core = n - usize::from(window.is_some());
        if let Some(d) = args[..core].iter().find(|a| matches!(a.kind, ExprKind::Duration { .. })) {
            return Err(err(&d.span, "a duration is a window, the last argument: mean(ndvi, 90d)"));
        }
        let reduction = match (f.kind, name) {
            (Kind::Many, "min") if core == 1 => Some(RedFn::Min),
            (Kind::Many, "max") if core == 1 => Some(RedFn::Max),
            (Kind::Many, "mean") if core == 1 => Some(RedFn::Mean),
            (Kind::Many, "sum") if core == 1 => Some(RedFn::Sum),
            (Kind::Reduce, "median") => Some(RedFn::Median),
            (Kind::Reduce, "std") => Some(RedFn::Std),
            (Kind::Reduce, "count") => Some(RedFn::Count),
            (Kind::Reduce, "argmax") => Some(RedFn::ArgMax),
            (Kind::Reduce, "argmin") => Some(RedFn::ArgMin),
            (Kind::Reduce, "first") => Some(RedFn::First),
            (Kind::Reduce, "last") => Some(RedFn::Last),
            (Kind::Reduce, "longest") => Some(RedFn::Longest),
            (Kind::Reduce, "at") => {
                let when = match args[1].kind {
                    ExprKind::Num(v) if v.fract() == 0.0 && v > 0.0 => v as u64,
                    _ => 0,
                };
                let (year, month, day) = match when {
                    1000..=9999 => (when, 0, 0),
                    100001..=999912 => (when / 100, when % 100, 0),
                    10000101..=99991231 => (when / 10000, when / 100 % 100, when % 100),
                    _ => (0, 0, 0),
                };
                if year == 0 || month > 12 || day > 31 || (month == 0 && day > 0) {
                    return Err(err(&args[1].span, "the date is a year (2003), a month (200307) or a day (20030715)"));
                }
                Some(RedFn::At { year: year as u16, month: month as u8, day: day as u8 })
            }
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
            let o = match (window, over) {
                (Some(_), Over::By(_)) => return Err(err(span, "a window and by: one or the other")),
                (Some(_), _) if matches!(r, RedFn::At { .. } | RedFn::ArgMax | RedFn::ArgMin) => {
                    return Err(err(span, format!("{name} takes no window")));
                }
                (Some(d), _) => Over::Window(d),
                (None, o) => o,
            };
            if matches!(r, RedFn::At { .. }) && o != Over::All {
                return Err(err(span, "at() is one date: no by"));
            }
            return self.reduction(b, r, o, &args[0], span, env);
        }
        if over != Over::All {
            return Err(err(span, "by follows a reduction over time: max(ndvi) by year"));
        }
        if window.is_some() {
            return Err(err(&args[n - 1].span, format!("{name}() takes no window")));
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
        Self::shapes(&vals, span)?;
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
            Kind::Reduce => return Err(err(span, format!("{name}({}) is over time", f.args))),
        })
    }

    /// Values computed together: a series of periods (by year) goes with images and the same periods only.
    fn shapes(vals: &[Value], span: &Range<usize>) -> Result<(), CalcError> {
        let mut seen: Option<usize> = None;
        for v in vals {
            let Some(s) = v.series else { continue };
            match seen {
                Some(a) if a != s && (is_periods(a) || is_periods(s)) => {
                    let what = |x: usize| match Period::of_series(x) {
                        Some(Period::Year) => "a series by year",
                        Some(Period::Month) => "a series by month",
                        None => "a series of dates",
                    };
                    return Err(err(span, format!("{} and {} have different dates", what(a), what(s))));
                }
                _ => seen = Some(s),
            }
        }
        Ok(())
    }

    /// anomaly(x, base=2015..2019, by=month): each date's z-score against the base years' dates of its month (by=doy:
    /// of its days of the year within window=15d).
    fn anomaly(
        &mut self,
        b: &mut Builder<S::K>,
        args: &[Expr],
        span: &Range<usize>,
        env: usize,
    ) -> Result<Value, CalcError> {
        let usage = "anomaly(series, base=2015..2019, by=month)";
        let (mut x, mut base, mut by, mut window) = (None, (i32::MIN, i32::MAX), "month", 15.0);
        for a in args {
            match &a.kind {
                ExprKind::Keyword { name, value } => match name.as_str() {
                    "base" => {
                        let year = |e: &Expr| match e.kind {
                            ExprKind::Num(v) if v.fract() == 0.0 && (1000.0..=9999.0).contains(&v) => Some(v as i32),
                            _ => None,
                        };
                        let (f, t) = match &value.kind {
                            ExprKind::Range(f, t) => (year(f), year(t)),
                            _ => (year(value), year(value)),
                        };
                        base = f
                            .zip(t)
                            .filter(|(f, t)| f <= t)
                            .ok_or_else(|| err(&value.span, "the base is a year or years: base=2015..2019"))?;
                    }
                    "by" => {
                        by = match &value.kind {
                            ExprKind::Name(n) if n == "month" => "month",
                            ExprKind::Name(n) if n == "doy" => "doy",
                            _ => return Err(err(&value.span, "by=month or by=doy")),
                        }
                    }
                    "window" => {
                        window = match value.kind {
                            ExprKind::Duration { n, unit } if n > 0.0 => duration_days(n, unit),
                            _ => return Err(err(&value.span, "the window is a number of days: window=15d")),
                        }
                    }
                    _ => return Err(err(&a.span, format!("anomaly's options are base, by and window: {usage}"))),
                },
                _ if x.is_none() => x = Some(a),
                _ => return Err(err(&a.span, format!("one series, then options: {usage}"))),
            }
        }
        let x = x.ok_or_else(|| err(span, format!("which series? {usage}")))?;
        let by = if by == "doy" { ClimBy::Doy(window) } else { ClimBy::Month };
        let over = Over::Clim { from: base.0, to: base.1, by };
        let m = self.reduction(b, RedFn::Mean, over, x, span, env)?;
        let s = self.reduction(b, RedFn::Std, over, x, span, env)?;
        let v = self.expr(b, x, env)?;
        if v.series.is_none() {
            return Err(err(&x.span, "an anomaly is of a series"));
        }
        let d = self.bin(b, BinFn::Sub, v, m);
        Ok(self.bin(b, BinFn::Div, d, s))
    }

    /// A reduction over time: its argument compiled as a body at every date of its series (of its slice), reduced
    /// over the dates `over` says; of a value that is the same at every date, the value itself (its count: 1 where
    /// it has one).
    fn reduction(
        &mut self,
        b: &mut Builder<S::K>,
        f: RedFn,
        over: Over,
        arg: &Expr,
        span: &Range<usize>,
        env: usize,
    ) -> Result<Value, CalcError> {
        let mut sub: Builder<S::K> = Builder::default();
        self.memo.push(HashMap::new());
        self.slices.push(None);
        let body = self.expr(&mut sub, arg, env);
        self.memo.pop();
        let slice = self.slices.pop().flatten();
        let body = body?;
        let Some(series) = body.series else {
            if matches!(f, RedFn::ArgMax | RedFn::ArgMin | RedFn::First | RedFn::Last | RedFn::Longest) {
                return Err(err(&arg.span, "this is the same at every date: a series has dates to find"));
            }
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
        if is_periods(series) && matches!(over, Over::By(_) | Over::Clim { .. }) {
            return Err(err(span, "a series by year (month) is grouped already"));
        }
        let program = sub.finish(body.node);
        let v = self.scope.reduce(b, f, Body { program, inputs: sub.inputs.clone(), series, over, slice }, span)?;
        Ok(Value {
            node: v.node,
            series: match over {
                Over::All => None,
                Over::By(p) => Some(p.series()),
                _ => Some(series),
            },
        })
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
    let mut c = Compiler { scope, locals: Vec::new(), memo: vec![HashMap::new()], slices: vec![None] };
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

//! The calculator's functions, constants and variables: what the compiler understands and what the editor lists,
//! completes and explains.

use super::program::{BinFn, TernFn, UnFn};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Math,
    Logic,
    Index,
    Bits,
    Time,
}

impl Group {
    pub fn name(self) -> &'static str {
        match self {
            Group::Math => "Math",
            Group::Logic => "Conditions and no data",
            Group::Index => "Indices",
            Group::Bits => "Bits and quality",
            Group::Time => "Over time (a series to one image)",
        }
    }
}

/// How a function computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Un(UnFn),
    Bin(BinFn),
    Tern(TernFn),
    /// min, max, mean, sum, coalesce: pixel by pixel over two or more values; with one argument, over time
    Many,
    /// in(x, v1, v2...)
    In,
    /// qa(x, q, "rule")
    Qa,
    /// over the dates of a series: median, std, count, percentile
    Reduce,
}

pub struct Func {
    pub name: &'static str,
    pub args: &'static str,
    pub doc: &'static str,
    pub group: Group,
    pub kind: Kind,
    /// fewest and most arguments (usize::MAX: any number)
    pub arity: (usize, usize),
}

const fn f(
    name: &'static str,
    args: &'static str,
    doc: &'static str,
    group: Group,
    kind: Kind,
    arity: (usize, usize),
) -> Func {
    Func { name, args, doc, group, kind, arity }
}

const ANY: usize = usize::MAX;

pub const FUNCS: &[Func] = &[
    f("sqrt", "x", "Square root", Group::Math, Kind::Un(UnFn::Sqrt), (1, 1)),
    f("abs", "x", "Absolute value", Group::Math, Kind::Un(UnFn::Abs), (1, 1)),
    f("exp", "x", "e to the power x", Group::Math, Kind::Un(UnFn::Exp), (1, 1)),
    f("log", "x", "Natural logarithm (as numpy's log)", Group::Math, Kind::Un(UnFn::Ln), (1, 1)),
    f("ln", "x", "Natural logarithm", Group::Math, Kind::Un(UnFn::Ln), (1, 1)),
    f("log10", "x", "Logarithm in base 10 (decibels: 10 * log10(x))", Group::Math, Kind::Un(UnFn::Log10), (1, 1)),
    f("log2", "x", "Logarithm in base 2", Group::Math, Kind::Un(UnFn::Log2), (1, 1)),
    f("pow", "x, y", "x to the power y (also x ^ y, x ** y)", Group::Math, Kind::Bin(BinFn::Pow), (2, 2)),
    f("mod", "x, y", "Remainder of x / y, with the sign of y (also x % y)", Group::Math, Kind::Bin(BinFn::Mod), (2, 2)),
    f("round", "x", "Nearest integer (halves to even, as numpy)", Group::Math, Kind::Un(UnFn::Round), (1, 1)),
    f("floor", "x", "Largest integer not above x", Group::Math, Kind::Un(UnFn::Floor), (1, 1)),
    f("ceil", "x", "Smallest integer not below x", Group::Math, Kind::Un(UnFn::Ceil), (1, 1)),
    f("trunc", "x", "x without its decimals", Group::Math, Kind::Un(UnFn::Trunc), (1, 1)),
    f("sign", "x", "-1, 0 or 1", Group::Math, Kind::Un(UnFn::Sign), (1, 1)),
    f(
        "min",
        "a, b, ...",
        "Smallest of the values; min(series): the smallest over time",
        Group::Math,
        Kind::Many,
        (1, ANY),
    ),
    f(
        "max",
        "a, b, ...",
        "Largest of the values; max(series): the largest over time",
        Group::Math,
        Kind::Many,
        (1, ANY),
    ),
    f("mean", "a, b, ...", "Mean of the values; mean(series): the mean over time", Group::Math, Kind::Many, (1, ANY)),
    f("sum", "a, b, ...", "Sum of the values; sum(series): the sum over time", Group::Math, Kind::Many, (1, ANY)),
    f("clamp", "x, lo, hi", "x limited to [lo, hi]", Group::Math, Kind::Tern(TernFn::Clamp), (3, 3)),
    f("hypot", "x, y", "sqrt(x^2 + y^2)", Group::Math, Kind::Bin(BinFn::Hypot), (2, 2)),
    f("sin", "x", "Sine (radians)", Group::Math, Kind::Un(UnFn::Sin), (1, 1)),
    f("cos", "x", "Cosine (radians)", Group::Math, Kind::Un(UnFn::Cos), (1, 1)),
    f("tan", "x", "Tangent (radians)", Group::Math, Kind::Un(UnFn::Tan), (1, 1)),
    f("asin", "x", "Arc sine", Group::Math, Kind::Un(UnFn::Asin), (1, 1)),
    f("acos", "x", "Arc cosine", Group::Math, Kind::Un(UnFn::Acos), (1, 1)),
    f("atan", "x", "Arc tangent", Group::Math, Kind::Un(UnFn::Atan), (1, 1)),
    f("atan2", "y, x", "Angle of (x, y) (radians)", Group::Math, Kind::Bin(BinFn::Atan2), (2, 2)),
    f("sinh", "x", "Hyperbolic sine", Group::Math, Kind::Un(UnFn::Sinh), (1, 1)),
    f("cosh", "x", "Hyperbolic cosine", Group::Math, Kind::Un(UnFn::Cosh), (1, 1)),
    f("tanh", "x", "Hyperbolic tangent", Group::Math, Kind::Un(UnFn::Tanh), (1, 1)),
    f(
        "nd",
        "a, b",
        "Normalized difference (a - b) / (a + b): nd(N, R) is NDVI",
        Group::Index,
        Kind::Bin(BinFn::Nd),
        (2, 2),
    ),
    f(
        "where",
        "cond, a, b",
        "a where cond is true, else b (no data where cond is)",
        Group::Logic,
        Kind::Tern(TernFn::Where),
        (3, 3),
    ),
    f("if", "cond, a, b", "Same as where", Group::Logic, Kind::Tern(TernFn::Where), (3, 3)),
    f("isnan", "x", "1 where x is no data, else 0", Group::Logic, Kind::Un(UnFn::IsNan), (1, 1)),
    f("isvalid", "x", "1 where x has a value, else 0", Group::Logic, Kind::Un(UnFn::IsValid), (1, 1)),
    f(
        "coalesce",
        "a, b, ...",
        "The first value that is not no data: coalesce(x, 0) fills the gaps",
        Group::Logic,
        Kind::Many,
        (2, ANY),
    ),
    f(
        "in",
        "x, v1, v2, ...",
        "1 where x is one of the values (classes): in(classes, 3, 4, 5)",
        Group::Logic,
        Kind::In,
        (2, ANY),
    ),
    f(
        "bit",
        "x, n",
        "Bit n of x (0 = lowest): bit(QA_PIXEL, 3) is Landsat's cloud",
        Group::Bits,
        Kind::Bin(BinFn::Bit),
        (2, 2),
    ),
    f(
        "bits",
        "x, start, count",
        "count bits of x from bit start, as a number",
        Group::Bits,
        Kind::Tern(TernFn::Bits),
        (3, 3),
    ),
    f(
        "qa",
        "x, q, \"rule\"",
        "x where the quality code q is usable, else no data; rule: \"fmask\", \"landsat_c2\", \"scl\" or \"nonzero\"",
        Group::Bits,
        Kind::Qa,
        (3, 3),
    ),
    f("median", "series", "The median over time", Group::Time, Kind::Reduce, (1, 1)),
    f(
        "std",
        "series",
        "The standard deviation over time (of the dates with a value)",
        Group::Time,
        Kind::Reduce,
        (1, 1),
    ),
    f(
        "count",
        "series",
        "How many dates it is true (not 0) on: count(ndvi < 0.3) counts the dates below 0.3, count(isvalid(ndvi)) those with a value",
        Group::Time,
        Kind::Reduce,
        (1, 1),
    ),
    f(
        "percentile",
        "series, p",
        "The p-th percentile over time (0 to 100): percentile(ndvi, 90)",
        Group::Time,
        Kind::Reduce,
        (2, 2),
    ),
];

pub fn func(name: &str) -> Option<&'static Func> {
    FUNCS.iter().find(|f| f.name == name)
}

/// The named constants (case matters, except nan).
pub const CONSTANTS: &[(&str, f32, &str)] = &[
    ("pi", std::f32::consts::PI, "3.14159..."),
    ("e", std::f32::consts::E, "2.71828..."),
    ("nan", f32::NAN, "No data (also NaN): img == nan tests for it"),
    ("inf", f32::INFINITY, "Infinity"),
];

pub fn constant(name: &str) -> Option<f32> {
    if name.eq_ignore_ascii_case("nan") {
        return Some(f32::NAN);
    }
    CONSTANTS.iter().find(|c| c.0 == name).map(|c| c.1)
}

/// The variables of the date being computed.
pub const DATE_VARS: &[(&str, &str)] = &[
    ("year", "The date's year (2020)"),
    ("month", "Its month, 1 to 12"),
    ("day", "Its day of the month"),
    ("doy", "Its day of the year, 1 to 366"),
    ("decyear", "Its decimal year (2020.54)"),
    ("t", "Its position in the series, from 0"),
];

/// The current value in rules (`cond = value` lines).
pub const RESULT: &str = "result";

/// Words that cannot name a variable.
pub fn reserved(name: &str) -> bool {
    matches!(name, "and" | "or" | "not") || func(name).is_some()
}

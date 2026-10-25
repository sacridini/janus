//! The calculator's grammar (a Pratt parser) into a syntax tree whose nodes keep their place in the text, for
//! errors pointing at a column and the editor's hints.
//!
//! ```text
//! program  := stmt ((';' | newline) stmt)*
//! stmt     := name '=' expr | cond '=' expr | expr
//! expr     := or, and, not, comparisons, + -, * / %, unary - +, ^ ** (right), postfix . [ ] ( )
//! primary  := number | name | "layer" | $k | '(' expr ')'
//! ```
//! A newline ends a statement unless inside brackets or after an operator; `#` starts a comment. `cond = value`
//! is a rule: where cond holds, the result takes the value (`img > 0 = 1`, `img == nan = 0`).

use std::ops::Range;

use super::lex::{Punct, Tok, Token, tokens};

/// An error at a place in the text.
#[derive(Clone, Debug, PartialEq)]
pub struct CalcError {
    pub span: Range<usize>,
    pub message: String,
}

impl CalcError {
    pub fn new(span: Range<usize>, message: impl Into<String>) -> Self {
        CalcError { span, message: message.into() }
    }

    /// The 1-based line and column of the error's start in `text`.
    pub fn line_col(&self, text: &str) -> (usize, usize) {
        let at = self.span.start.min(text.len());
        let before = &text[..at];
        let line = before.matches('\n').count() + 1;
        let col = before.rfind('\n').map_or(before.chars().count(), |k| before[k + 1..].chars().count()) + 1;
        (line, col)
    }
}

impl std::fmt::Display for CalcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Pow => "^",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::And => "and",
            BinOp::Or => "or",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Num(f64),
    /// a band, a role, a variable, a constant
    Name(String),
    /// `"a layer"`
    Layer(String),
    /// `$k` (1-based)
    Input(u32),
    /// `base.member`
    Member {
        base: Box<Expr>,
        member: String,
        member_span: Range<usize>,
    },
    /// `base[index]`
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call {
        name: String,
        name_span: Range<usize>,
        args: Vec<Expr>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    /// `name = ...`
    pub target: Option<(String, Range<usize>)>,
    /// a rule's condition: `cond = expr`
    pub cond: Option<Expr>,
    pub expr: Expr,
}

/// A parsed text: its statements, the last one's value is the result.
#[derive(Clone, Debug, PartialEq)]
pub struct Ast {
    pub stmts: Vec<Stmt>,
}

impl Ast {
    /// The name the result is given (`ndvi = ...` last), if any.
    pub fn result_name(&self) -> Option<&str> {
        self.stmts.last().and_then(|s| s.target.as_ref()).map(|t| t.0.as_str())
    }
}

struct Parser<'a> {
    text: &'a str,
    toks: Vec<Token>,
    pos: usize,
    /// brackets open: newlines inside them do not end a statement
    depth: usize,
}

const KW_AND: &str = "and";
const KW_OR: &str = "or";
const KW_NOT: &str = "not";

impl<'a> Parser<'a> {
    fn text_of(&self, t: &Token) -> &'a str {
        &self.text[t.span.clone()]
    }

    /// The next token, past newlines inside brackets (or always with `skip_newlines`).
    fn peek_at(&self, skip_newlines: bool) -> usize {
        let mut p = self.pos;
        while matches!(self.toks[p].tok, Tok::Newline) && (skip_newlines || self.depth > 0) {
            p += 1;
        }
        p
    }

    fn peek(&self) -> &Token {
        &self.toks[self.peek_at(false)]
    }

    fn bump(&mut self, skip_newlines: bool) -> Token {
        let p = self.peek_at(skip_newlines);
        self.pos = (p + 1).min(self.toks.len() - 1);
        self.toks[p].clone()
    }

    fn is_punct(&self, p: Punct) -> bool {
        self.peek().tok == Tok::Punct(p)
    }

    fn expect(&mut self, p: Punct, what: &str) -> Result<Token, CalcError> {
        let t = self.peek().clone();
        if t.tok == Tok::Punct(p) {
            return Ok(self.bump(false));
        }
        Err(CalcError::new(t.span.clone(), format!("expected {what}{}", self.found(&t))))
    }

    fn found(&self, t: &Token) -> String {
        match &t.tok {
            Tok::Eof => ", found the end".into(),
            Tok::Newline => ", found the end of the line".into(),
            _ => format!(", found '{}'", self.text_of(t)),
        }
    }

    /// The infix operator ahead, with its binding powers (left, right).
    fn infix(&self) -> Option<(BinOp, u8, u8)> {
        let t = self.peek();
        Some(match &t.tok {
            Tok::Ident if self.text_of(t) == KW_OR => (BinOp::Or, 1, 2),
            Tok::Ident if self.text_of(t) == KW_AND => (BinOp::And, 3, 4),
            Tok::Punct(p) => match p {
                Punct::OrOr => (BinOp::Or, 1, 2),
                Punct::AndAnd => (BinOp::And, 3, 4),
                Punct::Lt => (BinOp::Lt, 7, 8),
                Punct::Le => (BinOp::Le, 7, 8),
                Punct::Gt => (BinOp::Gt, 7, 8),
                Punct::Ge => (BinOp::Ge, 7, 8),
                Punct::EqEq => (BinOp::Eq, 7, 8),
                Punct::Ne => (BinOp::Ne, 7, 8),
                Punct::Plus => (BinOp::Add, 9, 10),
                Punct::Minus => (BinOp::Sub, 9, 10),
                Punct::Star => (BinOp::Mul, 11, 12),
                Punct::Slash => (BinOp::Div, 11, 12),
                Punct::Percent => (BinOp::Mod, 11, 12),
                // right-associative, tighter than a unary minus on its left: -x^2 = -(x^2)
                Punct::Pow => (BinOp::Pow, 16, 15),
                _ => return None,
            },
            _ => return None,
        })
    }

    fn expr(&mut self, min_bp: u8) -> Result<Expr, CalcError> {
        let mut lhs = self.prefix()?;
        loop {
            // postfix: member, index, call
            if self.is_punct(Punct::Dot) {
                self.bump(false);
                let t = self.bump(true);
                if t.tok != Tok::Ident {
                    return Err(CalcError::new(
                        t.span.clone(),
                        format!("expected a band's name after '.'{}", self.found(&t)),
                    ));
                }
                let span = lhs.span.start..t.span.end;
                lhs = Expr {
                    kind: ExprKind::Member {
                        base: Box::new(lhs),
                        member: self.text_of(&t).to_string(),
                        member_span: t.span,
                    },
                    span,
                };
                continue;
            }
            if self.is_punct(Punct::LBracket) {
                self.bump(false);
                self.depth += 1;
                let index = self.expr(0);
                let close = index.and_then(|i| self.expect(Punct::RBracket, "']'").map(|c| (i, c)));
                self.depth -= 1;
                let (index, close) = close?;
                let span = lhs.span.start..close.span.end;
                lhs = Expr { kind: ExprKind::Index { base: Box::new(lhs), index: Box::new(index) }, span };
                continue;
            }
            if self.is_punct(Punct::LParen) {
                let ExprKind::Name(name) = &lhs.kind else {
                    let t = self.peek().clone();
                    return Err(CalcError::new(t.span, "only a function can be called"));
                };
                let (name, name_span) = (name.clone(), lhs.span.clone());
                self.bump(false);
                self.depth += 1;
                let args = self.args();
                self.depth -= 1;
                let (args, end) = args?;
                lhs = Expr {
                    kind: ExprKind::Call { name, name_span: name_span.clone(), args },
                    span: name_span.start..end,
                };
                continue;
            }
            let Some((op, l_bp, r_bp)) = self.infix() else { break };
            if l_bp < min_bp {
                break;
            }
            self.bump(false);
            let rhs = self.expr(r_bp)?;
            let span = lhs.span.start..rhs.span.end;
            lhs = Expr { kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), span };
        }
        Ok(lhs)
    }

    /// A call's arguments after '(' up to ')': the arguments and the end of ')'.
    fn args(&mut self) -> Result<(Vec<Expr>, usize), CalcError> {
        let mut args = Vec::new();
        if self.is_punct(Punct::RParen) {
            return Ok((args, self.bump(false).span.end));
        }
        loop {
            args.push(self.expr(0)?);
            if self.is_punct(Punct::Comma) {
                self.bump(false);
                continue;
            }
            return Ok((args, self.expect(Punct::RParen, "',' or ')'")?.span.end));
        }
    }

    fn prefix(&mut self) -> Result<Expr, CalcError> {
        let t = self.bump(true);
        let span = t.span.clone();
        let unary = |p: &mut Parser, op: UnOp, bp: u8| -> Result<Expr, CalcError> {
            let e = p.expr(bp)?;
            let span = span.start..e.span.end;
            Ok(Expr { kind: ExprKind::Unary(op, Box::new(e)), span })
        };
        match &t.tok {
            Tok::Num(v) => Ok(Expr { kind: ExprKind::Num(*v), span }),
            Tok::Input(k) => Ok(Expr { kind: ExprKind::Input(*k), span }),
            Tok::Str { text, closed } => {
                if !closed {
                    return Err(CalcError::new(span, "a layer's name without its closing quote"));
                }
                Ok(Expr { kind: ExprKind::Layer(text.clone()), span })
            }
            Tok::Ident => {
                let name = self.text_of(&t);
                match name {
                    KW_NOT => unary(self, UnOp::Not, 5),
                    KW_AND | KW_OR => Err(CalcError::new(span, format!("'{name}' needs a value on its left"))),
                    _ => Ok(Expr { kind: ExprKind::Name(name.to_string()), span }),
                }
            }
            Tok::Punct(Punct::Minus) => unary(self, UnOp::Neg, 13),
            Tok::Punct(Punct::Plus) => self.expr(13),
            Tok::Punct(Punct::Bang) => unary(self, UnOp::Not, 5),
            Tok::Punct(Punct::LParen) => {
                self.depth += 1;
                let e = self.expr(0);
                let close = e.and_then(|e| self.expect(Punct::RParen, "')'").map(|c| (e, c)));
                self.depth -= 1;
                let (mut e, close) = close?;
                e.span = span.start..close.span.end;
                Ok(e)
            }
            Tok::Eof => Err(CalcError::new(span, "the expression ends too soon")),
            Tok::Newline | Tok::Semi => Err(CalcError::new(span, "a value is missing before the end of the line")),
            Tok::Unknown(c) => Err(CalcError::new(span, format!("'{c}' is not understood"))),
            Tok::Punct(Punct::Assign) => Err(CalcError::new(span, "'=' assigns a name (compare with '==')")),
            _ => Err(CalcError::new(span, format!("a value was expected, found '{}'", self.text_of(&t)))),
        }
    }

    fn stmt(&mut self) -> Result<Stmt, CalcError> {
        // name = expr
        let p = self.peek_at(true);
        if self.toks[p].tok == Tok::Ident
            && self.toks.get(p + 1).is_some_and(|t| t.tok == Tok::Punct(Punct::Assign))
            && ![KW_AND, KW_OR, KW_NOT].contains(&self.text_of(&self.toks[p]))
        {
            let t = self.bump(true);
            self.bump(false);
            let expr = self.expr(0)?;
            return Ok(Stmt { target: Some((self.text_of(&t).to_string(), t.span)), cond: None, expr });
        }
        let e = self.expr(0)?;
        if self.is_punct(Punct::Assign) {
            // a rule
            self.bump(false);
            let value = self.expr(0)?;
            return Ok(Stmt { target: None, cond: Some(e), expr: value });
        }
        Ok(Stmt { target: None, cond: None, expr: e })
    }
}

/// Parses a text (comments dropped).
pub fn parse(text: &str) -> Result<Ast, CalcError> {
    let toks: Vec<Token> = tokens(text).into_iter().filter(|t| t.tok != Tok::Comment).collect();
    let mut p = Parser { text, toks, pos: 0, depth: 0 };
    let mut stmts = Vec::new();
    loop {
        // blank lines and separators between statements
        while matches!(p.toks[p.pos].tok, Tok::Newline | Tok::Semi) {
            p.pos += 1;
        }
        if p.toks[p.pos].tok == Tok::Eof {
            break;
        }
        stmts.push(p.stmt()?);
        let t = p.peek().clone();
        match t.tok {
            Tok::Newline | Tok::Semi | Tok::Eof => {}
            Tok::Punct(Punct::RParen) | Tok::Punct(Punct::RBracket) => {
                return Err(CalcError::new(t.span.clone(), format!("'{}' without its opening bracket", p.text_of(&t))));
            }
            _ => {
                return Err(CalcError::new(
                    t.span.clone(),
                    format!("an operator is missing before '{}' (or a ';' between two statements)", p.text_of(&t)),
                ));
            }
        }
    }
    if stmts.is_empty() {
        return Err(CalcError::new(0..text.len(), "the expression is empty"));
    }
    Ok(Ast { stmts })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tree as a fully bracketed text.
    fn show(e: &Expr) -> String {
        match &e.kind {
            ExprKind::Num(v) => format!("{v}"),
            ExprKind::Name(n) => n.clone(),
            ExprKind::Layer(n) => format!("\"{n}\""),
            ExprKind::Input(k) => format!("${k}"),
            ExprKind::Member { base, member, .. } => format!("{}.{member}", show(base)),
            ExprKind::Index { base, index } => format!("{}[{}]", show(base), show(index)),
            ExprKind::Unary(UnOp::Neg, a) => format!("(-{})", show(a)),
            ExprKind::Unary(UnOp::Not, a) => format!("(not {})", show(a)),
            ExprKind::Binary(op, a, b) => format!("({} {} {})", show(a), op.symbol(), show(b)),
            ExprKind::Call { name, args, .. } => {
                format!("{name}({})", args.iter().map(show).collect::<Vec<_>>().join(", "))
            }
        }
    }

    fn one(s: &str) -> String {
        show(&parse(s).unwrap().stmts[0].expr)
    }

    #[test]
    fn precedence() {
        assert_eq!(one("1 + 2 * 3"), "(1 + (2 * 3))");
        assert_eq!(one("-x^2"), "(-(x ^ 2))");
        assert_eq!(one("2^3^2"), "(2 ^ (3 ^ 2))");
        assert_eq!(one("2**-1"), "(2 ^ (-1))");
        assert_eq!(one("a < b and not c or d"), "(((a < b) and (not c)) or d)");
        assert_eq!(one("!(a == b) && c != 1"), "((not (a == b)) and (c != 1))");
        assert_eq!(one("\"S2\".N - \"S2\"[4]"), "(\"S2\".N - \"S2\"[4])");
        assert_eq!(one("where(x > 0, sqrt(x), -1)"), "where((x > 0), sqrt(x), (-1))");
        assert_eq!(one("$1 % 3"), "($1 % 3)");
    }

    #[test]
    fn statements_and_lines() {
        let a = parse("# NDVI\nndvi = (N - R) /\n  (N + R)\nwhere(ndvi > 0.3, ndvi, nan)").unwrap();
        assert_eq!(a.stmts.len(), 2);
        assert_eq!(a.stmts[0].target.as_ref().unwrap().0, "ndvi");
        assert_eq!(show(&a.stmts[0].expr), "((N - R) / (N + R))");
        let a = parse("x = 1; y = x + (2\n + 3); y").unwrap();
        assert_eq!(a.stmts.len(), 3);
        assert_eq!(a.result_name(), None);
        assert_eq!(parse("evi = 1").unwrap().result_name(), Some("evi"));
        // rules
        let a = parse("img > 0 = 1\nimg == NaN = 0\nimg.band1 > 1 = 0").unwrap();
        assert_eq!(a.stmts.len(), 3);
        assert_eq!(show(a.stmts[0].cond.as_ref().unwrap()), "(img > 0)");
        assert_eq!(show(&a.stmts[1].expr), "0");
        assert_eq!(show(a.stmts[2].cond.as_ref().unwrap()), "(img.band1 > 1)");
    }

    #[test]
    fn errors_point_at_the_text() {
        let e = parse("(N - R) / (N + R").unwrap_err();
        assert!(e.message.contains("')'"), "{e:?}");
        let e = parse("N R").unwrap_err();
        assert_eq!(e.span, 2..3);
        let e = parse("a = 1\nb = a +* 2").unwrap_err();
        assert_eq!(e.line_col("a = 1\nb = a +* 2"), (2, 8));
        assert!(parse("").is_err());
        assert!(parse("x = ").is_err());
        assert!(parse("'unclosed").is_err());
        assert!(parse("2(3)").is_err());
    }
}

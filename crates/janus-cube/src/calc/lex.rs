//! The calculator's tokens. The lexer never fails: what it does not know becomes an `Unknown` token (the parser
//! reports it), so the editor can colour any text as it is being typed.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Punct {
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    /// `^` or `**`
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    Ne,
    /// `=` (assignment)
    Assign,
    /// `&&`
    AndAnd,
    /// `||`
    OrOr,
    /// `!`
    Bang,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
    /// `..`: a range of dates or years
    Range,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Num(f64),
    Ident,
    /// `"a layer"` (or 'a layer'): its text unescaped; `closed` false if the line ended first
    Str {
        text: String,
        closed: bool,
    },
    /// `$3`: the third input of a compiled expression (the canonical text a VRT carries)
    Input(u32),
    Punct(Punct),
    Semi,
    Newline,
    /// `# ...` to the end of the line
    Comment,
    Unknown(char),
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    /// byte offsets in the text
    pub span: Range<usize>,
}

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Every token of `text`, comments and newlines included, ending with `Eof`.
pub fn tokens(text: &str) -> Vec<Token> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let push = |out: &mut Vec<Token>, tok, s: usize, e: usize| out.push(Token { tok, span: s..e });
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        let s = i;
        let len = c.len_utf8();
        match c {
            '\n' => {
                push(&mut out, Tok::Newline, s, s + 1);
                i += 1;
            }
            c if c.is_whitespace() => i += len,
            '#' => {
                let e = text[i..].find('\n').map_or(text.len(), |k| i + k);
                push(&mut out, Tok::Comment, s, e);
                i = e;
            }
            '0'..='9' | '.' if c != '.' || b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let mut e = i;
                while e < b.len() && b[e].is_ascii_digit() {
                    e += 1;
                }
                // "2015..2019": the dots are a range, not the number's
                if e < b.len() && b[e] == b'.' && b.get(e + 1) != Some(&b'.') {
                    e += 1;
                    while e < b.len() && b[e].is_ascii_digit() {
                        e += 1;
                    }
                }
                if e < b.len() && (b[e] == b'e' || b[e] == b'E') {
                    let mut k = e + 1;
                    if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
                        k += 1;
                    }
                    if k < b.len() && b[k].is_ascii_digit() {
                        while k < b.len() && b[k].is_ascii_digit() {
                            k += 1;
                        }
                        e = k;
                    }
                }
                let v = text[s..e].parse::<f64>().unwrap_or(f64::NAN);
                push(&mut out, Tok::Num(v), s, e);
                i = e;
            }
            c if ident_start(c) => {
                let mut e = i + len;
                while let Some(c) = text[e..].chars().next().filter(|c| ident_char(*c)) {
                    e += c.len_utf8();
                }
                push(&mut out, Tok::Ident, s, e);
                i = e;
            }
            '"' | '\'' => {
                let q = c;
                let mut t = String::new();
                let mut e = i + 1;
                let mut closed = false;
                while let Some(ch) = text[e..].chars().next() {
                    if ch == '\n' {
                        break;
                    }
                    e += ch.len_utf8();
                    if ch == q {
                        closed = true;
                        break;
                    }
                    if ch == '\\'
                        && let Some(n) = text[e..].chars().next().filter(|n| *n != '\n')
                    {
                        t.push(n);
                        e += n.len_utf8();
                        continue;
                    }
                    t.push(ch);
                }
                push(&mut out, Tok::Str { text: t, closed }, s, e);
                i = e;
            }
            '$' => {
                let mut e = i + 1;
                while e < b.len() && b[e].is_ascii_digit() {
                    e += 1;
                }
                match text[i + 1..e].parse::<u32>() {
                    Ok(k) => push(&mut out, Tok::Input(k), s, e),
                    Err(_) => push(&mut out, Tok::Unknown('$'), s, s + 1),
                }
                i = e.max(i + 1);
            }
            ';' => {
                push(&mut out, Tok::Semi, s, s + 1);
                i += 1;
            }
            _ => {
                let two = text.get(i..i + 2).unwrap_or("");
                let (p, n) = match two {
                    "**" => (Some(Punct::Pow), 2),
                    "<=" => (Some(Punct::Le), 2),
                    ">=" => (Some(Punct::Ge), 2),
                    "==" => (Some(Punct::EqEq), 2),
                    "!=" | "<>" => (Some(Punct::Ne), 2),
                    "&&" => (Some(Punct::AndAnd), 2),
                    "||" => (Some(Punct::OrOr), 2),
                    ".." => (Some(Punct::Range), 2),
                    _ => (
                        match c {
                            '+' => Some(Punct::Plus),
                            '-' | '−' => Some(Punct::Minus),
                            '*' | '×' => Some(Punct::Star),
                            '/' => Some(Punct::Slash),
                            '%' => Some(Punct::Percent),
                            '^' => Some(Punct::Pow),
                            '<' => Some(Punct::Lt),
                            '>' => Some(Punct::Gt),
                            '=' => Some(Punct::Assign),
                            '!' => Some(Punct::Bang),
                            '(' => Some(Punct::LParen),
                            ')' => Some(Punct::RParen),
                            '[' => Some(Punct::LBracket),
                            ']' => Some(Punct::RBracket),
                            ',' => Some(Punct::Comma),
                            '.' => Some(Punct::Dot),
                            _ => None,
                        },
                        len,
                    ),
                };
                match p {
                    Some(p) => push(&mut out, Tok::Punct(p), s, s + n),
                    None => push(&mut out, Tok::Unknown(c), s, s + len),
                }
                i += n;
            }
        }
    }
    out.push(Token { tok: Tok::Eof, span: text.len()..text.len() });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &str) -> Vec<Tok> {
        tokens(s).into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn numbers_names_strings() {
        assert_eq!(
            kinds("2.5*N - .5e-1 ** x"),
            vec![
                Tok::Num(2.5),
                Tok::Punct(Punct::Star),
                Tok::Ident,
                Tok::Punct(Punct::Minus),
                Tok::Num(0.05),
                Tok::Punct(Punct::Pow),
                Tok::Ident,
                Tok::Eof
            ]
        );
        let t = tokens("\"S2 \\\"L2A\\\"\".B4 # c\n$12");
        assert_eq!(t[0].tok, Tok::Str { text: "S2 \"L2A\"".into(), closed: true });
        assert_eq!(t[1].tok, Tok::Punct(Punct::Dot));
        assert_eq!(&"\"S2 \\\"L2A\\\"\".B4 # c\n$12"[t[2].span.clone()], "B4");
        assert_eq!(t[3].tok, Tok::Comment);
        assert_eq!(t[5].tok, Tok::Input(12));
        // an unterminated layer name, an unknown character
        assert_eq!(kinds("'ab")[0], Tok::Str { text: "ab".into(), closed: false });
        assert_eq!(kinds("a ~ b")[1], Tok::Unknown('~'));
        // "1." is a number, "x.y" a member
        assert_eq!(kinds("1.")[0], Tok::Num(1.0));
        assert_eq!(kinds("x.y")[1], Tok::Punct(Punct::Dot));
        // a range of years, of months
        assert_eq!(kinds("2015..2019"), vec![Tok::Num(2015.0), Tok::Punct(Punct::Range), Tok::Num(2019.0), Tok::Eof]);
        assert_eq!(kinds("2019-06 .. 2019-09")[3], Tok::Punct(Punct::Range));
    }
}

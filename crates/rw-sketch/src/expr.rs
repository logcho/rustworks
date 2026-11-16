//! Tiny expression language for driving dimensions from named parameters,
//! like SOLIDWORKS "Equations": `width / 2 + 5`, `2*pi*r`, `sqrt(a^2 + b^2)`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// A dimension value: either a literal number or an expression string.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Num(f64),
    Expr(String),
}

impl Default for Value {
    fn default() -> Self {
        Value::Num(0.0)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Num(v)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Num(n) => write!(f, "{n}"),
            Value::Expr(s) => write!(f, "{s}"),
        }
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum ExprError {
    #[error("parse error in `{0}`: {1}")]
    Parse(String, String),
    #[error("unknown parameter `{0}`")]
    Unknown(String),
    #[error("circular parameter reference involving `{0}`")]
    Cycle(String),
}

/// Resolves parameter names to numbers, evaluating parameter expressions
/// recursively (with cycle detection).
pub struct Scope<'a> {
    params: &'a BTreeMap<String, Value>,
}

impl<'a> Scope<'a> {
    pub fn new(params: &'a BTreeMap<String, Value>) -> Self {
        Self { params }
    }

    pub fn eval(&self, v: &Value) -> Result<f64, ExprError> {
        match v {
            Value::Num(n) => Ok(*n),
            Value::Expr(s) => self.eval_str(s, &mut Vec::new()),
        }
    }

    fn eval_str(&self, s: &str, stack: &mut Vec<String>) -> Result<f64, ExprError> {
        let toks = lex(s).map_err(|e| ExprError::Parse(s.into(), e))?;
        let mut p = Parser { toks: &toks, pos: 0, src: s, scope: self, stack };
        let v = p.expr()?;
        if p.pos != toks.len() {
            return Err(ExprError::Parse(s.into(), "unexpected trailing input".into()));
        }
        Ok(v)
    }

    fn lookup(&self, name: &str, stack: &mut Vec<String>) -> Result<f64, ExprError> {
        match name {
            "pi" | "PI" => return Ok(std::f64::consts::PI),
            "e" => return Ok(std::f64::consts::E),
            _ => {}
        }
        if stack.iter().any(|s| s == name) {
            return Err(ExprError::Cycle(name.into()));
        }
        let v = self.params.get(name).ok_or_else(|| ExprError::Unknown(name.into()))?;
        match v {
            Value::Num(n) => Ok(*n),
            Value::Expr(s) => {
                stack.push(name.into());
                let r = self.eval_str(s, stack);
                stack.pop();
                r
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(char),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_digit() || ch == '.' {
            let st = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            if i < c.len() && (c[i] == 'e' || c[i] == 'E') {
                let save = i;
                i += 1;
                if i < c.len() && (c[i] == '+' || c[i] == '-') {
                    i += 1;
                }
                if i < c.len() && c[i].is_ascii_digit() {
                    while i < c.len() && c[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            let t: String = c[st..i].iter().collect();
            out.push(Tok::Num(t.parse().map_err(|_| format!("bad number `{t}`"))?));
        } else if ch.is_alphabetic() || ch == '_' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '.') {
                i += 1;
            }
            out.push(Tok::Ident(c[st..i].iter().collect()));
        } else if "+-*/^(),%".contains(ch) {
            out.push(Tok::Op(ch));
            i += 1;
        } else {
            return Err(format!("unexpected character `{ch}`"));
        }
    }
    Ok(out)
}

struct Parser<'a, 'b> {
    toks: &'a [Tok],
    pos: usize,
    src: &'a str,
    scope: &'a Scope<'b>,
    stack: &'a mut Vec<String>,
}

impl Parser<'_, '_> {
    fn err(&self, m: &str) -> ExprError {
        ExprError::Parse(self.src.into(), m.into())
    }
    fn peek_op(&self, c: char) -> bool {
        matches!(self.toks.get(self.pos), Some(Tok::Op(o)) if *o == c)
    }
    fn expr(&mut self) -> Result<f64, ExprError> {
        let mut v = self.term()?;
        loop {
            if self.peek_op('+') {
                self.pos += 1;
                v += self.term()?;
            } else if self.peek_op('-') {
                self.pos += 1;
                v -= self.term()?;
            } else {
                return Ok(v);
            }
        }
    }
    fn term(&mut self) -> Result<f64, ExprError> {
        let mut v = self.unary()?;
        loop {
            if self.peek_op('*') {
                self.pos += 1;
                v *= self.unary()?;
            } else if self.peek_op('/') {
                self.pos += 1;
                v /= self.unary()?;
            } else if self.peek_op('%') {
                self.pos += 1;
                v %= self.unary()?;
            } else {
                return Ok(v);
            }
        }
    }
    fn unary(&mut self) -> Result<f64, ExprError> {
        if self.peek_op('-') {
            self.pos += 1;
            return Ok(-self.unary()?);
        }
        if self.peek_op('+') {
            self.pos += 1;
            return self.unary();
        }
        self.power()
    }
    fn power(&mut self) -> Result<f64, ExprError> {
        let b = self.atom()?;
        if self.peek_op('^') {
            self.pos += 1;
            let e = self.unary()?; // right associative
            return Ok(b.powf(e));
        }
        Ok(b)
    }
    fn atom(&mut self) -> Result<f64, ExprError> {
        match self.toks.get(self.pos).cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(n)
            }
            Some(Tok::Op('(')) => {
                self.pos += 1;
                let v = self.expr()?;
                if !self.peek_op(')') {
                    return Err(self.err("expected `)`"));
                }
                self.pos += 1;
                Ok(v)
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                if self.peek_op('(') {
                    self.pos += 1;
                    let mut args = vec![self.expr()?];
                    while self.peek_op(',') {
                        self.pos += 1;
                        args.push(self.expr()?);
                    }
                    if !self.peek_op(')') {
                        return Err(self.err("expected `)`"));
                    }
                    self.pos += 1;
                    call(&name, &args).ok_or_else(|| self.err(&format!("unknown function `{name}`")))
                } else {
                    self.scope.lookup(&name, self.stack)
                }
            }
            _ => Err(self.err("unexpected token")),
        }
    }
}

fn call(name: &str, a: &[f64]) -> Option<f64> {
    let d2r = std::f64::consts::PI / 180.0;
    Some(match (name, a.len()) {
        ("sqrt", 1) => a[0].sqrt(),
        ("abs", 1) => a[0].abs(),
        // Trig functions take degrees, like SOLIDWORKS equations.
        ("sin", 1) => (a[0] * d2r).sin(),
        ("cos", 1) => (a[0] * d2r).cos(),
        ("tan", 1) => (a[0] * d2r).tan(),
        ("asin", 1) => a[0].asin() / d2r,
        ("acos", 1) => a[0].acos() / d2r,
        ("atan", 1) => a[0].atan() / d2r,
        ("atan2", 2) => a[0].atan2(a[1]) / d2r,
        ("min", n) if n >= 1 => a.iter().cloned().fold(f64::INFINITY, f64::min),
        ("max", n) if n >= 1 => a.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        ("round", 1) => a[0].round(),
        ("floor", 1) => a[0].floor(),
        ("ceil", 1) => a[0].ceil(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evaluates() {
        let mut p = BTreeMap::new();
        p.insert("w".into(), Value::Num(40.0));
        p.insert("h".into(), Value::Expr("w/2 + 1".into()));
        let s = Scope::new(&p);
        assert_eq!(s.eval(&Value::Expr("h * 2".into())).unwrap(), 42.0);
        assert_eq!(s.eval(&Value::Expr("-2^2".into())).unwrap(), -4.0);
        assert!((s.eval(&Value::Expr("cos(60)".into())).unwrap() - 0.5).abs() < 1e-12);
        assert_eq!(s.eval(&Value::Expr("max(1, w, 3)".into())).unwrap(), 40.0);
        p.insert("a".into(), Value::Expr("b".into()));
        p.insert("b".into(), Value::Expr("a".into()));
        let s = Scope::new(&p);
        assert!(matches!(s.eval(&Value::Expr("a".into())), Err(ExprError::Cycle(_))));
    }
}

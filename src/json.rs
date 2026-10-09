//! JSON in: turn a JSON document into a miniformat tree, so any tool that can
//! write JSON (`yq -o=json`, Python, ...) can feed miniformat.
//!
//! Types are kept: numbers, `true`, `false` and `null` load as such. A number
//! miniformat can't type (an integer beyond 64 bits, a float that overflows)
//! becomes a string with the text it was written with.

use crate::reader::{quoted_len, unquote};
use crate::{Error, Value};

type R<T> = Result<T, Error>;

const MAX_DEPTH: usize = 256;

pub fn from_json(text: &str) -> R<Value> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = P { s: text, i: 0 };
    p.ws();
    let v = p.value(0)?;
    if !matches!(v, Value::Map(_) | Value::List(_)) {
        return Err(p.err_at(0, "the document root must be an object or an array"));
    }
    p.ws();
    if p.i < p.s.len() {
        return Err(p.err("unexpected content after the document"));
    }
    Ok(v)
}

struct P<'a> {
    s: &'a str,
    i: usize,
}

impl P<'_> {
    fn err(&self, msg: &str) -> Error {
        self.err_at(self.i, msg)
    }

    fn err_at(&self, at: usize, msg: &str) -> Error {
        let at = at.min(self.s.len());
        let line = 1 + self.s[..at].matches('\n').count();
        let from = self.s[..at].rfind('\n').map_or(0, |p| p + 1);
        let to = self.s[at..].find('\n').map_or(self.s.len(), |p| at + p);
        Error::new(msg.into(), Some(line), Some(&self.s[from..to]), None)
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &str) -> bool {
        let ok = self.s[self.i..].starts_with(word);
        if ok {
            self.i += word.len();
        }
        ok
    }

    fn string(&mut self) -> R<String> {
        let rest = &self.s[self.i..];
        let Some(n) = quoted_len(rest) else {
            return Err(self.err("unterminated string"));
        };
        let token = &rest[..n];
        let out = match unquote(token) {
            Ok(None) => token[1..n - 1].to_owned(),
            Ok(Some(s)) => s,
            Err(m) => return Err(self.err(&m)),
        };
        self.i += n;
        Ok(out)
    }

    fn number(&mut self) -> R<String> {
        let b = self.s.as_bytes();
        let start = self.i;
        let digits = |p: &mut Self| {
            let from = p.i;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
            p.i > from
        };
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        if self.peek() == Some(b'0') {
            self.i += 1;
        } else if !digits(self) {
            return Err(self.err("invalid number"));
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(self.err("invalid number"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(self.err("invalid number"));
            }
        }
        Ok(String::from_utf8_lossy(&b[start..self.i]).into_owned())
    }

    fn value(&mut self, depth: usize) -> R<Value> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        match self.peek() {
            Some(b'{') => {
                self.i += 1;
                let mut m: Vec<(String, Value)> = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(Value::Map(m));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err(self.err("expected a string key"));
                    }
                    let at = self.i;
                    let k = self.string()?;
                    if m.iter().any(|(e, _)| *e == k) {
                        return Err(self.err_at(at, &format!("duplicate key {k:?}")));
                    }
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.i += 1;
                    self.ws();
                    m.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Map(m));
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut l = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(Value::List(l));
                }
                loop {
                    self.ws();
                    l.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::List(l));
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => self.string().map(Value::Str),
            Some(b'-' | b'0'..=b'9') => self.number().map(|n| crate::plain_value(&n)),
            _ if self.lit("true") => Ok(Value::Bool(true)),
            _ if self.lit("false") => Ok(Value::Bool(false)),
            _ if self.lit("null") => Ok(Value::Null),
            _ => Err(self.err("expected a JSON value")),
        }
    }
}

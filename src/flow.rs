//! One line of JSON in which every leaf is a string (`["a", "b"]`,
//! `{"k": [{"n": "v"}]}`), turned into reader events.
//!
//! Stricter than JSON so that YAML parsers read it the same way: only spaces
//! between tokens, no surrogate `\u` escapes, no duplicate keys. Nothing may
//! follow the value, not even a comment.

use crate::reader::{quoted_len, unquote, Event};
use std::borrow::Cow;
use std::collections::HashSet;

const MAX_DEPTH: usize = 100;

enum Open {
    List,
    Map(HashSet<String>),
}

pub(crate) fn parse(s: &str) -> Result<Vec<Event<'static>>, String> {
    let mut p = P { s, i: 0 };
    let mut out = Vec::new();
    let mut stack: Vec<Open> = Vec::new();
    'value: loop {
        p.ws();
        match p.peek() {
            Some(c @ (b'[' | b'{')) => {
                if stack.len() >= MAX_DEPTH {
                    return Err("JSON is nested too deeply".into());
                }
                p.i += 1;
                p.ws();
                let list = c == b'[';
                out.push(if list { Event::ListStart } else { Event::MapStart });
                let close = if list { b']' } else { b'}' };
                if p.peek() == Some(close) {
                    p.i += 1;
                    out.push(if list { Event::ListEnd } else { Event::MapEnd });
                } else {
                    stack.push(if list { Open::List } else { Open::Map(HashSet::new()) });
                    if !list {
                        p.key(&mut stack, &mut out)?;
                    }
                    continue 'value;
                }
            }
            Some(b'"') => out.push(Event::Scalar(Cow::Owned(p.string()?))),
            Some(b'-' | b'0'..=b'9' | b't' | b'f' | b'n' | b'N' | b'I') => {
                return Err("JSON values must be strings (quote it)".into());
            }
            _ => return Err("bad JSON (Expecting value)".into()),
        }
        // a value is complete: close containers, or move on to the next element
        loop {
            p.ws();
            match (stack.last(), p.peek()) {
                (None, _) => break 'value,
                (Some(_), Some(b',')) => {
                    p.i += 1;
                    if matches!(stack.last(), Some(Open::Map(_))) {
                        p.key(&mut stack, &mut out)?;
                    }
                    continue 'value;
                }
                (Some(Open::List), Some(b']')) => out.push(Event::ListEnd),
                (Some(Open::Map(_)), Some(b'}')) => out.push(Event::MapEnd),
                _ => return Err("bad JSON (Expecting ',' delimiter)".into()),
            }
            p.i += 1;
            stack.pop();
        }
    }
    let tail = &s[p.i..];
    if tail.starts_with('#') {
        return Err("a comment cannot follow a JSON value".into());
    }
    if !tail.is_empty() {
        return Err("unexpected text after JSON value".into());
    }
    Ok(out)
}

struct P<'s> {
    s: &'s str,
    i: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn ws(&mut self) {
        while self.peek() == Some(b' ') {
            self.i += 1;
        }
    }

    /// `"key":` of the map on top of the stack.
    fn key(&mut self, stack: &mut [Open], out: &mut Vec<Event<'static>>) -> Result<(), String> {
        self.ws();
        if self.peek() != Some(b'"') {
            return Err("bad JSON (Expecting property name enclosed in double quotes)".into());
        }
        let key = self.string()?;
        self.ws();
        if self.peek() != Some(b':') {
            return Err("bad JSON (Expecting ':' delimiter)".into());
        }
        self.i += 1;
        let Some(Open::Map(seen)) = stack.last_mut() else {
            unreachable!("key() is only called inside a map")
        };
        if !seen.insert(key.clone()) {
            return Err(format!("duplicate key {key:?}"));
        }
        out.push(Event::Key(Cow::Owned(key)));
        Ok(())
    }

    fn string(&mut self) -> Result<String, String> {
        let rest = &self.s[self.i..];
        let Some(n) = quoted_len(rest) else {
            return Err("bad JSON (Unterminated string)".into());
        };
        let token = &rest[..n];
        let b = token.as_bytes();
        let mut k = 1;
        while k + 1 < b.len() {
            if b[k] != b'\\' {
                k += 1;
                continue;
            }
            if b[k + 1] == b'u' {
                let hex = token
                    .get(k + 2..k + 6)
                    .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
                if hex
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .is_some_and(|cp| (0xd800..0xe000).contains(&cp))
                {
                    return Err("surrogate \\u escapes are not supported".into());
                }
            }
            k += 2;
        }
        let s = match unquote(token) {
            Ok(None) => token[1..n - 1].to_string(),
            Ok(Some(s)) => s,
            Err(m) => return Err(format!("bad JSON ({m})")),
        };
        self.i += n;
        Ok(s)
    }
}

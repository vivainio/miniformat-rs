//! Canonical writer and JSON output.

use crate::reader::tag_len;
use crate::{bad_char, Value};
use std::fmt::Write;

fn odd(s: &str) -> bool {
    s.chars().any(|c| bad_char(c) || c == '\u{feff}')
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if c == '\u{feff}' || bad_char(c) || c < ' ' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn has_colon(s: &str) -> bool {
    let b = s.as_bytes();
    b.iter()
        .enumerate()
        .any(|(i, &c)| c == b':' && b.get(i + 1).is_none_or(|&n| n == b' ' || n == b'\t'))
}

fn plain_ok(s: &str) -> bool {
    let b = s.as_bytes();
    let Some(&c) = b.first() else { return false };
    if s.trim_matches([' ', '\t']).len() != s.len() || odd(s) || s.contains(['\n', '\t']) {
        return false;
    }
    if b"[]{}&*!|>'\"%@`#,".contains(&c) {
        return false;
    }
    if b"-?:".contains(&c) && b.get(1).is_none_or(|&n| n == b' ' || n == b'\t') {
        return false;
    }
    !has_colon(s) && !s.contains(" #") && s != "{}" && s != "[]"
}

fn block_ok(s: &str) -> bool {
    if !s.contains('\n') || !s.ends_with('\n') || s.ends_with("\n\n") || s.contains('\r') {
        return false;
    }
    if odd(&s.replace(['\n', '\t'], "")) {
        return false;
    }
    let lines: Vec<&str> = s[..s.len() - 1].split('\n').collect();
    if lines
        .iter()
        .any(|l| !l.is_empty() && l.trim_matches([' ', '\t']).is_empty())
    {
        return false;
    }
    lines
        .iter()
        .find(|l| !l.is_empty())
        .is_some_and(|l| !l.starts_with(' '))
        && !lines.iter().any(|l| l.starts_with('\t'))
}

fn scalar(s: &str, out: &mut String) {
    if plain_ok(s) {
        out.push_str(s);
    } else {
        quote(s, out);
    }
}

fn pad(out: &mut String, n: usize) {
    out.extend(std::iter::repeat_n(' ', n));
}

/// The head ('key:' or '-') is already written; write the value after it.
/// A one-key map whose key is a tag name is written as a tag (`!Name inner`).
fn tag_of<S: AsRef<str>>(v: &Value<S>) -> Option<(&str, &Value<S>)> {
    match v {
        Value::Map(m) if m.len() == 1 => {
            let k = m[0].0.as_ref();
            (tag_len(k) == Some(k.len())).then_some((k, &m[0].1))
        }
        _ => None,
    }
}

fn value<S: AsRef<str>>(out: &mut String, ind: usize, v: &Value<S>) {
    value_in(out, ind, v, true)
}

/// A tag holds one value, so under a tag (`tagged` false) a tag-shaped map is written as a plain map.
fn value_in<S: AsRef<str>>(out: &mut String, ind: usize, v: &Value<S>, tagged: bool) {
    if let Some((tag, inner)) = tag_of(v).filter(|_| tagged) {
        out.push(' ');
        out.push_str(tag);
        return value_in(out, ind, inner, false);
    }
    match v {
        Value::Str(s) if block_ok(s.as_ref()) => {
            out.push_str(" |\n");
            let s = s.as_ref();
            for l in s[..s.len() - 1].split('\n') {
                if !l.is_empty() {
                    pad(out, ind + 2);
                    out.push_str(l);
                }
                out.push('\n');
            }
        }
        Value::Str(s) => {
            out.push(' ');
            scalar(s.as_ref(), out);
            out.push('\n');
        }
        Value::Map(m) if m.is_empty() => out.push_str(" {}\n"),
        Value::List(l) if l.is_empty() => out.push_str(" []\n"),
        Value::Map(m) => {
            out.push('\n');
            map(out, ind + 2, m, false);
        }
        Value::List(l) => {
            out.push('\n');
            list(out, ind + 2, l);
        }
    }
}

/// With `dash`, the first key replaces the last two columns of indent by '- '.
fn map<S: AsRef<str>>(out: &mut String, ind: usize, m: &[(S, Value<S>)], dash: bool) {
    for (n, (k, v)) in m.iter().enumerate() {
        if dash && n == 0 {
            pad(out, ind - 2);
            out.push_str("- ");
        } else {
            pad(out, ind);
        }
        scalar(k.as_ref(), out);
        out.push(':');
        value(out, ind, v);
    }
}

fn list<S: AsRef<str>>(out: &mut String, ind: usize, l: &[Value<S>]) {
    for v in l {
        match v {
            Value::Map(m) if !m.is_empty() && tag_of(v).is_none() => map(out, ind + 2, m, true),
            _ => {
                pad(out, ind);
                out.push('-');
                value(out, ind, v);
            }
        }
    }
}

/// Canonical miniformat text. Root must be a map or list.
pub fn dumps<S: AsRef<str>>(v: &Value<S>) -> String {
    let mut out = String::new();
    match v {
        Value::Map(m) if m.is_empty() => out.push_str("{}\n"),
        Value::List(l) if l.is_empty() => out.push_str("[]\n"),
        Value::Map(m) => map(&mut out, 0, m, false),
        Value::List(l) => list(&mut out, 0, l),
        Value::Str(_) => panic!("document root must be a map or a list"),
    }
    out
}

/// Pretty JSON (2-space indent, non-ASCII kept as is).
pub fn to_json<S: AsRef<str>>(v: &Value<S>) -> String {
    let mut out = String::new();
    json(&mut out, 0, v);
    out
}

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if c < ' ' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json<S: AsRef<str>>(out: &mut String, ind: usize, v: &Value<S>) {
    match v {
        Value::Str(s) => json_str(out, s.as_ref()),
        Value::Map(m) if m.is_empty() => out.push_str("{}"),
        Value::List(l) if l.is_empty() => out.push_str("[]"),
        Value::Map(m) => {
            out.push_str("{\n");
            for (n, (k, v)) in m.iter().enumerate() {
                pad(out, ind + 2);
                json_str(out, k.as_ref());
                out.push_str(": ");
                json(out, ind + 2, v);
                out.push_str(if n + 1 < m.len() { ",\n" } else { "\n" });
            }
            pad(out, ind);
            out.push('}');
        }
        Value::List(l) => {
            out.push_str("[\n");
            for (n, v) in l.iter().enumerate() {
                pad(out, ind + 2);
                json(out, ind + 2, v);
                out.push_str(if n + 1 < l.len() { ",\n" } else { "\n" });
            }
            pad(out, ind);
            out.push(']');
        }
    }
}

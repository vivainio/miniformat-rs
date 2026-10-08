//! miniformat: a tiny, strict config format with YAML syntax.
//!
//! Every scalar is a string; `#+include path` splices in other files.
//! `loads` / `load` parse, `dumps` writes the canonical form.

mod dump;
mod glob;
mod json;
mod query;
mod reader;
#[cfg(feature = "serde")]
mod serde_de;

use std::borrow::Cow;
use std::fmt;
use std::path::Path;

pub use dump::{dumps, to_json};
pub use json::from_json;
pub use query::{find, get, get_as, keys, len, Found};
pub use reader::{Event, Reader};
#[cfg(feature = "serde")]
pub use serde_de::{from_path, from_str};

/// A parsed document. `S` is the string type: `String` (owned, from `loads`)
/// or `Cow<str>` (from `loads_borrowed`, which copies only what it must).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value<S = String> {
    Str(S),
    Map(Vec<(S, Value<S>)>),
    List(Vec<Value<S>>),
}

impl<S: AsRef<str>> Value<S> {
    pub fn get(&self, key: &str) -> Option<&Value<S>> {
        match self {
            Value::Map(m) => m.iter().find(|(k, _)| k.as_ref() == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s.as_ref()),
            _ => None,
        }
    }
}

/// A parse error (boxed, so `Result`s stay small on the hot path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(Box<ErrorInner>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ErrorInner {
    msg: String,
    line: Option<usize>,
    file: Option<String>,
    text: String,
}

impl Error {
    pub(crate) fn new(msg: String, line: Option<usize>, source: Option<&str>, file: Option<&str>) -> Error {
        let mut text = match line {
            Some(n) => format!("line {n}: {msg}"),
            None => msg.clone(),
        };
        if let Some(f) = file {
            text = format!("{f}: {text}");
        }
        if let Some(s) = source {
            text.push_str("\n    ");
            text.push_str(s.trim());
        }
        Error(Box::new(ErrorInner {
            msg,
            line,
            file: file.map(String::from),
            text,
        }))
    }

    /// The message without position.
    pub fn msg(&self) -> &str {
        &self.0.msg
    }
    /// 1-based line number.
    pub fn line(&self) -> Option<usize> {
        self.0.line
    }
    /// Included file the error is in (None for the main document).
    pub fn file(&self) -> Option<&str> {
        self.0.file.as_deref()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.text)
    }
}
impl std::error::Error for Error {}

type R<T> = Result<T, Error>;

/// Parse a document into a tree of owned strings. `base` is the directory
/// `#+include` paths are relative to.
pub fn loads(text: &str, base: Option<&Path>) -> R<Value> {
    build(text, base)
}

/// Like `loads`, but strings borrow from `text` unless they had to be
/// unescaped, re-indented or come from an included file.
pub fn loads_borrowed<'a>(text: &'a str, base: Option<&Path>) -> R<Value<Cow<'a, str>>> {
    build(text, base)
}

fn build<'a, S: From<Cow<'a, str>>>(text: &'a str, base: Option<&Path>) -> R<Value<S>> {
    let mut stack: Vec<(Value<S>, Option<S>)> = Vec::new();
    let mut root = None;
    for ev in Reader::new(text, base)? {
        let done = match ev? {
            Event::MapStart => {
                stack.push((Value::Map(Vec::new()), None));
                continue;
            }
            Event::ListStart => {
                stack.push((Value::List(Vec::new()), None));
                continue;
            }
            Event::Key(k) => {
                stack.last_mut().unwrap().1 = Some(S::from(k));
                continue;
            }
            Event::Scalar(s) => Value::Str(S::from(s)),
            Event::MapEnd | Event::ListEnd => stack.pop().unwrap().0,
        };
        match stack.last_mut() {
            None => root = Some(done),
            Some((Value::Map(m), k)) => m.push((k.take().unwrap(), done)),
            Some((Value::List(l), _)) => l.push(done),
            Some(_) => unreachable!(),
        }
    }
    Ok(root.expect("the reader yields a root"))
}

/// Read and parse a file; includes are relative to its directory.
pub fn load(path: impl AsRef<Path>) -> Result<Value, Error> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| Error::new(e.to_string(), None, None, None))?;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    loads(&text, abs.parent())
}

/// Characters YAML parsers accept, minus the exotic line breaks (NEL, LS, PS).
pub(crate) fn bad_char(c: char) -> bool {
    matches!(c, '\0'..='\x08' | '\x0a'..='\x1f' | '\x7f'..='\u{9f}' | '\u{2028}' | '\u{2029}' | '\u{fffe}' | '\u{ffff}')
}

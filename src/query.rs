//! Streaming lookups: find a value by path while reading forward, without
//! building a tree. They stop at the match, so the rest of the document is
//! neither read nor validated. Duplicate-key checks are off.

use crate::{Error, Event, Reader};
use std::borrow::Cow;
use std::path::Path;
use std::str::FromStr;

type R<T> = Result<T, Error>;

/// What a path points at.
#[derive(Debug, Clone, PartialEq)]
pub enum Found<'a> {
    Scalar(Cow<'a, str>),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    Map,
    List,
}

fn next<'a>(r: &mut Reader<'a>) -> R<Event<'a>> {
    r.next()
        .unwrap_or_else(|| Err(Error::new("unexpected end of document".into(), None, None, None)))
}

/// Consume the rest of a value whose first event was `first`.
fn drain<'a>(r: &mut Reader<'a>, first: &Event<'a>) -> R<()> {
    let mut depth = matches!(first, Event::MapStart | Event::ListStart) as usize;
    while depth > 0 {
        match next(r)? {
            Event::MapStart | Event::ListStart => depth += 1,
            Event::MapEnd | Event::ListEnd => depth -= 1,
            _ => {}
        }
    }
    Ok(())
}

impl<'a> Reader<'a> {
    /// Read forward to the value at `path` (map keys, or list indexes as
    /// numbers) and return its first event: a `Scalar`, `MapStart` or
    /// `ListStart`. For a container, the reader is left just inside it, so
    /// iterating continues with its contents (up to the matching end).
    /// `Ok(None)` if the path is not there. An empty path is the root.
    pub fn seek(&mut self, path: &[&str]) -> R<Option<Event<'a>>> {
        let mut ev = next(self)?;
        for want in path {
            let child = match ev {
                Event::MapStart => loop {
                    match next(self)? {
                        Event::MapEnd => return Ok(None),
                        Event::Key(k) if k == *want => break next(self)?,
                        Event::Key(_) => {
                            let first = next(self)?;
                            drain(self, &first)?;
                        }
                        _ => unreachable!("a map holds keys"),
                    }
                },
                Event::ListStart => {
                    let Ok(idx) = want.parse::<usize>() else {
                        return Ok(None);
                    };
                    let mut i = 0;
                    loop {
                        let first = next(self)?;
                        if first == Event::ListEnd {
                            return Ok(None);
                        }
                        if i == idx {
                            break first;
                        }
                        drain(self, &first)?;
                        i += 1;
                    }
                }
                _ => return Ok(None), // a scalar where the path wants more
            };
            ev = child;
        }
        Ok(Some(ev))
    }
}

fn open<'a>(text: &'a str, base: Option<&Path>) -> R<Reader<'a>> {
    Ok(Reader::new(text, base)?.strict_keys(false))
}

/// What is at `path`, if anything.
pub fn find<'a>(text: &'a str, base: Option<&Path>, path: &[&str]) -> R<Option<Found<'a>>> {
    Ok(open(text, base)?.seek(path)?.map(|ev| match ev {
        Event::Scalar(s) => Found::Scalar(s),
        Event::Int(i) => Found::Int(i),
        Event::Float(f) => Found::Float(f),
        Event::Bool(b) => Found::Bool(b),
        Event::Null => Found::Null,
        Event::MapStart => Found::Map,
        _ => Found::List,
    }))
}

/// The scalar at a dotted path like `"db.port"` or `"servers.0.host"`, as
/// text (`None` if missing or not a scalar; `null` is `"null"`, and a float is
/// written in its shortest form, so `1.10` is `"1.1"`). Keys containing dots
/// need `find`.
pub fn get<'a>(text: &'a str, base: Option<&Path>, path: &str) -> R<Option<Cow<'a, str>>> {
    let segs: Vec<&str> = if path.is_empty() {
        Vec::new()
    } else {
        path.split('.').collect()
    };
    Ok(match find(text, base, &segs)? {
        Some(Found::Scalar(s)) => Some(s),
        Some(Found::Int(i)) => Some(Cow::Owned(i.to_string())),
        Some(Found::Float(f)) => Some(Cow::Owned(format!("{f:?}"))),
        Some(Found::Bool(b)) => Some(Cow::Borrowed(if b { "true" } else { "false" })),
        Some(Found::Null) => Some(Cow::Borrowed("null")),
        _ => None,
    })
}

/// Like `get`, converting with `FromStr` (`"8080"` to `u16`, `"true"` to `bool`, ...).
pub fn get_as<T: FromStr>(text: &str, base: Option<&Path>, path: &str) -> R<Option<T>>
where
    T::Err: std::fmt::Display,
{
    match get(text, base, path)? {
        None => Ok(None),
        Some(s) => s
            .parse()
            .map(Some)
            .map_err(|e| Error::new(format!("{path}: cannot parse {s:?}: {e}"), None, None, None)),
    }
}

/// The keys of the map at `path`, in order.
pub fn keys<'a>(text: &'a str, base: Option<&Path>, path: &[&str]) -> R<Option<Vec<Cow<'a, str>>>> {
    let mut r = open(text, base)?;
    if r.seek(path)? != Some(Event::MapStart) {
        return Ok(None);
    }
    let mut out = Vec::new();
    loop {
        match next(&mut r)? {
            Event::MapEnd => return Ok(Some(out)),
            Event::Key(k) => {
                out.push(k);
                let first = next(&mut r)?;
                drain(&mut r, &first)?;
            }
            _ => unreachable!("a map holds keys"),
        }
    }
}

/// The number of entries of the map or list at `path`.
pub fn len(text: &str, base: Option<&Path>, path: &[&str]) -> R<Option<usize>> {
    let mut r = open(text, base)?;
    let Some(start @ (Event::MapStart | Event::ListStart)) = r.seek(path)? else {
        return Ok(None);
    };
    let map = start == Event::MapStart;
    let mut n = 0;
    loop {
        match next(&mut r)? {
            Event::MapEnd | Event::ListEnd => return Ok(Some(n)),
            first => {
                // a map's entries are Key + value; a list's are just the value
                let first = if map { next(&mut r)? } else { first };
                drain(&mut r, &first)?;
                n += 1;
            }
        }
    }
}

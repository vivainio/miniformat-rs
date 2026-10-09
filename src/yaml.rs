//! YAML to miniformat tree, for the command line only. Scalars keep the text
//! they were written with (`no`, `1.10`, `~` stay as they are): this reads the
//! parser's events, not typed values.
//!
//! Handled: anchors and aliases (expanded), merge keys (`<<`), `!Name` tags
//! (become `{"!Name": value}`), flow collections, single-quoted and folded
//! strings (they are just strings once parsed). Refused: multiple documents,
//! non-scalar keys, duplicate keys, tags that are not `!Name`.
//! Comments are not carried over.

use miniformat::Value;
use saphyr_parser::{Event, Parser, ScalarStyle, Tag};
use std::collections::{HashMap, HashSet};

const MAX_NODES: usize = 5_000_000;

pub struct Converted {
    pub value: Value,
    pub warnings: Vec<String>,
}

pub struct ConvError {
    pub line: Option<usize>,
    pub msg: String,
}

type R<T> = Result<T, ConvError>;

fn err<T>(line: usize, msg: impl Into<String>) -> R<T> {
    Err(ConvError {
        line: Some(line),
        msg: msg.into(),
    })
}

enum Frame {
    Map {
        pairs: Vec<(String, Value)>,
        key: Option<String>,
        /// The next value is a `<<` merge source.
        merge_next: bool,
        merges: Vec<Value>,
        aid: usize,
        tag: Option<Tag>,
    },
    Seq {
        items: Vec<Value>,
        aid: usize,
        tag: Option<Tag>,
    },
}

#[derive(Default)]
struct State {
    stack: Vec<Frame>,
    root: Option<Value>,
    anchors: HashMap<usize, Value>,
    nodes: usize,
    docs: usize,
    warnings: Vec<String>,
}

pub fn convert(text: &str) -> R<Converted> {
    // a BOM is an encoding mark, not part of the first key
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    // The parser clips a final `|` block to one newline even when the file has
    // none (YAML, and the miniformat loader, keep it without). Give it a line
    // end, then take that newline off any block scalar that runs to the very end.
    let appended = !text.is_empty() && !text.ends_with('\n');
    let text = if appended {
        std::borrow::Cow::Owned(format!("{text}\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    };
    let end = text.chars().count();
    let mut st = State::default();
    for item in Parser::new_from_str(&text) {
        let (mut ev, span) = item.map_err(|e| ConvError {
            line: Some(e.marker().line()),
            msg: e.info().to_string(),
        })?;
        if let Event::Scalar(v, ScalarStyle::Literal | ScalarStyle::Folded, ..) = &mut ev {
            if appended && span.end.index() >= end && v.ends_with('\n') {
                let mut s = std::mem::take(v).into_owned();
                s.pop();
                *v = s.into();
            }
        }
        st.event(ev, span.start.line())?;
    }
    match st.root {
        Some(v @ (Value::Map(_) | Value::List(_))) => Ok(Converted {
            value: v,
            warnings: st.warnings,
        }),
        Some(_) => Err(ConvError {
            line: None,
            msg: "the document root must be a mapping or a sequence".into(),
        }),
        None => Err(ConvError {
            line: None,
            msg: "empty document".into(),
        }),
    }
}

fn count(v: &Value) -> usize {
    match v {
        Value::Map(m) => 1 + m.iter().map(|(_, v)| 1 + count(v)).sum::<usize>(),
        Value::List(l) => 1 + l.iter().map(count).sum::<usize>(),
        _ => 1,
    }
}

impl State {
    /// Is the innermost container a mapping waiting for its next key?
    fn expecting_key(&self) -> bool {
        matches!(
            self.stack.last(),
            Some(Frame::Map {
                key: None,
                merge_next: false,
                ..
            })
        )
    }

    fn event(&mut self, ev: Event<'_>, line: usize) -> R<()> {
        match ev {
            Event::DocumentStart(_) => {
                self.docs += 1;
                if self.docs > 1 {
                    return err(line, "multiple documents are not supported");
                }
            }
            Event::Scalar(v, style, aid, tag) => {
                // only a plain scalar is typed, and `!!str` says it is not
                let plain = style == ScalarStyle::Plain
                    && !tag
                        .as_ref()
                        .is_some_and(|t| t.is_yaml_core_schema() && t.suffix == "str");
                if self.expecting_key() {
                    if tag.is_some() {
                        return err(line, "tags on keys are not supported");
                    }
                    let Some(Frame::Map { key, merge_next, .. }) = self.stack.last_mut() else {
                        unreachable!()
                    };
                    if v == "<<" && style == ScalarStyle::Plain {
                        *merge_next = true;
                    } else {
                        *key = Some(v.into_owned());
                    }
                } else {
                    let value = if plain {
                        miniformat::plain_value(&v)
                    } else {
                        Value::Str(v.into_owned())
                    };
                    self.finish(value, aid, tag.map(|t| t.into_owned()), line)?;
                }
            }
            Event::Alias(id) => {
                if self.expecting_key() {
                    return err(line, "aliases as keys are not supported");
                }
                let Some(v) = self.anchors.get(&id).cloned() else {
                    return err(line, "alias to an unknown anchor");
                };
                self.nodes += count(&v);
                self.finish(v, 0, None, line)?;
            }
            Event::MappingStart(aid, tag) => {
                if self.expecting_key() {
                    return err(line, "a mapping as a key is not supported");
                }
                let tag = tag.map(|t| t.into_owned());
                self.stack.push(Frame::Map {
                    pairs: Vec::new(),
                    key: None,
                    merge_next: false,
                    merges: Vec::new(),
                    aid,
                    tag,
                });
            }
            Event::SequenceStart(aid, tag) => {
                if self.expecting_key() {
                    return err(line, "a sequence as a key is not supported");
                }
                let tag = tag.map(|t| t.into_owned());
                self.stack.push(Frame::Seq {
                    items: Vec::new(),
                    aid,
                    tag,
                });
            }
            Event::MappingEnd => {
                let Some(Frame::Map {
                    mut pairs,
                    merges,
                    aid,
                    tag,
                    ..
                }) = self.stack.pop()
                else {
                    return err(line, "unbalanced mapping");
                };
                apply_merges(&mut pairs, merges, line)?;
                self.finish(Value::Map(pairs), aid, tag, line)?;
            }
            Event::SequenceEnd => {
                let Some(Frame::Seq { items, aid, tag }) = self.stack.pop() else {
                    return err(line, "unbalanced sequence");
                };
                self.finish(Value::List(items), aid, tag, line)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// A node is complete: tag it, remember its anchor, and attach it to its parent.
    fn finish(&mut self, value: Value, aid: usize, tag: Option<Tag>, line: usize) -> R<()> {
        let value = self.tagged(value, tag, line)?;
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return err(line, "too many nodes (alias expansion bomb?)");
        }
        if aid != 0 {
            self.anchors.insert(aid, value.clone());
        }
        match self.stack.last_mut() {
            None => self.root = Some(value),
            Some(Frame::Seq { items, .. }) => items.push(value),
            Some(Frame::Map {
                pairs,
                key,
                merge_next,
                merges,
                ..
            }) => {
                if *merge_next {
                    *merge_next = false;
                    merges.push(value);
                } else {
                    let k = key.take().expect("a key came first");
                    if pairs.iter().any(|(e, _)| *e == k) {
                        return err(line, format!("duplicate key {k:?}"));
                    }
                    pairs.push((k, value));
                }
            }
        }
        Ok(())
    }

    fn tagged(&mut self, value: Value, tag: Option<Tag>, line: usize) -> R<Value> {
        let Some(t) = tag else { return Ok(value) };
        if t.handle == "!" {
            let name = format!("!{}", t.suffix);
            if !miniformat::is_tag_name(&name) {
                return err(
                    line,
                    format!("tag {name} is not a miniformat tag (!Name, letters, digits, _ . : -)"),
                );
            }
            return Ok(Value::Map(vec![(name, value)]));
        }
        if t.is_yaml_core_schema() {
            // plain scalars are typed by their text here, so `!!int`, ... say nothing
            self.warnings.push(format!("line {line}: dropped tag !!{}", t.suffix));
            return Ok(value);
        }
        err(line, format!("unsupported tag {}{}", t.handle, t.suffix))
    }
}

/// `<<: *base` (or a list of them): add the keys the mapping does not have.
fn apply_merges(pairs: &mut Vec<(String, Value)>, merges: Vec<Value>, line: usize) -> R<()> {
    if merges.is_empty() {
        return Ok(());
    }
    let mut have: HashSet<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
    for src in merges {
        let maps = match src {
            Value::Map(m) => vec![m],
            Value::List(l) => l
                .into_iter()
                .map(|v| match v {
                    Value::Map(m) => Ok(m),
                    _ => err(line, "a merge key (<<) needs mappings"),
                })
                .collect::<R<Vec<_>>>()?,
            _ => return err(line, "a merge key (<<) needs a mapping or a list of mappings"),
        };
        for m in maps {
            for (k, v) in m {
                if have.insert(k.clone()) {
                    pairs.push((k, v));
                }
            }
        }
    }
    Ok(())
}

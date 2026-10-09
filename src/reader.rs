//! Pull parser: `Reader` yields `Event`s in document order, borrowing keys and
//! scalars from the input wherever no unescaping or re-indenting is needed.

use crate::{bad_char, flow, glob, Error};
use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

const MAX_INCLUDES: usize = 1000;

/// One step through a document. `Scalar` is a string; a plain `true`, `false`,
/// `null` or number in JSON syntax arrives as `Bool`, `Null`, `Int` or `Float`.
#[derive(Debug, Clone, PartialEq)]
pub enum Event<'a> {
    MapStart,
    MapEnd,
    ListStart,
    ListEnd,
    Key(Cow<'a, str>),
    Scalar(Cow<'a, str>),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
}

/// A plain scalar that is a JSON literal or number: `true`, `false`, `null`,
/// or `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`. Integers outside
/// the 64-bit range and floats that overflow are not typed (`None`: strings).
pub(crate) fn typed(s: &str) -> Option<Event<'static>> {
    let b = s.as_bytes();
    match b.first()? {
        b't' | b'f' | b'n' => {
            return match s {
                "true" => Some(Event::Bool(true)),
                "false" => Some(Event::Bool(false)),
                "null" => Some(Event::Null),
                _ => None,
            }
        }
        b'-' | b'0'..=b'9' => {}
        _ => return None,
    }
    match json_number(s)? {
        false => s.parse().ok().map(Event::Int),
        true => s.parse::<f64>().ok().filter(|f| f.is_finite()).map(Event::Float),
    }
}

/// Is all of `s` a number in JSON syntax? `Some(true)` if it has a fraction or exponent.
pub(crate) fn json_number(s: &str) -> Option<bool> {
    let b = s.as_bytes();
    let digits = |i: &mut usize| {
        let from = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > from
    };
    let mut i = (b.first() == Some(&b'-')) as usize;
    match b.get(i)? {
        b'0' => i += 1,
        b'1'..=b'9' => {
            digits(&mut i);
        }
        _ => return None,
    }
    let mut float = false;
    if b.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return None;
        }
        float = true;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return None;
        }
        float = true;
    }
    (i == b.len()).then_some(float)
}

type R<T> = Result<T, Error>;

/// Byte-wise trims (std's pattern-based ones are slow on short lines).
trait TrimSp {
    fn ltrim_sp(&self) -> &str;
    fn rtrim_sp(&self) -> &str;
    fn rtrim_sptab(&self) -> &str;
}

impl TrimSp for str {
    #[inline]
    fn ltrim_sp(&self) -> &str {
        let b = self.as_bytes();
        let mut i = 0;
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        &self[i..]
    }
    #[inline]
    fn rtrim_sp(&self) -> &str {
        let b = self.as_bytes();
        let mut n = b.len();
        while n > 0 && b[n - 1] == b' ' {
            n -= 1;
        }
        &self[..n]
    }
    #[inline]
    fn rtrim_sptab(&self) -> &str {
        let b = self.as_bytes();
        let mut n = b.len();
        while n > 0 && (b[n - 1] == b' ' || b[n - 1] == b'\t') {
            n -= 1;
        }
        &self[..n]
    }
}

/// A physical line as read from a source.
#[derive(Clone, Copy)]
struct Raw {
    src: usize,
    ls: usize,
    /// Start of the content (after the leading spaces); only set by a scanning read.
    cs: usize,
    le: usize,
    pad: usize,
    n: usize,
    file: u32,
}

/// The current structural line: `start..end` is its content (after the indent).
#[derive(Clone, Copy)]
struct Cur {
    src: usize,
    start: usize,
    end: usize,
    indent: usize,
    n: usize,
    file: u32,
    scan: Scan,
}

const NONE: usize = usize::MAX;

/// One pass over a line's content, up to its first comment: where the first
/// tab, the first ` #`, and the first two `:` (followed by space, tab or the
/// end) are, relative to the content (`NONE` if absent).
#[derive(Clone, Copy)]
struct Scan {
    tab: usize,
    comment: usize,
    colon: usize,
    colon2: usize,
}

fn scan(c: &str) -> Scan {
    let b = c.as_bytes();
    let n = b.len();
    let mut s = Scan {
        tab: NONE,
        comment: NONE,
        colon: NONE,
        colon2: NONE,
    };
    for i in 0..n {
        match b[i] {
            b'\t' => {
                if s.tab == NONE {
                    s.tab = i;
                }
            }
            b'#' if i > 0 && b[i - 1] == b' ' => {
                s.comment = i - 1;
                break;
            }
            b':' if i + 1 == n || b[i + 1] == b' ' || b[i + 1] == b'\t' => {
                if s.colon == NONE {
                    s.colon = i;
                } else if s.colon2 == NONE {
                    s.colon2 = i;
                }
            }
            _ => {}
        }
    }
    s
}

const EMPTY_SCAN: Scan = Scan {
    tab: NONE,
    comment: NONE,
    colon: NONE,
    colon2: NONE,
};

const SPECIAL: [bool; 256] = {
    let mut t = [false; 256];
    t[b'\n' as usize] = true;
    t[b'\r' as usize] = true;
    t[b'\t' as usize] = true;
    t[b'#' as usize] = true;
    t[b':' as usize] = true;
    t
};

/// Find the end of the line at `pos` and `scan` its content in one pass.
/// Returns (end of text, start of next line, content start, scan); as in
/// `find_eol`, `next > len` means there is no further line.
fn scan_line(text: &str, pos: usize) -> (usize, usize, usize, Scan) {
    let b = text.as_bytes();
    let n = b.len();
    let mut i = pos;
    while i < n && b[i] == b' ' {
        i += 1;
    }
    let cs = i;
    let mut sc = EMPTY_SCAN;
    while i < n {
        let c = b[i];
        if !SPECIAL[c as usize] {
            i += 1;
            continue;
        }
        match c {
            b'\n' | b'\r' => break,
            b'\t' => {
                if sc.tab == NONE {
                    sc.tab = i - cs;
                }
            }
            b'#' => {
                if i > cs && b[i - 1] == b' ' {
                    sc.comment = i - 1 - cs;
                    // the rest of the line only matters for where it ends
                    i += b[i..].iter().position(|&c| c == b'\n' || c == b'\r').unwrap_or(n - i);
                    break;
                }
            }
            _ => {
                // ':' counts when followed by a space, a tab or the end of the line
                if b.get(i + 1).is_none_or(|&x| matches!(x, b' ' | b'\t' | b'\n' | b'\r')) {
                    if sc.colon == NONE {
                        sc.colon = i - cs;
                    } else if sc.colon2 == NONE {
                        sc.colon2 = i - cs;
                    }
                }
            }
        }
        i += 1;
    }
    if i >= n {
        return (n, n + 1, cs, sc);
    }
    let next = if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') {
        i + 2
    } else {
        i + 1
    };
    (i, next, cs, sc)
}

/// An included file; source 0 is the main document (text held by the reader).
struct Source {
    text: String,
    pos: usize,
    pad: usize,
    file: u32,
    lineno: usize,
    skip_at: Option<usize>,
    has_cr: bool,
}

enum Piece {
    Rng(usize, usize),
    Own(String),
}

enum Inl {
    /// A plain `true`, `false`, `null` or number.
    Typed(Event<'static>),
    /// One line of JSON: its events, the first one included.
    Flow(Vec<Event<'static>>),
    Scalar(Piece),
    Map,
    List,
}

type KeySet<'a> = HashSet<Cow<'a, str>, std::hash::BuildHasherDefault<Fx>>;

/// Cheap multiplicative hasher for key sets (keys are trusted-size config text).
#[derive(Default)]
struct Fx(u64);

impl std::hash::Hasher for Fx {
    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        let mut rest = bytes;
        while let Some((c, r)) = rest.split_first_chunk::<8>() {
            h = (h.rotate_left(5) ^ u64::from_le_bytes(*c)).wrapping_mul(0x517cc1b727220a95);
            rest = r;
        }
        for &b in rest {
            h = (h.rotate_left(5) ^ b as u64).wrapping_mul(0x517cc1b727220a95);
        }
        self.0 = h;
    }
    fn finish(&self) -> u64 {
        // the multiplies only carry low bits upward; fold the high bits back down
        let h = self.0;
        (h ^ (h >> 32)).wrapping_mul(0x9e3779b97f4a7c15) ^ (h >> 29)
    }
}

/// Events worked out ahead of the step that delivers them (at most four).
struct Queue<'a> {
    buf: [Option<Event<'a>>; 4],
    head: usize,
    len: usize,
}

impl<'a> Queue<'a> {
    fn new() -> Self {
        Queue {
            buf: [None, None, None, None],
            head: 0,
            len: 0,
        }
    }
    #[inline]
    fn push(&mut self, e: Event<'a>) {
        self.buf[(self.head + self.len) & 3] = Some(e);
        self.len += 1;
    }
    #[inline]
    fn pop(&mut self) -> Option<Event<'a>> {
        if self.len == 0 {
            return None;
        }
        let e = self.buf[self.head].take();
        self.head = (self.head + 1) & 3;
        self.len -= 1;
        e
    }
}

struct Frame<'a> {
    /// A tag's one-key map: ends as soon as its value is done.
    tag: bool,
    list: bool,
    ind: usize,
    ks: usize,
    set: Option<KeySet<'a>>,
}

#[derive(Clone, Copy)]
enum St {
    Init,
    Frame,
    /// Emit the end of an inline `{}` / `[]` (its start went out already).
    PendEnd {
        map: bool,
    },
    RootEnd,
    End,
}

pub struct Reader<'a> {
    main: &'a str,
    srcs: Vec<Source>,
    files: Vec<String>,
    base: Option<PathBuf>,
    includes: usize,
    pushed: Option<Raw>,
    cur: Option<Cur>,
    last_n: usize,
    stack: Vec<Frame<'a>>,
    keys: Vec<Cow<'a, str>>,
    strict: bool,
    state: St,
    /// Events already worked out, delivered before the next step.
    queue: Queue<'a>,
    /// The rest of a JSON flow value, delivered after the queue.
    flow: VecDeque<Event<'a>>,
}

// -- small helpers ---------------------------------------------------------

#[inline]
fn is_blank(content: &str) -> bool {
    content.is_empty() || content.as_bytes()[0] == b'#'
}

/// Drop a trailing `# comment` (`#` must follow a space).
#[inline]
fn split_comment(s: &str) -> &str {
    match find_comment(s) {
        Some(p) => &s[..p],
        None => s,
    }
}

/// Position of the first ` #`.
#[inline]
fn find_comment(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut from = 0;
    while let Some(p) = b[from..].iter().position(|&c| c == b'#') {
        let p = from + p;
        if p > 0 && b[p - 1] == b' ' {
            return Some(p - 1);
        }
        from = p + 1;
    }
    None
}

/// Position of the first `:` followed by a space, tab or the end.
#[inline]
pub(crate) fn find_colon(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut from = 0;
    while let Some(p) = b[from..].iter().position(|&c| c == b':') {
        let p = from + p;
        if p + 1 == b.len() || b[p + 1] == b' ' || b[p + 1] == b'\t' {
            return Some(p);
        }
        from = p + 1;
    }
    None
}

/// `---` optionally followed by whitespace and a comment.
fn is_doc_start(s: &str) -> bool {
    match s.strip_prefix("---") {
        Some("") => true,
        Some(r) => {
            let t = r.trim_start_matches([' ', '\t']);
            t.len() < r.len() && t.starts_with('#')
        }
        None => false,
    }
}

fn is_doc_mark(c: &str) -> bool {
    (c.starts_with("---") || c.starts_with("...")) && matches!(c.as_bytes().get(3), None | Some(b' ') | Some(b'\t'))
}

fn hint(c: u8) -> Option<&'static str> {
    Some(match c {
        b'[' | b'{' => "flow syntax is only supported as a one-line JSON value",
        b'&' => "anchors are not supported",
        b'*' => "aliases are not supported",
        b'!' => "a tag is !Name, a space, then the value (no !!, none on keys)",
        b'>' => "folded scalars are not supported; use '|'",
        b'\'' => "single quotes are not supported; use double quotes",
        b'%' => "directives are not supported",
        _ => return None,
    })
}

const BAD_START: [bool; 256] = {
    let mut t = [false; 256];
    let bad = b"[]{}&*!|>'\"%@`#,";
    let mut i = 0;
    while i < bad.len() {
        t[bad[i] as usize] = true;
        i += 1;
    }
    t
};

/// Length of the tag at the start of `s` (`!Name`, then a space or the end), if any.
pub(crate) fn tag_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 2 || b[0] != b'!' || !b[1].is_ascii_alphabetic() {
        return None;
    }
    let mut n = 2;
    while n < b.len() && (b[n].is_ascii_alphanumeric() || matches!(b[n], b'_' | b'.' | b':' | b'-')) {
        n += 1;
    }
    let ends_ok = b[n - 1].is_ascii_alphanumeric() || b[n - 1] == b'_';
    (ends_ok && (n == b.len() || b[n] == b' ')).then_some(n)
}

fn is_list_item(c: &str) -> bool {
    c == "-" || c.starts_with("- ")
}

/// Length of the double-quoted token at the start of `s`, if terminated.
pub(crate) fn quoted_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut k = 1;
    while k < b.len() {
        match b[k] {
            b'"' => return Some(k + 1),
            b'\\' => k += 2,
            _ => k += 1,
        }
    }
    None
}

/// Unescape a quoted token; `Ok(None)` means the inside is used as is.
pub(crate) fn unquote(token: &str) -> Result<Option<String>, String> {
    let inner = &token[1..token.len() - 1];
    if !inner.contains('\\') {
        return match inner.chars().find(|&c| c < ' ') {
            Some(_) => Err("bad escape in quoted string (invalid control character)".into()),
            None => Ok(None),
        };
    }
    let bad = |m: &str| Err(format!("bad escape in quoted string ({m})"));
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars();
    fn hex4(it: &mut std::str::Chars) -> Option<u32> {
        let mut v = 0;
        for _ in 0..4 {
            v = v * 16 + it.next()?.to_digit(16)?;
        }
        Some(v)
    }
    while let Some(c) = it.next() {
        if c < ' ' {
            return bad("invalid control character");
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('b') => out.push('\x08'),
            Some('f') => out.push('\x0c'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let Some(mut cp) = hex4(&mut it) else {
                    return bad("invalid \\u escape");
                };
                if (0xd800..0xdc00).contains(&cp) {
                    let mut ahead = it.clone();
                    if ahead.next() == Some('\\') && ahead.next() == Some('u') {
                        if let Some(lo) = hex4(&mut ahead).filter(|l| (0xdc00..0xe000).contains(l)) {
                            cp = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                            it = ahead;
                        }
                    }
                }
                match char::from_u32(cp) {
                    Some(ch) => out.push(ch),
                    None => return Err("lone surrogate in quoted string".into()),
                }
            }
            _ => return bad("invalid \\ escape"),
        }
    }
    Ok(Some(out))
}

pub(crate) fn py_repr(c: char) -> String {
    match c as u32 {
        n if n < 0x100 => format!("'\\x{n:02x}'"),
        n => format!("'\\u{n:04x}'"),
    }
}

/// Python-style repr of a string, for messages shared with the reference loader.
fn py_str(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// End of the line starting at `pos`: (end of its text, start of the next line).
/// A final line without a terminator, and the empty line after a final
/// terminator, both exist (like `split('\n')`); `next > len` means no more.
#[inline]
fn find_eol(text: &str, pos: usize, has_cr: bool) -> (usize, usize) {
    let b = text.as_bytes();
    if !has_cr {
        return match text[pos..].find('\n') {
            Some(p) => (pos + p, pos + p + 1),
            None => (b.len(), b.len() + 1),
        };
    }
    match b[pos..].iter().position(|&c| c == b'\n' || c == b'\r') {
        Some(p) => {
            let le = pos + p;
            let crlf = b[le] == b'\r' && b.get(le + 1) == Some(&b'\n');
            (le, le + 1 + crlf as usize)
        }
        None => (b.len(), b.len() + 1),
    }
}

/// Check every character is one YAML parsers accept (minus NEL, LS, PS).
fn validate(text: &str, name: Option<&str>) -> R<()> {
    let plain = |b: u8| (0x20..0x7f).contains(&b) || b == b'\t' || b == b'\n' || b == b'\r';
    let odd = text
        .as_bytes()
        .chunks(64)
        .any(|c| c.iter().fold(false, |a, &b| a | !plain(b)));
    if !odd {
        return Ok(());
    }
    let has_cr = text.contains('\r');
    let (mut pos, mut n) = (0, 0);
    loop {
        n += 1;
        let (le, next) = find_eol(text, pos, has_cr);
        let line = &text[pos..le];
        if let Some(c) = line.chars().find(|&c| bad_char(c)) {
            let msg = format!("unsupported character {}", py_repr(c));
            return Err(Error::new(msg, Some(n), Some(line), name));
        }
        if next > text.len() {
            return Ok(());
        }
        pos = next;
    }
}

#[inline]
fn text_of<'s>(main: &'s str, srcs: &'s [Source], src: usize) -> &'s str {
    if src == 0 {
        main
    } else {
        &srcs[src].text
    }
}

// -- the reader ----------------------------------------------------------------

impl<'a> Reader<'a> {
    /// `base` is the directory `#+include` paths are relative to; without it,
    /// `#+include` is an error. Checks the characters of the whole text up front.
    pub fn new(text: &'a str, base: Option<&Path>) -> R<Reader<'a>> {
        let main = text.strip_prefix('\u{feff}').unwrap_or(text);
        validate(main, None)?;
        let src = Source {
            text: String::new(),
            pos: 0,
            pad: 0,
            file: 0,
            lineno: 0,
            skip_at: None,
            has_cr: main.contains('\r'),
        };
        Ok(Reader {
            main,
            srcs: vec![src],
            files: vec![String::new()],
            base: base.map(Path::to_path_buf),
            includes: 0,
            pushed: None,
            cur: None,
            last_n: 0,
            stack: Vec::new(),
            keys: Vec::new(),
            strict: true,
            state: St::Init,
            queue: Queue::new(),
            flow: VecDeque::new(),
        })
    }

    /// Duplicate-key detection (on by default) keeps every key of the maps
    /// being read; turn it off to skip that work.
    pub fn strict_keys(mut self, on: bool) -> Self {
        self.strict = on;
        self
    }

    /// After a `Key` (or a list item's first event), consume the whole value.
    pub fn skip_value(&mut self) -> R<()> {
        let mut depth = 0usize;
        for ev in self.by_ref() {
            match ev? {
                Event::MapStart | Event::ListStart => depth += 1,
                Event::MapEnd | Event::ListEnd => depth = depth.saturating_sub(1),
                _ => {}
            }
            if depth == 0 {
                break;
            }
        }
        Ok(())
    }

    #[inline]
    fn text(&self, src: usize) -> &str {
        text_of(self.main, &self.srcs, src)
    }

    fn cow(&self, src: usize, a: usize, b: usize) -> Cow<'a, str> {
        if src == 0 {
            let m: &'a str = self.main;
            Cow::Borrowed(&m[a..b])
        } else {
            Cow::Owned(self.srcs[src].text[a..b].to_owned())
        }
    }

    fn piece(&self, src: usize, p: Piece) -> Cow<'a, str> {
        match p {
            Piece::Rng(a, b) => self.cow(src, a, b),
            Piece::Own(s) => Cow::Owned(s),
        }
    }

    fn fname(&self, f: u32) -> Option<&str> {
        (f != 0).then(|| self.files[f as usize].as_str())
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        match &self.cur {
            Some(c) => {
                let content = &self.text(c.src)[c.start..c.end];
                Error::new(msg.into(), Some(c.n), Some(content), self.fname(c.file))
            }
            None => Error::new(msg.into(), Some(self.last_n), None, None),
        }
    }

    fn err_raw(&self, msg: &str, n: usize, file: u32, source: &str) -> Error {
        Error::new(msg.into(), Some(n), Some(source), self.fname(file))
    }

    // -- lines -----------------------------------------------------------------

    fn read_raw(&mut self) -> Option<Raw> {
        self.read_line::<false>().map(|x| x.0)
    }

    /// The next physical line; with `SCAN`, also its content start and `Scan`.
    fn read_line<const SCAN: bool>(&mut self) -> Option<(Raw, Scan)> {
        if let Some(mut r) = self.pushed.take() {
            let mut sc = EMPTY_SCAN;
            if SCAN {
                let line = &self.text(r.src)[r.ls..r.le];
                r.cs = r.le - line.ltrim_sp().len();
                sc = scan(&self.text(r.src)[r.cs..r.le]);
            }
            return Some((r, sc));
        }
        loop {
            let i = self.srcs.len() - 1;
            let (pos, has_cr, pad, file, skip_at) = {
                let s = &self.srcs[i];
                (s.pos, s.has_cr, s.pad, s.file, s.skip_at)
            };
            let text = self.text(i);
            let (le, next, cs, sc) = if pos > text.len() {
                (usize::MAX, 0, 0, EMPTY_SCAN)
            } else if SCAN {
                scan_line(text, pos)
            } else {
                let (le, next) = find_eol(text, pos, has_cr);
                (le, next, pos, EMPTY_SCAN)
            };
            if le == usize::MAX {
                if i == 0 {
                    return None;
                }
                self.srcs.pop();
                continue;
            }
            let s = &mut self.srcs[i];
            s.pos = next;
            s.lineno += 1;
            if skip_at == Some(pos) {
                continue;
            }
            return Some((
                Raw {
                    src: i,
                    ls: pos,
                    cs,
                    le,
                    pad,
                    n: s.lineno,
                    file,
                },
                sc,
            ));
        }
    }

    /// Make `self.cur` the next structural line; false at the end of input.
    fn skip(&mut self) -> R<bool> {
        if self.cur.is_some() {
            return Ok(true);
        }
        loop {
            let Some((r, sc)) = self.read_line::<true>() else {
                return Ok(false);
            };
            self.last_n = r.n;
            let raw_ind = r.cs - r.ls;
            let c = &text_of(self.main, &self.srcs, r.src)[r.cs..r.le];
            let cur = Cur {
                src: r.src,
                start: r.cs,
                end: r.le,
                indent: r.pad + raw_ind,
                n: r.n,
                file: r.file,
                scan: sc,
            };
            if is_blank(c) {
                if c.starts_with("#+") && !matches!(c[2..].chars().next(), None | Some(' ') | Some('\t')) {
                    let c = c.to_owned();
                    self.cur = Some(cur);
                    self.pragma(&c)?;
                    self.cur = None;
                }
                continue;
            }
            self.cur = Some(cur);
            if c.as_bytes()[0] == b'\t' {
                return Err(self.err("tabs are not allowed for indentation"));
            }
            if cur.indent == 0 && is_doc_mark(c) {
                return Err(self.err("document markers are only allowed as a first-line '---'"));
            }
            if cur.scan.tab < cur.scan.comment {
                return Err(self.err("tab character outside a comment or '|' block (use \\t in quotes)"));
            }
            return Ok(true);
        }
    }

    fn content(&self, c: &Cur) -> &str {
        &self.text(c.src)[c.start..c.end]
    }

    // -- pragmas ---------------------------------------------------------------

    fn pragma(&mut self, content: &str) -> R<()> {
        let s = &content[2..];
        let name_len = s
            .bytes()
            .take_while(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
            .count();
        let (name, rest) = s.split_at(name_len);
        let arg = rest.ltrim_sp();
        let malformed = !name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || (!arg.is_empty() && (rest.len() == arg.len() || arg.starts_with(char::is_whitespace)));
        if malformed {
            return Err(self.err("malformed pragma (expected '#+name args', name in a-z)"));
        }
        if name != "include" {
            return Err(self.err(format!("unknown pragma '#+{name}'")));
        }
        let arg = arg.rtrim_sp();
        if arg.is_empty() {
            return Err(self.err("#+include needs a path"));
        }
        self.include(arg)
    }

    /// Push the named file (or, for a glob, every match in sorted order) so
    /// its lines are read next, indented to the pragma's column.
    fn include(&mut self, pattern: &str) -> R<()> {
        let cur = self.cur.unwrap();
        let base = if cur.file != 0 {
            Path::new(&self.files[cur.file as usize])
                .parent()
                .unwrap_or(Path::new(""))
                .to_path_buf()
        } else {
            match &self.base {
                Some(b) => b.clone(),
                None => return Err(self.err("#+include needs a base directory (pass base to loads)")),
            }
        };
        let is_glob = pattern.contains(['*', '?', '[']);
        let paths: Vec<PathBuf> = if is_glob {
            glob::glob(&base, pattern).into_iter().filter(|p| p.is_file()).collect()
        } else {
            vec![base.join(pattern)]
        };
        let mut new = Vec::with_capacity(paths.len());
        for path in paths {
            self.includes += 1;
            if self.includes > MAX_INCLUDES {
                return Err(self.err("too many includes (is there a loop?)"));
            }
            let shown = path.to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).map_err(|e| {
                let what = if is_glob { &shown } else { pattern };
                self.err(format!("cannot include '{what}': {e}"))
            })?;
            let text = match text.strip_prefix('\u{feff}') {
                Some(t) => t.to_owned(),
                None => text,
            };
            validate(&text, Some(&shown))?;
            let has_cr = text.contains('\r');
            // the included document's own leading '---' is dropped
            let (mut pos, mut skip_at) = (0, None);
            loop {
                let (le, next) = find_eol(&text, pos, has_cr);
                let line = &text[pos..le];
                if !is_blank(line.ltrim_sp()) {
                    if is_doc_start(line) {
                        skip_at = Some(pos);
                    }
                    break;
                }
                if next > text.len() {
                    break;
                }
                pos = next;
            }
            let file = self.files.len() as u32;
            self.files.push(shown);
            new.push(Source {
                text,
                pos: 0,
                pad: cur.indent,
                file,
                lineno: 0,
                skip_at,
                has_cr,
            });
        }
        self.srcs.extend(new.into_iter().rev());
        Ok(())
    }

    // -- events ----------------------------------------------------------------

    fn step(&mut self) -> R<Option<Event<'a>>> {
        if let Some(e) = self.queue.pop() {
            return Ok(Some(e));
        }
        if let Some(e) = self.flow.pop_front() {
            return Ok(Some(e));
        }
        match self.state {
            St::Frame => {
                let (list, ind) = {
                    let f = self.stack.last().unwrap();
                    if f.tag {
                        // its value is complete: close the tag's map
                        let f = self.stack.pop().unwrap();
                        self.keys.truncate(f.ks);
                        return Ok(Some(Event::MapEnd));
                    }
                    (f.list, f.ind)
                };
                if !self.skip()? || self.cur.as_ref().unwrap().indent < ind {
                    let f = self.stack.pop().unwrap();
                    self.keys.truncate(f.ks);
                    if self.stack.is_empty() {
                        self.state = St::RootEnd;
                    }
                    return Ok(Some(if f.list { Event::ListEnd } else { Event::MapEnd }));
                }
                if self.cur.as_ref().unwrap().indent > ind {
                    return Err(self.err("unexpected indentation"));
                }
                self.state = St::Frame;
                if list {
                    self.list_item(ind).map(Some)
                } else {
                    self.map_entry(ind).map(Some)
                }
            }
            St::PendEnd { map } => {
                self.state = St::Frame;
                Ok(Some(if map { Event::MapEnd } else { Event::ListEnd }))
            }
            St::Init => self.init().map(Some),
            St::RootEnd => {
                if self.skip()? {
                    return Err(self.err("unexpected content (inconsistent indentation?)"));
                }
                self.state = St::End;
                Ok(None)
            }
            St::End => Ok(None),
        }
    }

    fn push_frame(&mut self, list: bool, ind: usize) {
        self.stack.push(Frame {
            tag: false,
            list,
            ind,
            ks: self.keys.len(),
            set: None,
        });
    }

    fn init(&mut self) -> R<Event<'a>> {
        // an optional leading '---' (any blank or comment lines may come first)
        let (pos, lineno) = (self.srcs[0].pos, self.srcs[0].lineno);
        let mut found = false;
        while let Some(r) = self.read_raw() {
            let line = &self.text(r.src)[r.ls..r.le];
            if is_blank(line.ltrim_sp()) {
                continue;
            }
            found = is_doc_start(line);
            break;
        }
        if !found {
            self.srcs[0].pos = pos;
            self.srcs[0].lineno = lineno;
            self.pushed = None;
        }
        if !self.skip()? {
            return Err(Error::new("empty document".into(), None, None, None));
        }
        let cur = self.cur.unwrap();
        if cur.indent != 0 {
            return Err(self.err("document must start at column 0"));
        }
        let c = split_comment(self.content(&cur)).rtrim_sp();
        if c == "{}" || c == "[]" {
            let map = c == "{}";
            self.cur = None;
            self.queue.push(if map { Event::MapEnd } else { Event::ListEnd });
            self.state = St::RootEnd;
            return Ok(if map { Event::MapStart } else { Event::ListStart });
        }
        let ev = self.block(-1)?.expect("a structural line at column 0 starts a block");
        self.state = St::Frame;
        Ok(ev)
    }

    /// Start the map/list at the next line if it is indented deeper than `parent`.
    fn block(&mut self, parent: isize) -> R<Option<Event<'a>>> {
        if !self.skip()? {
            return Ok(None);
        }
        let cur = self.cur.unwrap();
        if cur.indent as isize <= parent {
            return Ok(None);
        }
        let list = is_list_item(self.content(&cur));
        self.push_frame(list, cur.indent);
        Ok(Some(if list { Event::ListStart } else { Event::MapStart }))
    }

    #[allow(clippy::ptr_arg)] // cloned into the key sets
    fn add_key(&mut self, key: &Cow<'a, str>) -> R<()> {
        if !self.strict {
            return Ok(());
        }
        let f = self.stack.last_mut().unwrap();
        let ks = f.ks;
        let dup = match &f.set {
            Some(s) => s.contains(&**key),
            None => self.keys[ks..].iter().any(|k| k == key),
        };
        if dup {
            return Err(self.err(format!("duplicate key {}", py_str(key))));
        }
        let f = self.stack.last_mut().unwrap();
        match &mut f.set {
            Some(s) => {
                s.insert(key.clone()); // big maps live in the set only
            }
            None if self.keys.len() - ks >= 16 => {
                let mut set: KeySet<'a> = self.keys[ks..].iter().cloned().collect();
                set.insert(key.clone());
                self.keys.truncate(ks);
                f.set = Some(set);
            }
            None => self.keys.push(key.clone()),
        }
        Ok(())
    }

    fn map_entry(&mut self, ind: usize) -> R<Event<'a>> {
        let cur = self.cur.as_ref().unwrap();
        let (src, start) = (cur.src, cur.start);
        let content = &text_of(self.main, &self.srcs, src)[start..cur.end];
        if is_list_item(content) {
            return Err(self.err("list item inside a map; indent list items below their key"));
        }
        match self.split_entry(content, start, &cur.scan, 0)? {
            Some((key, off)) => {
                let key = self.piece(src, key);
                self.add_key(&key)?;
                self.value(off, ind as isize)?;
                Ok(Event::Key(key))
            }
            None => {
                let c = content.as_bytes()[0];
                Err(if hint(c).is_some() {
                    self.bad_start(c as char)
                } else {
                    self.err("expected 'key: value'")
                })
            }
        }
    }

    /// The event that starts a list item (a map's start also queues its first entry).
    fn list_item(&mut self, ind: usize) -> R<Event<'a>> {
        let cur = self.cur.unwrap();
        let content = self.content(&cur);
        if content == "-" {
            self.value(1, ind as isize)?;
            return Ok(self.queue.pop().unwrap());
        }
        let Some(rest) = content.strip_prefix("- ") else {
            return Err(self.err("expected '- ' list item"));
        };
        if rest.starts_with(' ') && !is_blank(rest.ltrim_sp()) {
            return Err(self.err("exactly one space is allowed after '-'"));
        }
        if !is_blank(rest) && !rest.starts_with(['|', '!', '[', '{']) {
            if let Some((key, off)) = self.split_entry(rest, cur.start + 2, &cur.scan, 2)? {
                // '- key: v': treat the line as a map line indented under the dash
                let key = self.piece(cur.src, key);
                let c = self.cur.as_mut().unwrap();
                c.indent = ind + 2;
                c.start += 2;
                for f in [
                    &mut c.scan.tab,
                    &mut c.scan.comment,
                    &mut c.scan.colon,
                    &mut c.scan.colon2,
                ] {
                    if *f != NONE {
                        *f -= 2;
                    }
                }
                self.push_frame(false, ind + 2);
                self.add_key(&key)?;
                self.queue.push(Event::Key(key));
                self.value(off, (ind + 2) as isize)?;
                return Ok(Event::MapStart);
            }
        }
        self.value(2, ind as isize)?;
        Ok(self.queue.pop().unwrap())
    }

    /// The value after 'key:' or '-' on the current line; pushes its events.
    fn value(&mut self, off: usize, parent: isize) -> R<()> {
        let cur = self.cur.as_ref().unwrap();
        let (src, start) = (cur.src, cur.start);
        let content = &text_of(self.main, &self.srcs, src)[start..cur.end];
        let raw = &content[off..];
        let lead = raw.len() - raw.ltrim_sp().len();
        let rest = raw[lead..].rtrim_sp();
        let rs = off + lead;
        self.state = St::Frame;
        if rest.is_empty() || rest.starts_with('#') {
            self.cur = None;
            let ev = self.block(parent)?.unwrap_or(Event::Scalar(Cow::Borrowed("")));
            self.queue.push(ev);
            return Ok(());
        }
        if rest.starts_with('|') {
            if split_comment(rest).rtrim_sp() != "|" {
                return Err(self.err("only a plain '|' block scalar header is supported"));
            }
            self.cur = None;
            let s = self.block_scalar(parent)?;
            self.queue.push(Event::Scalar(Cow::Owned(s)));
            return Ok(());
        }
        if let Some(tl) = tag_len(rest) {
            // '!Name value' is the one-key map {"!Name": value}
            if tag_len(rest[tl..].ltrim_sp()).is_some() {
                return Err(self.err("a value can have only one tag"));
            }
            let after = rest[tl..].ltrim_sp();
            if after.starts_with(['[', '{']) && !matches!(split_comment(after).rtrim_sp(), "{}" | "[]") {
                return Err(self.err("a tag cannot be followed by a flow collection; put the value on indented lines"));
            }
            let tag = self.cow(src, start + rs, start + rs + tl);
            self.stack.push(Frame {
                tag: true,
                list: false,
                ind: 0,
                ks: self.keys.len(),
                set: None,
            });
            self.queue.push(Event::MapStart);
            self.queue.push(Event::Key(tag));
            return self.value(rs + tl, parent);
        }
        let ev = match self.inline(content, &cur.scan, rs, rs + rest.len(), start)? {
            Inl::Typed(ev) => ev,
            Inl::Flow(evs) => {
                let mut evs = VecDeque::from(evs);
                let first = evs.pop_front().expect("a flow value has events");
                self.flow = evs;
                first
            }
            Inl::Scalar(p) => Event::Scalar(self.piece(src, p)),
            Inl::Map => {
                self.state = St::PendEnd { map: true };
                Event::MapStart
            }
            Inl::List => {
                self.state = St::PendEnd { map: false };
                Event::ListStart
            }
        };
        self.cur = None;
        self.queue.push(ev);
        Ok(())
    }

    /// A value on one line: `content[rs..re]` is the trimmed text after the
    /// header; `abs` is the content's offset in its source.
    fn inline(&self, content: &str, sc: &Scan, rs: usize, re: usize, abs: usize) -> R<Inl> {
        let rest = &content[rs..re];
        if rest.starts_with('"') {
            let Some(n) = quoted_len(rest) else {
                return Err(self.err("unterminated or multi-line quoted string"));
            };
            let tail = &rest[n..];
            let t = tail.ltrim_sp();
            if !(t.is_empty() || (t.len() < tail.len() && t.starts_with('#'))) {
                return Err(self.err("unexpected text after quoted string"));
            }
            return self.unquote(&rest[..n], abs + rs).map(Inl::Scalar);
        }
        let end = if sc.comment != NONE && sc.comment >= rs {
            sc.comment.min(re)
        } else {
            re
        };
        let cut = content[rs..end].rtrim_sp();
        if cut == "{}" {
            return Ok(Inl::Map);
        }
        if cut == "[]" {
            return Ok(Inl::List);
        }
        if rest.starts_with(['[', '{']) {
            return flow::parse(rest).map(Inl::Flow).map_err(|m| self.err(m));
        }
        let s = cut.rtrim_sptab();
        if let Some(ev) = typed(s) {
            return Ok(Inl::Typed(ev)); // (a typed scalar has no colon or bad start)
        }
        self.check_start(s)?;
        let colon = if content.starts_with('"') {
            // a quoted key may itself hold ': ', so the scan's first two colons don't tell
            find_colon(s).is_some()
        } else {
            [sc.colon, sc.colon2]
                .iter()
                .any(|&c| c != NONE && c >= rs && c < rs + s.len())
        };
        if colon {
            return Err(self.err("': ' inside a plain value; quote it with double quotes"));
        }
        // (a tab inside s would already have been rejected by `skip`)
        Ok(Inl::Scalar(Piece::Rng(abs + rs, abs + rs + s.len())))
    }

    fn unquote(&self, token: &str, abs: usize) -> R<Piece> {
        match unquote(token) {
            Ok(None) => Ok(Piece::Rng(abs + 1, abs + token.len() - 1)),
            Ok(Some(s)) => Ok(Piece::Own(s)),
            Err(m) => Err(self.err(m)),
        }
    }

    fn bad_start(&self, c: char) -> Error {
        let h = hint(c as u8).unwrap_or("quote it with double quotes");
        let r = if c == '\'' {
            "\"'\"".to_string()
        } else {
            format!("'{c}'")
        };
        self.err(format!("a plain scalar cannot start with {r}: {h}"))
    }

    fn check_start(&self, s: &str) -> R<()> {
        let Some(&c) = s.as_bytes().first() else {
            return Err(self.err("empty value"));
        };
        if BAD_START[c as usize] {
            return Err(self.bad_start(c as char));
        }
        if b"-?:".contains(&c) && s.as_bytes().get(1).is_none_or(|&n| n == b' ' || n == b'\t') {
            return Err(self.err(format!("a plain scalar cannot start with '{}'; quote it", c as char)));
        }
        Ok(())
    }

    /// Split 'key: rest' -> (key, offset of rest), or None if not a map entry.
    /// `abs` is the offset of `content` in its source.
    fn split_entry(&self, content: &str, abs: usize, sc: &Scan, shift: usize) -> R<Option<(Piece, usize)>> {
        if content.starts_with('"') {
            let Some(n) = quoted_len(content) else { return Ok(None) };
            let after = content.as_bytes().get(n..).unwrap_or(&[]);
            if !matches!(after, [b':'] | [b':', b' ' | b'\t', ..]) {
                return Ok(None);
            }
            return Ok(Some((self.unquote(&content[..n], abs)?, n + 1)));
        }
        // the scan stops at the first comment, so a colon it saw comes before it
        if sc.colon == NONE {
            return Ok(None);
        }
        let colon = sc.colon - shift;
        let key = content[..colon].rtrim_sptab();
        self.check_start(key)?;
        if key.ends_with(':') {
            // the key's own colon sits right before the separator ('a:: b')
            return Err(self.err("': ' inside a plain value; quote it with double quotes"));
        }
        Ok(Some((Piece::Rng(abs, abs + key.len()), (colon + 1).min(content.len()))))
    }

    /// Read a '|' block whose header line has been consumed.
    fn block_scalar(&mut self, parent: isize) -> R<String> {
        const TOO_LONG: &str = "whitespace-only line longer than the block indent";
        let mut out = String::new();
        let mut pending = 0; // blank lines not yet known to be inside the block
        let mut ci: Option<usize> = None;
        let mut lead: Vec<(usize, usize, u32)> = Vec::new(); // blank lines before the first text line
        let (mut last_text, mut at_end) = (false, true);
        while let Some(r) = self.read_raw() {
            let line = &self.text(r.src)[r.ls..r.le];
            let trimmed = line.ltrim_sp();
            if trimmed.is_empty() {
                let len = if line.is_empty() { 0 } else { r.pad + line.len() };
                match ci {
                    Some(c) if len > c => return Err(self.err_raw(TOO_LONG, r.n, r.file, "")),
                    None if len > 0 => lead.push((len, r.n, r.file)),
                    _ => {}
                }
                pending += 1;
                last_text = false;
                continue;
            }
            let ind = r.pad + line.len() - trimmed.len();
            match ci {
                None => {
                    if ind as isize <= parent {
                        self.pushed = Some(r);
                        at_end = false;
                        break;
                    }
                    ci = Some(ind);
                    if let Some(&(_, n, f)) = lead.iter().find(|&&(l, _, _)| l > ind) {
                        return Err(self.err_raw(TOO_LONG, n, f, ""));
                    }
                }
                Some(c) if ind < c => {
                    self.pushed = Some(r);
                    at_end = false;
                    break;
                }
                _ => {}
            }
            let c = ci.unwrap();
            if ind == c && trimmed.starts_with('\t') {
                let msg = "a block line cannot start with a tab; use a quoted string";
                return Err(self.err_raw(msg, r.n, r.file, trimmed));
            }
            out.extend(std::iter::repeat_n('\n', pending));
            pending = 0;
            out.push_str(&line[c - r.pad..]);
            out.push('\n');
            last_text = true;
        }
        if at_end && last_text {
            out.pop(); // no final newline at the end of the file, as in YAML
        }
        Ok(out)
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = R<Event<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.step() {
            Ok(ev) => ev.map(Ok),
            Err(e) => {
                self.state = St::End;
                self.queue = Queue::new();
                self.flow.clear();
                Some(Err(e))
            }
        }
    }
}

# miniformat-rs

Minimal, fast Rust port of [miniformat](https://github.com/vivainio/miniformat): a strict config
format with YAML syntax where plain scalars are typed like JSON (`true`, `false`, `null`, numbers; all else is a string), `#+include` splices in files, and
`key: !Name value` loads as `{"!Name": value}`.

**The format is defined by the Python original**: see its
[README](https://github.com/vivainio/miniformat#the-format) for the syntax, the rules, `#+include`
and tags. This crate follows it and runs the same fixture suite (`tests/cases`, synced from
the original), so the two should agree on every input. If they differ, the Python
implementation is the reference and this is a bug.

Two rules trip people up. **Plain scalars are typed like JSON, nothing more**: `port: 80` is an
integer, `ok: true` a bool, `x: null` null, `ratio: 1.5` a float; `debug: yes`, `code: 010` and
`1_000` are strings, and so is anything quoted. A bare `1.10` is the float `1.1`, so quote
versions (`"1.10"`). And **flow syntax is only valid as one line of JSON** (plus empty `{}` and
`[]`), never after a tag: `branches: ["main"]` and `ports: [80, 81]` are fine; `branches: [ main ]`
and `!Join ["a", "b"]` are errors; write those as block lists.

Zero runtime dependencies (serde support and the command line are optional features).

```rust
// tree of owned strings, ints, floats, bools and nulls (Value::{Str, Int, Float, Bool, Null, Map, List})
let cfg = miniformat::load("app.yaml")?;          // or loads(text, Some(base_dir))
cfg.get("db").and_then(|d| d.get("port"));        // Some(Value::Str("5432"))
let text = miniformat::dumps(&cfg);               // canonical form
let flat = miniformat::flatten(&text, Some(dir))?; // same, #+include expanded

// tree that borrows from the input where it can
let cfg = miniformat::loads_borrowed(&text, None)?;

// read forward: events, no tree, keys and scalars borrowed from the input
use miniformat::Event::*;
for ev in miniformat::Reader::new(&text, None)? {
    match ev? {
        Key(k) => ..,          // Cow<str>: Borrowed unless unescaped / re-indented / included
        Scalar(s) => ..,       // a string; also Int(i64), Float(f64), Bool(bool), Null
        MapStart | MapEnd | ListStart | ListEnd => ..,
    }
}
```

Streaming lookups: no tree, stop at the match (so the rest is never read or validated):

```rust
miniformat::get(&text, None, "servers.0.host")?;        // Option<Cow<str>>; dotted path, list indexes are numbers
miniformat::get_as::<u16>(&text, None, "db.port")?;     // parsed with FromStr
miniformat::find(&text, None, &["db", "a.b"])?;         // Found::{Scalar, Int, Float, Bool, Null, Map, List}; slice form allows dots in keys
miniformat::keys(&text, None, &["db"])?;                // keys of a map
miniformat::len(&text, None, &["servers"])?;            // entries of a map or list
let mut r = miniformat::Reader::new(&text, None)?;
r.seek(&["servers", "0"])?;                              // then keep iterating events inside that subtree
```

Optional serde (`miniformat = { version = "..", features = ["serde"] }`): deserialize
straight from the event stream, no tree:

```rust
#[derive(serde::Deserialize)]
struct Config { name: String, db: Db, servers: Vec<Server>, debug: bool }
#[derive(serde::Deserialize)]
struct Db { port: u16 }
let cfg: Config = miniformat::from_str(&text, None)?;   // or from_path("app.yaml")
```

Typed scalars go to the field that wants them (`port: 5432` into `u16`, `debug: true` into
`bool`); a quoted `"5432"` is parsed from its text for a number field, and a `String` field needs
a string (quote `version: "1.10"`). `key:` (empty) and `null` are `None` / `()`. Enums are a string (`mode: Fast`) or a single-key map (`mode:` / `  Slow:` / `    delay: 5`).
`&str` and `Cow<str>` fields borrow from the input. Unknown fields are skipped.

`Reader::strict_keys(false)` skips duplicate-key detection (on by default).
`Reader::skip_value()` consumes the value after a `Key`.

Tags (as in the Python original): `key: !Name value` is the one-key map `{"!Name": value}`, so
`queue: !Ref MyQueue` reads as `queue` -> `{"!Ref": "MyQueue"}`. In events that is
`MapStart, Key("!Ref"), Scalar("MyQueue"), MapEnd`; in lookups the tag is a path segment
(`queue.!Ref`); `dumps` writes such maps back as tags. One tag per value, none on keys or the
root, no `!!`.

JSON in (`miniformat::from_json(text)`, or the CLI below): types are kept. A number miniformat can't
type (an integer beyond 64 bits, a float that overflows) becomes a string with its text. Duplicate
keys and a non-object/array root are errors. `miniformat::plain_value(text)` gives the value of a
plain scalar's text.

CLI (optional `cli` feature): JSON, canonical text, `--get`, and YAML/JSON to miniformat; see
[Command line](#command-line).

`cargo test` runs the shared fixtures plus reader/include tests.
`cargo run --release --example bench -- FILE` times each API in-process.

## Command line

The `miniformat` command is an optional feature, so the library stays dependency-free (the command
needs a YAML parser, `saphyr-parser`). Install it with `cargo install miniformat --features cli`, or
download a binary from the releases page.

```
miniformat FILE                  print JSON
miniformat --fmt FILE            print the canonical text
miniformat --check FILE...       validate files (silent if valid, exit 1 if any is not)
miniformat --get db.port FILE    print one scalar (streams, no tree)
miniformat --from-json [FILE]    JSON (FILE or stdin) to miniformat on stdout
miniformat --from-yaml [FILE]    YAML (FILE or stdin) to miniformat on stdout
miniformat --from-yaml -i FILE...    rewrite YAML files as miniformat, in place
```

### Checking files

`miniformat --check FILE...` parses each file completely, `#+include` files and duplicate keys
included, without building a tree. It prints nothing for valid files, `FILE: line N: message` on
stderr for invalid ones (an error inside an include names the include), and exits 1 if any file is
invalid, so it fits CI and pre-commit hooks. `-q` prints nothing at all (exit status only);
`--list` prints just the names of the invalid files on stdout.

### Converting existing YAML

`--from-yaml` reads real YAML and writes miniformat. Plain scalars are typed by miniformat's rules
(`80`, `true`, `null`, `1.5` are typed; `no`, `007`, `~`, `1_000` stay strings), and quoted ones
stay strings. Watch for floats: YAML `1.10` becomes the float `1.1`, so quote versions first:

| YAML | miniformat |
|---|---|
| `'hello'`, `'it''s'` | `hello`, `it's` (single quotes are not allowed) |
| `{x: 1, y: [a, b]}` | block map and list |
| `text: >` folded block | the folded string, as a `\|` block |
| `&base` / `*base` | expanded |
| `<<: *base` | merged (keys of the map win) |
| `!Ref x` | kept: `{"!Ref": "x"}` |
| `!!str 5` | the string `"5"`, with a warning (`!!` tags are dropped) |

Flow collections are only valid miniformat as one line of JSON, and not after a tag
(`!Join [a, b]`), so `--from-yaml` always writes block form.

Refused with a line number: multiple documents, duplicate keys, mappings or sequences as keys,
tags other than `!Name`. **Comments are not carried over.**

A plain `<<` key is YAML's merge key here, so it must hold a mapping (or a list of mappings),
otherwise it is an error. (Quote it, `"<<"`, for an ordinary key. miniformat itself treats `<<` as an
ordinary key, so files that are already valid miniformat are never converted.)

`-i` rewrites each file (via a temp file and a rename) and keeps going if one fails, exiting 1 at the
end. It will not lose data silently:

- A file that is **already valid miniformat** (most plain block YAML is) is left byte-for-byte as it is,
  comments included, so running it again changes nothing.
- A file with a `#+` pragma such as `#+include` is refused, since YAML sees those as comments and
  the conversion would drop them; `--force` converts anyway.
- The result is read back and compared before anything is written.

Anything else that reads YAML can also feed `--from-json` (`yq -o=json app.yaml | miniformat
--from-json`), but that route types scalars first (`no` is already `false`), so prefer `--from-yaml`.

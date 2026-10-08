# miniformat-rs

Minimal, fast Rust port of [miniformat](https://github.com/vivainio/miniformat): a strict config
format with YAML syntax where every scalar is a string, `#+include` splices in files, and
`key: !Name value` loads as `{"!Name": value}`.

**The format is defined by the Python original**: see its
[README](https://github.com/vivainio/miniformat#the-format) for the syntax, the rules, `#+include`
and tags. This crate follows it and runs the same fixture suite (`tests/cases`, synced from
the original), so the two should agree on every input. If they differ, the Python
implementation is the reference and this is a bug.

Zero runtime dependencies (serde support and the command line are optional features).

```rust
// tree of owned strings
let cfg = miniformat::load("app.yaml")?;          // or loads(text, Some(base_dir))
cfg.get("db").and_then(|d| d.get("port"));        // Some(Value::Str("5432"))
let text = miniformat::dumps(&cfg);               // canonical form

// tree that borrows from the input where it can
let cfg = miniformat::loads_borrowed(&text, None)?;

// read forward: events, no tree, keys and scalars borrowed from the input
use miniformat::Event::*;
for ev in miniformat::Reader::new(&text, None)? {
    match ev? {
        Key(k) => ..,          // Cow<str>: Borrowed unless unescaped / re-indented / included
        Scalar(s) => ..,
        MapStart | MapEnd | ListStart | ListEnd => ..,
    }
}
```

Streaming lookups: no tree, stop at the match (so the rest is never read or validated):

```rust
miniformat::get(&text, None, "servers.0.host")?;        // Option<Cow<str>>; dotted path, list indexes are numbers
miniformat::get_as::<u16>(&text, None, "db.port")?;     // parsed with FromStr
miniformat::find(&text, None, &["db", "a.b"])?;         // Found::{Scalar, Map, List}; slice form allows dots in keys
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

Scalars are strings, so numbers and bools are parsed from their text where your type asks
for them (`port: 5432` into `u16`, `true`/`false` into `bool`); `key:` (empty) is `None` /
`()`. Enums are a string (`mode: Fast`) or a single-key map (`mode:` / `  Slow:` / `    delay: 5`).
`&str` and `Cow<str>` fields borrow from the input. Unknown fields are skipped.

`Reader::strict_keys(false)` skips duplicate-key detection (on by default).
`Reader::skip_value()` consumes the value after a `Key`.

Tags (as in the Python original): `key: !Name value` is the one-key map `{"!Name": value}`, so
`queue: !Ref MyQueue` reads as `queue` -> `{"!Ref": "MyQueue"}`. In events that is
`MapStart, Key("!Ref"), Scalar("MyQueue"), MapEnd`; in lookups the tag is a path segment
(`queue.!Ref`); `dumps` writes such maps back as tags. One tag per value, none on keys or the
root, no `!!`.

JSON in (`miniformat::from_json(text)`, or the CLI below): every scalar becomes a string, numbers keep
the text they were written with (`1.10` stays `1.10`), `true`/`false` become `"true"`/`"false"`,
`null` becomes the empty value. Duplicate keys and a non-object/array root are errors.

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
miniformat --get db.port FILE    print one scalar (streams, no tree)
miniformat --from-json [FILE]    JSON (FILE or stdin) to miniformat on stdout
miniformat --from-yaml [FILE]    YAML (FILE or stdin) to miniformat on stdout
miniformat --from-yaml -i FILE...    rewrite YAML files as miniformat, in place
```

### Converting existing YAML

`--from-yaml` reads real YAML and writes miniformat. Scalars keep the text they were written with
(`no`, `1.10`, `007`, `~` stay as they are), so nothing is retyped:

| YAML | miniformat |
|---|---|
| `'hello'`, `'it''s'` | `hello`, `it's` (single quotes are not allowed) |
| `{x: 1, y: [a, b]}` | block map and list |
| `text: >` folded block | the folded string, as a `\|` block |
| `&base` / `*base` | expanded |
| `<<: *base` | merged (keys of the map win) |
| `!Ref x` | kept: `{"!Ref": "x"}` |
| `!!str 5` | `5`, with a warning (`!!` tags are dropped) |

Refused with a line number: multiple documents, duplicate keys, mappings or sequences as keys,
tags other than `!Name`. **Comments are not carried over.**

`-i` rewrites each file (via a temp file and a rename) and keeps going if one fails, exiting 1 at the
end. It will not lose data silently:

- A file that is **already valid miniformat** (most plain block YAML is) is left byte-for-byte as it is,
  comments included, so running it again changes nothing.
- A file with a `#+` pragma such as `#+include` is refused, since YAML sees those as comments and
  the conversion would drop them; `--force` converts anyway.
- The result is read back and compared before anything is written.

Anything else that reads YAML can also feed `--from-json` (`yq -o=json app.yaml | miniformat
--from-json`), but that route types scalars first (`no` is already `false`), so prefer `--from-yaml`.

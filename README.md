# miniformat-rs

Minimal, fast Rust port of [miniformat](https://github.com/vivainio/miniformat): a strict config
format with YAML syntax where every scalar is a string, `#+include` splices in files, and
`key: !Name value` loads as `{"!Name": value}`.

**The format is defined by the Python original**: see its
[README](https://github.com/vivainio/miniformat#the-format) for the syntax, the rules, `#+include`
and tags. This crate follows it and runs the same fixture suite (`tests/cases`, synced from
the original), so the two should agree on every input. If they differ, the Python
implementation is the reference and this is a bug.

Zero runtime dependencies (serde support is an optional feature).

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

CLI: `miniformat FILE` prints JSON, `miniformat --fmt FILE` prints canonical text,
`miniformat --get db.port FILE` prints one scalar (streaming),
`miniformat --from-json [FILE]` reads JSON (FILE or stdin) and prints miniformat.

`cargo test` runs the shared fixtures plus reader/include tests.
`cargo run --release --example bench -- FILE` times each API in-process.

## Converting existing YAML

There is no YAML parser here (that would break "zero dependencies"); convert through JSON with
whatever you already have. The one thing to get right is that the YAML-to-JSON step must **not
type scalars**, otherwise `no` is already `false` and `1.10` already `1.1` before miniformat
sees them. With PyYAML, load plain scalars as strings:

```
python3 -c 'import yaml,json,sys
class L(yaml.SafeLoader): pass
L.yaml_implicit_resolvers = {}
print(json.dumps(yaml.load(sys.stdin, L)))' < app.yaml | miniformat --from-json > app.mini.yaml
```

A typed converter such as `yq -o=json` also works when you do want the typed values written out
as strings (`true`, `1.1`). Comments, anchors and tags are not carried over by either route.

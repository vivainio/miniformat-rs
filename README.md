# miniformat-rs

Minimal, fast Rust port of [miniformat](../miniformat): a strict config format with
YAML syntax where every scalar is a string and `#+include` splices in files.
Zero runtime dependencies; same fixtures (`tests/cases`) as the Python original.

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

CLI: `miniformat FILE` prints JSON, `miniformat --fmt FILE` prints canonical text,
`miniformat --get db.port FILE` prints one scalar (streaming).

`cargo test` runs the shared fixtures plus reader/include tests.
`cargo run --release --example bench -- FILE` times each API in-process.

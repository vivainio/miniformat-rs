#![cfg(feature = "serde")]

use serde::Deserialize;
use std::borrow::Cow;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, PartialEq)]
struct Config {
    name: String,
    debug: bool,
    ratio: f64,
    db: Db,
    servers: Vec<Server>,
    #[serde(default)]
    timeout: Option<u32>,
    note: Option<String>,
    labels: BTreeMap<String, String>,
    mode: Mode,
    tags: (String, u8),
    letter: char,
    nothing: (),
}

#[derive(Debug, Deserialize, PartialEq)]
struct Db {
    port: u16,
    #[serde(default = "d")]
    retries: i64,
}
fn d() -> i64 {
    3
}

#[derive(Debug, Deserialize, PartialEq)]
struct Server {
    host: String,
    weight: Option<u8>,
}

#[derive(Debug, Deserialize, PartialEq)]
enum Mode {
    Fast,
    Slow { delay: u32 },
    Custom(String),
    Pair(u8, u8),
}

const DOC: &str = r#"
name: my-app
debug: true
ratio: 0.5
db:
  port: 5432
servers:
  - host: a
    weight: 3
  - host: b
note:
labels:
  env: prod
  "x y": z
mode: Fast
tags:
  - t
  - 7
letter: q
nothing:
"#;

#[test]
fn deserializes_a_config() {
    let c: Config = miniformat::from_str(DOC, None).unwrap();
    assert_eq!(c.name, "my-app");
    assert!(c.debug);
    assert_eq!(c.ratio, 0.5);
    assert_eq!(c.db, Db { port: 5432, retries: 3 });
    assert_eq!(
        c.servers,
        [
            Server {
                host: "a".into(),
                weight: Some(3)
            },
            Server {
                host: "b".into(),
                weight: None
            }
        ]
    );
    assert_eq!((c.timeout, c.note), (None, None)); // missing field and empty value
    assert_eq!(c.labels["x y"], "z");
    assert_eq!(c.mode, Mode::Fast);
    assert_eq!(c.tags, ("t".to_string(), 7));
    assert_eq!(c.letter, 'q');
}

#[test]
fn enum_forms() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct W {
        modes: Vec<Mode>,
    }
    let w: W = miniformat::from_str(
        "modes:\n  - Fast\n  - Slow:\n      delay: 5\n  - Custom: hi\n  - Pair:\n      - 1\n      - 2\n",
        None,
    )
    .unwrap();
    assert_eq!(
        w.modes,
        [
            Mode::Fast,
            Mode::Slow { delay: 5 },
            Mode::Custom("hi".into()),
            Mode::Pair(1, 2)
        ]
    );
}

#[test]
fn typed_errors() {
    let e = miniformat::from_str::<Db>("port: eighty\n", None).unwrap_err();
    assert!(e.to_string().contains("invalid u16"), "{e}");
    let e = miniformat::from_str::<Db>("port: 70000\n", None).unwrap_err();
    assert!(e.to_string().contains("invalid u16"), "{e}");
    let e = miniformat::from_str::<Db>("retries: 1\n", None).unwrap_err();
    assert!(e.to_string().contains("missing field `port`"), "{e}");
    let e = miniformat::from_str::<Config>("debug: yes\n", None).unwrap_err();
    assert!(
        e.to_string().contains("missing field") || e.to_string().contains("invalid bool"),
        "{e}"
    );
    // syntax errors keep their line numbers
    let e = miniformat::from_str::<Db>("port: 1\na: b: c\n", None).unwrap_err();
    assert_eq!(e.line(), Some(2), "{e}");
}

#[test]
fn unknown_fields_are_skipped_and_borrowing_works() {
    #[derive(Deserialize)]
    struct B<'a> {
        #[serde(borrow)]
        name: Cow<'a, str>,
        s: &'a str,
    }
    let text = "extra:\n  deep:\n    - 1\n    - {}\nname: n\ns: plain\n";
    let b: B = miniformat::from_str(text, None).unwrap();
    assert!(matches!(b.name, Cow::Borrowed("n")));
    assert_eq!(b.s, "plain");
}

#[test]
fn generic_values_and_includes() {
    let v: BTreeMap<String, Vec<u32>> = miniformat::from_str("a:\n  - 1\n  - 2\nb: []\n", None).unwrap();
    assert_eq!(v["a"], [1, 2]);
    assert!(v["b"].is_empty());
    let dir = std::env::temp_dir().join(format!("mf-serde-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("db.yaml"), "port: 99\n").unwrap();
    std::fs::write(dir.join("app.yaml"), "db:\n  #+include db.yaml\n").unwrap();
    #[derive(Deserialize)]
    struct A {
        db: Db,
    }
    let a: A = miniformat::from_path(dir.join("app.yaml")).unwrap();
    assert_eq!(a.db.port, 99);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn trailing_garbage_is_still_an_error() {
    assert!(miniformat::from_str::<BTreeMap<String, String>>("a: 1\n  b: 2\n", None).is_err());
}

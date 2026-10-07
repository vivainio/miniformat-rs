//! Runs the language-neutral fixtures in tests/cases (shared with the Python original).

use miniformat::{dumps, loads, to_json, Value};
use std::fs;
use std::path::Path;

fn names(dir: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(Path::new("tests/cases").join(dir))
        .unwrap()
        .filter_map(|e| e.ok()?.path().file_stem()?.to_str().map(String::from))
        .collect();
    v.sort();
    v.dedup();
    v
}

fn compact(v: &Value) -> serde_json::Value {
    serde_json::from_str(&to_json(v)).unwrap()
}

#[test]
fn valid() {
    for n in names("valid") {
        let dir = Path::new("tests/cases/valid");
        let Ok(text) = fs::read_to_string(dir.join(format!("{n}.yaml"))) else {
            continue;
        };
        let want: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join(format!("{n}.json"))).unwrap()).unwrap();
        let got = loads(&text, Some(dir)).unwrap_or_else(|e| panic!("{n}: {e}"));
        // preserve_order makes this compare key order too
        assert_eq!(compact(&got).to_string(), want.to_string(), "{n}");
        assert_eq!(loads(&dumps(&got), None).unwrap(), got, "{n}: dumps round trip");
    }
}

#[test]
fn invalid() {
    for n in names("invalid") {
        let dir = Path::new("tests/cases/invalid");
        let Ok(text) = fs::read_to_string(dir.join(format!("{n}.yaml"))) else {
            continue;
        };
        let err = fs::read_to_string(dir.join(format!("{n}.err"))).unwrap();
        let mut it = err.lines();
        let (line, needle) = (it.next().unwrap(), it.next().unwrap_or(""));
        let e = loads(&text, None).expect_err(&n);
        let got = e.line().map_or("-".to_string(), |l| l.to_string());
        assert_eq!(got, line, "{n}: {e}");
        assert!(e.to_string().contains(needle), "{n}: {e} lacks {needle:?}");
    }
}

fn events(text: &str) -> Vec<String> {
    miniformat::Reader::new(text, None)
        .unwrap()
        .map(|e| match e.unwrap() {
            miniformat::Event::MapStart => "{".into(),
            miniformat::Event::MapEnd => "}".into(),
            miniformat::Event::ListStart => "[".into(),
            miniformat::Event::ListEnd => "]".into(),
            miniformat::Event::Key(k) => format!("{k}:"),
            miniformat::Event::Scalar(s) => format!("={s:?}"),
        })
        .collect()
}

#[test]
fn reader_events() {
    let ev = events("a: x\nb:\n  - 1\n  - k: v\n    w: \"q\\n\"\nc: |\n  t\ne: {}\nf:\n");
    assert_eq!(
        ev.join(" "),
        r#"{ a: ="x" b: [ ="1" { k: ="v" w: ="q\n" } ] c: ="t\n" e: { } f: ="" }"#
    );
}

#[test]
fn reader_borrows_plain_scalars() {
    use std::borrow::Cow;
    let text = "key: value\nq: \"plain\"\n";
    for ev in miniformat::Reader::new(text, None).unwrap() {
        if let miniformat::Event::Key(s) | miniformat::Event::Scalar(s) = ev.unwrap() {
            assert!(matches!(s, Cow::Borrowed(_)), "{s:?} was copied");
        }
    }
}

#[test]
fn reader_strict_keys_toggle() {
    let text = "a: 1\na: 2\n";
    assert!(loads(text, None).is_err());
    let n = miniformat::Reader::new(text, None).unwrap().strict_keys(false).count();
    assert_eq!(n, 6);
}

#[test]
fn reader_skip_value() {
    let mut r = miniformat::Reader::new("a:\n  b: 1\n  c: []\nd: 2\n", None).unwrap();
    use miniformat::Event::*;
    assert_eq!(r.next().unwrap().unwrap(), MapStart);
    assert_eq!(r.next().unwrap().unwrap(), Key("a".into()));
    r.skip_value().unwrap();
    assert_eq!(r.next().unwrap().unwrap(), Key("d".into()));
}

#[test]
fn includes_through_reader() {
    let dir = std::env::temp_dir().join(format!("mf-inc-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("conf.d")).unwrap();
    std::fs::write(dir.join("db.yaml"), "---\nhost: h\nnote: |\n  a\n").unwrap();
    std::fs::write(dir.join("conf.d/10.yaml"), "x: 1\n").unwrap();
    std::fs::write(dir.join("conf.d/2.yaml"), "y: 2\n").unwrap();
    let text = "db:\n  #+include db.yaml\nl:\n  #+include conf.d/*.yaml\nz: 1\n";
    let v = loads(text, Some(&dir)).unwrap();
    assert_eq!(
        to_json(&v).replace([' ', '\n'], ""),
        r#"{"db":{"host":"h","note":"a\n"},"l":{"x":"1","y":"2"},"z":"1"}"#
    );
    let e = loads("a:\n  #+include conf.d/2.yaml\n  y: 3\n", Some(&dir)).unwrap_err();
    assert!(
        e.to_string().contains("duplicate key 'y'") && e.line() == Some(3),
        "{e}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn borrowed_tree_matches_owned() {
    let text = "a: x\nl:\n  - \"q\"\n  - k: \"e\\n\"\n";
    let owned = loads(text, None).unwrap();
    let b = miniformat::loads_borrowed(text, None).unwrap();
    assert_eq!(dumps(&owned), dumps(&b));
    assert_eq!(b.get("a").and_then(|v| v.as_str()), Some("x"));
}

const DOC: &str = "name: app\ndb:\n  port: 5432\n  opts: {}\nservers:\n  - host: a\n    tags:\n      - x\n      - y\n  - host: b\nflag: yes\n";

#[test]
fn lookups() {
    use miniformat::{find, get, get_as, keys, len, Found};
    assert_eq!(get(DOC, None, "name").unwrap().as_deref(), Some("app"));
    assert_eq!(get(DOC, None, "db.port").unwrap().as_deref(), Some("5432"));
    assert_eq!(get(DOC, None, "servers.1.host").unwrap().as_deref(), Some("b"));
    assert_eq!(get(DOC, None, "servers.0.tags.1").unwrap().as_deref(), Some("y"));
    assert_eq!(get(DOC, None, "servers.5.host").unwrap(), None);
    assert_eq!(get(DOC, None, "db").unwrap(), None); // a map, not a scalar
    assert_eq!(get(DOC, None, "db.port.x").unwrap(), None);
    assert_eq!(get(DOC, None, "servers.host").unwrap(), None);
    assert_eq!(get_as::<u16>(DOC, None, "db.port").unwrap(), Some(5432));
    assert_eq!(get_as::<u16>(DOC, None, "nope").unwrap(), None);
    assert!(get_as::<u16>(DOC, None, "name")
        .unwrap_err()
        .to_string()
        .contains("cannot parse"));
    assert_eq!(find(DOC, None, &["db", "opts"]).unwrap(), Some(Found::Map));
    assert_eq!(find(DOC, None, &["servers"]).unwrap(), Some(Found::List));
    assert_eq!(find(DOC, None, &[]).unwrap(), Some(Found::Map));
    let k = keys(DOC, None, &[]).unwrap().unwrap();
    assert_eq!(
        k.iter().map(|c| &**c).collect::<Vec<_>>(),
        ["name", "db", "servers", "flag"]
    );
    assert_eq!(len(DOC, None, &["servers"]).unwrap(), Some(2));
    assert_eq!(len(DOC, None, &["db"]).unwrap(), Some(2));
    assert_eq!(len(DOC, None, &["name"]).unwrap(), None);
}

#[test]
fn seek_then_iterate_a_subtree() {
    use miniformat::Event::*;
    let mut r = miniformat::Reader::new(DOC, None).unwrap();
    assert_eq!(r.seek(&["servers", "0", "tags"]).unwrap(), Some(ListStart));
    let rest: Vec<_> = r.take(3).map(|e| e.unwrap()).collect();
    assert_eq!(rest, [Scalar("x".into()), Scalar("y".into()), ListEnd]);
}

#[test]
fn lookup_stops_early() {
    // everything after the match is never read, so it is not validated
    assert_eq!(
        miniformat::get("a: 1\nb: [oops\n", None, "a").unwrap().as_deref(),
        Some("1")
    );
    assert!(miniformat::get("a: 1\nb: [oops\n", None, "b").is_err());
}

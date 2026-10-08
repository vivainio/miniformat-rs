#![cfg(feature = "cli")]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_miniformat"))
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mf-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const YAML: &str = "# comment
plain: 'hello'
escaped: 'it''s'
colon: 'a: b'
typed: 'yes'
num: 007
nulls: [~, null]
flow: {x: 1, y: [a, b]}
base: &base
  host: h
  port: 1
derived:
  <<: *base
  port: 2
copy: *base
text: >
  folded
  text
queue: !Ref MyQueue
";

#[test]
fn yaml_to_stdout_keeps_text_and_reads_back() {
    let mut child = bin()
        .arg("--from-yaml")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(YAML.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains("'hello'") && !text.contains("''"), "{text}");
    let v = miniformat::loads(&text, None).unwrap();
    let got: serde_json::Value = serde_json::from_str(&miniformat::to_json(&v)).unwrap();
    let want: serde_json::Value = serde_json::from_str(concat!(
        r#"{"plain":"hello","escaped":"it's","colon":"a: b","typed":"yes","num":"007","nulls":["~","null"],"#,
        r#""flow":{"x":"1","y":["a","b"]},"base":{"host":"h","port":"1"},"#,
        r#""derived":{"port":"2","host":"h"},"copy":{"host":"h","port":"1"},"#,
        r#""text":"folded text\n","queue":{"!Ref":"MyQueue"}}"#
    ))
    .unwrap();
    assert_eq!(got, want);
}

#[test]
fn in_place_rewrites_yaml_and_leaves_miniformat_alone() {
    let d = temp("inplace");
    let (a, b) = (d.join("a.yaml"), d.join("b.yaml"));
    std::fs::write(&a, YAML).unwrap();
    let fine = "k:\n  - v   # a comment\n";
    std::fs::write(&b, fine).unwrap();
    let out = bin().args(["--from-yaml", "-i"]).arg(&a).arg(&b).output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("converted") && stderr(&out).contains("already miniformat"));
    assert!(!std::fs::read_to_string(&a)
        .unwrap()
        .contains('\''.to_string().repeat(2).as_str()));
    miniformat::load(&a).unwrap(); // it is miniformat now
    assert_eq!(
        std::fs::read_to_string(&b).unwrap(),
        fine,
        "valid miniformat is not touched"
    );
    // running it again changes nothing
    let again = std::fs::read_to_string(&a).unwrap();
    assert!(bin()
        .args(["--from-yaml", "-i"])
        .arg(&a)
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(std::fs::read_to_string(&a).unwrap(), again);
    assert!(std::fs::read_dir(&d)
        .unwrap()
        .all(|e| !e.unwrap().file_name().to_string_lossy().contains("tmp")));
}

#[test]
fn in_place_refuses_what_would_lose_data() {
    let d = temp("refuse");
    let pragma = d.join("p.yaml");
    std::fs::write(&pragma, "a: 'x'\n#+include more.yaml\n").unwrap();
    let multi = d.join("m.yaml");
    std::fs::write(&multi, "a: 'x'\n---\nb: 2\n").unwrap();
    let dup = d.join("d.yaml");
    std::fs::write(&dup, "a: 'x'\na: 'y'\n").unwrap();
    let good = d.join("g.yaml");
    std::fs::write(&good, "a: 'x'\n").unwrap();
    let before: Vec<_> = [&pragma, &multi, &dup]
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    // one bad file does not stop the others; exit status says something failed
    let out = bin()
        .args(["--from-yaml", "-i"])
        .args([&pragma, &multi, &dup, &good])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("pragma") && err.contains("multiple documents") && err.contains("duplicate key"),
        "{err}"
    );
    let after: Vec<_> = [&pragma, &multi, &dup]
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    assert_eq!(before, after, "failed files are untouched");
    assert_eq!(std::fs::read_to_string(&good).unwrap(), "a: x\n");
    // --force converts the pragma file (the pragma line is dropped)
    let out = bin()
        .args(["--from-yaml", "-i", "--force"])
        .arg(&pragma)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&pragma).unwrap(), "a: x\n");
}

#[test]
fn json_and_usage() {
    let mut child = bin()
        .arg("--from-json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"a": 1.10, "b": [true, null]}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(stdout(&out), "a: 1.10\nb:\n  - true\n  - \"\"\n");
    assert_eq!(bin().arg("-i").output().unwrap().status.code(), Some(2)); // -i needs --from-yaml
    assert_eq!(bin().output().unwrap().status.code(), Some(2));
}

#[test]
fn check_validates_files() {
    let d = temp("check");
    let ok = d.join("ok.yaml");
    std::fs::write(&ok, "a:\n  - 1\n  - k: v\n").unwrap();
    let dup = d.join("dup.yaml");
    std::fs::write(&dup, "a: 1\nb: 2\na: 3\n").unwrap();
    let inc = d.join("inc.yaml");
    std::fs::write(&inc, "x:\n  #+include part.yaml\n").unwrap();
    std::fs::write(d.join("part.yaml"), "y: 'single'\n").unwrap();
    let missing = d.join("nope.yaml");

    let out = bin().arg("--check").arg(&ok).output().unwrap();
    assert!(
        out.status.success() && out.stdout.is_empty() && out.stderr.is_empty(),
        "silent when valid"
    );

    let out = bin().arg("--check").args([&ok, &dup, &inc, &missing]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("dup.yaml: line 3: duplicate key"), "{err}");
    assert!(
        err.contains("part.yaml: line 1") && err.contains("single quotes"),
        "an include reports its own file: {err}"
    );
    assert!(err.contains("nope.yaml"), "{err}");
    assert!(!err.contains("ok.yaml"), "{err}");

    let out = bin().args(["--check", "-q"]).args([&ok, &dup]).output().unwrap();
    assert!(out.status.code() == Some(1) && out.stdout.is_empty() && out.stderr.is_empty());

    let out = bin()
        .args(["--check", "--list"])
        .args([&ok, &dup, &inc])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), format!("{}\n{}\n", dup.display(), inc.display()));
    assert!(out.stderr.is_empty());

    assert_eq!(
        bin().arg("--check").output().unwrap().status.code(),
        Some(2),
        "needs files"
    );
    assert_eq!(
        bin().arg("-q").arg(&ok).output().unwrap().status.code(),
        Some(2),
        "-q is for --check"
    );
}

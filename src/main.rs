mod yaml;

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: miniformat [--fmt] FILE         print JSON, or the canonical text with --fmt
       miniformat --check [-q|--list] FILE...   validate files; quiet when valid, exit 1 if any is not
                                      (-q: no output at all, --list: print only the invalid names)
       miniformat --get PATH FILE     print the scalar at a dotted path (db.port); streams, no tree
       miniformat --from-json [FILE]  JSON (FILE or stdin) to miniformat on stdout
       miniformat --from-yaml [FILE]  YAML (FILE or stdin) to miniformat on stdout
       miniformat --from-yaml -i FILE...   rewrite YAML files as miniformat in place
                                      (--force: also files with #+ pragmas, which would be dropped)";

#[derive(Default)]
struct Opts {
    fmt: bool,
    get: Option<String>,
    from_json: bool,
    from_yaml: bool,
    in_place: bool,
    force: bool,
    check: bool,
    quiet: bool,
    list: bool,
    files: Vec<String>,
}

fn main() -> ExitCode {
    let mut o = Opts::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--fmt" => o.fmt = true,
            "--from-json" => o.from_json = true,
            "--from-yaml" => o.from_yaml = true,
            "-i" | "--in-place" => o.in_place = true,
            "--force" => o.force = true,
            "--check" => o.check = true,
            "-q" | "--quiet" => o.quiet = true,
            "--list" => o.list = true,
            "--get" => match args.next() {
                Some(p) => o.get = Some(p),
                None => return usage(),
            },
            _ => o.files.push(a),
        }
    }
    if (o.in_place || o.force) && !o.from_yaml || (o.quiet || o.list) && !o.check {
        return usage();
    }
    if o.check {
        return check(&o);
    }
    if o.from_yaml {
        return from_yaml(&o);
    }
    if o.from_json {
        return match o.files.as_slice() {
            [] => from_json(None),
            [f] => from_json(Some(f.as_str()).filter(|f| *f != "-")),
            _ => usage(),
        };
    }
    if o.files.len() != 1 || o.files[0].starts_with('-') {
        return usage();
    }
    let file = &o.files[0];
    if let Some(path) = &o.get {
        return query(file, path);
    }
    let v = match miniformat::load(file) {
        Ok(v) => v,
        Err(e) if e.file().is_some() || e.line().is_none() => {
            eprintln!("{}{e}", if e.line().is_none() { "miniformat: " } else { "" });
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("{file}: {e}");
            return ExitCode::from(1);
        }
    };
    let out = if o.fmt {
        miniformat::dumps(&v)
    } else {
        miniformat::to_json(&v) + "\n"
    };
    let _ = std::io::stdout().lock().write_all(out.as_bytes());
    ExitCode::SUCCESS
}

/// `--check`: parse every file completely (includes too) and report the invalid ones.
fn check(o: &Opts) -> ExitCode {
    if o.files.is_empty() || o.files.iter().any(|f| f.starts_with('-')) {
        return usage();
    }
    let mut bad = false;
    for f in &o.files {
        if let Err(msg) = check_file(f) {
            bad = true;
            if o.list {
                println!("{f}");
            } else if !o.quiet {
                eprintln!("{msg}");
            }
        }
    }
    ExitCode::from(bad as u8)
}

fn check_file(file: &str) -> Result<(), String> {
    let text = read(Some(file))?;
    let base = std::path::absolute(file).ok();
    let reader = miniformat::Reader::new(&text, base.as_deref().and_then(Path::parent));
    // events only: nothing is built, but all of it is read, duplicate keys included
    let r = reader.and_then(|r| r.into_iter().try_for_each(|ev| ev.map(drop)));
    r.map_err(|e| {
        if e.file().is_some() || e.line().is_none() {
            e.to_string()
        } else {
            format!("{file}: {e}")
        }
    })
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn read(file: Option<&str>) -> Result<String, String> {
    let r = match file {
        Some(f) => std::fs::read_to_string(f),
        None => std::io::read_to_string(std::io::stdin()),
    };
    r.map_err(|e| format!("miniformat: {}: {e}", file.unwrap_or("<stdin>")))
}

/// `--get`: stream to one scalar. Exit 0 and print it, 1 if it is missing.
fn query(file: &str, path: &str) -> ExitCode {
    let text = match read(Some(file)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let base = std::path::absolute(file).ok();
    match miniformat::get(&text, base.as_deref().and_then(|p| p.parent()), path) {
        Ok(Some(v)) => {
            let _ = std::io::stdout().lock().write_all(v.as_bytes());
            println!();
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("{file}: no scalar at {path}");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("{file}: {e}");
            ExitCode::from(1)
        }
    }
}

fn from_json(file: Option<&str>) -> ExitCode {
    let name = file.unwrap_or("<stdin>");
    let text = match read(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    match miniformat::from_json(&text) {
        Ok(v) => {
            let _ = std::io::stdout().lock().write_all(miniformat::dumps(&v).as_bytes());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{name}: {e}");
            ExitCode::from(1)
        }
    }
}

fn from_yaml(o: &Opts) -> ExitCode {
    if !o.in_place {
        let file = match o.files.as_slice() {
            [] => None,
            [f] => Some(f.as_str()).filter(|f| *f != "-"),
            _ => return usage(),
        };
        return match yaml_to_text(file, file.unwrap_or("<stdin>"), o.force) {
            Ok((out, warnings)) => {
                warn(file.unwrap_or("<stdin>"), &warnings);
                let _ = std::io::stdout().lock().write_all(out.as_bytes());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::from(1)
            }
        };
    }
    if o.files.is_empty() || o.files.iter().any(|f| f == "-") {
        return usage();
    }
    let mut failed = false;
    for f in &o.files {
        match rewrite_in_place(f, o.force) {
            Ok(Status::Converted(warnings)) => {
                warn(f, &warnings);
                eprintln!("{f}: converted");
            }
            Ok(Status::Unchanged) => eprintln!("{f}: already miniformat, left as is"),
            Err(e) => {
                eprintln!("{e}");
                failed = true;
            }
        }
    }
    ExitCode::from(failed as u8)
}

fn warn(file: &str, warnings: &[String]) {
    for w in warnings {
        eprintln!("{file}: warning: {w}");
    }
}

enum Status {
    Converted(Vec<String>),
    Unchanged,
}

/// A `#+name` line is a pragma to miniformat but just a comment to YAML, so
/// converting would silently drop it.
fn has_pragma(text: &str) -> Option<usize> {
    text.lines().position(|l| {
        let l = l.trim_start_matches(' ');
        l.starts_with("#+") && !matches!(l[2..].chars().next(), None | Some(' ') | Some('\t'))
    })
}

/// YAML text to canonical miniformat (plus warnings), checking it reads back the same.
fn yaml_to_text(file: Option<&str>, name: &str, force: bool) -> Result<(String, Vec<String>), String> {
    let text = read(file)?;
    convert_text(&text, name, force)
}

fn convert_text(text: &str, name: &str, force: bool) -> Result<(String, Vec<String>), String> {
    if let (Some(n), false) = (has_pragma(text), force) {
        return Err(format!(
            "{name}: line {}: a #+ pragma (e.g. #+include) would be dropped by the conversion; use --force to convert anyway",
            n + 1
        ));
    }
    let c = yaml::convert(text).map_err(|e| match e.line {
        Some(n) => format!("{name}: line {n}: {}", e.msg),
        None => format!("{name}: {}", e.msg),
    })?;
    let out = miniformat::dumps(&c.value);
    match miniformat::loads(&out, None) {
        Ok(back) if back == c.value => Ok((out, c.warnings)),
        _ => Err(format!(
            "{name}: internal error: the result does not read back the same; not written"
        )),
    }
}

fn rewrite_in_place(file: &str, force: bool) -> Result<Status, String> {
    let text = read(Some(file))?;
    let base = std::path::absolute(file).ok();
    if miniformat::loads(&text, base.as_deref().and_then(Path::parent)).is_ok() {
        return Ok(Status::Unchanged); // already valid miniformat (which includes plain block YAML)
    }
    let (out, warnings) = convert_text(&text, file, force)?;
    // write beside the file, then rename over it, so a failure never leaves half a file
    let tmp = format!("{file}.miniformat-tmp");
    let io = |e: std::io::Error| format!("miniformat: {file}: {e}");
    std::fs::write(&tmp, out).map_err(io)?;
    if let Ok(meta) = std::fs::metadata(file) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, file).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io(e)
    })?;
    Ok(Status::Converted(warnings))
}

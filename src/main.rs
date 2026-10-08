use std::io::Write;
use std::process::ExitCode;

const USAGE: &str = "usage: miniformat [--fmt] FILE      (prints JSON, or the canonical text with --fmt)
       miniformat --get PATH FILE  (prints the scalar at a dotted path, e.g. db.port; streams, no tree)
       miniformat --from-json [FILE]  (reads JSON from FILE or stdin, prints miniformat;
                                       e.g. yq -o=json app.yaml | miniformat --from-json)";

fn main() -> ExitCode {
    let mut fmt = false;
    let mut get = None;
    let mut from_json = false;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--fmt" {
            fmt = true;
        } else if a == "--from-json" {
            from_json = true;
        } else if a == "--get" {
            get = args.next();
            if get.is_none() {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        } else {
            files.push(a);
        }
    }
    if from_json {
        return convert_json(files.first().map(String::as_str).filter(|f| *f != "-"));
    }
    if files.len() != 1 || files[0].starts_with('-') {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    if let Some(path) = get {
        return query(&files[0], &path);
    }
    let v = match miniformat::load(&files[0]) {
        Ok(v) => v,
        Err(e) if e.file().is_some() || e.line().is_none() => {
            eprintln!("{}{e}", if e.line().is_none() { "miniformat: " } else { "" });
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("{}: {e}", files[0]);
            return ExitCode::from(1);
        }
    };
    let out = if fmt {
        miniformat::dumps(&v)
    } else {
        miniformat::to_json(&v) + "\n"
    };
    let _ = std::io::stdout().lock().write_all(out.as_bytes());
    ExitCode::SUCCESS
}

/// `--get`: stream to one scalar. Exit 0 and print it, 1 if it is missing.
fn query(file: &str, path: &str) -> ExitCode {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("miniformat: {e}");
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

/// `--from-json`: JSON from a file or stdin to miniformat on stdout.
fn convert_json(file: Option<&str>) -> ExitCode {
    let text = match file {
        Some(f) => std::fs::read_to_string(f),
        None => std::io::read_to_string(std::io::stdin()),
    };
    let name = file.unwrap_or("<stdin>");
    let text = match text {
        Ok(t) => t,
        Err(e) => {
            eprintln!("miniformat: {name}: {e}");
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

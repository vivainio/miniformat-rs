//! Tiny glob: `*`, `?`, `[seq]`, `[!seq]` within a path component, and `**`
//! for any depth of directories. Hidden (dot) entries need an explicit dot.

use std::fs;
use std::path::{Path, PathBuf};

pub fn glob(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let comps: Vec<&str> = pattern.split('/').filter(|c| !c.is_empty()).collect();
    let mut out = Vec::new();
    expand(base, &comps, &mut out);
    out.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    out.dedup();
    out
}

fn entries(dir: &Path) -> Vec<(String, PathBuf)> {
    match fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn expand(dir: &Path, comps: &[&str], out: &mut Vec<PathBuf>) {
    let Some((&comp, rest)) = comps.split_first() else {
        out.push(dir.to_path_buf());
        return;
    };
    if comp == "**" {
        if rest.is_empty() {
            for (name, p) in entries(dir) {
                if name.starts_with('.') {
                    continue;
                }
                if p.is_dir() {
                    expand(&p, comps, out);
                }
                out.push(p);
            }
        } else {
            expand(dir, rest, out);
            for (name, p) in entries(dir) {
                if !name.starts_with('.') && p.is_dir() {
                    expand(&p, comps, out);
                }
            }
        }
    } else if comp.contains(['*', '?', '[']) {
        let pat: Vec<char> = comp.chars().collect();
        for (name, p) in entries(dir) {
            if name.starts_with('.') && !comp.starts_with('.') {
                continue;
            }
            let n: Vec<char> = name.chars().collect();
            if matches(&pat, &n) && (rest.is_empty() || p.is_dir()) {
                expand(&p, rest, out);
            }
        }
    } else {
        let p = dir.join(comp);
        if rest.is_empty() {
            if p.exists() {
                out.push(p);
            }
        } else if p.is_dir() {
            expand(&p, rest, out);
        }
    }
}

fn matches(pat: &[char], s: &[char]) -> bool {
    match pat.first() {
        None => s.is_empty(),
        Some('*') => (0..=s.len()).any(|k| matches(&pat[1..], &s[k..])),
        Some('?') => !s.is_empty() && matches(&pat[1..], &s[1..]),
        Some('[') => {
            let Some(&c) = s.first() else { return false };
            match class(&pat[1..], c) {
                Some((hit, used)) => hit && matches(&pat[1 + used..], &s[1..]),
                None => c == '[' && matches(&pat[1..], &s[1..]), // no closing ]: literal
            }
        }
        Some(&p) => s.first() == Some(&p) && matches(&pat[1..], &s[1..]),
    }
}

/// Match `c` against a class body (after `[`); returns (hit, chars consumed
/// including the closing `]`), or None if the class is unterminated.
fn class(p: &[char], c: char) -> Option<(bool, usize)> {
    let (neg, mut k) = if p.first() == Some(&'!') { (true, 1) } else { (false, 0) };
    let start = k;
    let mut hit = false;
    loop {
        let &ch = p.get(k)?;
        if ch == ']' && k > start {
            return Some((hit != neg, k + 1));
        }
        if p.get(k + 1) == Some(&'-') && p.get(k + 2).is_some_and(|&e| e != ']') {
            hit |= ch <= c && c <= p[k + 2];
            k += 3;
        } else {
            hit |= ch == c;
            k += 1;
        }
    }
}
